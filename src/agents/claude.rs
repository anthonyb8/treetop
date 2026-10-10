//! Claude Code sessions. `EnterWorktree` moves a session into a tree without
//! changing its process's cwd, so the process scan cannot see it; Claude
//! Code's own files can. These files are internal and undocumented, so every
//! field is read by name, extra ones are ignored, and anything unexpected
//! yields no agent rather than an error.
//!
//! - `<config>/sessions/<pid>.json`: one per running session, with its id,
//!   name, status, kind (`interactive` or `bg`) and process start time.
//! - `<config>/projects/<dir>/<sessionId>.jsonl`: the transcript. Every entry
//!   carries the session's `cwd`, which after `EnterWorktree` is the tree.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Deserialize;
use serde_json::Value;

use super::{Agent, Sighting, Source, Status};
use crate::live;

/// How much of a transcript's end is read for its latest `cwd`. One entry is
/// rarely more than a few kilobytes.
const TAIL: u64 = 64 * 1024;

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
}

/// `CLAUDE_CONFIG_DIR`, else `~/.claude`.
fn config_dir() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".claude")))
}

/// The session as an agent, if it is still running: its pid is alive and
/// started when the session file says, so a pid reused since does not count.
/// `start` is the pid's start time now, None when it has exited.
fn agent(session: &Session, start: Option<u64>) -> Option<Agent> {
    let start = start?;
    let recorded = match &session.proc_start {
        Some(Value::String(text)) => text.parse().ok(),
        Some(Value::Number(number)) => number.as_u64(),
        _ => None,
    };
    if recorded.is_some_and(|recorded| recorded != start) {
        return None;
    }
    Some(Agent {
        name: session.name.clone().unwrap_or_else(|| "claude".into()),
        pid: session.pid,
        status: session.status.as_deref().and_then(Status::parse),
        source: Source::Claude,
        is_background: session.kind.as_deref() == Some("bg"),
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

pub fn find() -> Vec<Sighting> {
    let Some(config) = config_dir() else {
        return Vec::new();
    };
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
            let agent = agent(&session, live::stat(session.pid).map(|s| s.start))?;
            let cwd = last_cwd(&tail(&transcript(&projects, &session.session_id)?)?)?;
            Some((cwd, agent))
        })
        .collect()
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

    #[test]
    fn a_live_session_becomes_an_agent_with_its_name_and_status() {
        let agent = agent(&session(), Some(163482243)).unwrap();
        assert_eq!(agent.name, "trading-card-check");
        assert_eq!(agent.status, Some(Status::Waiting));
        assert!(agent.is_background);
    }

    #[test]
    fn an_exited_or_reused_pid_is_no_agent() {
        assert_eq!(agent(&session(), None), None);
        assert_eq!(agent(&session(), Some(999)), None);
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
}
