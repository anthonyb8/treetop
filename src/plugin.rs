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

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::sync::{LazyLock, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::builtin;
use crate::child::{self, End};

/// The protocol this treetop speaks.
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// For an agent plugin, how its chat's screen shows that it takes a
    /// message, so one is typed in only when it can land.
    #[serde(default)]
    pub screen: Option<Screen>,
}

/// What an agent's chat looks like on screen, for typing a message into it.
/// The input box is a line starting with `input` between two lines starting
/// with `rule`; a screen with no input box and one of `dialogs` on it is
/// waiting on an answer that a message would land in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    pub input: String,
    pub rule: String,
    #[serde(default)]
    pub dialogs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Error {
    pub code: Code,
    #[serde(default)]
    pub message: String,
}

impl Error {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
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

/// Runs `op` on the plugin at `program` with `args`, and returns its result.
pub fn call(program: &Path, op: &str, args: Value, limit: Duration) -> Result<Value, Error> {
    let request = json!({ "protocol": PROTOCOL, "op": op, "config": {}, "args": args });
    let ran = child::run(
        &mut Command::new(program),
        Some(request.to_string().into_bytes()),
        limit,
    )
    .map_err(|err| Error::new(Code::Unavailable, err.to_string()))?;
    let stderr = String::from_utf8_lossy(&ran.stderr).trim().to_string();
    match ran.end {
        End::Exited(status) => reply(&ran.stdout, status, stderr),
        End::TimedOut => Err(Error::new(
            Code::Unavailable,
            format!("{op} took over {limit:?}"),
        )),
        End::HeldOpen(_) => Err(Error::new(
            Code::Unavailable,
            format!("{op} exited but left its output open"),
        )),
    }
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

/// An agent as an agent plugin's `find` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// What `attach`, `send` and `stop` take; None for an agent they cannot
    /// reach.
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    /// Where it works, which places it in a tree.
    pub dir: PathBuf,
    /// Running now. One that is not is cold, and opening it starts it again.
    #[serde(default = "running")]
    pub live: bool,
    #[serde(default)]
    pub pid: Option<u32>,
    /// `busy`, `idle` or `waiting`, where the plugin can tell.
    #[serde(default)]
    pub status: Option<String>,
    /// Runs with no terminal of its own.
    #[serde(default)]
    pub background: bool,
    /// The command that opens its chat in a terminal.
    #[serde(default)]
    pub attach: Option<Vec<String>>,
}

fn running() -> bool {
    true
}

/// A plugin treetop can call: one it ships, answered in process with the
/// same requests and replies `treetop plugin <name>` serves, or one in the
/// plugin directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plugin {
    Builtin(&'static str),
    External { name: String, path: PathBuf },
}

impl Plugin {
    pub fn name(&self) -> &str {
        match self {
            Plugin::Builtin(name) => name,
            Plugin::External { name, .. } => name,
        }
    }

    pub fn call(&self, op: &str, args: Value) -> Result<Value, Error> {
        match self {
            Plugin::Builtin(name) => builtin::handle(name, op, &args),
            Plugin::External { path, .. } => call(path, op, args, timeout(op)),
        }
    }

    /// What it says it is.
    pub fn description(&self) -> Result<Description, Error> {
        match self {
            Plugin::Builtin(name) => {
                let result = builtin::handle(name, "describe", &json!({}))?;
                serde_json::from_value(result)
                    .map_err(|err| Error::new(Code::Failed, format!("invalid description: {err}")))
            }
            Plugin::External { path, .. } => described(path),
        }
    }
}

/// What each plugin file said it is, with the file's modification time, so
/// a plugin is described again only once it changes.
type Described = HashMap<PathBuf, (Option<SystemTime>, Result<Description, Error>)>;

static DESCRIBED: LazyLock<Mutex<Described>> = LazyLock::new(Mutex::default);

fn described(path: &Path) -> Result<Description, Error> {
    let modified = fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some((at, description)) = DESCRIBED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        && *at == modified
    {
        return description.clone();
    }
    let description = describe(path);
    DESCRIBED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(path.to_path_buf(), (modified, description.clone()));
    description
}

/// Every plugin of `kind`: the user's, then each built-in no plugin of the
/// user's replaces by name.
pub fn of_kind(kind: Kind) -> Vec<Plugin> {
    let mut plugins: Vec<Plugin> = dir()
        .map(|dir| executables(&dir))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|path| {
            let description = described(&path).ok()?;
            (description.kind == kind).then_some(Plugin::External {
                name: description.name,
                path,
            })
        })
        .collect();
    for (name, builtin_kind) in builtin::KINDS {
        if builtin_kind == kind && !plugins.iter().any(|p| p.name() == name) {
            plugins.push(Plugin::Builtin(name));
        }
    }
    plugins
}

/// How the chats of agent plugin `name`'s agents show they take a message.
pub fn screen_of(name: &str) -> Option<Screen> {
    named(Kind::Agent, name)?.description().ok()?.screen
}

/// The plugin of `kind` named `name`, whoever provides it.
pub fn named(kind: Kind, name: &str) -> Option<Plugin> {
    of_kind(kind).into_iter().find(|p| p.name() == name)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

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
