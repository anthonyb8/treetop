//! The GitHub ship plugin, built in: `treetop plugin github` serves it. It
//! reads pull requests through `gh`, so the user's own login and host carry
//! over and treetop never holds a token, and it only ever reads: it never
//! pushes, opens, merges or reviews a PR.
//!
//! `status` is one `gh pr list` for the whole repository, matched to the
//! branches asked about, so a pool of twenty trees costs one call a minute.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::child::{self, End};
use crate::plugin::{Code, Error, PROTOCOL};
use crate::ship::{self, Asked, Ship, Stage};

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
            "ops": ["status", "logs"],
        })),
        "status" => status(args),
        "logs" => logs(args),
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
}
