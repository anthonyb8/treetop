//! Returning and destroying, run by one worker thread from a queue while the
//! TUI stays up. Jobs run one at a time, so two git operations never contend
//! for one repository's locks, and every command's output is captured for the
//! log pane instead of reaching the terminal.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::pool::Tree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Starts a new agent in the tree, leasing it first if nobody holds it.
    Start,
    Return,
    /// `treehouse destroy`'s dry run, shown for review before anything goes.
    Preview,
    Destroy,
}

impl Kind {
    /// The row's STATUS while the job runs.
    pub fn running(self) -> &'static str {
        match self {
            Kind::Start => "starting",
            Kind::Return => "returning",
            Kind::Preview => "previewing",
            Kind::Destroy => "destroying",
        }
    }

    /// The row's STATUS once the job has succeeded.
    pub fn done(self) -> &'static str {
        match self {
            Kind::Start => "started",
            Kind::Return => "returned",
            Kind::Preview => "previewed",
            Kind::Destroy => "destroyed",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Start => "agent start",
            Kind::Return => "return",
            Kind::Preview => "destroy preview",
            Kind::Destroy => "destroy",
        }
    }
}

/// What a Start is asked to do beyond starting an agent in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The agent's first message; empty starts it idle, to be typed to in
    /// its chat.
    pub task: String,
    /// Its session name, when it is Claude Code.
    pub name: Option<String>,
    /// Lease a fresh tree from the pool first and start it there; `tree` is
    /// a stand-in until then.
    pub is_new_tree: bool,
    /// Start the coordinator rather than an agent for the task.
    pub is_coordinator: bool,
}

impl Launch {
    /// A new agent on `task`, named for it.
    pub fn agent(task: String, is_new_tree: bool) -> Self {
        Launch {
            name: crate::dispatch::session_name(&task),
            task,
            is_new_tree,
            is_coordinator: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub kind: Kind,
    pub tree: Tree,
    /// The row the job's state shows on: the tree's path, or a stand-in for
    /// a tree not leased yet, which the job's tree replaces once it is.
    pub key: PathBuf,
    pub launch: Option<Launch>,
}

impl Job {
    pub fn new(kind: Kind, tree: Tree) -> Self {
        Job {
            kind,
            key: tree.path.clone(),
            tree,
            launch: None,
        }
    }
}

pub enum Event {
    Started(Job),
    Finished {
        job: Job,
        is_ok: bool,
        output: String,
    },
}

pub struct Worker {
    jobs: Sender<Job>,
    pub events: Receiver<Event>,
}

impl Worker {
    pub fn spawn() -> Self {
        let (jobs, queue) = mpsc::channel::<Job>();
        let (events_tx, events) = mpsc::channel();
        thread::spawn(move || {
            for job in queue {
                if events_tx.send(Event::Started(job.clone())).is_err() {
                    return;
                }
                let mut job = job;
                let (is_ok, output) = run(&mut job);
                if events_tx
                    .send(Event::Finished { job, is_ok, output })
                    .is_err()
                {
                    return;
                }
            }
        });
        Worker { jobs, events }
    }

    pub fn push(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// Every risk `treehouse destroy` would otherwise skip a tree for. The preview
/// names which ones apply, and the destroy only runs once that is confirmed.
const INCLUDE: [&str; 3] = ["--include-leased", "--include-in-use", "--include-unlanded"];

/// Runs `command` with no stdin, so nothing can stop to prompt, appending its
/// stdout and stderr to `log`.
fn capture(command: &mut Command, log: &mut String) -> bool {
    match command.stdin(Stdio::null()).output() {
        Ok(out) => {
            log.push_str(&String::from_utf8_lossy(&out.stdout));
            log.push_str(&String::from_utf8_lossy(&out.stderr));
            out.status.success()
        }
        Err(err) => {
            let _ = writeln!(log, "{err}");
            false
        }
    }
}

/// The command a Start runs in a tree: the coordinator's when it is the one
/// starting, else `TREETOP_NEW_AGENT`, split on whitespace, with the task as
/// its last argument, else Claude Code in the background, named for the
/// task, whose chat `c` opens and which Claude Code's own agent view lists
/// too.
fn new_agent(launch: Option<&Launch>, dir: &Path) -> Vec<String> {
    if let Some(launch) = launch.filter(|l| l.is_coordinator) {
        return crate::coordinator::command(&crate::coordinator::treetop(), dir, &launch.task);
    }
    let custom = std::env::var("TREETOP_NEW_AGENT")
        .ok()
        .map(|command| {
            command
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|command| !command.is_empty());
    agent_command(custom, launch)
}

fn agent_command(custom: Option<Vec<String>>, launch: Option<&Launch>) -> Vec<String> {
    let task = launch
        .map(|l| l.task.clone())
        .filter(|t| !t.trim().is_empty());
    let mut command = match custom {
        Some(command) => command,
        None => {
            let mut command = vec!["claude".to_string(), "--bg".to_string()];
            if let Some(name) = launch.and_then(|l| l.name.clone()) {
                command.extend(["--name".to_string(), name]);
            }
            command
        }
    };
    command.extend(task);
    command
}

/// The pool number of a tree at `path`, `<pool>/<number>/<repo>`, else its
/// last component.
fn pool_name(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .or_else(|| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Leases a fresh tree, the way `treehouse get --lease` hands them out, and
/// points the job at it.
fn lease_new(job: &mut Job, log: &mut String) -> bool {
    let out = Command::new("treehouse")
        .args(["get", "--lease", "--lease-holder", "treetop"])
        .stdin(Stdio::null())
        .output();
    let out = match out {
        Ok(out) => out,
        Err(err) => {
            let _ = writeln!(log, "{err}");
            return false;
        }
    };
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(path) = stdout.lines().map(str::trim).rfind(|l| !l.is_empty()) else {
        let _ = writeln!(log, "treehouse get printed no tree");
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let path = PathBuf::from(path);
    job.tree.name = pool_name(&path);
    job.tree.path = path;
    job.tree.status = "leased".into();
    true
}

/// Leases the tree when nobody holds it, so the pool cannot hand it to
/// someone else under the agent, then starts the agent in it.
fn start(job: &mut Job, log: &mut String) -> bool {
    if job.launch.as_ref().is_some_and(|l| l.is_new_tree) {
        if !lease_new(job, log) {
            return false;
        }
    } else if !job.tree.is_held() {
        let leased = capture(
            Command::new("treehouse").args(["lease", &job.tree.name, "--lease-holder", "treetop"]),
            log,
        );
        if !leased {
            return false;
        }
    }
    let command = new_agent(job.launch.as_ref(), &job.tree.path);
    let Some((program, args)) = command.split_first() else {
        return false;
    };
    capture(
        Command::new(program).args(args).current_dir(&job.tree.path),
        log,
    )
}

fn treehouse(subcommand: &str, path: &Path) -> Command {
    let mut command = Command::new("treehouse");
    command.arg(subcommand).arg(path);
    command
}

/// Runs one job to completion, here or on the worker. Return passes `--force` because treetop's own
/// confirm, which names every tree's uncommitted and unpushed work, has
/// already replaced treehouse's prompt. treehouse stops the processes left in
/// the tree; anything started outside it, such as a container, is for
/// whatever started it to stop.
pub fn run(job: &mut Job) -> (bool, String) {
    let mut log = String::new();
    let is_ok = match job.kind {
        Kind::Start => start(job, &mut log),
        Kind::Return => capture(treehouse("return", &job.tree.path).arg("--force"), &mut log),
        Kind::Preview => capture(treehouse("destroy", &job.tree.path).args(INCLUDE), &mut log),
        Kind::Destroy => capture(
            treehouse("destroy", &job.tree.path)
                .args(INCLUDE)
                .arg("--yes"),
            &mut log,
        ),
    };
    (is_ok, log)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(task: &str) -> Launch {
        Launch::agent(task.into(), false)
    }

    #[test]
    fn a_task_starts_claude_named_for_it_and_nothing_starts_it_idle() {
        assert_eq!(
            agent_command(None, Some(&launch("END-9 fix login"))),
            [
                "claude",
                "--bg",
                "--name",
                "END-9 fix login",
                "END-9 fix login"
            ]
        );
        assert_eq!(agent_command(None, Some(&launch("  "))), ["claude", "--bg"]);
        assert_eq!(agent_command(None, None), ["claude", "--bg"]);
    }

    #[test]
    fn a_custom_agent_gets_the_task_as_its_last_argument() {
        let custom = Some(vec!["codex".to_string(), "exec".to_string()]);
        assert_eq!(
            agent_command(custom, Some(&launch("fix login"))),
            ["codex", "exec", "fix login"]
        );
    }

    #[test]
    fn names_a_tree_by_its_pool_number() {
        assert_eq!(
            pool_name(Path::new("/home/me/.treehouse/app-1/12/app")),
            "12"
        );
    }
}
