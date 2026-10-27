//! The Claude Code agent plugin, built in: `treetop plugin claude` serves it.
//! `EnterWorktree` moves a session into a tree without changing its
//! process's cwd, so the process scan cannot see it; Claude Code's own files
//! can. These files are internal and undocumented, so every field is read by
//! name, extra ones are ignored, and anything unexpected yields no agent
//! rather than an error.
//!
//! - `<config>/sessions/<pid>.json`: one per running session, with its id,
//!   name, status, kind (`interactive` or `bg`) and process start time.
//! - `<config>/projects/<dir>/<sessionId>.jsonl`: the transcript. Every entry
//!   carries the session's `cwd`, which after `EnterWorktree` is the tree,
//!   and every reply the tokens its request sent, which is how full the
//!   context is, and what it cost, which summed is what the session spent. The attachments before the first request say how much of
//!   it is memory files and skills.
//! - `<config>/jobs/<id>/state.json`: every background job, running or not,
//!   which `reports` reads. One with no running session is a cold agent.
//!
//! - `<config>/jobs/pins.json`: the jobs Claude Code's daemon never retires
//!   for sitting idle, which its agent view toggles with Ctrl+T. `keep` adds
//!   to it and `stop` takes away, under the lock Claude Code takes.
//!
//! `resume` starts a stopped or failed job again as `claude --bg --resume`
//! with the `respawnFlags` its `state.json` keeps, where it was started.
//!
//! - `~/.claude/.credentials.json` (in `CLAUDE_CONFIG_DIR` when that is
//!   set): Claude Code's login, with which `usage` asks Anthropic how much
//!   of the plan is used, as `/usage` does. It is only read, and an expired
//!   one is not refreshed, since that would sign Claude Code out.
//! - `~/.claude.json`: the plan's limits as `/usage` last fetched them,
//!   under `cachedUsageUtilization`, which `usage` falls back on.
//!
//! `clear` deletes every job that worked in a tree, whatever its state,
//! failed ones included, when the tree goes back to the pool.
//!
//! A background session opens in a terminal with `claude attach <id>`, where
//! the id is the short one `claude --bg` prints: the session id's first eight
//! characters. Attaching to a cold one starts it again, and so does `claude
//! --bg --resume` with its session id, without a terminal or a new message.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::child::{self, End};
use crate::live;
use crate::plugin::{Code, Context, Error, Limit, PROTOCOL, Part, Seen, Spend, Usage as Plan};
use crate::reports::Reader;

/// How much of a transcript's end is read for its latest `cwd`. One entry is
/// rarely more than a few kilobytes.
const TAIL: u64 = 64 * 1024;

/// How long `claude` gets to start, stop or resume a session.
const CLAUDE_LIMIT: Duration = Duration::from_secs(60);

/// How long a started session gets to show up in `claude agents`.
const CONFIRM_LIMIT: Duration = Duration::from_secs(20);

/// How often `claude agents` is asked again while confirming.
const CONFIRM_EVERY: Duration = Duration::from_millis(500);

/// How long after waking a job it is not woken again, so a job that does not
/// come up, or that the daemon retires again because memory is short, is not
/// started over and over.
const WAKE_AGAIN: Duration = Duration::from_secs(10 * 60);

/// How long Claude Code's lock on `pins.json` lasts unrefreshed before it is
/// stale, as Claude Code takes it.
const PINS_STALE: Duration = Duration::from_secs(5);

/// How long `keep` and `stop` wait for that lock.
const PINS_WAIT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Session {
    pid: u32,
    session_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    /// The process start time in clock ticks, written as a string; a number
    /// is accepted too, in case that changes.
    #[serde(default)]
    proc_start: Option<Value>,
    #[serde(default)]
    job_id: Option<String>,
}

/// `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".claude")))
}

fn attach(id: &str) -> Vec<String> {
    vec!["claude".into(), "attach".into(), id.into()]
}

/// The session working in `dir`, if it is still running: its pid is alive
/// and started when the session file says, so a pid reused since does not
/// count. `start` is the pid's start time now, None when it has exited.
fn live_session(session: &Session, start: Option<u64>, dir: PathBuf) -> Option<Seen> {
    let start = start?;
    let recorded = match &session.proc_start {
        Some(Value::String(text)) => text.parse().ok(),
        Some(Value::Number(number)) => number.as_u64(),
        _ => None,
    };
    if recorded.is_some_and(|recorded| recorded != start) {
        return None;
    }
    let is_background = session.kind.as_deref() == Some("bg");
    // An interactive session belongs to the terminal it runs in; only a
    // background one can be attached from another.
    let short = session.session_id.get(..8).filter(|_| is_background);
    Some(Seen {
        id: session.job_id.clone().or_else(|| short.map(str::to_string)),
        name: session.name.clone().unwrap_or_else(|| "claude".into()),
        dir,
        live: true,
        pid: Some(session.pid),
        status: session.status.clone(),
        background: is_background,
        attach: short.map(attach),
        active_at: None,
        context: None,
        spend: None,
    })
}

/// The last `cwd` in a transcript's tail. The tail can begin or end mid-entry,
/// so a value cut off by either end is skipped for the one before it.
fn last_cwd(tail: &str) -> Option<PathBuf> {
    const KEY: &str = "\"cwd\":";
    let mut end = tail.len();
    while let Some(at) = tail[..end].rfind(KEY) {
        let value = &tail[at + KEY.len()..];
        let mut stream = serde_json::Deserializer::from_str(value).into_iter::<String>();
        if let Some(Ok(cwd)) = stream.next() {
            return Some(PathBuf::from(cwd));
        }
        end = at;
    }
    None
}

fn tail(path: &Path) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// How much of a transcript's start is read for its first request, which
/// follows the session's opening attachments and first message.
const HEAD: u64 = 1024 * 1024;

/// The window of a model run with Claude Code's long context, and of the
/// rest.
const LONG_WINDOW: u64 = 1_000_000;
const WINDOW: u64 = 200_000;

/// A transcript entry, as far as its context is concerned.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    #[serde(rename = "type")]
    kind: String,
    /// A subagent's turn, whose context is its own.
    #[serde(default)]
    is_sidechain: bool,
    #[serde(default)]
    message: Option<Message>,
    #[serde(default)]
    attachment: Option<Attachment>,
    /// The text an attachment puts in the context.
    #[serde(default)]
    rendered: Option<Value>,
}

#[derive(Deserialize)]
struct Message {
    /// The reply's id, the same on every entry of one reply.
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Attachment {
    #[serde(rename = "type")]
    kind: String,
}

/// The tokens of one request: every token of its input was in the context.
#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

impl Entry {
    /// The context of the session's own request this entry answers, in
    /// tokens, and the model it went to.
    fn request(&self) -> Option<(u64, Option<&str>)> {
        if self.kind != "assistant" || self.is_sidechain {
            return None;
        }
        let message = self.message.as_ref()?;
        let usage = message.usage.as_ref()?;
        let used =
            usage.input_tokens + usage.cache_creation_input_tokens + usage.cache_read_input_tokens;
        Some((used, message.model.as_deref()))
    }

    /// The attachment's type and roughly how many tokens it adds, at four
    /// characters a token.
    fn attached(&self) -> Option<(&str, u64)> {
        let kind = &self.attachment.as_ref()?.kind;
        let chars = match self.rendered.as_ref()? {
            Value::String(text) => text.chars().count(),
            other => other.to_string().chars().count(),
        };
        Some((kind, u64::try_from(chars).unwrap_or(u64::MAX) / 4))
    }
}

/// What the session's first request held: everything that is there before
/// the conversation is, and of it, the memory files and the skills listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Opening {
    used: u64,
    memory: u64,
    skills: u64,
}

/// The opening read from the start of a transcript, None while it has had
/// no request.
fn opening(head: &str) -> Option<Opening> {
    let (mut memory, mut skills) = (0, 0);
    for line in head.lines() {
        let Ok(entry) = serde_json::from_str::<Entry>(line) else {
            continue;
        };
        if let Some((used, _)) = entry.request() {
            return Some(Opening {
                used,
                memory,
                skills,
            });
        }
        match entry.attached() {
            Some(("instructions", tokens)) => memory += tokens,
            Some(("skill_listing", tokens)) => skills += tokens,
            _ => {}
        }
    }
    None
}

/// The context of the latest request in a transcript's tail, and its
/// model. The tail can begin mid-entry, which is skipped.
fn latest(tail: &str) -> Option<(u64, Option<String>)> {
    tail.lines()
        .rev()
        .filter(|line| line.contains("\"usage\""))
        .filter_map(|line| serde_json::from_str::<Entry>(line).ok())
        .find_map(|entry| {
            entry
                .request()
                .map(|(used, model)| (used, model.map(str::to_string)))
        })
}

/// The window a session's model has. Claude Code runs a model with a
/// million tokens when its id says `[1m]`, and Claude 5 models with one by
/// default; a context already past the shorter window had the longer one.
fn window(model: Option<&str>, used: u64) -> u64 {
    let is_long = model.is_some_and(|m| m.ends_with("[1m]") || m.contains("-5-"));
    if is_long || used > WINDOW {
        LONG_WINDOW
    } else {
        WINDOW
    }
}

/// How full the context is and where it comes from: the conversation is
/// what it has grown by since the first request, and the rest of that
/// request is the system prompt and tools.
fn split(used: u64, window: u64, opening: Option<Opening>) -> Context {
    let mut parts = Vec::new();
    if let Some(o) = opening {
        let start = [
            ("memory files", o.memory),
            ("skills", o.skills),
            ("system & tools", o.used.saturating_sub(o.memory + o.skills)),
        ];
        parts.push(("conversation", used.saturating_sub(o.used)));
        parts.extend(start);
    }
    parts.retain(|(_, tokens)| *tokens > 0);
    parts.sort_by_key(|(_, tokens)| std::cmp::Reverse(*tokens));
    Context {
        used,
        window,
        parts: parts
            .into_iter()
            .map(|(name, tokens)| Part {
                name: name.into(),
                tokens,
            })
            .collect(),
    }
}

/// What is known of each transcript: its opening once it has one, its
/// latest request, kept for a tail that holds none, and what it has spent up
/// to `read_to`, with the replies already counted.
#[derive(Default)]
struct Known {
    opening: Option<Opening>,
    latest: Option<(u64, Option<String>)>,
    spend: Spend,
    read_to: u64,
    counted: HashSet<String>,
}

static KNOWN: LazyLock<Mutex<HashMap<PathBuf, Known>>> = LazyLock::new(Mutex::default);

fn head(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(HEAD)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// How full the session's context is, from its transcript at `path`, whose
/// tail is `end`. The opening is read once.
fn context(path: &Path, end: &str) -> Option<Context> {
    let mut known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    let known = known.entry(path.to_path_buf()).or_default();
    if known.opening.is_none() {
        known.opening = head(path).as_deref().and_then(opening);
    }
    if let Some(latest) = latest(end) {
        known.latest = Some(latest);
    }
    let (used, model) = known.latest.as_ref()?;
    Some(split(*used, window(model.as_deref(), *used), known.opening))
}

/// What the session of the transcript at `path` has spent, every request
/// counted once, subagents' included: Claude Code writes each content block
/// of a reply as an entry of its own, every one carrying the reply's usage.
/// Only what was written since the last read is read.
fn spend(path: &Path) -> Spend {
    let mut known = KNOWN.lock().unwrap_or_else(PoisonError::into_inner);
    let known = known.entry(path.to_path_buf()).or_default();
    let Ok(mut file) = File::open(path) else {
        return known.spend;
    };
    let len = file.metadata().map_or(0, |m| m.len());
    if len < known.read_to {
        // Written over: count it again from the start.
        (known.spend, known.read_to) = (Spend::default(), 0);
        known.counted.clear();
    }
    let mut bytes = Vec::new();
    let read = file
        .seek(SeekFrom::Start(known.read_to))
        .and_then(|_| file.take(len - known.read_to).read_to_end(&mut bytes));
    if read.is_err() {
        return known.spend;
    }
    // A line still being written is read next time, whole.
    let complete = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    for line in bytes[..complete].split(|b| *b == b'\n') {
        let line = String::from_utf8_lossy(line);
        if !line.contains("\"usage\"") {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Entry>(&line) else {
            continue;
        };
        let Some(message) = entry.message.filter(|_| entry.kind == "assistant") else {
            continue;
        };
        let (Some(usage), is_new) = (
            message.usage,
            message.id.is_none_or(|id| known.counted.insert(id)),
        ) else {
            continue;
        };
        if is_new {
            known.spend.input += usage.input_tokens;
            known.spend.cache_read += usage.cache_read_input_tokens;
            known.spend.cache_write += usage.cache_creation_input_tokens;
            known.spend.output += usage.output_tokens;
        }
    }
    known.read_to += u64::try_from(complete).unwrap_or(0);
    known.spend
}

/// The session's transcript: the most recently written `<sessionId>.jsonl`
/// across the project directories.
fn transcript(projects: &[PathBuf], session_id: &str) -> Option<PathBuf> {
    let file = format!("{session_id}.jsonl");
    projects
        .iter()
        .map(|dir| dir.join(&file))
        .filter_map(|path| {
            let modified = fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .max_by_key(|(modified, _): &(SystemTime, PathBuf)| *modified)
        .map(|(_, path)| path)
}

fn dirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Every running session and where it works.
fn live_sessions(config: &Path) -> Vec<Seen> {
    let sessions = dirs(&config.join("sessions"));
    if sessions.is_empty() {
        return Vec::new();
    }
    let projects = dirs(&config.join("projects"));
    sessions
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| {
            let session: Session = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
            // A session that has had no message yet has no transcript; until
            // it does, it works where its process runs.
            let transcript = transcript(&projects, &session.session_id);
            let end = transcript.as_deref().and_then(tail);
            let cwd = end
                .as_deref()
                .and_then(last_cwd)
                .or_else(|| fs::read_link(format!("/proc/{}/cwd", session.pid)).ok())?;
            let context = transcript
                .as_deref()
                .zip(end.as_deref())
                .and_then(|(path, end)| context(path, end));
            let spend = transcript.as_deref().map(spend);
            // It last worked when its transcript last grew.
            let active_at = transcript
                .and_then(|path| fs::metadata(path).and_then(|m| m.modified()).ok())
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_secs());
            live_session(&session, live::stat(session.pid).map(|s| s.start), cwd).map(|seen| Seen {
                active_at,
                context,
                spend,
                ..seen
            })
        })
        .collect()
}

/// The background jobs' reports, read again only where they changed.
static JOBS: LazyLock<Mutex<Reader>> = LazyLock::new(Mutex::default);

/// Every background job with no running session, as a cold agent where it
/// last worked.
fn cold(live: &[Seen]) -> Vec<Seen> {
    let running: HashSet<&str> = live.iter().filter_map(|s| s.id.as_deref()).collect();
    let reports = JOBS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .read_all();
    reports
        .into_iter()
        .filter(|report| !running.contains(report.job.as_str()))
        .filter_map(|report| {
            let active_at = report
                .updated
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_secs());
            Some(Seen {
                active_at,
                attach: Some(attach(&report.job)),
                id: Some(report.job),
                name: report.name,
                dir: report.dir?,
                live: false,
                pid: None,
                status: None,
                background: true,
                context: None,
                spend: None,
            })
        })
        .collect()
}

/// Every Claude Code agent, running or cold.
fn find() -> Vec<Seen> {
    let Some(config) = config_dir() else {
        return Vec::new();
    };
    let mut seen = live_sessions(&config);
    seen.extend(cold(&seen));
    seen
}

/// The id `claude --bg` prints for `claude attach`, from the line it
/// suggests running: `claude attach cbcfd8c8    open in this terminal`.
pub fn attach_id(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let id = line
            .split("claude attach ")
            .nth(1)?
            .split_whitespace()
            .next()?;
        id.chars().all(|c| c.is_ascii_alphanumeric()).then_some(id)
    })
}

/// Runs `claude` with `args` in `dir`, and returns its stdout.
fn claude(args: &[&str], dir: Option<&Path>) -> Result<String, Error> {
    let mut command = Command::new("claude");
    command.args(args);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let ran = child::run(&mut command, None, CLAUDE_LIMIT)
        .map_err(|err| Error::new(Code::Unavailable, format!("claude: {err}")))?;
    let stdout = String::from_utf8_lossy(&ran.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&ran.stderr);
    let last = |text: &str| {
        text.lines()
            .map(str::trim)
            .rfind(|l| !l.is_empty())
            .map(str::to_string)
    };
    match ran.end {
        End::Exited(status) if status.success() => Ok(stdout),
        End::Exited(status) => Err(Error::new(
            Code::Failed,
            last(&stderr)
                .or_else(|| last(&stdout))
                .unwrap_or_else(|| format!("claude: {status}")),
        )),
        End::TimedOut | End::HeldOpen(_) => Err(Error::new(
            Code::Unavailable,
            format!("claude took over {}s", CLAUDE_LIMIT.as_secs()),
        )),
    }
}

/// Waits for Claude Code to list session `id`, which is what says it
/// started: `claude --bg` exits 0 once the session is queued, not running.
pub fn confirm(id: &str) -> Result<(), Error> {
    let deadline = Instant::now() + CONFIRM_LIMIT;
    loop {
        let is_listed = claude(&["agents", "--json", "--all"], None).is_ok_and(|json| {
            serde_json::from_str::<Vec<Value>>(&json).is_ok_and(|sessions| {
                sessions
                    .iter()
                    .any(|s| s.get("id").and_then(Value::as_str) == Some(id))
            })
        });
        if is_listed {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                Code::Unavailable,
                format!(
                    "claude never listed session {id} in {}s",
                    CONFIRM_LIMIT.as_secs()
                ),
            ));
        }
        thread::sleep(CONFIRM_EVERY);
    }
}

fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    text(args, key).ok_or_else(|| Error::new(Code::Failed, format!("no {key} given")))
}

/// What a session started in a tree of its own is told, so it works there
/// rather than taking a second tree with `EnterWorktree`, which would leave
/// the first held with no agent in it.
fn stay(dir: &Path) -> String {
    format!(
        "You are already isolated: {} is a pool worktree leased for you alone, \
         on a branch of its own. It is the worktree your instructions ask you to take, \
         so do all your work in it and run everything from it. Do not call EnterWorktree \
         or lease another tree: that would separate you from the tree you were given.",
        dir.display()
    )
}

/// The arguments `start` runs `claude` with: in the background, named, told
/// to stay in its tree when it has one of its own, and the prompt after
/// `--`, so one that starts with `-` is never read as a flag.
fn start_args<'a>(
    name: Option<&'a str>,
    stay: Option<&'a str>,
    prompt: Option<&'a str>,
) -> Vec<&'a str> {
    let mut args = vec!["--bg"];
    if let Some(name) = name {
        args.extend(["--name", name]);
    }
    if let Some(stay) = stay {
        args.extend(["--append-system-prompt", stay]);
    }
    if let Some(prompt) = prompt {
        args.extend(["--", prompt]);
    }
    args
}

/// Starts a session in `dir` and waits until it runs, stopping it again if
/// it never does. `isolated` says `dir` is a tree of the session's own.
fn start(args: &Value) -> Result<Value, Error> {
    let dir = PathBuf::from(required(args, "dir")?);
    let is_isolated = args.get("isolated").and_then(Value::as_bool) == Some(true);
    let told = is_isolated.then(|| stay(&dir));
    let out = claude(
        &start_args(text(args, "name"), told.as_deref(), text(args, "prompt")),
        Some(&dir),
    )?;
    let id = attach_id(&out)
        .ok_or_else(|| Error::new(Code::Failed, "claude printed no session id"))?
        .to_string();
    if let Err(err) = confirm(&id) {
        let _ = claude(&["stop", &id], None);
        return Err(err);
    }
    Ok(json!({ "id": id }))
}

/// What a background job's `state.json` says about how to start it again.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JobState {
    #[serde(default)]
    resume_session_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    /// Where it was started, which `--resume` runs in again.
    #[serde(default)]
    cwd: Option<PathBuf>,
    /// The tree it took with `EnterWorktree`, if it did.
    #[serde(default)]
    worktree_path: Option<PathBuf>,
    /// The flags it was started with, which a resume passes again.
    #[serde(default)]
    respawn_flags: Vec<String>,
}

impl JobState {
    fn session(&self) -> Option<&str> {
        self.resume_session_id
            .as_deref()
            .or(self.session_id.as_deref())
    }

    /// Where it works: the tree it took, else where it was started.
    fn dir(self) -> Option<PathBuf> {
        self.worktree_path.or(self.cwd)
    }
}

fn job_state(text: &str) -> Option<JobState> {
    serde_json::from_str(text).ok()
}

fn read_job(id: &str) -> Option<JobState> {
    let path = config_dir()?.join("jobs").join(id).join("state.json");
    job_state(&fs::read_to_string(path).ok()?)
}

/// The session a background job ran as, which `--resume` takes.
fn resume_id(id: &str) -> Option<String> {
    read_job(id)?.session().map(str::to_string)
}

/// Sends `text` to a cold session by starting it again in the background
/// with `text` as its next message, under the same id. A running one is
/// messaged through its chat instead.
fn send(args: &Value) -> Result<Value, Error> {
    let id = required(args, "id")?;
    let message = required(args, "text")?;
    if find().iter().any(|s| s.live && s.id.as_deref() == Some(id)) {
        return Err(Error::new(
            Code::Conflict,
            format!("{id} is running; type into its chat"),
        ));
    }
    let session = resume_id(id)
        .ok_or_else(|| Error::new(Code::NotFound, format!("no background job {id}")))?;
    claude(&["--bg", "--resume", &session, "--", message], None)?;
    Ok(json!({ "delivered": true }))
}

/// Starts a stopped or failed job again where it was started, with its
/// conversation and the flags it was started with: job `id`, else the job
/// that last worked in `dir` and is not running, written since `since`
/// seconds after the epoch when that is given.
fn resume(args: &Value) -> Result<Value, Error> {
    let seen = find();
    let is_live = |id: &str| seen.iter().any(|s| s.live && s.id.as_deref() == Some(id));
    let id = match text(args, "id") {
        Some(id) => id.to_string(),
        None => {
            let dir = PathBuf::from(required(args, "dir")?);
            let since = args.get("since").and_then(Value::as_u64).unwrap_or(0);
            let since = SystemTime::UNIX_EPOCH + Duration::from_secs(since);
            worked_in(&dir)
                .into_iter()
                .filter(|(id, at)| *at >= since && !is_live(id))
                .max_by_key(|(_, at)| *at)
                .map(|(id, _)| id)
                .ok_or_else(|| {
                    Error::new(
                        Code::NotFound,
                        format!("no stopped agent worked in {}", dir.display()),
                    )
                })?
        }
    };
    if is_live(&id) {
        return Err(Error::new(
            Code::Conflict,
            format!("{id} is running; open its chat"),
        ));
    }
    let state = read_job(&id)
        .ok_or_else(|| Error::new(Code::NotFound, format!("no background job {id}")))?;
    let projects = config_dir()
        .map(|config| dirs(&config.join("projects")))
        .unwrap_or_default();
    let session = state
        .session()
        .filter(|session| transcript(&projects, session).is_some())
        .ok_or_else(|| {
            Error::new(
                Code::NotFound,
                format!("{id} has no conversation to resume"),
            )
        })?;
    let mut argv = vec!["--bg", "--resume", session];
    argv.extend(state.respawn_flags.iter().map(String::as_str));
    let dir = state.cwd.as_deref().filter(|dir| dir.is_dir());
    let out = claude(&argv, dir)?;
    let started = attach_id(&out)
        .ok_or_else(|| Error::new(Code::Failed, "claude printed no session id"))?
        .to_string();
    confirm(&started)?;
    Ok(json!({ "id": started, "resumed": id }))
}

/// Claude Code's lock on `pins.json`: a directory beside it, as the lock
/// library it uses makes one, removed when this is dropped.
struct PinsLock(PathBuf);

impl PinsLock {
    /// Takes the lock, clearing one left stale by a process that died
    /// holding it.
    fn take(pins: &Path) -> Result<Self, Error> {
        let lock = pins.with_extension("json.lock");
        let deadline = Instant::now() + PINS_WAIT;
        loop {
            match fs::create_dir(&lock) {
                Ok(()) => return Ok(PinsLock(lock)),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    let is_stale = fs::metadata(&lock)
                        .and_then(|m| m.modified())
                        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > PINS_STALE));
                    if is_stale {
                        let _ = fs::remove_dir(&lock);
                        continue;
                    }
                }
                Err(err) => {
                    return Err(Error::new(
                        Code::Failed,
                        format!("could not lock {}: {err}", pins.display()),
                    ));
                }
            }
            if Instant::now() >= deadline {
                return Err(Error::new(
                    Code::Unavailable,
                    format!("{} stayed locked", pins.display()),
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for PinsLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

/// Pins `ids`, or unpins them, in `<config>/jobs/pins.json`. A file that is
/// not the list of ids Claude Code writes is left as it is.
fn set_pinned(ids: &[&str], is_pinned: bool) -> Result<(), Error> {
    let config = config_dir().ok_or_else(|| Error::new(Code::Unavailable, "no HOME"))?;
    let pins = config.join("jobs").join("pins.json");
    fs::create_dir_all(config.join("jobs"))
        .map_err(|err| Error::new(Code::Failed, format!("{}: {err}", pins.display())))?;
    let _lock = PinsLock::take(&pins)?;
    let mut pinned: Vec<String> = match fs::read_to_string(&pins) {
        Ok(text) => serde_json::from_str(&text).map_err(|err| {
            Error::new(
                Code::Failed,
                format!("{} is not a list of ids: {err}", pins.display()),
            )
        })?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(err) => {
            return Err(Error::new(
                Code::Failed,
                format!("{}: {err}", pins.display()),
            ));
        }
    };
    let before = pinned.clone();
    if is_pinned {
        let had: BTreeSet<String> = pinned.iter().cloned().collect();
        pinned.extend(
            ids.iter()
                .filter(|id| !had.contains(**id))
                .map(|id| id.to_string()),
        );
    } else {
        pinned.retain(|id| !ids.contains(&id.as_str()));
    }
    if pinned == before {
        return Ok(());
    }
    let partial = pins.with_extension("json.treetop");
    let text = serde_json::to_string_pretty(&pinned).unwrap_or_else(|_| "[]".into());
    fs::write(&partial, text)
        .and_then(|()| fs::rename(&partial, &pins))
        .map_err(|err| Error::new(Code::Failed, format!("{}: {err}", pins.display())))
}

/// When each job was last woken, so it is not woken again within WAKE_AGAIN.
static WOKEN: LazyLock<Mutex<HashMap<String, Instant>>> = LazyLock::new(Mutex::default);

/// Keeps the jobs `ids` awake: pins them, so Claude Code's daemon does not
/// retire them for sitting idle, and starts each one that is cold again in
/// the background where it last worked, with its conversation and no new
/// message. A cold job with no conversation to resume is left alone:
/// `claude --bg --resume` would start a new job in its place that fails,
/// cold in its turn, and so on without end.
fn keep(args: &Value) -> Result<Value, Error> {
    let ids: Vec<&str> = args
        .get("ids")
        .and_then(Value::as_array)
        .map(|ids| ids.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if ids.is_empty() {
        return Ok(json!({ "woken": [] }));
    }
    let seen = find();
    let projects = config_dir()
        .map(|config| dirs(&config.join("projects")))
        .unwrap_or_default();
    let resumable = |id: &str| resume_id(id).filter(|s| transcript(&projects, s).is_some());
    let ids: Vec<&str> = ids
        .into_iter()
        .filter(|id| {
            seen.iter().any(|s| s.live && s.id.as_deref() == Some(id)) || resumable(id).is_some()
        })
        .collect();
    if ids.is_empty() {
        return Ok(json!({ "woken": [] }));
    }
    set_pinned(&ids, true)?;
    let mut woken = Vec::new();
    for id in ids {
        let Some(agent) = seen.iter().find(|s| s.id.as_deref() == Some(id)) else {
            continue;
        };
        if agent.live {
            continue;
        }
        {
            let mut at = WOKEN.lock().unwrap_or_else(PoisonError::into_inner);
            if at.get(id).is_some_and(|at| at.elapsed() < WAKE_AGAIN) {
                continue;
            }
            at.insert(id.to_string(), Instant::now());
        }
        let Some(session) = resumable(id) else {
            continue;
        };
        let dir = Some(agent.dir.as_path()).filter(|dir| dir.is_dir());
        let Ok(output) = claude(&["--bg", "--resume", &session], dir) else {
            continue;
        };
        match attach_id(&output) {
            // It came up as a job of its own rather than as `id`, which
            // stays cold: the new one is no agent anyone started.
            Some(other) if other != id => {
                let _ = claude(&["rm", other], None);
            }
            _ => woken.push(id),
        }
    }
    Ok(json!({ "woken": woken }))
}

/// Lets job `id` rest: unpins it, and stops it if it runs.
fn stop(args: &Value) -> Result<Value, Error> {
    let id = required(args, "id")?;
    set_pinned(&[id], false)?;
    if find().iter().any(|s| s.live && s.id.as_deref() == Some(id)) {
        claude(&["stop", id], None)?;
    }
    Ok(json!({}))
}

/// Deletes a background session, running or not, so nothing finds it again:
/// `claude rm`. A session that entered a tree with `EnterWorktree` hands that
/// tree back through the `WorktreeRemove` hook as it goes.
fn remove(args: &Value) -> Result<Value, Error> {
    let id = required(args, "id")?;
    set_pinned(&[id], false)?;
    claude(&["rm", id], None)?;
    Ok(json!({}))
}

/// Every background job that worked in `dir` or under it, in any state:
/// failed ones too, which no report shows.
fn jobs_in(dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = worked_in(dir).into_iter().map(|(id, _)| id).collect();
    ids.sort();
    ids
}

/// Every background job that worked in `dir` or under it, with when its
/// state was last written.
fn worked_in(dir: &Path) -> Vec<(String, SystemTime)> {
    let Some(jobs) = config_dir().map(|config| config.join("jobs")) else {
        return Vec::new();
    };
    dirs(&jobs)
        .into_iter()
        .filter_map(|job| {
            let state = job.join("state.json");
            let worked = job_state(&fs::read_to_string(&state).ok()?)?.dir()?;
            let at = fs::metadata(&state).and_then(|m| m.modified()).ok()?;
            let id = job.file_name()?.to_str()?.to_string();
            worked.starts_with(dir).then_some((id, at))
        })
        .collect()
}

/// Deletes every background job that worked in `dir`, running or not, so a
/// tree handed back keeps none of them, and nothing keeps them awake.
fn clear(args: &Value) -> Result<Value, Error> {
    let dir = PathBuf::from(required(args, "dir")?);
    let ids = jobs_in(&dir);
    if ids.is_empty() {
        return Ok(json!({ "removed": [], "failed": [] }));
    }
    set_pinned(&ids.iter().map(String::as_str).collect::<Vec<_>>(), false)?;
    let (mut removed, mut failed) = (Vec::new(), Vec::new());
    for id in ids {
        match claude(&["rm", &id], None) {
            Ok(_) => removed.push(id),
            Err(err) => failed.push(format!("{id}: {err}")),
        }
    }
    Ok(json!({ "removed": removed, "failed": failed }))
}

/// Where Claude Code asks how much of the plan is used, as `/usage` does.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// How long a reading of the plan is used before it is asked for again.
const USAGE_EVERY: Duration = Duration::from_secs(60);

/// How long after a failed ask the plan is not asked for again, so an
/// outage or a rate limit is never pressed on.
const USAGE_BACKOFF: Duration = Duration::from_secs(5 * 60);

/// How long one ask gets.
const USAGE_LIMIT: Duration = Duration::from_secs(10);

/// `name` in `CLAUDE_CONFIG_DIR`, else in the home directory under
/// `inside`, as Claude Code keeps its account files.
fn account_path(name: &str, inside: &str) -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(Path::new(&dir).join(name)),
        None => std::env::var_os("HOME").map(|home| Path::new(&home).join(inside).join(name)),
    }
}

/// A limit's name on screen, from its kind and the model it is scoped to.
fn limit_name(kind: &str, model: Option<&str>) -> String {
    let name = match kind {
        "session" => "5-hour".to_string(),
        "weekly_all" | "weekly_scoped" => "weekly".to_string(),
        other => other.replace('_', " "),
    };
    match model {
        Some(model) => format!("{name} {model}"),
        None => name,
    }
}

/// The plan's limits from `utilization`, as the usage endpoint answers and
/// Claude Code caches it, read at `at`.
fn plan(utilization: &Value, at: Option<u64>) -> Option<Plan> {
    let limits = utilization
        .get("limits")?
        .as_array()?
        .iter()
        .filter_map(|limit| {
            let model = limit
                .pointer("/scope/model/display_name")
                .and_then(Value::as_str);
            let percent = limit.get("percent")?.as_f64()?;
            Some(Limit {
                name: limit_name(limit.get("kind")?.as_str()?, model),
                percent: percent.round().clamp(0.0, 1000.0) as u64,
                resets_at: limit
                    .get("resets_at")
                    .and_then(Value::as_str)
                    .and_then(crate::pool::seconds),
            })
        })
        .collect();
    Some(Plan { limits, at })
}

/// The plan as Claude Code last fetched it for `/usage`, from the text of
/// its `.claude.json`.
fn cached_plan(text: &str) -> Option<Plan> {
    let account: Value = serde_json::from_str(text).ok()?;
    let cached = account.get("cachedUsageUtilization")?;
    let at = cached
        .get("fetchedAtMs")
        .and_then(Value::as_u64)
        .map(|ms| ms / 1000);
    plan(cached.get("utilization")?, at)
}

/// Claude Code's login, from the text of its `.credentials.json`, while it
/// has not expired at `now_ms`. It is never refreshed here: a refresh
/// replaces the token Claude Code holds, and would sign it out.
fn token(text: &str, now_ms: u64) -> Option<String> {
    let credentials: Value = serde_json::from_str(text).ok()?;
    let oauth = credentials.get("claudeAiOauth")?;
    let expires = oauth.get("expiresAt").and_then(Value::as_u64)?;
    (expires > now_ms)
        .then(|| oauth.get("accessToken")?.as_str().map(str::to_string))
        .flatten()
}

fn now() -> Duration {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
}

/// Asks how much of the plan is used now, with Claude Code's login.
fn fetch_plan() -> Result<Plan, String> {
    let credentials = account_path(".credentials.json", ".claude")
        .and_then(|path| fs::read_to_string(path).ok())
        .ok_or("no Claude Code login to ask with")?;
    let token = token(
        &credentials,
        u64::try_from(now().as_millis()).unwrap_or(u64::MAX),
    )
    .ok_or("Claude Code's login has expired; it renews it when next used")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(USAGE_LIMIT))
        .build()
        .into();
    let mut response = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", "oauth-2025-04-20")
        .call()
        .map_err(|err| format!("{USAGE_URL}: {err}"))?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|err| format!("{USAGE_URL}: {err}"))?;
    let body: Value = serde_json::from_str(&text).map_err(|err| format!("{USAGE_URL}: {err}"))?;
    plan(&body, Some(now().as_secs())).ok_or_else(|| format!("{USAGE_URL} gave no limits"))
}

/// The last plan read here, and when the next ask may go.
struct Asked {
    plan: Option<Plan>,
    next: Instant,
}

static ASKED: LazyLock<Mutex<Option<Asked>>> = LazyLock::new(Mutex::default);

/// How much of the plan is used: asked of Anthropic as `/usage` asks, at
/// most every USAGE_EVERY, else the latest reading, here or Claude Code's;
/// none when there is neither.
fn usage() -> Result<Value, Error> {
    let mut asked = ASKED.lock().unwrap_or_else(PoisonError::into_inner);
    if asked.as_ref().is_none_or(|a| Instant::now() >= a.next) {
        let fetched = fetch_plan();
        let wait = if fetched.is_ok() {
            USAGE_EVERY
        } else {
            USAGE_BACKOFF
        };
        let last = asked.take().and_then(|a| a.plan);
        *asked = Some(Asked {
            plan: fetched.ok().or(last),
            next: Instant::now() + wait,
        });
    }
    let cached = || {
        account_path(".claude.json", "")
            .and_then(|path| fs::read_to_string(path).ok())
            .as_deref()
            .and_then(cached_plan)
    };
    // Whichever reading is newer: Claude Code may have asked since.
    let plan = [asked.as_ref().and_then(|a| a.plan.clone()), cached()]
        .into_iter()
        .flatten()
        .max_by_key(|plan| plan.at)
        .unwrap_or_default();
    serde_json::to_value(plan).map_err(|err| Error::new(Code::Failed, err.to_string()))
}

/// Answers one request as the Claude Code plugin.
pub fn op(op: &str, args: &Value) -> Result<Value, Error> {
    match op {
        "describe" => Ok(json!({
            "protocol": PROTOCOL,
            "name": "claude",
            "kind": "agent",
            "ops": [
                "start", "find", "attach", "send", "resume", "stop", "remove", "keep", "clear",
                "usage",
            ],
            // The prompt box sits between two rules; a permission or
            // question dialog takes its place and numbers its choices. The
            // agent view, which `claude attach` goes back to, prompts for a
            // new session and offers to return.
            "screen": {
                "input": "❯",
                "rule": "─",
                "dialogs": ["Do you want to", "❯ 1."],
                "away": ["describe a task for a new session", "enter to return"],
            },
        })),
        "find" => Ok(json!({ "agents": find() })),
        "start" => start(args),
        "attach" => Ok(json!({ "argv": attach(required(args, "id")?) })),
        "send" => send(args),
        "resume" => resume(args),
        "usage" => usage(),
        "stop" => stop(args),
        "remove" => remove(args),
        "keep" => keep(args),
        "clear" => clear(args),
        _ => Err(Error::new(
            Code::Unsupported,
            format!("claude does not answer {op}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a session file as Claude Code 2.1 writes it.
    const SESSION: &str = r#"{"pid":2263518,"sessionId":"e6b79280-2509-446f-ab75-bf36407bf701",
      "cwd":"/home/me/app","startedAt":1791475782827,"procStart":"163482243","version":"2.1.294",
      "kind":"bg","entrypoint":"cli","tmux":null,"name":"trading-card-check","nameSource":"auto",
      "status":"waiting","updatedAt":1791637420174}"#;

    fn session() -> Session {
        serde_json::from_str(SESSION).unwrap()
    }

    fn here() -> PathBuf {
        PathBuf::from("/p/4/app")
    }

    #[test]
    fn a_live_session_is_seen_with_its_name_status_and_chat() {
        let seen = live_session(&session(), Some(163482243), here()).unwrap();
        assert_eq!(seen.name, "trading-card-check");
        assert_eq!(seen.status.as_deref(), Some("waiting"));
        assert!(seen.background && seen.live);
        assert_eq!(seen.id.as_deref(), Some("e6b79280"));
        assert_eq!(
            seen.attach,
            Some(vec!["claude".into(), "attach".into(), "e6b79280".into()])
        );
    }

    #[test]
    fn an_interactive_session_has_no_attach_command() {
        let interactive: Session =
            serde_json::from_str(&SESSION.replace(r#""kind":"bg""#, r#""kind":"interactive""#))
                .unwrap();
        let seen = live_session(&interactive, Some(163482243), here()).unwrap();
        assert!(!seen.background);
        assert_eq!((seen.attach, seen.id), (None, None));
    }

    #[test]
    fn an_exited_or_reused_pid_is_no_live_session() {
        assert_eq!(live_session(&session(), None, here()), None);
        assert_eq!(live_session(&session(), Some(999), here()), None);
    }

    #[test]
    fn takes_the_last_cwd_and_skips_one_cut_off_by_the_tail() {
        let tail = concat!(
            r#"d":"/p/4/app","type":"user"}"#,
            "\n",
            r#"{"cwd":"/p/11/app","type":"assistant"}"#,
            "\n",
            r#"{"cwd":"/p/11/app/ui","message":"a \"quoted\" cwd\":\"/nope\""}"#,
            "\n",
            r#"{"cwd":"/p/1"#,
        );
        assert_eq!(last_cwd(tail), Some(PathBuf::from("/p/11/app/ui")));
        assert_eq!(last_cwd("no entries"), None);
    }

    #[test]
    fn a_long_context_is_measured_against_a_million_tokens() {
        assert_eq!(window(Some("claude-sonnet-4-5"), 150_000), 200_000);
        assert_eq!(window(Some("claude-sonnet-4-5"), 250_000), 1_000_000);
        assert_eq!(window(Some("claude-sonnet-4-5[1m]"), 50_000), 1_000_000);
        assert_eq!(window(Some("claude-opus-5-5"), 50_000), 1_000_000);
        assert_eq!(window(None, 50_000), 200_000);
    }

    #[test]
    fn finds_the_newest_transcript_across_project_dirs() {
        let root = std::env::temp_dir().join(format!("treetop-claude-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (launch, tree) = (root.join("launch"), root.join("tree"));
        fs::create_dir_all(&launch).unwrap();
        fs::create_dir_all(&tree).unwrap();
        fs::write(tree.join("s1.jsonl"), r#"{"cwd":"/p/11/app"}"#).unwrap();
        assert_eq!(
            transcript(&[launch.clone(), tree.clone()], "s1"),
            Some(tree.join("s1.jsonl"))
        );
        assert_eq!(transcript(&[launch], "s2"), None);
    }

    #[test]
    fn starts_in_the_background_with_the_prompt_after_a_double_dash() {
        assert_eq!(
            start_args(Some("END-9 fix login"), None, Some("-fix it")),
            ["--bg", "--name", "END-9 fix login", "--", "-fix it"]
        );
        assert_eq!(start_args(None, None, None), ["--bg"]);
    }

    #[test]
    fn a_session_in_a_tree_of_its_own_is_told_to_stay_there() {
        let stay = stay(Path::new("/p/7/app"));
        assert!(stay.contains("/p/7/app") && stay.contains("Do not call EnterWorktree"));
        assert_eq!(
            start_args(None, Some(&stay), Some("fix it")),
            ["--bg", "--append-system-prompt", &stay, "--", "fix it"]
        );
    }

    #[test]
    fn finds_the_attach_id_claude_prints() {
        let output = "started\n  claude attach cbcfd8c8    open in this terminal\n";
        assert_eq!(attach_id(output), Some("cbcfd8c8"));
        assert_eq!(attach_id("claude attach <id>"), None);
    }

    #[test]
    fn a_login_is_used_only_until_it_expires_and_never_refreshed() {
        let credentials =
            r#"{"claudeAiOauth":{"accessToken":"sk-ant-oat","refreshToken":"r","expiresAt":2000}}"#;
        assert_eq!(token(credentials, 1999).as_deref(), Some("sk-ant-oat"));
        assert_eq!(
            token(credentials, 2000),
            None,
            "expired: left for Claude Code"
        );
        assert_eq!(token("{}", 0), None);
        assert_eq!(token("not json", 0), None);
    }

    #[test]
    fn reads_the_limits_as_the_usage_endpoint_answers() {
        let body = json!({
            "five_hour": { "utilization": 21.0 },
            "limits": [
                { "kind": "session", "percent": 21, "resets_at": "2026-10-27T22:00:00+00:00" },
                { "kind": "weekly_scoped", "percent": 0.4, "resets_at": null,
                  "scope": { "model": { "display_name": "Fable" } } },
                { "kind": "monthly_spend", "percent": 3 },
                { "percent": 9 },
            ],
        });
        let plan = plan(&body, Some(7)).unwrap();
        assert_eq!(plan.at, Some(7));
        let read: Vec<(&str, u64, Option<u64>)> = plan
            .limits
            .iter()
            .map(|l| (l.name.as_str(), l.percent, l.resets_at))
            .collect();
        assert_eq!(
            read,
            [
                ("5-hour", 21, Some(1_793_138_400)),
                ("weekly Fable", 0, None),
                ("monthly spend", 3, None),
            ],
            "a limit with no kind is left out"
        );
        assert_eq!(super::plan(&json!({}), None), None);
    }

    #[test]
    fn ops_it_does_not_answer_are_unsupported() {
        assert_eq!(op("list", &json!({})).unwrap_err().code, Code::Unsupported);
        assert_eq!(op("attach", &json!({})).unwrap_err().message, "no id given");
    }
}
