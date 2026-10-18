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
//!   carries the session's `cwd`, which after `EnterWorktree` is the tree.
//! - `<config>/jobs/<id>/state.json`: every background job, running or not,
//!   which `reports` reads. One with no running session is a cold agent.
//!
//! A background session opens in a terminal with `claude attach <id>`, where
//! the id is the short one `claude --bg` prints: the session id's first eight
//! characters. Attaching to a cold one starts it again.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::child::{self, End};
use crate::live;
use crate::plugin::{Code, Error, PROTOCOL, Seen};
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
            let cwd = transcript
                .as_deref()
                .and_then(tail)
                .and_then(|tail| last_cwd(&tail))
                .or_else(|| fs::read_link(format!("/proc/{}/cwd", session.pid)).ok())?;
            // It last worked when its transcript last grew.
            let active_at = transcript
                .and_then(|path| fs::metadata(path).and_then(|m| m.modified()).ok())
                .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_secs());
            live_session(&session, live::stat(session.pid).map(|s| s.start), cwd)
                .map(|seen| Seen { active_at, ..seen })
        })
        .collect()
}

/// The background jobs' reports, read again only where they changed.
static JOBS: LazyLock<Mutex<Reader>> = LazyLock::new(Mutex::default);

/// Every background job with no running session, as a cold agent where it
/// last worked.
fn cold(live: &[Seen]) -> Vec<Seen> {
    let running: HashSet<&str> = live.iter().filter_map(|s| s.id.as_deref()).collect();
    let reports = JOBS.lock().unwrap_or_else(PoisonError::into_inner).read();
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

/// The arguments `start` runs `claude` with: in the background, named, and
/// the prompt after `--`, so one that starts with `-` is never read as a
/// flag.
fn start_args<'a>(name: Option<&'a str>, prompt: Option<&'a str>) -> Vec<&'a str> {
    let mut args = vec!["--bg"];
    if let Some(name) = name {
        args.extend(["--name", name]);
    }
    if let Some(prompt) = prompt {
        args.extend(["--", prompt]);
    }
    args
}

/// Starts a session in `dir` and waits until it runs, stopping it again if
/// it never does.
fn start(args: &Value) -> Result<Value, Error> {
    let dir = PathBuf::from(required(args, "dir")?);
    let out = claude(
        &start_args(text(args, "name"), text(args, "prompt")),
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

/// The session a background job ran as, which `--resume` takes.
fn resume_id(id: &str) -> Option<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct State {
        #[serde(default)]
        resume_session_id: Option<String>,
        #[serde(default)]
        session_id: Option<String>,
    }
    let path = config_dir()?.join("jobs").join(id).join("state.json");
    let state: State = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    state.resume_session_id.or(state.session_id)
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

/// Answers one request as the Claude Code plugin.
pub fn op(op: &str, args: &Value) -> Result<Value, Error> {
    match op {
        "describe" => Ok(json!({
            "protocol": PROTOCOL,
            "name": "claude",
            "kind": "agent",
            "ops": ["start", "find", "attach", "send", "stop"],
            // The prompt box sits between two rules; a permission or
            // question dialog takes its place and numbers its choices.
            "screen": {
                "input": "❯",
                "rule": "─",
                "dialogs": ["Do you want to", "❯ 1."],
            },
        })),
        "find" => Ok(json!({ "agents": find() })),
        "start" => start(args),
        "attach" => Ok(json!({ "argv": attach(required(args, "id")?) })),
        "send" => send(args),
        "stop" => {
            claude(&["stop", required(args, "id")?], None)?;
            Ok(json!({}))
        }
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
            start_args(Some("END-9 fix login"), Some("-fix it")),
            ["--bg", "--name", "END-9 fix login", "--", "-fix it"]
        );
        assert_eq!(start_args(None, None), ["--bg"]);
    }

    #[test]
    fn finds_the_attach_id_claude_prints() {
        let output = "started\n  claude attach cbcfd8c8    open in this terminal\n";
        assert_eq!(attach_id(output), Some("cbcfd8c8"));
        assert_eq!(attach_id("claude attach <id>"), None);
    }

    #[test]
    fn ops_it_does_not_answer_are_unsupported() {
        assert_eq!(op("list", &json!({})).unwrap_err().code, Code::Unsupported);
        assert_eq!(op("attach", &json!({})).unwrap_err().message, "no id given");
    }
}
