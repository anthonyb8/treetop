//! Plugins: executables that speak JSON on stdin and stdout, so treetop can
//! start agents and read tickets without knowing whose they are. Every file
//! in the plugin directory that can be executed is one.
//!
//! One process per call: treetop writes one request and closes stdin, then
//! reads one reply from stdout. stderr is kept for the error. The process
//! runs in its own process group, which is killed when the call outlives its
//! timeout, or when the plugin exits but leaves a child holding stdout open.
//!
//! ```json
//! {"protocol": 1, "op": "describe", "config": {}, "args": {}}
//! {"ok": true, "result": {"protocol": 1, "name": "claude", "kind": "agent", "ops": ["start"]}}
//! {"ok": false, "error": {"code": "not_found", "message": "no job 0908e7f3"}}
//! ```
//!
//! Within a protocol, fields are only ever added, and both sides ignore the
//! ones they do not know.

use std::fmt;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

/// The protocol this treetop speaks.
pub const PROTOCOL: u32 = 1;

/// How often a running plugin is checked for having exited.
const POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Starts, finds, attaches to, messages and stops agents.
    Agent,
    /// Lists tickets, reads one, names its branch and marks it started.
    Tickets,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Agent => "agent",
            Kind::Tickets => "tickets",
        }
    }
}

/// What `describe` says a plugin is.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Description {
    pub protocol: u32,
    pub name: String,
    pub kind: Kind,
    /// The ops it answers besides `describe`.
    #[serde(default)]
    pub ops: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    NotFound,
    Unsupported,
    Auth,
    Conflict,
    /// Timed out, or could not be reached.
    Unavailable,
    /// Anything else, including a code this treetop does not know.
    #[serde(other)]
    Failed,
}

impl Code {
    pub fn label(self) -> &'static str {
        match self {
            Code::NotFound => "not found",
            Code::Unsupported => "unsupported",
            Code::Auth => "auth",
            Code::Conflict => "conflict",
            Code::Unavailable => "unavailable",
            Code::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Error {
    pub code: Code,
    #[serde(default)]
    pub message: String,
}

impl Error {
    fn new(code: Code, message: impl Into<String>) -> Self {
        Error {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.message.is_empty() {
            f.write_str(self.code.label())
        } else {
            write!(f, "{}: {}", self.code.label(), self.message)
        }
    }
}

impl std::error::Error for Error {}

#[derive(Deserialize)]
struct Reply {
    ok: bool,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<Error>,
}

/// How long `op` may run before its plugin is killed.
pub fn timeout(op: &str) -> Duration {
    match op {
        "describe" | "find" => Duration::from_secs(3),
        "list" | "get" => Duration::from_secs(10),
        _ => Duration::from_secs(30),
    }
}

/// `$XDG_CONFIG_HOME/treetop/plugins`, else `~/.config/treetop/plugins`.
pub fn dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .map(|config| config.join("treetop").join("plugins"))
}

/// Reads a pipe to its end on a thread of its own, so a plugin that writes
/// more than a pipe holds is never stuck waiting for treetop to read.
fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    rx
}

fn kill_group(child: &mut Child) {
    // The plugin leads its own group, so this reaches whatever it started.
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.wait();
}

/// Waits for `child` to exit, killing its group at `deadline`.
fn wait(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => thread::sleep(POLL),
            _ => {
                kill_group(child);
                return None;
            }
        }
    }
}

/// Starts the plugin in a process group of its own. A plugin file still
/// open for writing, because it is being installed or because another
/// thread forked while writing it, is busy for a moment, so that is retried
/// until `deadline`.
fn spawn(program: &Path, deadline: Instant) -> Result<Child, Error> {
    loop {
        let spawned = Command::new(program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn();
        match spawned {
            Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline => {
                thread::sleep(POLL);
            }
            Err(err) => {
                let message = format!("{}: {err}", program.display());
                return Err(Error::new(Code::Unavailable, message));
            }
            Ok(child) => return Ok(child),
        }
    }
}

/// Runs `op` on the plugin at `program` with `args`, and returns its result.
pub fn call(program: &Path, op: &str, args: Value, limit: Duration) -> Result<Value, Error> {
    let request = json!({ "protocol": PROTOCOL, "op": op, "config": {}, "args": args });
    let deadline = Instant::now() + limit;
    let mut child = spawn(program, deadline)?;
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        kill_group(&mut child);
        return Err(Error::new(Code::Failed, "no pipes to the plugin"));
    };
    // Written on a thread too: a plugin that never reads its request must
    // not hold treetop past the deadline.
    thread::spawn(move || {
        let _ = stdin.write_all(request.to_string().as_bytes());
    });
    let (stdout, stderr) = (drain(stdout), drain(stderr));
    let timed_out = || Error::new(Code::Unavailable, format!("{op} took over {limit:?}"));
    let status = wait(&mut child, deadline).ok_or_else(timed_out)?;
    let left = deadline.saturating_duration_since(Instant::now());
    let Ok(stdout) = stdout.recv_timeout(left) else {
        kill_group(&mut child);
        return Err(Error::new(
            Code::Unavailable,
            format!("{op} exited but left its output open"),
        ));
    };
    let stderr = stderr.recv_timeout(POLL).unwrap_or_default();
    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    reply(&stdout, status, stderr)
}

/// The result a plugin's stdout gives, or its error.
fn reply(stdout: &[u8], status: ExitStatus, stderr: String) -> Result<Value, Error> {
    if !status.success() {
        let message = if stderr.is_empty() {
            format!("exited with {status}")
        } else {
            stderr
        };
        return Err(Error::new(Code::Failed, message));
    }
    let reply: Reply = serde_json::from_slice(stdout).map_err(|err| {
        let message = if stderr.is_empty() {
            format!("invalid reply: {err}")
        } else {
            format!("invalid reply: {err}; {stderr}")
        };
        Error::new(Code::Failed, message)
    })?;
    match (reply.ok, reply.error) {
        (true, _) => Ok(reply.result),
        (false, Some(error)) => Err(error),
        (false, None) => Err(Error::new(Code::Failed, stderr)),
    }
}

/// What the plugin at `program` says it is, if it speaks this protocol.
pub fn describe(program: &Path) -> Result<Description, Error> {
    let result = call(program, "describe", json!({}), timeout("describe"))?;
    let description: Description = serde_json::from_value(result)
        .map_err(|err| Error::new(Code::Failed, format!("invalid description: {err}")))?;
    if description.protocol != PROTOCOL {
        return Err(Error::new(
            Code::Unsupported,
            format!(
                "speaks protocol {}, treetop speaks {PROTOCOL}",
                description.protocol
            ),
        ));
    }
    Ok(description)
}

/// A file in the plugin directory and what it said it is.
pub struct Found {
    pub path: PathBuf,
    pub description: Result<Description, Error>,
}

/// Every executable file in `dir`, by name.
fn executables(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            fs::metadata(path)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .collect();
    paths.sort();
    paths
}

/// Every plugin in `dir`, described at once, so one slow plugin costs its
/// own timeout rather than adding to everyone else's.
pub fn discover(dir: &Path) -> Vec<Found> {
    let handles: Vec<_> = executables(dir)
        .into_iter()
        .map(|path| {
            thread::spawn(move || Found {
                description: describe(&path),
                path,
            })
        })
        .collect();
    handles.into_iter().filter_map(|h| h.join().ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory for one test's plugins.
    fn scratch(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("treetop-plugin-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    const DESCRIBE: &str = r#"cat >/dev/null; echo '{"ok":true,"result":{"protocol":1,"name":"echo","kind":"agent","ops":["start"],"extra":1}}'"#;

    #[test]
    fn a_plugin_reads_the_request_and_its_result_comes_back() {
        let dir = scratch("echo");
        let echo = script(&dir, "echo", r#"printf '{"ok":true,"result":%s}' "$(cat)""#);
        let result = call(
            &echo,
            "start",
            json!({"dir": "/p/1"}),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"protocol": 1, "op": "start", "config": {}, "args": {"dir": "/p/1"}})
        );
    }

    #[test]
    fn an_error_reply_keeps_its_code_and_an_unknown_code_reads_as_failed() {
        let dir = scratch("errors");
        let missing = script(
            &dir,
            "missing",
            r#"echo '{"ok":false,"error":{"code":"not_found","message":"no job 9"}}'"#,
        );
        let odd = script(
            &dir,
            "odd",
            r#"echo '{"ok":false,"error":{"code":"melted"}}'"#,
        );
        let limit = Duration::from_secs(5);
        assert_eq!(
            call(&missing, "send", json!({}), limit),
            Err(Error::new(Code::NotFound, "no job 9"))
        );
        assert_eq!(
            call(&odd, "send", json!({}), limit).unwrap_err().code,
            Code::Failed
        );
    }

    #[test]
    fn a_crash_or_garbage_fails_with_what_the_plugin_said() {
        let dir = scratch("crash");
        let crash = script(&dir, "crash", "echo 'no API key' >&2; exit 3");
        let garbage = script(&dir, "garbage", "echo hello");
        let limit = Duration::from_secs(5);
        assert_eq!(
            call(&crash, "list", json!({}), limit),
            Err(Error::new(Code::Failed, "no API key"))
        );
        let err = call(&garbage, "list", json!({}), limit).unwrap_err();
        assert_eq!(err.code, Code::Failed);
        assert!(err.message.starts_with("invalid reply"), "{err}");
    }

    #[test]
    fn a_hung_plugin_and_its_children_are_killed_at_the_timeout() {
        let dir = scratch("hang");
        let marker = dir.join("child-survived");
        let hang = script(
            &dir,
            "hang",
            &format!("(sleep 1; touch {}) & sleep 30", marker.display()),
        );
        let started = Instant::now();
        let err = call(&hang, "start", json!({}), Duration::from_millis(200)).unwrap_err();
        assert_eq!(err.code, Code::Unavailable);
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "the plugin's child outlived the timeout");
    }

    #[test]
    fn a_plugin_that_exits_but_leaves_a_child_on_its_output_times_out() {
        let dir = scratch("leaky");
        let leaky = script(&dir, "leaky", r#"sleep 30 & echo '{"ok":true}'"#);
        let err = call(&leaky, "start", json!({}), Duration::from_millis(300)).unwrap_err();
        assert_eq!(err.code, Code::Unavailable);
        assert!(err.message.contains("left its output open"), "{err}");
    }

    #[test]
    fn discovery_describes_every_executable_and_skips_other_files() {
        let dir = scratch("discover");
        script(&dir, "a-good", DESCRIBE);
        script(
            &dir,
            "b-future",
            r#"echo '{"ok":true,"result":{"protocol":2,"name":"future","kind":"agent"}}'"#,
        );
        fs::write(dir.join("c-notes.txt"), "not a plugin").unwrap();
        let found = discover(&dir);
        let names: Vec<_> = found.iter().map(|f| f.path.file_name().unwrap()).collect();
        assert_eq!(names, ["a-good", "b-future"]);
        let good = found[0].description.as_ref().unwrap();
        assert_eq!((good.name.as_str(), good.kind), ("echo", Kind::Agent));
        assert_eq!(good.ops, ["start"]);
        let future = found[1].description.as_ref().unwrap_err();
        assert_eq!(future.code, Code::Unsupported);
    }

    #[test]
    fn a_missing_directory_has_no_plugins() {
        assert!(discover(Path::new("/nonexistent/treetop/plugins")).is_empty());
    }
}
