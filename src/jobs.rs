//! Returning and destroying, run by one worker thread from a queue while the
//! TUI stays up. Jobs run one at a time, so two git operations never contend
//! for one repository's locks, and every command's output is captured for the
//! log pane instead of reaching the terminal.

use std::fmt::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::pool::Tree;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Return,
    /// `treehouse destroy`'s dry run, shown for review before anything goes.
    Preview,
    Destroy,
}

impl Kind {
    /// The row's STATUS while the job runs.
    pub fn running(self) -> &'static str {
        match self {
            Kind::Return => "returning",
            Kind::Preview => "previewing",
            Kind::Destroy => "destroying",
        }
    }

    /// The row's STATUS once the job has succeeded.
    pub fn done(self) -> &'static str {
        match self {
            Kind::Return => "returned",
            Kind::Preview => "previewed",
            Kind::Destroy => "destroyed",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Return => "return",
            Kind::Preview => "destroy preview",
            Kind::Destroy => "destroy",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub kind: Kind,
    pub tree: Tree,
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
                let (is_ok, output) = run(&job);
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

fn treehouse(subcommand: &str, path: &Path) -> Command {
    let mut command = Command::new("treehouse");
    command.arg(subcommand).arg(path);
    command
}

/// Runs one job to completion. Return passes `--force` because treetop's own
/// confirm, which names every tree's uncommitted and unpushed work, has
/// already replaced treehouse's prompt. treehouse stops the processes left in
/// the tree; anything started outside it, such as a container, is for
/// whatever started it to stop.
fn run(job: &Job) -> (bool, String) {
    let path = &job.tree.path;
    let mut log = String::new();
    let is_ok = match job.kind {
        Kind::Return => capture(treehouse("return", path).arg("--force"), &mut log),
        Kind::Preview => capture(treehouse("destroy", path).args(INCLUDE), &mut log),
        Kind::Destroy => capture(
            treehouse("destroy", path).args(INCLUDE).arg("--yes"),
            &mut log,
        ),
    };
    (is_ok, log)
}
