//! An agent's conversation for the Chat screen, read from Claude Code's
//! transcript of its session, `<config>/projects/<dir>/<sessionId>.jsonl`.
//! Each line is one record; the screen shows what a person reads in the
//! terminal: what you typed, what Claude wrote, and each tool call folded to
//! a line with how it went. Thinking, hooks, system records, subagents'
//! work and everything else are left out, as the terminal leaves them out.
//!
//! The records are Claude Code's own and undocumented, so every field is
//! read by name and a record that does not parse is skipped.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

/// Messages sent when a chat opens, the newest; older ones are not shown.
const BACKLOG: usize = 300;

/// How often the transcript is looked at for new lines.
const POLL: Duration = Duration::from_millis(400);

/// How often a quiet stream says it is still there, which is also how a
/// phone that has gone away is noticed.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// The most of a tool's output kept for when its line is opened.
const OUTPUT: usize = 2000;

/// The transcript of the background job `job`: its `state.json` names the
/// session, whose transcript is the most recently written of that name.
pub fn transcript_of(config: &Path, job: &str) -> Option<PathBuf> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct State {
        session_id: String,
    }
    let state = std::fs::read_to_string(config.join("jobs").join(job).join("state.json")).ok()?;
    let state: State = serde_json::from_str(&state).ok()?;
    let file = format!("{}.jsonl", state.session_id);
    std::fs::read_dir(config.join("projects"))
        .ok()?
        .flatten()
        .map(|dir| dir.path().join(&file))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// One transcript line's worth of what the screen shows: messages, and
/// tool calls finishing, each keyed so a later one replaces an earlier.
pub fn read_line(line: &str) -> Vec<Value> {
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    if record["isSidechain"] == true || record["isMeta"] == true {
        return Vec::new();
    }
    let at = record["timestamp"].as_str().unwrap_or_default();
    let uuid = record["uuid"].as_str().unwrap_or_default();
    let content = &record["message"]["content"];
    match record["type"].as_str() {
        Some("user") => {
            let mut out = Vec::new();
            let is_yours = record["origin"]["kind"]
                .as_str()
                .is_none_or(|k| k == "human");
            if let Some(text) = content.as_str().filter(|_| is_yours) {
                out.extend(yours(uuid, at, text));
            }
            for (i, block) in content.as_array().into_iter().flatten().enumerate() {
                match block["type"].as_str() {
                    Some("text") if is_yours => {
                        let text = block["text"].as_str().unwrap_or_default();
                        out.extend(yours(&format!("{uuid}:{i}"), at, text));
                    }
                    Some("tool_result") => out.push(finished(block)),
                    _ => {}
                }
            }
            out
        }
        Some("assistant") => content
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(i, block)| match block["type"].as_str() {
                Some("text") => {
                    let text = block["text"].as_str()?.trim();
                    (!text.is_empty()).then(|| {
                        json!({
                            "id": format!("{uuid}:{i}"),
                            "kind": "claude",
                            "at": at,
                            "text": text,
                        })
                    })
                }
                Some("tool_use") => Some(called(block, at)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A message you sent, unless it is one Claude Code wrote in your name:
/// a command's output, an interruption, a reminder.
fn yours(id: &str, at: &str, text: &str) -> Option<Value> {
    let text = text.trim();
    let is_machine = text.is_empty()
        || text.starts_with('<')
        || text.starts_with("[Request interrupted")
        || text.starts_with("Caveat:");
    (!is_machine).then(|| json!({ "id": id, "kind": "you", "at": at, "text": text }))
}

/// A tool call as one line: what it did, in a few words.
fn called(block: &Value, at: &str) -> Value {
    let name = block["name"].as_str().unwrap_or("tool");
    let input = &block["input"];
    let text = |key: &str| input[key].as_str().map(str::to_string);
    let (verb, what) = match name {
        "Bash" => (
            "Ran",
            text("description").or_else(|| first_line(text("command"))),
        ),
        "Read" => ("Read", text("file_path")),
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            ("Edited", text("file_path").or(text("notebook_path")))
        }
        "Write" => ("Wrote", text("file_path")),
        "Grep" | "Glob" => ("Searched", text("pattern")),
        "WebFetch" => ("Fetched", text("url")),
        "WebSearch" => ("Searched the web", text("query")),
        "Agent" | "Task" => ("Asked an agent", text("description")),
        "Skill" => ("Used skill", text("skill")),
        "AskUserQuestion" => ("Asked you", first_question(input)),
        _ => (name, None),
    };
    let lines = |key: &str| input[key].as_str().map_or(0, |s| s.lines().count());
    let change = match name {
        "Edit" => Some((lines("new_string"), lines("old_string"))),
        "Write" => Some((lines("content"), 0)),
        _ => None,
    };
    json!({
        "id": block["id"],
        "kind": "tool",
        "at": at,
        "tool": {
            "verb": verb,
            "what": what,
            "added": change.map(|c| c.0),
            "removed": change.map(|c| c.1),
            "status": "running",
            "output": Value::Null,
        },
    })
}

/// A tool call's result: the same id as the call, so it takes its place.
fn finished(block: &Value) -> Value {
    let output = match &block["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    let output: String = output.chars().take(OUTPUT).collect();
    json!({
        "id": block["tool_use_id"],
        "kind": "result",
        "status": if block["is_error"] == true { "error" } else { "ok" },
        "output": output,
    })
}

fn first_line(text: Option<String>) -> Option<String> {
    text.and_then(|t| t.lines().next().map(str::to_string))
}

fn first_question(input: &Value) -> Option<String> {
    input["questions"][0]["question"]
        .as_str()
        .map(str::to_string)
}

/// The chat as a stream of server-sent events: the newest BACKLOG messages
/// as one `backlog` event, then a `message` event for each new one as the
/// transcript grows, until the phone goes away.
pub fn stream(path: PathBuf) -> impl Read + Send + 'static {
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    thread::spawn(move || follow(&path, &tx));
    Events {
        rx,
        pending: Vec::new(),
    }
}

fn follow(path: &Path, tx: &SyncSender<Vec<u8>>) {
    let Ok(file) = File::open(path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    let mut backlog: Vec<Value> = Vec::new();
    let mut offset = 0;
    let mut line = String::new();
    // Only whole lines are read: a line still being written waits for
    // the next look.
    let mut read_whole = |reader: &mut BufReader<File>, offset: &mut u64, out: &mut Vec<Value>| {
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(n) if n > 0 && line.ends_with('\n') => {
                    *offset += n as u64;
                    out.extend(read_line(&line));
                }
                _ => {
                    let _ = reader.seek(SeekFrom::Start(*offset));
                    return;
                }
            }
        }
    };
    read_whole(&mut reader, &mut offset, &mut backlog);
    let backlog = fold(backlog);
    let start = backlog.len().saturating_sub(BACKLOG);
    if tx
        .send(event("backlog", &Value::Array(backlog[start..].to_vec())))
        .is_err()
    {
        return;
    }
    let mut quiet_since = Instant::now();
    loop {
        thread::sleep(POLL);
        let mut fresh = Vec::new();
        read_whole(&mut reader, &mut offset, &mut fresh);
        for message in &fresh {
            if tx.send(event("message", message)).is_err() {
                return;
            }
        }
        if !fresh.is_empty() {
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= HEARTBEAT {
            if tx.send(b": still here\n\n".to_vec()).is_err() {
                return;
            }
            quiet_since = Instant::now();
        }
    }
}

/// Messages with each tool result folded into its call, as the screen
/// would after taking them one by one.
pub fn fold(messages: Vec<Value>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    for message in messages {
        if message["kind"] == "result" {
            if let Some(call) = out.iter_mut().rev().find(|m| m["id"] == message["id"]) {
                call["tool"]["status"] = message["status"].clone();
                call["tool"]["output"] = message["output"].clone();
            }
        } else {
            out.push(message);
        }
    }
    out
}

fn event(name: &str, data: &Value) -> Vec<u8> {
    format!("event: {name}\ndata: {data}\n\n").into_bytes()
}

/// The events a following thread sends, as the body tiny_http writes out.
/// It ends when the thread does; the thread ends when the phone has gone,
/// which the next write after that tells it.
struct Events {
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl Read for Events {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pending.is_empty() {
            match self.rx.recv() {
                Ok(bytes) => self.pending = bytes,
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len());
        buf[..n].copy_from_slice(&self.pending[..n]);
        self.pending.drain(..n);
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_all(lines: &[String]) -> Vec<Value> {
        fold(lines.iter().flat_map(|l| read_line(l)).collect())
    }

    #[test]
    fn a_transcript_reads_as_the_terminal_shows_it() {
        let lines = [
            json!({"type": "user", "uuid": "u1", "timestamp": "t1", "origin": {"kind": "human"},
                   "message": {"role": "user", "content": "Fix the login loop."}}),
            json!({"type": "user", "uuid": "u2", "isMeta": true,
                   "message": {"content": [{"type": "text", "text": "Base directory for this skill"}]}}),
            json!({"type": "user", "uuid": "u3", "origin": {"kind": "task-notification"},
                   "message": {"content": "<task-notification>done</task-notification>"}}),
            json!({"type": "assistant", "uuid": "a1", "timestamp": "t2", "message": {"content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "The guard runs too early."},
                {"type": "tool_use", "id": "tu1", "name": "Edit", "input": {
                    "file_path": "src/guard.ts", "old_string": "a\nb", "new_string": "a\nb\nc"}},
                {"type": "tool_use", "id": "tu2", "name": "Bash", "input": {
                    "command": "npm test\n--watch", "description": "Run the tests"}},
            ]}}),
            json!({"type": "user", "uuid": "u4", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "tu1", "content": "ok"},
                {"type": "tool_result", "tool_use_id": "tu2", "is_error": true,
                 "content": [{"type": "text", "text": "1 failed"}]},
            ]}}),
            json!({"type": "assistant", "uuid": "a2", "isSidechain": true,
                   "message": {"content": [{"type": "text", "text": "a subagent's words"}]}}),
            json!({"type": "system", "uuid": "s1", "content": "compacted"}),
        ];
        let mut lines: Vec<String> = lines.iter().map(Value::to_string).collect();
        // Lines that are not records are skipped, and what follows them kept.
        lines.insert(1, "{\"type\": \"assistant\", \"message\": ".into());
        lines.insert(2, "not json".into());
        let messages = read_all(&lines);
        let shown: Vec<(&str, String)> = messages
            .iter()
            .map(|m| {
                let kind = m["kind"].as_str().unwrap();
                let text = match kind {
                    "tool" => format!(
                        "{} {} {}",
                        m["tool"]["verb"].as_str().unwrap(),
                        m["tool"]["what"].as_str().unwrap(),
                        m["tool"]["status"].as_str().unwrap()
                    ),
                    _ => m["text"].as_str().unwrap().to_string(),
                };
                (kind, text)
            })
            .collect();
        assert_eq!(
            shown,
            [
                ("you", "Fix the login loop.".to_string()),
                ("claude", "The guard runs too early.".to_string()),
                ("tool", "Edited src/guard.ts ok".to_string()),
                ("tool", "Ran Run the tests error".to_string()),
            ]
        );
        assert_eq!(
            (
                messages[2]["tool"]["added"].clone(),
                messages[2]["tool"]["removed"].clone()
            ),
            (json!(3), json!(2))
        );
        assert_eq!(messages[3]["tool"]["output"], "1 failed");
    }

    /// Reads from a chat stream until `want` has come, or a few seconds
    /// have passed.
    fn read_until(stream: &mut impl Read, want: &str) -> String {
        let mut seen = String::new();
        let started = Instant::now();
        let mut buf = [0; 4096];
        while !seen.contains(want) && started.elapsed() < Duration::from_secs(5) {
            let n = stream.read(&mut buf).unwrap();
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        seen
    }

    #[test]
    fn the_stream_sends_the_backlog_then_each_line_as_it_is_written() {
        let dir = std::env::temp_dir().join(format!("treetop-chat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        let said = |uuid: &str, text: &str| {
            format!(
                "{}\n",
                json!({"type": "assistant", "uuid": uuid,
                       "message": {"content": [{"type": "text", "text": text}]}})
            )
        };
        std::fs::write(&path, said("a1", "first")).unwrap();
        let mut stream = stream(path.clone());
        let backlog = read_until(&mut stream, "\n\n");
        assert!(backlog.starts_with("event: backlog\n"), "{backlog}");
        assert!(backlog.contains("\"first\""));

        // A line written in two goes is sent once it is whole.
        let line = said("a2", "second");
        let (head, tail) = line.split_at(20);
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        std::io::Write::write_all(&mut file, head.as_bytes()).unwrap();
        thread::sleep(POLL * 2);
        std::io::Write::write_all(&mut file, tail.as_bytes()).unwrap();
        let next = read_until(&mut stream, "\n\n");
        assert!(next.starts_with("event: message\n"), "{next}");
        assert!(next.contains("\"second\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn each_message_has_only_fields_the_app_reads() {
        use crate::serve::pool::tests::{declared, keys};
        let lines = [
            json!({"type": "user", "uuid": "u1", "message": {"content": "hello"}}),
            json!({"type": "assistant", "uuid": "a1", "message": {"content": [
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls"}},
            ]}}),
            json!({"type": "user", "uuid": "u2", "message": {"content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "a"},
            ]}}),
        ];
        let message = declared("ChatMessage");
        let read: Vec<Value> = lines
            .iter()
            .flat_map(|l| read_line(&l.to_string()))
            .collect();
        assert_eq!(read.len(), 4);
        for value in &read {
            assert!(keys(value).is_subset(&message), "{value}");
        }
        assert_eq!(keys(&read[2]["tool"]), declared("ToolCall"));
    }
}
