//! The GitHub ship plugin, built in: `treetop plugin github` serves it. It
//! reads pull requests through `gh`, so the user's own login and host carry
//! over and treetop never holds a token, and it only ever reads: it never
//! pushes, opens, merges or reviews a PR.
//!
//! `status` is one `gh pr list` for the whole repository, matched to the
//! branches asked about, so a pool of twenty trees costs one call a minute.
//! `view` reads one PR whole for its popup: `gh pr view`, its review threads
//! over GraphQL, and the end of each failed check's log.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::child::{self, End};
use crate::coderabbit::{self, Finding};
use crate::plugin::{Code, Error, PROTOCOL};
use crate::ship::{self, Asked, BotReview, CheckView, PrView, Reply, Ship, Stage, Thread};

/// How long one `gh` command gets.
const GH_LIMIT: Duration = Duration::from_secs(9);

/// How many of the repository's latest PRs `status` reads.
const PRS: &str = "100";

/// The lines of a failure's log an agent gets.
const LOG_LINES: usize = 200;

const FIELDS: &str = "number,headRefName,headRefOid,state,isDraft,reviewDecision,\
                      statusCheckRollup,url,isCrossRepository";

/// A pull request, as `gh pr list --json` gives it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    head_ref_oid: Option<String>,
    /// `OPEN`, `CLOSED` or `MERGED`.
    #[serde(default)]
    state: String,
    #[serde(default)]
    is_draft: bool,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or empty.
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    status_check_rollup: Option<Vec<Check>>,
    #[serde(default)]
    url: Option<String>,
    /// From a fork, whose branch name says nothing about ours.
    #[serde(default)]
    is_cross_repository: bool,
}

/// One check on a PR: a check run (`status`, `conclusion`) or a commit
/// status (`state`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Check {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    context: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
    #[serde(default)]
    workflow_name: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Passed,
    Failed,
    Pending,
}

impl Check {
    fn name(&self) -> &str {
        self.name
            .as_deref()
            .or(self.context.as_deref())
            .unwrap_or("a check")
    }

    fn outcome(&self) -> Outcome {
        if let Some(state) = &self.state {
            return match state.as_str() {
                "FAILURE" | "ERROR" => Outcome::Failed,
                "PENDING" | "EXPECTED" => Outcome::Pending,
                _ => Outcome::Passed,
            };
        }
        if self.status.as_deref().is_some_and(|s| s != "COMPLETED") {
            return Outcome::Pending;
        }
        match self.conclusion.as_deref() {
            Some("FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE") => {
                Outcome::Failed
            }
            _ => Outcome::Passed,
        }
    }

    /// The Actions run behind a check run, from its link:
    /// `https://github.com/o/r/actions/runs/<run>/job/<job>`.
    fn run_id(&self) -> Option<&str> {
        let url = self.details_url.as_deref()?;
        let (_, rest) = url.split_once("/actions/runs/")?;
        let id = rest.split('/').next()?;
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
    }

    /// The Actions job behind a check run, from the same link.
    fn job_id(&self) -> Option<&str> {
        let url = self.details_url.as_deref()?;
        let (_, rest) = url.split_once("/job/")?;
        let id = rest.split(['/', '?', '#']).next()?;
        (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
    }

    fn view(&self) -> CheckView {
        let finished = |at: &Option<String>| at.as_deref().and_then(crate::pool::seconds);
        let skipped = matches!(self.conclusion.as_deref(), Some("SKIPPED" | "NEUTRAL"));
        CheckView {
            name: self.name().to_string(),
            outcome: match self.outcome() {
                Outcome::Passed if skipped => ship::Outcome::Skipped,
                Outcome::Passed => ship::Outcome::Passed,
                Outcome::Failed => ship::Outcome::Failed,
                Outcome::Pending => ship::Outcome::Pending,
            },
            workflow: self.workflow_name.clone().unwrap_or_default(),
            seconds: finished(&self.completed_at)
                .zip(finished(&self.started_at))
                .and_then(|(end, start)| end.checked_sub(start)),
            url: self.details_url.clone().or_else(|| self.target_url.clone()),
            log: None,
        }
    }
}

impl Pr {
    fn checks(&self) -> &[Check] {
        self.status_check_rollup.as_deref().unwrap_or_default()
    }

    fn failed(&self) -> Vec<&Check> {
        self.checks()
            .iter()
            .filter(|c| c.outcome() == Outcome::Failed)
            .collect()
    }

    fn review(&self) -> &str {
        self.review_decision.as_deref().unwrap_or_default()
    }

    /// Where the PR stands, and how the column and the detail pane say it.
    fn ship(&self) -> Ship {
        let number = format!("#{}", self.number);
        let failed = self.failed();
        let pending = self
            .checks()
            .iter()
            .filter(|c| c.outcome() == Outcome::Pending)
            .count();
        let passed = self.checks().len() - failed.len() - pending;
        let (stage, label) = match self.state.as_str() {
            "MERGED" => (Stage::Merged, format!("{number} merged")),
            "CLOSED" => (Stage::Closed, format!("{number} closed")),
            _ if !failed.is_empty() => (Stage::CiFailed, format!("{number} ✗ ci")),
            _ if self.review() == "CHANGES_REQUESTED" => {
                (Stage::Changes, format!("{number} changes"))
            }
            _ if self.is_draft => (Stage::Draft, format!("{number} draft")),
            _ if pending > 0 => (Stage::Checks, format!("{number} ci …")),
            _ if self.review() == "APPROVED" => (Stage::Approved, format!("{number} approved")),
            _ => (Stage::Review, format!("{number} review")),
        };
        let mut detail = Vec::new();
        if !self.checks().is_empty() {
            let mut counts = vec![format!("{passed} passed")];
            if pending > 0 {
                counts.push(format!("{pending} running"));
            }
            if !failed.is_empty() {
                let names: Vec<&str> = failed.iter().map(|c| c.name()).collect();
                counts.push(format!("{} failed: {}", failed.len(), names.join(", ")));
            }
            detail.push(format!("checks {}", counts.join(", ")));
        }
        let review = match self.review() {
            "APPROVED" => Some("approved"),
            "CHANGES_REQUESTED" => Some("changes requested"),
            "REVIEW_REQUIRED" => Some("required"),
            _ => None,
        };
        if let Some(review) = review.filter(|_| self.state == "OPEN") {
            detail.push(format!("review {review}"));
        }
        Ship {
            branch: self.head_ref_name.clone(),
            stage,
            label,
            url: self.url.clone(),
            head: self.head_ref_oid.clone(),
            detail,
            plugin: String::new(),
        }
    }
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(Code::Failed, message)
}

/// Runs `gh` with `args` in `dir` and returns what it printed.
fn gh(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let mut command = Command::new("gh");
    command.args(args).current_dir(dir);
    let ran = child::run(&mut command, None, GH_LIMIT).map_err(|err| {
        Error::new(
            Code::Unavailable,
            if err.kind() == std::io::ErrorKind::NotFound {
                "gh is not installed".to_string()
            } else {
                format!("gh: {err}")
            },
        )
    })?;
    let stderr = String::from_utf8_lossy(&ran.stderr).trim().to_string();
    match ran.end {
        End::Exited(status) if status.success() => {
            Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
        }
        End::Exited(_) => {
            let code = if stderr.contains("gh auth login") {
                Code::Auth
            } else {
                Code::Unavailable
            };
            let message = stderr.lines().next().unwrap_or("gh failed").to_string();
            Err(Error::new(code, message))
        }
        End::TimedOut | End::HeldOpen(_) => Err(Error::new(
            Code::Unavailable,
            format!("gh {} took over {GH_LIMIT:?}", args.join(" ")),
        )),
    }
}

/// The newest PR from this repository for each branch in `trees`.
fn matched(prs: Vec<Pr>, trees: &[Asked]) -> Vec<Ship> {
    let branches: BTreeSet<&str> = trees.iter().map(|t| t.branch.as_str()).collect();
    let mut newest: Vec<Pr> = Vec::new();
    for pr in prs
        .into_iter()
        .filter(|pr| !pr.is_cross_repository && branches.contains(pr.head_ref_name.as_str()))
    {
        match newest
            .iter_mut()
            .find(|seen| seen.head_ref_name == pr.head_ref_name)
        {
            Some(seen) if seen.number < pr.number => *seen = pr,
            Some(_) => {}
            None => newest.push(pr),
        }
    }
    newest.iter().map(Pr::ship).collect()
}

fn status(args: &Value) -> Result<Value, Error> {
    let dir = args
        .get("dir")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("no dir given"))?;
    let trees: Vec<Asked> = serde_json::from_value(args.get("trees").cloned().unwrap_or(json!([])))
        .map_err(|err| failed(format!("invalid trees: {err}")))?;
    if trees.is_empty() {
        return Ok(json!({ "branches": [] }));
    }
    let listed = gh(
        Path::new(dir),
        &[
            "pr", "list", "--state", "all", "--limit", PRS, "--json", FIELDS,
        ],
    )?;
    let prs: Vec<Pr> = serde_json::from_str(&listed)
        .map_err(|err| failed(format!("unreadable gh pr list: {err}")))?;
    Ok(json!({ "branches": matched(prs, &trees) }))
}

/// A review comment on a line, as the pulls comments API gives it.
#[derive(Debug, Deserialize)]
struct LineComment {
    #[serde(default)]
    path: String,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    body: String,
    #[serde(default)]
    user: Option<Login>,
}

#[derive(Debug, Deserialize)]
struct Login {
    #[serde(default)]
    login: String,
}

#[derive(Debug, Deserialize)]
struct Review {
    #[serde(default)]
    state: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    author: Option<Login>,
}

#[derive(Debug, Deserialize)]
struct Reviews {
    #[serde(default)]
    reviews: Vec<Review>,
}

/// What the failing checks say: their names, then the end of each failed
/// Actions run's log.
fn failure_text(dir: &Path, pr: &Pr) -> String {
    let failed = pr.failed();
    let names: Vec<&str> = failed.iter().map(|c| c.name()).collect();
    let mut text = format!("Failing checks: {}", names.join(", "));
    let mut runs: Vec<&str> = failed.iter().filter_map(|c| c.run_id()).collect();
    runs.dedup();
    for run in runs {
        match gh(dir, &["run", "view", run, "--log-failed"]) {
            Ok(log) => text.push_str(&format!(
                "\n\nThe failed steps of run {run}, last {LOG_LINES} lines:\n{}",
                ship::tail(&log, LOG_LINES)
            )),
            Err(err) => text.push_str(&format!("\n\nRun {run}'s log could not be read: {err}")),
        }
    }
    for check in failed.iter().filter(|c| c.run_id().is_none()) {
        if let Some(url) = check.target_url.as_deref().or(check.details_url.as_deref()) {
            text.push_str(&format!("\n\n{} has its details at {url}", check.name()));
        }
    }
    text
}

/// What the reviewers asked for: each review requesting changes, then the
/// comments on lines.
fn review_text(dir: &Path, pr: &Pr) -> Result<String, Error> {
    let number = pr.number.to_string();
    let read: Reviews =
        serde_json::from_str(&gh(dir, &["pr", "view", &number, "--json", "reviews"])?)
            .map_err(|err| failed(format!("unreadable reviews: {err}")))?;
    let mut text = String::from("Changes requested in review:");
    for review in read
        .reviews
        .iter()
        .filter(|r| r.state == "CHANGES_REQUESTED" && !r.body.trim().is_empty())
    {
        let who = review
            .author
            .as_ref()
            .map_or("a reviewer", |a| a.login.as_str());
        text.push_str(&format!("\n\n{who}: {}", review.body.trim()));
    }
    let path = format!("repos/{{owner}}/{{repo}}/pulls/{number}/comments");
    let comments: Vec<LineComment> = gh(dir, &["api", &path])
        .ok()
        .and_then(|listed| serde_json::from_str(&listed).ok())
        .unwrap_or_default();
    for comment in comments {
        let who = comment
            .user
            .as_ref()
            .map_or("a reviewer", |u| u.login.as_str());
        let at = match comment.line {
            Some(line) => format!("{}:{line}", comment.path),
            None => comment.path.clone(),
        };
        text.push_str(&format!("\n\n{at} ({who}): {}", comment.body.trim()));
    }
    Ok(text)
}

/// What `view` reads with `gh pr view`.
const VIEW_FIELDS: &str = "number,title,url,state,isDraft,author,baseRefName,headRefName,\
                           headRefOid,body,additions,deletions,changedFiles,commits,\
                           reviewDecision,mergeStateStatus,statusCheckRollup,reviews,comments";

/// The failed checks whose logs `view` reads, each a `gh` call.
const VIEW_LOGS: usize = 3;

/// A PR's review threads, which only GraphQL says are resolved.
const THREADS: &str = "query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewThreads(first: 100) {
        nodes {
          isResolved path line originalLine startLine
          comments(first: 50) { nodes { author { login } body diffHunk } }
        }
      }
    }
  }
}";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Viewed {
    number: u64,
    title: String,
    url: Option<String>,
    state: String,
    is_draft: bool,
    author: Option<Login>,
    base_ref_name: String,
    head_ref_name: String,
    head_ref_oid: String,
    body: String,
    additions: u64,
    deletions: u64,
    changed_files: u64,
    commits: Vec<Value>,
    review_decision: Option<String>,
    merge_state_status: Option<String>,
    status_check_rollup: Option<Vec<Check>>,
    reviews: Vec<ViewedReview>,
    comments: Vec<Said>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ViewedReview {
    author: Option<Login>,
    body: String,
    commit: Option<Oid>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Oid {
    oid: String,
}

/// A comment, on the PR or in a thread.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Said {
    author: Option<Login>,
    body: String,
    diff_hunk: String,
}

impl Said {
    fn who(&self) -> &str {
        login(&self.author)
    }
}

/// Who wrote something, as GitHub names a deleted account.
fn login(author: &Option<Login>) -> &str {
    author.as_ref().map_or("ghost", |a| a.login.as_str())
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ThreadNode {
    is_resolved: bool,
    path: String,
    line: Option<u64>,
    original_line: Option<u64>,
    start_line: Option<u64>,
    comments: Nodes<Said>,
}

#[derive(Debug, Default, Deserialize)]
struct Nodes<T> {
    #[serde(default)]
    nodes: Vec<T>,
}

/// The last lines of a thread's diff hunk, ending at its line.
const CODE_LINES: usize = 4;

/// One PR as its popup shows it, from `gh pr view`'s answer and its review
/// threads; the checks' logs are read after.
fn pr_view(viewed: Viewed, threads: Vec<ThreadNode>) -> PrView {
    let checks: Vec<CheckView> = viewed
        .status_check_rollup
        .iter()
        .flatten()
        .map(Check::view)
        .collect();
    let mut people = Vec::new();
    let mut findings: Vec<Finding> = Vec::new();
    for node in threads {
        let mut comments = node.comments.nodes.into_iter();
        let Some(first) = comments.next() else {
            continue;
        };
        let line = node.line.or(node.original_line);
        if coderabbit::is_coderabbit(first.who()) {
            let lines = match (node.start_line, line) {
                (Some(start), Some(end)) if start != end => Some(format!("{start}-{end}")),
                (_, end) => end.map(|l| l.to_string()),
            };
            for mut finding in coderabbit::line_comment(&first.body) {
                finding.path = Some(node.path.clone());
                finding.lines.clone_from(&lines);
                finding.resolved |= node.is_resolved;
                findings.push(finding);
            }
            continue;
        }
        let code: Vec<String> = first
            .diff_hunk
            .lines()
            .filter(|l| !l.starts_with("@@"))
            .map(str::to_string)
            .collect();
        people.push(Thread {
            author: first.who().to_string(),
            path: Some(node.path),
            line,
            resolved: node.is_resolved,
            code: code[code.len().saturating_sub(CODE_LINES)..].to_vec(),
            // The hunk is from the commit the comment was made on.
            code_line: node.original_line.or(node.line),
            body: first.body.trim().to_string(),
            replies: comments
                .map(|c| Reply {
                    author: c.who().to_string(),
                    body: c.body.trim().to_string(),
                })
                .collect(),
        });
    }
    let mut reviewed = None;
    for review in &viewed.reviews {
        let who = login(&review.author).to_string();
        if coderabbit::is_coderabbit(&who) {
            reviewed = review.commit.as_ref().map(|c| c.oid.clone()).or(reviewed);
            for finding in coderabbit::review(&review.body) {
                let seen = findings.iter().any(|f| {
                    (&f.path, &f.lines, &f.title) == (&finding.path, &finding.lines, &finding.title)
                });
                if !seen {
                    findings.push(finding);
                }
            }
        } else if !review.body.trim().is_empty() {
            people.push(general(who, &review.body));
        }
    }
    for comment in &viewed.comments {
        if !coderabbit::is_coderabbit(comment.who()) && !comment.body.trim().is_empty() {
            people.push(general(comment.who().to_string(), &comment.body));
        }
    }
    let bot_check = checks
        .iter()
        .find(|c| c.name.to_lowercase().contains("coderabbit"));
    let bot = (reviewed.is_some() || !findings.is_empty() || bot_check.is_some()).then(|| {
        findings.sort_by_key(|f| (f.resolved, f.kind));
        BotReview {
            reviewed,
            reviewing: bot_check.is_some_and(|c| c.outcome == ship::Outcome::Pending),
            findings,
        }
    });
    PrView {
        number: viewed.number,
        title: viewed.title,
        url: viewed.url,
        state: viewed.state,
        draft: viewed.is_draft,
        author: login(&viewed.author).to_string(),
        base: viewed.base_ref_name,
        branch: viewed.head_ref_name,
        head: viewed.head_ref_oid,
        body: coderabbit::without_comments(&viewed.body)
            .trim()
            .to_string(),
        additions: viewed.additions,
        deletions: viewed.deletions,
        files: viewed.changed_files,
        commits: viewed.commits.len() as u64,
        review: match viewed.review_decision.as_deref().unwrap_or_default() {
            "APPROVED" => "approved",
            "CHANGES_REQUESTED" => "changes requested",
            "REVIEW_REQUIRED" => "required",
            _ => "",
        }
        .to_string(),
        merge: viewed.merge_state_status.unwrap_or_default().to_lowercase(),
        checks,
        threads: people,
        bot,
    }
}

/// A comment on the whole PR, as a thread with no line.
fn general(author: String, body: &str) -> Thread {
    Thread {
        author,
        body: body.trim().to_string(),
        ..Thread::default()
    }
}

/// A failed job's log as a person reads it: `gh --log-failed` puts the job,
/// the step and a timestamp before every line, and Actions colours some.
fn clean_log(log: &str) -> String {
    log.lines()
        .map(|line| {
            let line = match line.splitn(3, '\t').collect::<Vec<_>>()[..] {
                [_, _, rest] => rest,
                _ => line,
            };
            let line = match line.split_once(' ') {
                Some((stamp, rest)) if crate::pool::seconds(stamp).is_some() => rest,
                _ => line,
            };
            without_escapes(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `line` with its terminal colour codes (`ESC [ ... m`) taken out.
fn without_escapes(line: &str) -> String {
    // gh writes the escape itself as the two characters `^[`.
    let line = line.replace("^[", "\u{1b}");
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn view(args: &Value) -> Result<Value, Error> {
    let dir = args
        .get("dir")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("no dir given"))?;
    let branch = args
        .get("branch")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("no branch given"))?;
    let dir = Path::new(dir);
    let viewed = match gh(dir, &["pr", "view", branch, "--json", VIEW_FIELDS]) {
        Ok(viewed) => viewed,
        Err(err) if err.message.contains("no pull requests found") => {
            return Ok(json!({ "pr": null }));
        }
        Err(err) => return Err(err),
    };
    let viewed: Viewed =
        serde_json::from_str(&viewed).map_err(|err| failed(format!("unreadable PR: {err}")))?;
    let failing: Vec<Check> = viewed
        .status_check_rollup
        .iter()
        .flatten()
        .filter(|c| c.outcome() == Outcome::Failed)
        .take(VIEW_LOGS)
        .cloned()
        .collect();
    let number = format!("number={}", viewed.number);
    let threads: Vec<ThreadNode> = gh(
        dir,
        &[
            "api",
            "graphql",
            "-F",
            "owner={owner}",
            "-F",
            "repo={repo}",
            "-F",
            &number,
            "-f",
            &format!("query={THREADS}"),
        ],
    )
    .ok()
    .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    .and_then(|v| {
        let nodes = v.pointer("/data/repository/pullRequest/reviewThreads/nodes")?;
        serde_json::from_value(nodes.clone()).ok()
    })
    .unwrap_or_default();
    let mut pr = pr_view(viewed, threads);
    for check in failing {
        let log = match (check.job_id(), check.run_id()) {
            (Some(job), _) => gh(dir, &["run", "view", "--job", job, "--log-failed"]),
            (None, Some(run)) => gh(dir, &["run", "view", run, "--log-failed"]),
            (None, None) => continue,
        };
        if let Some(seen) = pr.checks.iter_mut().find(|c| c.name == check.name()) {
            seen.log = Some(match log {
                Ok(log) => ship::tail(&clean_log(&log), LOG_LINES),
                Err(err) => format!("its log could not be read: {err}"),
            });
        }
    }
    Ok(json!({ "pr": pr }))
}

fn logs(args: &Value) -> Result<Value, Error> {
    let dir = args
        .get("dir")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("no dir given"))?;
    let branch = args
        .get("branch")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("no branch given"))?;
    let dir = Path::new(dir);
    let viewed = gh(dir, &["pr", "view", branch, "--json", FIELDS])?;
    let pr: Pr =
        serde_json::from_str(&viewed).map_err(|err| failed(format!("unreadable PR: {err}")))?;
    let mut parts = Vec::new();
    if !pr.failed().is_empty() {
        parts.push(failure_text(dir, &pr));
    }
    if pr.review() == "CHANGES_REQUESTED" {
        parts.push(review_text(dir, &pr)?);
    }
    if parts.is_empty() {
        return Err(Error::new(
            Code::NotFound,
            format!("nothing failed on #{}", pr.number),
        ));
    }
    Ok(json!({ "text": parts.join("\n\n") }))
}

/// Answers one request as the GitHub plugin.
pub fn op(op: &str, args: &Value) -> Result<Value, Error> {
    match op {
        "describe" => Ok(json!({
            "protocol": PROTOCOL,
            "name": "github",
            "kind": "ship",
            "ops": ["status", "logs", "view"],
        })),
        "status" => status(args),
        "logs" => logs(args),
        "view" => view(args),
        _ => Err(Error::new(
            Code::Unsupported,
            format!("github does not answer {op}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// PRs as `gh pr list --json` printed them, one per stage.
    const LISTED: &str = r#"[
      {"number":12,"headRefName":"a/merged","headRefOid":"aaa","state":"MERGED","isDraft":false,"reviewDecision":"","statusCheckRollup":[],"url":"https://github.com/o/r/pull/12","isCrossRepository":false},
      {"number":13,"headRefName":"a/failing","headRefOid":"bbb","state":"OPEN","isDraft":false,"reviewDecision":"APPROVED","statusCheckRollup":[
        {"__typename":"CheckRun","name":"test (ubuntu)","status":"COMPLETED","conclusion":"FAILURE","detailsUrl":"https://github.com/o/r/actions/runs/991/job/5"},
        {"__typename":"CheckRun","name":"lint","status":"COMPLETED","conclusion":"SUCCESS"}],"url":"https://github.com/o/r/pull/13","isCrossRepository":false},
      {"number":14,"headRefName":"a/running","state":"OPEN","isDraft":false,"reviewDecision":"REVIEW_REQUIRED","statusCheckRollup":[
        {"__typename":"CheckRun","name":"test","status":"IN_PROGRESS","conclusion":""},
        {"__typename":"StatusContext","context":"deploy","state":"SUCCESS"}],"isCrossRepository":false},
      {"number":15,"headRefName":"a/changes","state":"OPEN","isDraft":false,"reviewDecision":"CHANGES_REQUESTED","statusCheckRollup":[],"isCrossRepository":false},
      {"number":16,"headRefName":"a/draft","state":"OPEN","isDraft":true,"reviewDecision":"","statusCheckRollup":[],"isCrossRepository":false},
      {"number":17,"headRefName":"a/approved","state":"OPEN","isDraft":false,"reviewDecision":"APPROVED","statusCheckRollup":null,"isCrossRepository":false},
      {"number":18,"headRefName":"a/review","state":"OPEN","isDraft":false,"reviewDecision":"","statusCheckRollup":[{"__typename":"StatusContext","context":"ci/old","state":"ERROR"}],"isCrossRepository":true},
      {"number":19,"headRefName":"a/closed","state":"CLOSED","isDraft":false,"reviewDecision":"","statusCheckRollup":[],"isCrossRepository":false}
    ]"#;

    fn asked(branches: &[&str]) -> Vec<Asked> {
        branches
            .iter()
            .map(|b| Asked {
                dir: PathBuf::from("/p/1"),
                branch: b.to_string(),
            })
            .collect()
    }

    fn stages(ships: &[Ship]) -> Vec<(&str, Stage, &str)> {
        let mut stages: Vec<_> = ships
            .iter()
            .map(|s| (s.branch.as_str(), s.stage, s.label.as_str()))
            .collect();
        stages.sort_by_key(|(branch, _, _)| *branch);
        stages
    }

    #[test]
    fn each_pr_reads_as_its_stage() {
        let prs: Vec<Pr> = serde_json::from_str(LISTED).unwrap();
        let ships = matched(
            prs,
            &asked(&[
                "a/merged",
                "a/failing",
                "a/running",
                "a/changes",
                "a/draft",
                "a/approved",
                "a/closed",
                "a/review",
                "a/nothing",
            ]),
        );
        assert_eq!(
            stages(&ships),
            [
                ("a/approved", Stage::Approved, "#17 approved"),
                ("a/changes", Stage::Changes, "#15 changes"),
                ("a/closed", Stage::Closed, "#19 closed"),
                ("a/draft", Stage::Draft, "#16 draft"),
                ("a/failing", Stage::CiFailed, "#13 ✗ ci"),
                ("a/merged", Stage::Merged, "#12 merged"),
                ("a/running", Stage::Checks, "#14 ci …"),
            ]
        );
        let failing = ships.iter().find(|s| s.branch == "a/failing").unwrap();
        assert_eq!(failing.head.as_deref(), Some("bbb"));
        assert_eq!(
            failing.detail,
            [
                "checks 1 passed, 1 failed: test (ubuntu)",
                "review approved"
            ]
        );
        let running = ships.iter().find(|s| s.branch == "a/running").unwrap();
        assert_eq!(
            running.detail,
            ["checks 1 passed, 1 running", "review required"]
        );
    }

    #[test]
    fn the_newest_pr_wins_for_a_reused_branch() {
        let prs: Vec<Pr> = serde_json::from_str(
            r#"[
              {"number":30,"headRefName":"x","state":"CLOSED"},
              {"number":41,"headRefName":"x","state":"OPEN"},
              {"number":35,"headRefName":"x","state":"MERGED"}
            ]"#,
        )
        .unwrap();
        let ships = matched(prs, &asked(&["x"]));
        assert_eq!(stages(&ships), [("x", Stage::Review, "#41 review")]);
    }

    #[test]
    fn a_failed_check_names_its_actions_run() {
        let prs: Vec<Pr> = serde_json::from_str(LISTED).unwrap();
        let failing = prs.iter().find(|p| p.number == 13).unwrap();
        let runs: Vec<_> = failing.failed().iter().filter_map(|c| c.run_id()).collect();
        assert_eq!(runs, ["991"]);
    }

    #[test]
    fn describes_itself_and_asks_nothing_for_no_trees() {
        assert_eq!(op("describe", &json!({})).unwrap()["kind"], "ship");
        assert_eq!(
            op("status", &json!({"dir": "/nowhere", "trees": []})).unwrap(),
            json!({ "branches": [] })
        );
        assert_eq!(op("run", &json!({})).unwrap_err().code, Code::Unsupported);
    }

    /// A line comment as CodeRabbit writes one now: its rating, title and
    /// text, its prompt for agents, and the markers that close it.
    const RABBIT_LINE: &str = "<!-- coderabbit-ai-generated:v1 -->\n\n**<picture><img src=\"i.svg\"></picture> Major** · **Correctness**\n\n**Guard the empty list.**\n\n`items[0]` panics on an empty list.\n\n<!-- ai-agent-prompt:start -->\n\n<details>\n<summary>Prompt for AI Agents</summary>\n\n```\nIn src/a.rs at line 12, return early on an empty list.\n```\n\n</details>\n\n<!-- ai-agent-prompt:end -->\n\n<!-- cr-indicator-types:potential_issue -->\n\n<!-- cr-comment:v1:abc -->";

    /// A CodeRabbit review whose text carries one nitpick, under its file.
    const RABBIT_REVIEW: &str = "**Actionable comments posted: 1**\n\n<details>\n<summary>Nitpick comments: 1</summary><blockquote>\n\n<details>\n<summary>src/b.rs: 1</summary><blockquote>\n\n`3-4`: **Trivial** · **Style**\n\n**Name the closure.**\n\nIt reads better named.\n\n<!-- cr-comment:v1:def -->\n\n</blockquote></details>\n\n</blockquote></details>";

    #[test]
    fn a_pr_reads_whole_for_its_popup() {
        let viewed: Viewed = serde_json::from_value(json!({
            "number": 37, "title": "Highlight syntax", "state": "OPEN", "author": {"login": "me"},
            "body": "<!-- This is an auto-generated comment: release notes by coderabbit.ai --><!--\ncoderabbit-ai-generated:v1 -->Bumps the workspace.",
            "headRefOid": "head1", "commits": [{}, {}], "reviewDecision": "REVIEW_REQUIRED",
            "mergeStateStatus": "BLOCKED",
            "statusCheckRollup": [
                {"name": "clippy", "status": "COMPLETED", "conclusion": "FAILURE", "workflowName": "CI",
                 "startedAt": "2026-11-08T10:00:00Z", "completedAt": "2026-11-08T10:01:12Z",
                 "detailsUrl": "https://github.com/o/r/actions/runs/991/job/5"},
                {"name": "CodeRabbit", "status": "IN_PROGRESS"}
            ],
            "reviews": [
                {"author": {"login": "coderabbitai"}, "body": RABBIT_REVIEW, "commit": {"oid": "old1"}},
                {"author": {"login": "jlin"}, "body": "Looks good once clippy passes."},
                {"author": {"login": "jlin"}, "body": ""},
                // A re-review repeats the nitpick it left before.
                {"author": {"login": "coderabbitai"}, "body": RABBIT_REVIEW, "commit": {"oid": "new1"}}
            ],
            "comments": [{"author": {"login": "coderabbitai"}, "body": "Walkthrough"}]
        }))
        .unwrap();
        let fixed = RABBIT_LINE.replace("Guard the empty list.", "Close the file.");
        let threads: Vec<ThreadNode> = serde_json::from_value(json!([
            {"isResolved": true, "path": "src/d.rs", "line": 5,
             "comments": {"nodes": [{"author": {"login": "coderabbitai"}, "body": fixed}]}},
            {"isResolved": false, "path": "src/a.rs", "line": 12, "originalLine": 12, "startLine": 10,
             "comments": {"nodes": [{"author": {"login": "coderabbitai"}, "body": RABBIT_LINE}]}},
            {"isResolved": false, "path": "src/c.rs", "line": 40, "originalLine": 31,
             "comments": {"nodes": [
                 {"author": {"login": "jlin"}, "body": "Why here?",
                  "diffHunk": "@@ -28,2 +28,4 @@\n a\n b\n+c\n+d\n+e"},
                 {"author": {"login": "me"}, "body": "Moved it."}
             ]}}
        ]))
        .unwrap();
        let pr = pr_view(viewed, threads);
        assert_eq!(
            (pr.commits, pr.review.as_str(), pr.merge.as_str()),
            (2, "required", "blocked")
        );
        assert_eq!(
            pr.body, "Bumps the workspace.",
            "HTML comments are not for reading"
        );
        assert_eq!(pr.checks[0].seconds, Some(72));
        assert_eq!(pr.checks[0].outcome, ship::Outcome::Failed);

        let bot = pr.bot.expect("CodeRabbit reviews this repository");
        assert!(bot.reviewing, "its check is still running");
        assert_eq!(
            bot.reviewed.as_deref(),
            Some("new1"),
            "the commit its latest review is of"
        );
        let found: Vec<_> = bot
            .findings
            .iter()
            .map(|f| {
                (
                    f.kind,
                    f.path.as_deref(),
                    f.lines.as_deref(),
                    f.title.as_str(),
                    f.resolved,
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    coderabbit::Kind::Issue,
                    Some("src/a.rs"),
                    Some("10-12"),
                    "Guard the empty list.",
                    false
                ),
                (
                    coderabbit::Kind::Nitpick,
                    Some("src/b.rs"),
                    Some("3-4"),
                    "Name the closure.",
                    false
                ),
                (
                    coderabbit::Kind::Issue,
                    Some("src/d.rs"),
                    Some("5"),
                    "Close the file.",
                    true
                ),
            ],
            "open ones by kind, then resolved ones; a repeated one once"
        );

        let people: Vec<(&str, Option<&str>)> = pr
            .threads
            .iter()
            .map(|t| (t.author.as_str(), t.path.as_deref()))
            .collect();
        assert_eq!(
            people,
            [("jlin", Some("src/c.rs")), ("jlin", None)],
            "CodeRabbit's own and empty reviews are not people's comments"
        );
        let thread = &pr.threads[0];
        assert_eq!(
            thread.code,
            [" b", "+c", "+d", "+e"],
            "the hunk's last lines"
        );
        assert_eq!(thread.line, Some(40), "the thread shows where it is now");
        assert_eq!(
            thread.code_line,
            Some(31),
            "the hunk is the comment's own, ending at its original line"
        );
        assert_eq!(thread.replies[0].body, "Moved it.");
    }

    #[test]
    fn a_failed_log_reads_as_its_lines_alone() {
        let log = "guard\tReject triggers\t2026-11-08T22:32:19.8973361Z \u{1b}[36;1mruby check.rb\u{1b}[0m\n\
                   guard\tReject triggers\t2026-11-08T22:32:20.0000000Z ##[error]only workflow_dispatch is permitted\n\
                   guard\tReject triggers\t2026-11-08T22:32:21.0000000Z ^[[36;1mln -sf node^[[0m\n\
                   plain line";
        assert_eq!(
            clean_log(log),
            "ruby check.rb\n##[error]only workflow_dispatch is permitted\nln -sf node\nplain line",
            "no job, step or time before each line, and no colour codes"
        );
    }
}
