//! Starting agents, returning and destroying, run by a few worker threads
//! from one queue while the TUI stays up. Every command runs with a deadline,
//! so one that hangs costs its own job and never holds up the rest, and its
//! output is captured for the log instead of reaching the terminal. Two jobs
//! never run on one tree at once: the app queues one per tree at a time.
//!
//! Starting an agent is a transaction. Each step treetop takes is recorded,
//! and when a later one fails, the earlier ones are undone in reverse, so a
//! failed dispatch leaves the tree as it found it and says which step failed.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use issues::Backend;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agents::claude;
use crate::branch;
use crate::child::{self, End};
use crate::config::{self, Hooks};
use crate::diff;
use crate::plugin::{self, Kind as PluginKind, Plugin};
use crate::pool::{self, Tree};
use crate::ship::{self, Ship};
use crate::sidecar::{self, Sidecar};
use crate::tickets;

/// Jobs that run at once, each on its own tree.
const WORKERS: usize = 4;

/// How long treehouse gets to lease, return or destroy a tree, which can
/// mean creating a worktree or stopping what runs in it.
const TREEHOUSE_LIMIT: Duration = Duration::from_secs(120);

/// How long one git command gets.
const GIT_LIMIT: Duration = Duration::from_secs(30);

/// How long a hook gets, such as one stopping a dev stack.
const HOOK_LIMIT: Duration = Duration::from_secs(120);

/// How long an agent's command gets to start it.
const AGENT_LIMIT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Starts a new agent in the tree, leasing it first if nobody holds it.
    Start,
    Return,
    /// `treehouse destroy`'s dry run, shown for review before anything goes.
    Preview,
    Destroy,
    /// Gives the tree's branch the job's `rename`, or names one on a
    /// detached HEAD.
    Rename,
    /// Sends the job's `message` to a cold agent through its plugin.
    Send,
    /// Reads what failed on the tree's PR from its ship plugin, for its agent.
    Fix,
    /// Follows up the merge of the tree's PR in its ticket's project: marks
    /// the ticket done where its source says to, and finds the tickets it
    /// leaves with nothing blocking them.
    Land,
    /// Starts a ship run of the tree's branch, or finds the command that
    /// shows the one under way.
    Ship,
    /// Starts the tree's stopped or failed agent again, with its
    /// conversation and the flags it was started with.
    Resume,
    /// Starts a new agent in the tree on the handoff its agent wrote, then
    /// stops the old one.
    Handoff,
}

impl Kind {
    /// The row's STATUS while the job runs.
    pub fn running(self) -> &'static str {
        match self {
            Kind::Start => "starting",
            Kind::Return => "returning",
            Kind::Preview => "previewing",
            Kind::Destroy => "destroying",
            Kind::Rename => "renaming",
            Kind::Send => "sending",
            Kind::Fix => "reading",
            Kind::Land => "following up",
            Kind::Ship => "shipping",
            Kind::Resume => "resuming",
            Kind::Handoff => "starting",
        }
    }

    /// The row's STATUS once the job has succeeded.
    pub fn done(self) -> &'static str {
        match self {
            Kind::Start => "started",
            Kind::Return => "returned",
            Kind::Preview => "previewed",
            Kind::Destroy => "destroyed",
            Kind::Rename => "renamed",
            Kind::Send => "sent",
            Kind::Fix => "read",
            Kind::Land => "followed up",
            Kind::Ship => "run started",
            Kind::Resume => "resumed",
            Kind::Handoff => "handed off",
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            Kind::Start => "agent start",
            Kind::Return => "return",
            Kind::Preview => "destroy preview",
            Kind::Destroy => "destroy",
            Kind::Rename => "branch rename",
            Kind::Send => "message",
            Kind::Fix => "failure read",
            Kind::Land => "merge follow-up",
            Kind::Ship => "ship run",
            Kind::Resume => "resume",
            Kind::Handoff => "handoff",
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
    /// The branch it works on in a tree treetop leases for it; None names
    /// one from the task.
    pub branch: Option<String>,
    /// Lease a fresh tree from the pool first and start it there; `tree` is
    /// a stand-in until then.
    pub is_new_tree: bool,
    /// The branch that fresh tree is cut from; None cuts it from the pool's
    /// base.
    pub base: Option<String>,
    /// Start the assistant rather than an agent for the task.
    pub is_assistant: bool,
    /// The ticket it picks up, which gives it its task, name and branch.
    pub ticket: Option<Pick>,
}

/// A ticket to pick up, from the ticket source of the repository checked
/// out at `checkout`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    pub id: String,
    pub checkout: PathBuf,
    /// The issue source it is in; None for the repository's own.
    pub source: Option<String>,
}

impl Pick {
    fn backend(&self) -> Result<Arc<dyn Backend>, String> {
        tickets::named(self.source.as_deref(), &self.checkout)
    }
}

impl Launch {
    /// An agent on ticket `id`, in a new tree; its task, name and branch come
    /// from the ticket when the job runs.
    pub fn pick(id: String, checkout: PathBuf, source: Option<String>) -> Self {
        Launch {
            ticket: Some(Pick {
                id: id.clone(),
                checkout,
                source,
            }),
            name: Some(id),
            ..Launch::agent(String::new(), true)
        }
    }
}

impl Launch {
    /// A new agent on `task`, named for it.
    pub fn agent(task: String, is_new_tree: bool) -> Self {
        Launch {
            name: crate::dispatch::session_name(&task),
            task,
            branch: None,
            is_new_tree,
            base: None,
            is_assistant: false,
            ticket: None,
        }
    }
}

/// A message for an agent that is not running, which its plugin delivers by
/// starting it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub plugin: String,
    pub id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub kind: Kind,
    pub tree: Tree,
    /// The row the job's state shows on: the tree's path, or a stand-in for
    /// a tree not leased yet, which the job's tree replaces once it is.
    pub key: PathBuf,
    pub launch: Option<Launch>,
    /// The branch name a Rename gives the tree.
    pub rename: Option<String>,
    /// What a Send sends, and to whom.
    pub message: Option<Message>,
    /// The command that opens the chat of the agent a Start started, once it
    /// has.
    pub attach: Option<Vec<String>>,
    /// The ticket whose merge a Land follows up.
    pub landing: Option<Pick>,
    /// The tickets a Land found ready, once it has.
    pub ready: Vec<String>,
    /// What a Ship asks of which ship plugin.
    pub shipping: Option<ShipCall>,
    /// The agent a Handoff's new agent takes over from, stopped once the new
    /// one runs.
    pub replaces: Option<Replaced>,
}

/// An agent another takes over from, as its plugin knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Replaced {
    pub agent: String,
    pub plugin: String,
    pub id: String,
}

/// A ship plugin's run of a tree's branch: started, or shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShipCall {
    pub plugin: String,
    pub action: ShipAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShipAction {
    /// Start a run, its intent from the tree's ticket in the ticket source
    /// of the repository checked out here, else from its task.
    Run { checkout: PathBuf },
    /// Find the command that shows the run under way, into the job's
    /// `attach`.
    Attach,
}

impl Job {
    pub fn new(kind: Kind, tree: Tree) -> Self {
        Job {
            kind,
            key: tree.path.clone(),
            tree,
            launch: None,
            rename: None,
            message: None,
            attach: None,
            landing: None,
            ready: Vec::new(),
            shipping: None,
            replaces: None,
        }
    }
}

pub enum Event {
    Started(Job),
    /// A start finished one of its steps, for the job on row `key`, so the
    /// window it was asked from can show it under way.
    Step {
        key: PathBuf,
        step: &'static str,
    },
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
        let queue = Arc::new(Mutex::new(queue));
        for _ in 0..WORKERS {
            let (queue, events_tx) = (Arc::clone(&queue), events_tx.clone());
            thread::spawn(move || {
                loop {
                    let next = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
                    let Ok(mut job) = next else {
                        return;
                    };
                    if events_tx.send(Event::Started(job.clone())).is_err() {
                        return;
                    }
                    PROGRESS.set(Some((events_tx.clone(), job.key.clone())));
                    let (is_ok, output) = run(&mut job);
                    PROGRESS.set(None);
                    if events_tx
                        .send(Event::Finished { job, is_ok, output })
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
        Worker { jobs, events }
    }

    pub fn push(&self, job: Job) {
        let _ = self.jobs.send(job);
    }
}

/// Every risk `treehouse destroy` would otherwise skip a tree for. The preview
/// names which ones apply, and the destroy only runs once that is confirmed.
const INCLUDE: [&str; 3] = ["--include-leased", "--include-in-use", "--include-unlanded"];

/// The last line worth reading in a command's output.
fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|line| !line.is_empty())
}

/// Runs `command` with no stdin, so nothing can stop to prompt, and a
/// deadline, appending its stdout and stderr to `log`. Its stdout, or why it
/// failed.
fn exec(command: &mut Command, limit: Duration, log: &mut String) -> Result<String, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let ran = child::run(command, None, limit).map_err(|err| format!("{program}: {err}"))?;
    log.push_str(&ran.text());
    let stdout = String::from_utf8_lossy(&ran.stdout).into_owned();
    match ran.end {
        End::Exited(status) if status.success() => Ok(stdout),
        End::Exited(status) => Err(last_line(&String::from_utf8_lossy(&ran.stderr))
            .or_else(|| last_line(&stdout))
            .map_or_else(|| format!("{program}: {status}"), str::to_string)),
        End::TimedOut => Err(format!("{program} took over {}s", limit.as_secs())),
        End::HeldOpen(_) => Err(format!("{program} left its output open")),
    }
}

/// `exec` for a command whose output is only for the log.
fn capture(command: &mut Command, limit: Duration, log: &mut String) -> bool {
    match exec(command, limit, log) {
        Ok(_) => true,
        Err(why) => {
            let _ = writeln!(log, "{why}");
            false
        }
    }
}

/// The agent plugin new agents start with when nobody names another.
const DEFAULT_AGENT: &str = "claude";

/// How a Start runs its agent.
enum Starter {
    /// `TREETOP_NEW_AGENT`, split on whitespace, with the task as its last
    /// argument: any agent, with no plugin.
    Command(Vec<String>),
    /// The assistant, a Claude Code session with its own flags, run by the
    /// Claude Code plugin's rules.
    Assistant(Vec<String>, Plugin),
    /// An agent plugin's `start`.
    Plugin(Plugin),
}

/// How `launch` starts: the assistant's command, else `TREETOP_NEW_AGENT`,
/// else the default agent plugin, else the first there is.
fn starter(launch: &Launch, dir: &Path) -> Result<Starter, String> {
    let agent = || {
        plugin::named(PluginKind::Agent, DEFAULT_AGENT)
            .or_else(|| plugin::of_kind(PluginKind::Agent).into_iter().next())
            .ok_or_else(|| "start: no agent plugin".to_string())
    };
    if launch.is_assistant {
        let command = crate::assistant::command(&crate::assistant::treetop(), dir, &launch.task);
        return Ok(Starter::Assistant(command, agent()?));
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
    match custom {
        Some(command) => Ok(Starter::Command(custom_command(command, launch))),
        None => agent().map(Starter::Plugin),
    }
}

/// A custom agent gets the task as its last argument, as it always has.
fn custom_command(mut command: Vec<String>, launch: &Launch) -> Vec<String> {
    command.extend(Some(launch.task.clone()).filter(|t| !t.trim().is_empty()));
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

/// What `treehouse get --lease --json` and `treehouse lease --json` print.
#[derive(Debug, Deserialize)]
struct Lease {
    #[serde(default)]
    path: Option<PathBuf>,
    #[serde(default)]
    lease_id: Option<String>,
}

/// A step already taken, undone in reverse when a later one fails.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Undo {
    /// Give the tree back, if the lease is still the one taken.
    Lease {
        lease_id: Option<String>,
    },
    /// Leave the branch and delete it.
    Branch(String),
    /// Leave a branch that was there before, keeping it.
    Detach,
    Sidecar,
    /// Stop the agent with this id through its plugin.
    Agent(Plugin, String),
}

fn git(tree: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(tree);
    command
}

fn treehouse(subcommand: &str, path: &Path) -> Command {
    let mut command = Command::new("treehouse");
    command.arg(subcommand).arg(path);
    command
}

fn undo(step: Undo, tree: &Path, log: &mut String) {
    let _ = match step {
        Undo::Agent(plugin, id) => match plugin.call("stop", json!({ "id": id })) {
            Ok(_) => true,
            Err(err) => {
                let _ = writeln!(log, "could not stop {id}: {err}");
                false
            }
        },
        Undo::Sidecar => sidecar::remove(tree).is_ok(),
        Undo::Branch(name) => {
            capture(git(tree).args(["switch", "--detach"]), GIT_LIMIT, log)
                && capture(git(tree).args(["branch", "-D", &name]), GIT_LIMIT, log)
        }
        Undo::Detach => capture(git(tree).args(["switch", "--detach"]), GIT_LIMIT, log),
        Undo::Lease { lease_id } => {
            let mut command = treehouse("return", tree);
            command.arg("--force");
            match &lease_id {
                Some(id) => command.args(["--if-lease-id", id]),
                None => command.args(["--if-lease-holder", "treetop"]),
            };
            capture(&mut command, TREEHOUSE_LIMIT, log)
        }
    };
}

/// The tree treehouse handed over is one: a directory with a git directory
/// of its own and nothing uncommitted in it.
fn check(tree: &Path, log: &mut String) -> Result<(), String> {
    if !tree.is_dir() {
        return Err(format!(
            "treehouse gave {}, which is no directory",
            tree.display()
        ));
    }
    if pool::git_dir(tree).is_none_or(|dir| !dir.is_dir()) {
        return Err(format!("{} is not a git worktree", tree.display()));
    }
    let status = exec(git(tree).args(["status", "--porcelain"]), GIT_LIMIT, log)?;
    if !status.trim().is_empty() {
        return Err(format!("{} has uncommitted changes", tree.display()));
    }
    Ok(())
}

/// The branch the tree has checked out, None on a detached HEAD.
fn checked_out(tree: &Path) -> Option<String> {
    let head = std::fs::read_to_string(pool::git_dir(tree)?.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

/// Leases a tree, then names its branch, checks it, writes its sidecar,
/// starts the agent and confirms it runs, recording each step in `undone`.
/// A tree someone already holds is only started in: its branch and lease
/// stay theirs. Says the agent's id, where its plugin gave one.
fn start_steps(
    job: &mut Job,
    log: &mut String,
    undone: &mut Vec<Undo>,
) -> Result<Option<String>, String> {
    let mut timed = Timed::new();
    let started = timed_steps(job, log, undone, &mut timed);
    let _ = writeln!(log, "{timed}");
    started
}

thread_local! {
    /// Where the job this worker thread runs says each step it finishes,
    /// and the row it is on: set round each job, so a start's steps reach
    /// the screen as they end without passing a sender down every call.
    static PROGRESS: std::cell::RefCell<Option<(Sender<Event>, PathBuf)>> =
        const { std::cell::RefCell::new(None) };
}

/// How long each step of a start took, for its log, so a slow start says
/// which step was slow.
struct Timed {
    at: std::time::Instant,
    steps: Vec<(&'static str, Duration)>,
}

impl Timed {
    fn new() -> Self {
        Timed {
            at: std::time::Instant::now(),
            steps: Vec::new(),
        }
    }

    /// Ends `step`, which ran since the last one ended.
    fn mark(&mut self, step: &'static str) {
        let now = std::time::Instant::now();
        self.steps.push((step, now - self.at));
        self.at = now;
        PROGRESS.with_borrow(|progress| {
            if let Some((events, key)) = progress {
                let _ = events.send(Event::Step {
                    key: key.clone(),
                    step,
                });
            }
        });
    }
}

impl std::fmt::Display for Timed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let total: Duration = self.steps.iter().map(|(_, took)| *took).sum();
        write!(f, "took {:.1}s:", total.as_secs_f64())?;
        for (step, took) in &self.steps {
            write!(f, " {step} {:.1}s", took.as_secs_f64())?;
        }
        Ok(())
    }
}

fn timed_steps(
    job: &mut Job,
    log: &mut String,
    undone: &mut Vec<Undo>,
    timed: &mut Timed,
) -> Result<Option<String>, String> {
    let mut launch = job
        .launch
        .clone()
        .unwrap_or_else(|| Launch::agent(String::new(), false));
    if let Some(pick) = launch.ticket.clone() {
        take_up(&pick, &mut launch).map_err(|why| format!("ticket: {why}"))?;
        job.launch = Some(launch.clone());
        timed.mark("ticket");
    }
    let is_leasing = launch.is_new_tree || !job.tree.is_held();
    // What nobody named, the agent or a leased tree's branch, a model names
    // from the task. The start never waits for it: the task's words name both
    // until the answer comes, and `name_later` puts it in place then.
    let is_name_from_task = launch.name == crate::dispatch::session_name(&launch.task);
    let is_branch_from_task = is_leasing && launch.branch.is_none();
    let naming = (!launch.is_assistant
        && launch.ticket.is_none()
        && !launch.task.trim().is_empty()
        && (is_name_from_task || is_branch_from_task))
        .then(|| {
            let task = launch.task.clone();
            thread::spawn(move || crate::naming::suggest(&task))
        });
    let branch = (is_leasing && !launch.is_assistant).then(|| {
        launch
            .branch
            .clone()
            .unwrap_or_else(|| branch::for_launch(&branch::user(), &launch.task))
    });
    if let Some(name) = &branch
        && let Some(problem) = branch::problem(name)
    {
        return Err(format!("branch {name}: {problem}"));
    }
    // A ticket picked up again resumes its branch rather than failing on it.
    let resumes = launch
        .ticket
        .as_ref()
        .zip(branch.as_deref())
        .is_some_and(|(pick, name)| has_branch(&pick.checkout, name));
    let mut record = Sidecar::leased_now();
    record.ticket = launch.ticket.as_ref().map(|pick| pick.id.clone());
    record.source = launch.ticket.as_ref().and_then(|pick| pick.source.clone());
    record.task = Some(launch.task.trim().to_string()).filter(|t| !t.is_empty());
    if launch.is_new_tree {
        let mut command = Command::new("treehouse");
        command.args(["get", "--lease", "--json", "--lease-holder", "treetop"]);
        if let Some(name) = branch.as_ref().filter(|_| !resumes) {
            command.args(["--branch", name]);
        }
        if let Some(base) = &launch.base {
            command.args(["--base", base]);
            record.base = Some(base.clone());
        }
        let out =
            exec(&mut command, TREEHOUSE_LIMIT, log).map_err(|why| format!("lease: {why}"))?;
        let lease: Lease = serde_json::from_str(out.trim())
            .map_err(|err| format!("lease: treehouse printed no lease: {err}"))?;
        let path = lease.path.ok_or("lease: treehouse printed no tree")?;
        undone.push(Undo::Lease {
            lease_id: lease.lease_id.clone(),
        });
        job.tree.name = pool_name(&path);
        job.tree.path = path;
        job.tree.status = "leased".into();
        if let Some(name) = branch.as_ref().filter(|_| !resumes) {
            undone.push(Undo::Branch(name.clone()));
        }
        record.lease_id = lease.lease_id;
    } else if is_leasing {
        let out = exec(
            Command::new("treehouse").args([
                "lease",
                &job.tree.name,
                "--json",
                "--lease-holder",
                "treetop",
            ]),
            TREEHOUSE_LIMIT,
            log,
        )
        .map_err(|why| format!("lease: {why}"))?;
        let lease_id = serde_json::from_str::<Lease>(out.trim())
            .ok()
            .and_then(|l| l.lease_id);
        undone.push(Undo::Lease {
            lease_id: lease_id.clone(),
        });
        record.lease_id = lease_id;
    }
    // A step that did not run here is not marked, so the window says it
    // was not needed rather than ticking it.
    if launch.is_new_tree || is_leasing {
        timed.mark("lease");
    }
    if is_leasing {
        check(&job.tree.path, log).map_err(|why| format!("check: {why}"))?;
        timed.mark("check");
    }
    let mut is_branched = false;
    if let Some(name) = &branch
        && resumes
    {
        exec(git(&job.tree.path).args(["switch", name]), GIT_LIMIT, log)
            .map_err(|why| format!("branch: {why}"))?;
        undone.push(Undo::Detach);
        is_branched = true;
    } else if let Some(name) = &branch
        && !launch.is_new_tree
        && checked_out(&job.tree.path).is_none()
    {
        exec(
            git(&job.tree.path).args(["switch", "-c", name]),
            GIT_LIMIT,
            log,
        )
        .map_err(|why| format!("branch: {why}"))?;
        undone.push(Undo::Branch(name.clone()));
        is_branched = true;
    }
    // A fresh tree is leased on its branch.
    if is_branched || launch.is_new_tree && branch.is_some() {
        timed.mark("branch");
    }
    let starter = starter(&launch, &job.tree.path)?;
    let later = Later {
        naming,
        tree: job.tree.path.clone(),
        branch: branch.clone().filter(|_| is_branch_from_task),
        is_titled: is_name_from_task,
    };
    if is_leasing {
        record.agent = Some(match &starter {
            Starter::Command(command) => agent_name(&command[0]),
            Starter::Assistant(_, plugin) | Starter::Plugin(plugin) => plugin.name().to_string(),
        });
        record.branch = checked_out(&job.tree.path);
        sidecar::write(&job.tree.path, &record)
            .map_err(|err| format!("sidecar: could not write {}: {err}", sidecar::FILE))?;
        undone.push(Undo::Sidecar);
        timed.mark("record");
    }
    let (plugin, id) = match starter {
        Starter::Command(command) => {
            let (program, args) = command.split_first().ok_or("start: no agent command")?;
            exec(
                Command::new(program).args(args).current_dir(&job.tree.path),
                AGENT_LIMIT,
                log,
            )
            .map_err(|why| format!("start: {why}"))?;
            timed.mark("agent");
            later.spawn(None);
            return Ok(None);
        }
        Starter::Assistant(command, plugin) => {
            let (program, args) = command.split_first().ok_or("start: no agent command")?;
            let out = exec(
                Command::new(program).args(args).current_dir(&job.tree.path),
                AGENT_LIMIT,
                log,
            )
            .map_err(|why| format!("start: {why}"))?;
            let id = claude::attach_id(&out)
                .ok_or("start: claude printed no session id")?
                .to_string();
            undone.push(Undo::Agent(plugin.clone(), id.clone()));
            claude::confirm(&id).map_err(|err| format!("confirm: {}", err.message))?;
            (plugin, id)
        }
        Starter::Plugin(plugin) => {
            let started = plugin
                .call(
                    "start",
                    json!({
                        "dir": job.tree.path,
                        "name": launch.name,
                        "prompt": launch.task,
                        // A pool tree is the agent's own to stay in; the main
                        // checkout is everyone's.
                        "isolated": !job.tree.is_main,
                    }),
                )
                .map_err(|err| format!("start: {}", err.message))?;
            let id = started
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| format!("start: {} gave no id", plugin.name()))?
                .to_string();
            undone.push(Undo::Agent(plugin.clone(), id.clone()));
            (plugin, id)
        }
    };
    timed.mark("agent");
    let _ = writeln!(log, "{} started {id}", plugin.name());
    job.attach = plugin
        .call("attach", json!({ "id": id }))
        .ok()
        .and_then(|reply| serde_json::from_value(reply["argv"].clone()).ok());
    timed.mark("attach");
    if is_leasing {
        record.job = Some(id.clone());
        if let Err(err) = sidecar::write(&job.tree.path, &record) {
            let _ = writeln!(log, "could not record the job in {}: {err}", sidecar::FILE);
        }
    }
    later.spawn(Some(id.clone()));
    Ok(Some(id))
}

/// A started agent's name, once the model gives it: what `name_later` needs.
struct Later {
    naming: Option<thread::JoinHandle<Option<crate::naming::Name>>>,
    tree: PathBuf,
    /// The branch named from the task's words, which the model's name
    /// replaces; None when the branch was named another way.
    branch: Option<String>,
    /// The agent was named from the task's words, so its title replaces them.
    is_titled: bool,
}

/// The threads putting names in place, which a command that exits once its
/// agent starts waits for.
static NAMING: Mutex<Vec<thread::JoinHandle<()>>> = Mutex::new(Vec::new());

impl Later {
    /// Waits for the name off the start's thread, then puts it in place for
    /// agent `id`.
    fn spawn(self, id: Option<String>) {
        let Some(naming) = self.naming else {
            return;
        };
        let handle = thread::spawn(move || {
            if let Some(name) = naming.join().ok().flatten() {
                name_later(
                    &self.tree,
                    self.branch.as_deref(),
                    id.filter(|_| self.is_titled),
                    &name,
                );
            }
        });
        NAMING
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(handle);
    }
}

/// Waits until every started agent's name is in place.
pub fn wait_for_names() {
    let handles = std::mem::take(&mut *NAMING.lock().unwrap_or_else(PoisonError::into_inner));
    for handle in handles {
        let _ = handle.join();
    }
}

/// Gives agent `job` the model's title, as treetop shows it, and renames
/// the tree's branch from `words`, the task's, to the model's, while that is
/// still safe: the tree is on it and nothing has been pushed from it.
fn name_later(tree: &Path, words: Option<&str>, job: Option<String>, name: &crate::naming::Name) {
    if let Some(job) = job {
        crate::state::add_title(&job, &name.title);
    }
    let Some(words) = words else {
        return;
    };
    let user = branch::user();
    let named = if user.is_empty() {
        name.slug.clone()
    } else {
        format!("{user}/{}", name.slug)
    };
    let is_pushed = pool::git(tree, &["rev-parse", "--verify", "--quiet", "@{upstream}"]).is_some();
    if named == words
        || checked_out(tree).as_deref() != Some(words)
        || is_pushed
        || branch::problem(&named).is_some()
        || has_branch(tree, &named)
        || pool::git(tree, &["branch", "-m", words, &named]).is_none()
    {
        return;
    }
    if let Some(mut record) = sidecar::read(tree).filter(|r| r.branch.as_deref() == Some(words)) {
        record.branch = Some(named);
        let _ = sidecar::write(tree, &record);
    }
}

/// Fills `launch` from the ticket it picks up: the prompt from the config's
/// template, a session name led by the ticket's id, and the branch the
/// ticket system names, unless one was given.
fn take_up(pick: &Pick, launch: &mut Launch) -> Result<(), String> {
    let ticket = pick.backend()?.detail(&pick.id)?.issue;
    let template = config::load()?.prompt;
    launch.task = tickets::prompt(template.as_deref(), &ticket);
    launch.name = crate::dispatch::session_name(&format!("{} {}", ticket.id, ticket.title));
    if launch.branch.is_none() {
        launch.branch = ticket.branch;
    }
    Ok(())
}

/// The repository checked out at `checkout` has `branch`, here or on
/// `origin`, so `git switch` can check it out.
fn has_branch(checkout: &Path, branch: &str) -> bool {
    [
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
    ]
    .iter()
    .any(|r| pool::git(checkout, &["rev-parse", "--verify", "--quiet", r]).is_some())
}

/// Marks the picked ticket started and assigned once its agent runs. The
/// agent keeps running if this fails, and the log says so.
fn mark_started(pick: &Pick, log: &mut String) {
    let marked = pick.backend().and_then(|source| source.start(&pick.id));
    let _ = match marked {
        Ok(()) => writeln!(log, "marked {} started and assigned to you", pick.id),
        Err(why) => writeln!(log, "could not mark {} started: {why}", pick.id),
    };
}

/// Runs the start's steps and, when one fails, undoes the ones taken, then
/// says which failed and why on the last line, which the status line shows.
fn start(job: &mut Job, log: &mut String) -> bool {
    let mut undone = Vec::new();
    match start_steps(job, log, &mut undone) {
        Ok(_) => {
            if let Some(pick) = job.launch.as_ref().and_then(|l| l.ticket.as_ref()) {
                mark_started(pick, log);
            }
            run_hook("on_dispatch", &hooks(log).on_dispatch, &job.tree.path, log);
            true
        }
        Err(why) => {
            roll_back(undone, &job.tree.path, &why, log);
            false
        }
    }
}

/// Undoes the steps a failed start took, in reverse, then says why it
/// failed on the last line.
fn roll_back(undone: Vec<Undo>, tree: &Path, why: &str, log: &mut String) {
    if !undone.is_empty() {
        let _ = writeln!(log, "rolling back");
    }
    for step in undone.into_iter().rev() {
        undo(step, tree, log);
    }
    let _ = writeln!(log, "{why}");
}

/// Starts the job's agent in its tree on the handoff the agent it replaces
/// wrote, points the tree's record at the new one, so its reports and `R`
/// follow it, then stops the old one, whose conversation is kept. A start
/// that fails leaves the old agent working as it was.
fn hand_off(job: &mut Job, log: &mut String) -> bool {
    let Some(old) = job.replaces.clone() else {
        let _ = writeln!(log, "no agent to hand off from");
        return false;
    };
    // The tree's record must name the new agent, or the old one, stopped,
    // would still count in the tree and be woken to keep it awake. A
    // custom command starts an agent with no id to name, so it is refused
    // before it starts one beside the old.
    if let Some(launch) = &job.launch
        && let Ok(Starter::Command(_)) = starter(launch, &job.tree.path)
    {
        let _ = writeln!(
            log,
            "TREETOP_NEW_AGENT starts agents treetop cannot name in the tree, so it cannot hand off; {} keeps working",
            old.agent
        );
        return false;
    }
    let Some(mut record) = sidecar::read(&job.tree.path) else {
        let _ = writeln!(
            log,
            "tree {} has no {} to name the new agent in",
            job.tree.name,
            sidecar::FILE
        );
        return false;
    };
    let mut undone = Vec::new();
    let id = match start_steps(job, log, &mut undone) {
        Ok(Some(id)) => id,
        Ok(None) => {
            let _ = writeln!(
                log,
                "the new agent has no id to record, so {} keeps working too; stop one of them",
                old.agent
            );
            return false;
        }
        Err(why) => {
            roll_back(undone, &job.tree.path, &why, log);
            return false;
        }
    };
    record.job = Some(id);
    if let Err(err) = sidecar::write(&job.tree.path, &record) {
        let _ = writeln!(
            log,
            "could not record the new agent in {}: {err}, so {} keeps working too",
            sidecar::FILE,
            old.agent
        );
        return false;
    }
    let stopped = plugin::named(PluginKind::Agent, &old.plugin)
        .ok_or_else(|| format!("no agent plugin {}", old.plugin))
        .and_then(|plugin| {
            plugin
                .call("stop", json!({ "id": old.id }))
                .map_err(|err| err.to_string())
        });
    let _ = match stopped {
        Ok(_) => writeln!(
            log,
            "stopped {} ({}); its conversation is kept",
            old.agent, old.id
        ),
        Err(why) => writeln!(log, "could not stop {} ({}): {why}", old.agent, old.id),
    };
    true
}

/// The agent a command starts, as the sidecar names it: the program's file
/// name.
fn agent_name(program: &str) -> String {
    Path::new(program).file_name().map_or_else(
        || program.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Gives the tree's branch the name `to`, or checks a new one out on a
/// detached HEAD, and keeps its sidecar in step.
fn rename(tree: &Path, to: &str, log: &mut String) -> bool {
    if let Some(problem) = branch::problem(to) {
        let _ = writeln!(log, "branch {to}: {problem}");
        return false;
    }
    let mut command = git(tree);
    match checked_out(tree) {
        Some(from) if from == to => return true,
        Some(_) => command.args(["branch", "-m", to]),
        None => command.args(["switch", "-c", to]),
    };
    if !capture(&mut command, GIT_LIMIT, log) {
        return false;
    }
    if let Some(mut record) = sidecar::read(tree) {
        record.branch = Some(to.to_string());
        if let Err(err) = sidecar::write(tree, &record) {
            let _ = writeln!(
                log,
                "could not record the branch in {}: {err}",
                sidecar::FILE
            );
        }
    }
    true
}

/// The hooks the config names, or none when it cannot be read, which the log
/// says.
fn hooks(log: &mut String) -> Hooks {
    config::load().map_or_else(
        |err| {
            let _ = writeln!(log, "config: {err}; running no hooks");
            Hooks::default()
        },
        |config| config.hooks,
    )
}

/// The `on_return` of the repository `tree` belongs to, under `[repos]`.
fn repo_on_return(tree: &Path) -> Vec<String> {
    let checkout = pool::git_dir(tree)
        .map(|dir| pool::common_dir(&dir))
        .and_then(|common| std::fs::canonicalize(common).ok())
        .and_then(|common| common.parent().map(Path::to_path_buf));
    checkout
        .zip(config::load().ok())
        .and_then(|(checkout, config)| config.repo(&checkout).map(|r| r.on_return.clone()))
        .unwrap_or_default()
}

/// Runs a hook in `tree`. A failure goes to the log and nothing else.
fn run_hook(name: &str, command: &[String], tree: &Path, log: &mut String) {
    run_hook_for(name, command, tree, HOOK_LIMIT, log);
}

/// `run_hook`, given `limit` to finish in.
pub fn run_hook_for(
    name: &str,
    command: &[String],
    tree: &Path,
    limit: Duration,
    log: &mut String,
) -> bool {
    let Some((program, args)) = command.split_first() else {
        return true;
    };
    let record = sidecar::read(tree);
    let field = |pick: fn(&Sidecar) -> Option<&String>| {
        record.as_ref().and_then(pick).cloned().unwrap_or_default()
    };
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(tree)
        .env("TREETOP_TREE", tree)
        .env("TREETOP_BRANCH", field(|r| r.branch.as_ref()))
        .env("TREETOP_TICKET", field(|r| r.ticket.as_ref()));
    match exec(&mut command, limit, log) {
        Ok(_) => true,
        Err(why) => {
            let _ = writeln!(log, "{name} hook failed: {why}");
            false
        }
    }
}

/// Ends every background agent a plugin reports in the tree, through that
/// plugin, since treehouse stops only processes standing in it, and a
/// session that moved in with `EnterWorktree` stands elsewhere. A plugin
/// that answers `clear` deletes every job that worked in the tree, those it
/// does not report, such as failed ones, included. Elsewhere each agent is
/// removed where its plugin can, so it is not found in the tree once the
/// tree is leased again, else stopped, cold ones too, so its plugin stops
/// keeping it awake.
fn end_agents(tree: &Tree, log: &mut String) {
    let answers = |plugin: &Plugin, op: &str| {
        plugin
            .description()
            .is_ok_and(|d| d.ops.iter().any(|o| o == op))
    };
    let mut cleared = Vec::new();
    for plugin in plugin::of_kind(PluginKind::Agent) {
        if !answers(&plugin, "clear") {
            continue;
        }
        let name = plugin.name().to_string();
        match plugin.call("clear", json!({ "dir": tree.path })) {
            Ok(result) => {
                let count = |key: &str| result[key].as_array().map_or(0, Vec::len);
                if count("removed") > 0 {
                    let _ = writeln!(log, "{name} removed {} agents", count("removed"));
                }
                for why in result["failed"].as_array().into_iter().flatten() {
                    let _ = writeln!(
                        log,
                        "{name} could not remove {}",
                        why.as_str().unwrap_or_default()
                    );
                }
                cleared.push(name);
            }
            Err(err) => {
                let _ = writeln!(log, "{name} could not clear the tree: {err}");
            }
        }
    }
    for agent in tree.agents.iter().filter(|a| a.is_background) {
        let (Some(name), Some(id)) = (&agent.plugin, &agent.job) else {
            continue;
        };
        if cleared.contains(name) {
            continue;
        }
        let Some(plugin) = plugin::named(PluginKind::Agent, name) else {
            let _ = writeln!(
                log,
                "could not end {} ({id}): no agent plugin {name}",
                agent.name
            );
            continue;
        };
        if answers(&plugin, "remove") {
            match plugin.call("remove", json!({ "id": id })) {
                Ok(_) => {
                    let _ = writeln!(log, "{name} removed {} ({id})", agent.name);
                    continue;
                }
                Err(err) => {
                    let _ = writeln!(
                        log,
                        "could not remove {} ({id}): {err}; stopping it",
                        agent.name
                    );
                }
            }
        }
        let _ = match plugin.call("stop", json!({ "id": id })) {
            Ok(_) => writeln!(log, "{name} stopped {} ({id})", agent.name),
            Err(err) => writeln!(log, "could not stop {} ({id}): {err}", agent.name),
        };
    }
}

/// Whether the tree at `path` is back in its pool, as treehouse says.
fn is_returned(path: &Path) -> bool {
    pool::status(path).is_ok_and(|(trees, _)| trees.iter().any(|t| t.path == path && !t.is_held()))
}

/// Deletes `branch` once the tree is back in the pool, if its work is in the
/// pool's base, or its PR merged with the branch where it is now, which
/// covers a squash or rebase merge git cannot see; one that is not merged is
/// kept, and the log says so.
fn delete_if_merged(tree: &Path, branch: &str, pr: Option<&Ship>, log: &mut String) {
    let base_ref = pool::main_checkout(tree)
        .ok()
        .and_then(|c| diff::base(&c))
        .and_then(|base| {
            [format!("origin/{base}"), base]
                .into_iter()
                .find(|r| pool::git(tree, &["rev-parse", "--verify", "--quiet", r]).is_some())
        });
    let in_base = base_ref.as_deref().is_some_and(|base_ref| {
        let mut scratch = String::new();
        exec(
            git(tree).args(["merge-base", "--is-ancestor", branch, base_ref]),
            GIT_LIMIT,
            &mut scratch,
        )
        .is_ok()
    });
    let merged = pr.filter(|p| p.stage == ship::Stage::Merged);
    let how = if let (true, Some(base_ref)) = (in_base, &base_ref) {
        format!("merged into {base_ref}")
    } else if let Some(pr) = merged {
        let number = pr.number().unwrap_or_else(|| "its PR".into());
        let tip = pool::git(tree, &["rev-parse", "--verify", "--quiet", branch]);
        if tip.as_deref().map(str::trim) != pr.head.as_deref() {
            let _ = writeln!(
                log,
                "kept branch {branch}: it has commits after {number} merged"
            );
            return;
        }
        format!("merged as {number}")
    } else {
        let why = match &base_ref {
            Some(base_ref) => format!("not merged into {base_ref}"),
            None => "no base branch to check it against".into(),
        };
        let _ = writeln!(log, "kept branch {branch}: {why}");
        return;
    };
    if capture(git(tree).args(["branch", "-D", branch]), GIT_LIMIT, log) {
        let _ = writeln!(log, "deleted branch {branch}, {how}");
    }
}

/// Sends `message` through its agent's plugin.
pub fn send_cold(message: &Message) -> Result<(), String> {
    let plugin = plugin::named(PluginKind::Agent, &message.plugin)
        .ok_or_else(|| format!("no agent plugin {}", message.plugin))?;
    plugin
        .call("send", json!({ "id": message.id, "text": message.text }))
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// The agent plugins that can start a stopped agent again.
fn resumers() -> Vec<Plugin> {
    plugin::of_kind(PluginKind::Agent)
        .into_iter()
        .filter(|p| {
            p.description()
                .is_ok_and(|d| d.ops.iter().any(|o| o == "resume"))
        })
        .collect()
}

/// Starts background job `id` again through the first agent plugin that
/// knows it, and says the id it runs under now.
pub fn resume_job(id: &str) -> Result<String, String> {
    resume_with(&json!({ "id": id }))
}

/// Asks each plugin that resumes to resume what `args` names, until one
/// does: the id it started, or every plugin's reason it did not.
fn resume_with(args: &Value) -> Result<String, String> {
    let mut why = Vec::new();
    for plugin in resumers() {
        match plugin.call("resume", args.clone()) {
            Ok(result) => {
                return result["id"]
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("{} said no id", plugin.name()));
            }
            Err(err) => why.push(format!("{}: {err}", plugin.name())),
        }
    }
    Err(if why.is_empty() {
        "no agent plugin resumes agents".into()
    } else {
        why.join("; ")
    })
}

/// Starts the tree's stopped or failed agent again: the job its sidecar
/// names, else a cold one placed in it, else the one that last worked in it
/// since its lease began. A tree whose agent runs is left alone. The sidecar
/// follows the agent to the id it runs under now.
fn resume_in(tree: &Tree, log: &mut String) -> bool {
    if let Some(agent) = tree.agents.iter().find(|a| a.pid.is_some()) {
        let _ = writeln!(
            log,
            "{} is running in tree {}; open its chat",
            agent.name, tree.name
        );
        return false;
    }
    let record = sidecar::read(&tree.path);
    let mut ids: Vec<&str> = record.iter().filter_map(|r| r.job.as_deref()).collect();
    ids.extend(tree.agents.iter().filter_map(|a| a.job.as_deref()));
    ids.dedup();
    let since = tree
        .leased_at
        .as_deref()
        .and_then(pool::seconds)
        .or_else(|| record.as_ref().map(|r| r.leased_at));
    let tries = ids
        .iter()
        .map(|id| json!({ "id": id }))
        .chain([json!({ "dir": tree.path, "since": since })]);
    let mut why = Vec::new();
    for args in tries {
        match resume_with(&args) {
            Ok(id) => {
                let _ = writeln!(log, "resumed the agent in tree {} as {id}", tree.name);
                if let Some(mut record) = record.filter(|r| r.job.as_deref() != Some(&id)) {
                    record.job = Some(id);
                    if let Err(err) = sidecar::write(&tree.path, &record) {
                        let _ =
                            writeln!(log, "could not record the job in {}: {err}", sidecar::FILE);
                    }
                }
                return true;
            }
            Err(err) => why.push(err),
        }
    }
    let _ = writeln!(
        log,
        "nothing to resume in tree {}: {}",
        tree.name,
        why.join("; ")
    );
    false
}

/// Writes what failed on the tree's PR to `log`, as the ship plugin that
/// reported it tells it; the log is the text its agent is sent.
fn read_failure(tree: &Tree, log: &mut String) -> bool {
    let Some(ship) = &tree.ship else {
        let _ = writeln!(log, "tree {} has no PR", tree.name);
        return false;
    };
    let Some(plugin) = plugin::named(PluginKind::Ship, &ship.plugin) else {
        let _ = writeln!(log, "no ship plugin {}", ship.plugin);
        return false;
    };
    match ship::logs(&plugin, &tree.path, &ship.branch) {
        Ok(text) => {
            log.push_str(&text);
            true
        }
        Err(err) => {
            let _ = writeln!(log, "{}: {err}", ship.plugin);
            false
        }
    }
}

/// What a ship run of the tree's branch sets out to do: its ticket's title
/// and text, else the task its agent was given.
fn intent(tree: &Path, checkout: &Path) -> Result<String, String> {
    let record = sidecar::read(tree);
    if let Some(id) = record.as_ref().and_then(|r| r.ticket.as_deref()) {
        let source = record.as_ref().and_then(|r| r.source.as_deref());
        let ticket = tickets::named(source, checkout)?.detail(id)?.issue;
        let text = ticket.description.unwrap_or_default();
        return Ok(format!("{}: {}\n\n{text}", ticket.id, ticket.title)
            .trim()
            .to_string());
    }
    record
        .and_then(|r| r.task)
        .ok_or_else(|| "the tree has neither a ticket nor a task to give as the intent".into())
}

/// Starts a ship run of the tree's branch through its plugin, or reads the
/// command that shows the one under way into the job's `attach`.
fn ship_run(job: &mut Job, log: &mut String) -> bool {
    let (Some(call), Some(branch)) = (job.shipping.clone(), job.tree.branch.clone()) else {
        let _ = writeln!(log, "tree {} has no branch to ship", job.tree.name);
        return false;
    };
    let Some(plugin) = plugin::named(PluginKind::Ship, &call.plugin) else {
        let _ = writeln!(log, "no ship plugin {}", call.plugin);
        return false;
    };
    let dir = job.tree.path.clone();
    let answer = match call.action {
        ShipAction::Attach => plugin
            .call("attach", json!({ "dir": dir, "branch": branch }))
            .map_err(|err| err.to_string())
            .and_then(|reply| {
                serde_json::from_value::<Vec<String>>(reply["argv"].clone())
                    .map_err(|err| format!("no command to show the run: {err}"))
            })
            .map(|argv| {
                let _ = writeln!(log, "{} shows the run with {}", call.plugin, argv.join(" "));
                job.attach = Some(argv);
            }),
        ShipAction::Run { checkout } => intent(&dir, &checkout).and_then(|intent| {
            let _ = writeln!(log, "intent: {}", intent.lines().next().unwrap_or_default());
            plugin
                .call(
                    "run",
                    json!({ "dir": dir, "branch": branch, "intent": intent }),
                )
                .map(|_| {
                    let _ = writeln!(log, "{} started a run of {branch}", call.plugin);
                })
                .map_err(|err| err.to_string())
        }),
    };
    match answer {
        Ok(()) => true,
        Err(why) => {
            let _ = writeln!(log, "{}: {why}", call.plugin);
            false
        }
    }
}

/// Marks the merged ticket done where its source says to, then finds the
/// tickets it leaves ready, into the job's `ready`.
fn land(job: &mut Job, log: &mut String) -> bool {
    let Some(pick) = &job.landing else {
        return false;
    };
    let source = match pick.backend() {
        Ok(source) => source,
        Err(why) => {
            let _ = writeln!(log, "{why}");
            return false;
        }
    };
    if source.finishes() {
        let _ = match source.finish(&pick.id) {
            Ok(()) => writeln!(log, "marked {} done", pick.id),
            Err(why) => writeln!(log, "could not mark {} done: {why}", pick.id),
        };
    }
    match source.ready_after(&pick.id) {
        Ok(ready) => {
            let _ = match ready.as_slice() {
                [] => writeln!(log, "{} merged; it leaves nothing ready", pick.id),
                ready => writeln!(log, "{} merged; ready now: {}", pick.id, ready.join(", ")),
            };
            job.ready = ready;
            true
        }
        Err(why) => {
            let _ = writeln!(log, "could not read what {} blocks: {why}", pick.id);
            false
        }
    }
}

/// Stops the tree's agents, runs the `on_return` hook, returns the tree,
/// deletes its branch if merged, and drops its sidecar. A tree treetop
/// leased is returned only while the lease is still the one it took.
fn give_back(tree: &Tree, log: &mut String) -> bool {
    let path = &tree.path;
    let record = sidecar::read(path);
    end_agents(tree, log);
    run_hook("on_return", &hooks(log).on_return, path, log);
    run_hook("on_return", &repo_on_return(path), path, log);
    let mut command = treehouse("return", path);
    command.arg("--force");
    if let Some(id) = record.as_ref().and_then(|r| r.lease_id.as_deref()) {
        command.args(["--if-lease-id", id]);
    }
    // Removing an agent that entered the tree with `EnterWorktree` can hand
    // the tree back already, through its `WorktreeRemove` hook.
    if !capture(&mut command, TREEHOUSE_LIMIT, log) {
        if !is_returned(path) {
            return false;
        }
        let _ = writeln!(log, "the tree was already back in the pool");
    }
    if let Some(branch) = record.as_ref().and_then(|r| r.branch.as_deref()) {
        delete_if_merged(path, branch, tree.ship.as_deref(), log);
    }
    if let Err(err) = sidecar::remove(path) {
        let _ = writeln!(log, "could not remove {}: {err}", sidecar::FILE);
    }
    true
}

/// Runs one job to completion, here or on a worker. Return passes `--force`
/// because treetop's own confirm, which names every tree's uncommitted and
/// unpushed work, has already replaced treehouse's prompt. treehouse stops
/// the processes left in the tree; anything started outside it, such as a
/// container, is for whatever started it to stop.
pub fn run(job: &mut Job) -> (bool, String) {
    let mut log = String::new();
    let is_ok = match job.kind {
        Kind::Start => start(job, &mut log),
        Kind::Rename => {
            let to = job.rename.clone().unwrap_or_default();
            rename(&job.tree.path, &to, &mut log)
        }
        Kind::Send => match &job.message {
            Some(message) => match send_cold(message) {
                Ok(()) => true,
                Err(why) => {
                    let _ = writeln!(log, "{why}");
                    false
                }
            },
            None => false,
        },
        Kind::Return => give_back(&job.tree, &mut log),
        Kind::Fix => read_failure(&job.tree, &mut log),
        Kind::Land => land(job, &mut log),
        Kind::Ship => ship_run(job, &mut log),
        Kind::Resume => resume_in(&job.tree, &mut log),
        Kind::Handoff => hand_off(job, &mut log),
        Kind::Preview => capture(
            treehouse("destroy", &job.tree.path).args(INCLUDE),
            TREEHOUSE_LIMIT,
            &mut log,
        ),
        Kind::Destroy => {
            end_agents(&job.tree, &mut log);
            capture(
                treehouse("destroy", &job.tree.path)
                    .args(INCLUDE)
                    .arg("--yes"),
                TREEHOUSE_LIMIT,
                &mut log,
            )
        }
    };
    (is_ok, log)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn launch(task: &str) -> Launch {
        Launch::agent(task.into(), false)
    }

    #[test]
    fn a_custom_agent_gets_the_task_as_its_last_argument() {
        let custom = vec!["codex".to_string(), "exec".to_string()];
        assert_eq!(
            custom_command(custom.clone(), &launch("fix login")),
            ["codex", "exec", "fix login"]
        );
        assert_eq!(custom_command(custom, &launch("  ")), ["codex", "exec"]);
    }

    #[test]
    fn the_sidecar_names_the_agent_by_its_program() {
        assert_eq!(agent_name("claude"), "claude");
        assert_eq!(agent_name("/usr/local/bin/codex"), "codex");
    }

    #[test]
    fn names_a_tree_by_its_pool_number() {
        assert_eq!(
            pool_name(Path::new("/home/me/.treehouse/app-1/12/app")),
            "12"
        );
    }

    #[test]
    fn a_failed_command_says_its_last_line_and_a_hang_its_limit() {
        let mut log = String::new();
        let failed = exec(
            Command::new("sh").args(["-c", "echo working; echo 'no such tree' >&2; exit 1"]),
            GIT_LIMIT,
            &mut log,
        );
        assert_eq!(failed, Err("no such tree".to_string()));
        assert!(log.contains("working"));
        let hung = exec(
            Command::new("sh").args(["-c", "sleep 30"]),
            Duration::from_millis(100),
            &mut log,
        );
        assert_eq!(hung, Err("sh took over 0s".to_string()));
    }

    /// A repository whose base is `main`, with one commit on it, checked
    /// out on `main`.
    fn repository(test: &str) -> PathBuf {
        let repo = std::env::temp_dir().join(format!("treetop-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("treehouse.toml"), "base_branch = \"main\"\n").unwrap();
        commit_in(&repo, &[&["init", "-q", "-b", "main"]], "first");
        repo
    }

    /// Runs each of `steps` in `repo`, then commits everything as `message`.
    fn commit_in(repo: &Path, steps: &[&[&str]], message: &str) {
        let mut log = String::new();
        for args in steps {
            assert!(capture(git(repo).args(*args), GIT_LIMIT, &mut log), "{log}");
        }
        let commit = [
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-am",
            message,
        ];
        assert!(
            capture(git(repo).args(commit), GIT_LIMIT, &mut log),
            "{log}"
        );
    }

    fn tip(repo: &Path, branch: &str) -> String {
        pool::git(repo, &["rev-parse", branch])
            .unwrap()
            .trim()
            .to_string()
    }

    fn merged_as(branch: &str, head: &str) -> Ship {
        Ship {
            branch: branch.into(),
            stage: ship::Stage::Merged,
            label: "#12 merged".into(),
            url: Some("https://github.com/o/r/pull/12".into()),
            head: Some(head.into()),
            detail: Vec::new(),
            plugin: "github".into(),
        }
    }

    fn has_branch_in(repo: &Path, branch: &str) -> bool {
        pool::git(repo, &["rev-parse", "--verify", "--quiet", branch]).is_some()
    }

    /// A PR squash-merged on GitHub leaves its branch out of the base's
    /// history, so git alone calls it unmerged; its PR says otherwise.
    #[test]
    fn returning_a_squash_merged_tree_deletes_its_branch() {
        let repo = repository("squash");
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        commit_in(
            &repo,
            &[&["switch", "-q", "-c", "me/fix"], &["add", "a.txt"]],
            "add a",
        );
        let head = tip(&repo, "me/fix");
        commit_in(
            &repo,
            &[
                &["switch", "-q", "main"],
                &["merge", "--squash", "-q", "me/fix"],
            ],
            "add a (#12)",
        );
        let mut log = String::new();
        delete_if_merged(&repo, "me/fix", Some(&merged_as("me/fix", &head)), &mut log);
        assert!(!has_branch_in(&repo, "me/fix"), "{log}");
        assert!(
            log.contains("deleted branch me/fix, merged as #12"),
            "{log}"
        );
    }

    #[test]
    fn a_branch_with_work_after_its_merge_or_no_merged_pr_is_kept() {
        let repo = repository("kept");
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        commit_in(
            &repo,
            &[&["switch", "-q", "-c", "me/more"], &["add", "a.txt"]],
            "add a",
        );
        let merged_head = tip(&repo, "me/more");
        std::fs::write(repo.join("a.txt"), "a and more\n").unwrap();
        commit_in(&repo, &[], "more after the merge");
        commit_in(&repo, &[&["switch", "-q", "main"]], "elsewhere");
        let mut log = String::new();
        delete_if_merged(
            &repo,
            "me/more",
            Some(&merged_as("me/more", &merged_head)),
            &mut log,
        );
        assert!(has_branch_in(&repo, "me/more"), "{log}");
        assert!(
            log.contains("kept branch me/more: it has commits after #12 merged"),
            "{log}"
        );
        let mut closed = merged_as("me/more", &tip(&repo, "me/more"));
        closed.stage = ship::Stage::Closed;
        delete_if_merged(&repo, "me/more", Some(&closed), &mut log);
        delete_if_merged(&repo, "me/more", None, &mut log);
        assert!(has_branch_in(&repo, "me/more"), "{log}");
    }

    #[test]
    fn a_branch_git_sees_in_the_base_is_deleted_as_before() {
        let repo = repository("ancestor");
        commit_in(&repo, &[&["switch", "-q", "-c", "me/done"]], "done");
        commit_in(
            &repo,
            &[
                &["switch", "-q", "main"],
                &["merge", "-q", "--ff-only", "me/done"],
            ],
            "after",
        );
        let mut log = String::new();
        delete_if_merged(&repo, "me/done", None, &mut log);
        assert!(!has_branch_in(&repo, "me/done"), "{log}");
        assert!(
            log.contains("deleted branch me/done, merged into main"),
            "{log}"
        );
    }

    #[test]
    fn rename_names_a_detached_head_then_renames_its_branch() {
        let repo = std::env::temp_dir().join(format!("treetop-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let mut log = String::new();
        for args in [
            &["init", "-q"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
            &["switch", "-q", "--detach"],
        ] {
            assert!(capture(git(&repo).args(args), GIT_LIMIT, &mut log), "{log}");
        }
        assert_eq!(checked_out(&repo), None);
        assert!(rename(&repo, "me/fix-login", &mut log), "{log}");
        assert_eq!(checked_out(&repo).as_deref(), Some("me/fix-login"));
        assert!(rename(&repo, "me/fix-auth", &mut log), "{log}");
        assert_eq!(checked_out(&repo).as_deref(), Some("me/fix-auth"));
        assert!(!rename(&repo, "has space", &mut log));
        assert!(log.ends_with("a branch cannot hold spaces or any of ~ ^ : ? * [ \\\n"));
    }

    #[test]
    fn a_failed_start_undoes_its_steps_and_ends_on_why() {
        // A held tree is only started in, so the only step is the agent's,
        // and a program that is not there fails it without touching git.
        let mut job = Job {
            launch: Some(launch("fix login")),
            ..Job::new(
                Kind::Start,
                Tree {
                    status: "leased".into(),
                    ..pool::stand_in(std::env::temp_dir())
                },
            )
        };
        let mut log = String::new();
        let mut undone = Vec::new();
        // SAFETY: no other test reads or sets TREETOP_NEW_AGENT.
        unsafe { std::env::set_var("TREETOP_NEW_AGENT", "/nonexistent/agent") };
        let failed = start_steps(&mut job, &mut log, &mut undone);
        unsafe { std::env::remove_var("TREETOP_NEW_AGENT") };
        assert!(failed.unwrap_err().starts_with("start: /nonexistent/agent"));
        assert!(undone.is_empty(), "nothing was leased, branched or written");
    }

    /// Each step a start ends reaches the screen as it ends, for the
    /// start's row, before the start finishes: a held tree's start says
    /// its lease, branch and record, then fails at its agent.
    #[test]
    fn a_starts_steps_are_said_for_its_row_as_they_end() {
        let root = config_root("steps");
        if !in_own_process("a_starts_steps_are_said_for_its_row_as_they_end", &root) {
            return;
        }
        // A tree already held is started in as it is, by an agent command
        // that runs and returns, as `TREETOP_NEW_AGENT` names one.
        // SAFETY: this process runs only this test, on this thread.
        unsafe { std::env::set_var("TREETOP_NEW_AGENT", "true") };
        let job = Job {
            // Named, so no model is asked for a name.
            launch: Some(Launch {
                name: Some("login fixer".into()),
                ..launch("fix login")
            }),
            ..Job::new(
                Kind::Start,
                Tree {
                    status: "leased".into(),
                    ..pool::stand_in(std::env::temp_dir())
                },
            )
        };
        let row = job.key.clone();
        let worker = Worker::spawn();
        worker.push(job);
        let mut said = Vec::new();
        for event in worker.events.iter() {
            match event {
                Event::Started(_) => {}
                Event::Step { key, step } => said.push((key, step)),
                Event::Finished { is_ok, output, .. } => {
                    assert!(is_ok, "{output}");
                    break;
                }
            }
        }
        // Nothing was leased, branched or recorded in a tree already held,
        // so only the agent's start is said.
        assert_eq!(said, [(row, "agent")]);
    }

    /// A held tree treetop leased, its record naming agent `old00001`, and
    /// an agent plugin named `claude` in a config directory of its own that
    /// starts `new00001`, or fails to where `fail-start` exists, and records
    /// each id it is asked to stop. In the process `in_own_process` runs
    /// the test in, the one this process made.
    fn handoff_fixture(test: &str) -> (PathBuf, Tree) {
        if let Some(root) = std::env::var_os("TREETOP_HANDOFF_CHILD") {
            let root = PathBuf::from(root);
            let tree = Tree {
                name: "4".into(),
                status: "leased".into(),
                ..pool::stand_in(root.join("4/app"))
            };
            return (root, tree);
        }
        let root =
            std::env::temp_dir().join(format!("treetop-handoff-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (path, git_dir) = (root.join("4/app"), root.join("main/.git/worktrees/app4"));
        std::fs::create_dir_all(&path).unwrap();
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            path.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        let record = Sidecar {
            job: Some("old00001".into()),
            task: Some("fix the login redirect".into()),
            ..Sidecar::leased_now()
        };
        sidecar::write(&path, &record).unwrap();
        let plugins = root.join("config/treetop/plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let plugin = plugins.join("claude");
        std::fs::write(
            &plugin,
            format!(
                r#"#!/bin/sh
req=$(cat)
case "$req" in
  *'"op":"describe"'*) echo '{{"ok":true,"result":{{"protocol":1,"name":"claude","kind":"agent","ops":["start","attach","stop"]}}}}' ;;
  *'"op":"start"'*)
    if [ -f {root}/fail-start ]; then echo '{{"ok":false,"error":{{"code":"failed","message":"no session"}}}}'; exit 0; fi
    echo "$req" > {root}/started
    echo '{{"ok":true,"result":{{"id":"new00001"}}}}' ;;
  *'"op":"attach"'*) echo '{{"ok":true,"result":{{"argv":["claude","attach","new00001"]}}}}' ;;
  *'"op":"stop"'*) echo "$req" >> {root}/stopped; echo '{{"ok":true,"result":{{}}}}' ;;
esac
"#,
                root = root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&plugin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let tree = Tree {
            name: "4".into(),
            status: "leased".into(),
            ..pool::stand_in(path)
        };
        (root, tree)
    }

    /// Runs test `name` again in a process of its own, on the fixture at
    /// `root`, with its config directory there, where it finds its agent
    /// plugin, since every test in this one shares the environment. True in
    /// that process, where the test goes on; false here, once it has passed
    /// there.
    fn in_own_process(name: &str, root: &Path) -> bool {
        in_process_of_its_own(&format!("jobs::tests::{name}"), root)
    }

    /// `in_own_process` for test `path`, from anywhere in the crate.
    pub(crate) fn in_process_of_its_own(path: &str, root: &Path) -> bool {
        if std::env::var_os("TREETOP_HANDOFF_CHILD").is_some() {
            return true;
        }
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", path, "--nocapture"])
            .env("TREETOP_HANDOFF_CHILD", root)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env_remove("TREETOP_NEW_AGENT")
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(said.contains("1 passed"), "the test ran there: {said}");
        false
    }

    /// An empty config directory for test `test`, at `<root>/config/treetop`,
    /// and in the process `in_process_of_its_own` made, the same one.
    pub(crate) fn config_root(test: &str) -> PathBuf {
        if let Some(root) = std::env::var_os("TREETOP_HANDOFF_CHILD") {
            return PathBuf::from(root);
        }
        let root =
            std::env::temp_dir().join(format!("treetop-config-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/treetop")).unwrap();
        root
    }

    fn handoff_job(tree: Tree) -> Job {
        Job {
            launch: Some(Launch {
                name: Some("fix login".into()),
                ..launch("Carry on with the login redirect: the test is left.")
            }),
            replaces: Some(Replaced {
                agent: "fix login".into(),
                plugin: "claude".into(),
                id: "old00001".into(),
            }),
            ..Job::new(Kind::Handoff, tree)
        }
    }

    #[test]
    fn a_handoff_starts_the_new_agent_in_the_tree_names_it_there_then_stops_the_old() {
        let (root, tree) = handoff_fixture("hands-off");
        if !in_own_process(
            "a_handoff_starts_the_new_agent_in_the_tree_names_it_there_then_stops_the_old",
            &root,
        ) {
            return;
        }
        let path = tree.path.clone();
        let (is_ok, log) = run(&mut handoff_job(tree));
        assert!(is_ok, "{log}");
        let started = std::fs::read_to_string(root.join("started")).unwrap();
        assert!(
            started.contains("Carry on with the login redirect"),
            "{started}"
        );
        assert!(
            started.contains(&format!(r#""dir":"{}""#, path.display())),
            "{started}"
        );
        let record = sidecar::read(&path).unwrap();
        assert_eq!(
            record.job.as_deref(),
            Some("new00001"),
            "the tree is the new agent's"
        );
        assert_eq!(
            record.task.as_deref(),
            Some("fix the login redirect"),
            "the task a ship run gives as its intent stays the tree's"
        );
        let stopped = std::fs::read_to_string(root.join("stopped")).unwrap();
        assert!(stopped.contains(r#""id":"old00001""#), "{stopped}");
    }

    #[test]
    fn a_handoff_whose_new_agent_fails_to_start_leaves_the_old_one_working() {
        let (root, tree) = handoff_fixture("fails");
        if !in_own_process(
            "a_handoff_whose_new_agent_fails_to_start_leaves_the_old_one_working",
            &root,
        ) {
            return;
        }
        std::fs::write(root.join("fail-start"), "").unwrap();
        let path = tree.path.clone();
        let (is_ok, log) = run(&mut handoff_job(tree));
        assert!(!is_ok);
        assert!(log.trim_end().ends_with("start: no session"), "{log}");
        assert!(
            !root.join("stopped").exists(),
            "the old agent was not stopped"
        );
        assert_eq!(
            sidecar::read(&path).unwrap().job.as_deref(),
            Some("old00001")
        );
    }

    /// A ticket plugin for the tracker fixture: `name`, recording each change
    /// it is asked for in `<root>/ops`, with `tickets` the `case` arms that
    /// answer `get` by the ticket's `$id`.
    fn tracker_plugin(root: &Path, file: &str, name: &str, tickets: &str) {
        let plugin = root.join("config/treetop/plugins").join(file);
        std::fs::write(
            &plugin,
            format!(
                r#"#!/bin/sh
req=$(cat)
id=$(echo "$req" | sed 's/.*"args":{{"id":"\([^"]*\)"}}.*/\1/')
ok() {{ echo "{{\"ok\":true,\"result\":$1}}"; }}
ticket() {{ ok "{{\"id\":\"$id\",\"title\":\"Fix the login\",\"description\":\"Keep return_to.\",\"branch\":\"me/$id-login\",\"links\":[$1]}}"; }}
case "$req" in
  *'"op":"describe"'*) ok '{{"protocol":1,"name":"{name}","kind":"tickets","ops":["get","start","finish"]}}' ;;
  *'"op":"start"'*|*'"op":"finish"'*) printf '%s %s %s\n' {name} "$(echo "$req" | sed 's/.*"op":"\([a-z]*\)".*/\1/')" "$id" >> {root}/ops; ok '{{}}' ;;
  *'"op":"get"'*)
    case "$id" in
{tickets}
    esac ;;
esac
"#,
                root = root.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&plugin, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
    }

    /// A repository at `<root>/main` whose own issues come from a ticket
    /// plugin of your own named `linear`, which replaces the built-in client.
    /// It knows END-1, which blocks END-2 (blocked by nothing else) and END-3
    /// (still blocked by END-9). A second source, `personal`, knows PER-1,
    /// which blocks PER-2, `keeps` is the same plugin without `finish`, and
    /// another is broken, on a plugin nobody has. A
    /// tree at `<root>/4/app` is working on END-1. In the process
    /// `in_own_process` runs the test in, the one this process made.
    pub(crate) fn tracker_fixture(test: &str) -> PathBuf {
        if let Some(root) = std::env::var_os("TREETOP_HANDOFF_CHILD") {
            return PathBuf::from(root);
        }
        let root =
            std::env::temp_dir().join(format!("treetop-tracker-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config/treetop/plugins")).unwrap();
        std::fs::create_dir_all(root.join("main")).unwrap();
        std::fs::write(
            root.join("config/treetop/config.toml"),
            format!(
                "[issues.broken]\nplugin = \"no-such-plugin\"\n\
                 [issues.keeps]\nplugin = \"side\"\n\
                 [issues.personal]\nplugin = \"side\"\nfinish = true\n\
                 [issues.team]\nfinish = true\n\
                 [repos.\"{}\"]\nissues = \"team\"\n",
                root.join("main").display()
            ),
        )
        .unwrap();
        tracker_plugin(
            &root,
            "my-linear",
            "linear",
            r#"END-1) ticket '{"id":"END-2","kind":"blocks"},{"id":"END-3","kind":"blocks"}' ;;
END-2) ticket '{"id":"END-1","kind":"blocked by"}' ;;
END-3) ticket '{"id":"END-1","kind":"blocked by"},{"id":"END-9","kind":"blocked by"}' ;;
*) echo '{"ok":false,"error":{"code":"not_found","message":"no such ticket"}}' ;;"#,
        );
        tracker_plugin(
            &root,
            "side",
            "side",
            r#"PER-1) ticket '{"id":"PER-2","kind":"blocks"}' ;;
PER-2) ticket '{"id":"PER-1","kind":"blocked by"}' ;;
*) echo '{"ok":false,"error":{"code":"not_found","message":"no such ticket"}}' ;;"#,
        );
        let tree = root.join("4/app");
        std::fs::create_dir_all(&tree).unwrap();
        let git_dir = root.join("main/.git/worktrees/app4");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        let record = Sidecar {
            ticket: Some("END-1".into()),
            ..Sidecar::leased_now()
        };
        sidecar::write(&tree, &record).unwrap();
        root
    }

    /// Puts on the `PATH` a `treehouse` that leases `<root>/5/app` and records
    /// what it was asked in `<root>/leased`, and has agents started by a
    /// program that records its prompt in `<root>/started`.
    fn fake_pool_and_agent(root: &Path) -> PathBuf {
        let (bin, tree) = (root.join("bin"), root.join("5/app"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&tree).unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&tree)
                .status()
                .unwrap()
                .success()
        );
        let script = |name: &str, body: String| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
                .unwrap();
            path
        };
        script(
            "treehouse",
            format!(
                "echo \"$@\" >> {root}/leased\necho '{{\"path\":\"{tree}\",\"lease_id\":\"l5\"}}'",
                root = root.display(),
                tree = tree.display()
            ),
        );
        let agent = script(
            "agent",
            format!("printf '%s' \"$1\" > {}/started", root.display()),
        );
        let path = std::env::var("PATH").unwrap();
        // SAFETY: this process runs only this test, on this thread.
        unsafe {
            std::env::set_var("PATH", format!("{}:{path}", bin.display()));
            std::env::set_var("TREETOP_NEW_AGENT", agent);
        }
        tree
    }

    /// A task typed in a hurry starts on a branch and under a name the model
    /// gives it, the tree leased while it answers; a model that cannot
    /// answer leaves the task's own words, as before.
    #[test]
    fn a_new_agent_is_named_by_the_model_else_by_its_task() {
        let root = config_root("named");
        if !in_own_process("a_new_agent_is_named_by_the_model_else_by_its_task", &root) {
            return;
        }
        let tree = fake_pool_and_agent(&root);
        // treehouse hands trees out on a detached HEAD.
        for args in [
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "0",
            ][..],
            &["checkout", "-q", "--detach"][..],
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(&tree)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let claude = root.join("bin/claude");
        let answer =
            r#"{"title": "Scan project for useful tools", "branch": "scan-project-tools"}"#;
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh
cat > /dev/null
echo '{answer}'
"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        // A treehouse that cuts the branch it is asked for, as the real one does.
        let treehouse = root.join("bin/treehouse");
        std::fs::write(
            &treehouse,
            format!(
                "#!/bin/sh\necho \"$@\" >> {root}/leased\n\
                 while [ $# -gt 0 ]; do [ \"$1\" = --branch ] && git -C {tree} switch -q -c \"$2\"; shift; done\n\
                 echo '{{\"path\":\"{tree}\",\"lease_id\":\"l5\"}}'\n",
                root = root.display(),
                tree = tree.display()
            ),
        )
        .unwrap();
        let task = "can you scan he project and cmpiel a list of thngs";
        let words = branch::for_task(&branch::user(), task).unwrap();
        let mut job = Job {
            launch: Some(Launch::agent(task.into(), true)),
            ..Job::new(Kind::Start, pool::stand_in(PathBuf::from("(new tree)")))
        };
        let (is_ok, log) = run(&mut job);
        assert!(is_ok, "{log}");
        assert!(
            !log.contains("naming"),
            "the start never waits for the name: {log}"
        );
        let leased = std::fs::read_to_string(root.join("leased")).unwrap();
        assert!(leased.contains(&format!("--branch {words}")), "{leased}");
        wait_for_names();
        let user = branch::user();
        let named = if user.is_empty() {
            "scan-project-tools".to_string()
        } else {
            format!("{user}/scan-project-tools")
        };
        assert_eq!(checked_out(&tree).as_deref(), Some(named.as_str()));

        // A model that cannot answer leaves the task's words.
        std::fs::write(&claude, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(
            Command::new("git")
                .args(["checkout", "-q", "--detach"])
                .current_dir(&tree)
                .status()
                .unwrap()
                .success()
        );
        let task = "fix the login redirect";
        let mut job = Job {
            launch: Some(Launch::agent(task.into(), true)),
            ..Job::new(Kind::Start, pool::stand_in(PathBuf::from("(new tree)")))
        };
        let (is_ok, log) = run(&mut job);
        assert!(is_ok, "{log}");
        wait_for_names();
        assert_eq!(checked_out(&tree), branch::for_task(&user, task));
    }

    /// A title given after the start is what the agent is called from then
    /// on, the newest kept when one job is titled twice.
    #[test]
    fn a_title_given_after_the_start_names_its_agent() {
        let root = config_root("titled");
        if !in_own_process("a_title_given_after_the_start_names_its_agent", &root) {
            return;
        }
        // SAFETY: this process runs only this test, on this thread.
        unsafe { std::env::set_var("XDG_STATE_HOME", root.join("state")) };
        crate::state::add_title("j1", "Fix login");
        crate::state::add_title("j2", "Scan the project");
        crate::state::add_title("j1", "Fix the login redirect");
        let titles = crate::state::titles();
        assert_eq!(
            titles.get("j1").map(String::as_str),
            Some("Fix the login redirect")
        );
        assert_eq!(
            titles.get("j2").map(String::as_str),
            Some("Scan the project")
        );
    }

    /// ANB-25, and the base of ANB-29: `w` to another source, then `p`, as the
    /// Issues screen does: the job reads and starts the ticket in that source
    /// and records it on the tree, and all that follows reads that record.
    #[test]
    fn a_ticket_picked_from_another_source_is_worked_in_that_source() {
        let root = tracker_fixture("other");
        if !in_own_process(
            "a_ticket_picked_from_another_source_is_worked_in_that_source",
            &root,
        ) {
            return;
        }
        let tree = fake_pool_and_agent(&root);
        let checkout = root.join("main");
        let mut launch = Launch::pick("PER-1".into(), checkout.clone(), Some("personal".into()));
        launch.base = Some("feature/billing".into());
        let mut job = Job {
            launch: Some(launch),
            ..Job::new(Kind::Start, pool::stand_in(PathBuf::from("(new tree)")))
        };
        let (is_ok, log) = run(&mut job);
        assert!(is_ok, "{log}");
        assert_eq!(job.tree.path, tree);
        let work = "PER-1: Fix the login\n\nKeep return_to.";
        assert_eq!(std::fs::read_to_string(root.join("started")).unwrap(), work);
        let leased = std::fs::read_to_string(root.join("leased")).unwrap();
        assert!(
            leased.ends_with("--branch me/PER-1-login --base feature/billing\n"),
            "{leased}"
        );
        let record = sidecar::read(&tree).unwrap();
        assert_eq!(record.ticket.as_deref(), Some("PER-1"));
        assert_eq!(record.source.as_deref(), Some("personal"));
        assert_eq!(intent(&tree, &checkout).unwrap(), work);
        let mut landing = Job {
            landing: Some(Pick {
                id: "PER-1".into(),
                checkout,
                source: record.source,
            }),
            ..Job::new(Kind::Land, pool::stand_in(tree))
        };
        let (is_ok, log) = run(&mut landing);
        assert!(is_ok, "{log}");
        assert_eq!(landing.ready, ["PER-2"]);
        assert_eq!(
            std::fs::read_to_string(root.join("ops")).unwrap(),
            "side start PER-1\nside finish PER-1\n"
        );
    }

    #[test]
    fn a_picked_ticket_gives_its_agent_its_work_and_is_marked_started() {
        let root = tracker_fixture("picked");
        if !in_own_process(
            "a_picked_ticket_gives_its_agent_its_work_and_is_marked_started",
            &root,
        ) {
            return;
        }
        let pick = Pick {
            id: "END-1".into(),
            checkout: root.join("main"),
            source: None,
        };
        let mut launch = Launch::pick(pick.id.clone(), pick.checkout.clone(), None);
        take_up(&pick, &mut launch).unwrap();
        assert_eq!(launch.task, "END-1: Fix the login\n\nKeep return_to.");
        assert_eq!(launch.branch.as_deref(), Some("me/END-1-login"));
        assert_eq!(launch.name.as_deref(), Some("END-1 Fix the login"));
        let mut log = String::new();
        mark_started(&pick, &mut log);
        assert_eq!(log, "marked END-1 started and assigned to you\n");
        assert_eq!(
            std::fs::read_to_string(root.join("ops")).unwrap(),
            "linear start END-1\n"
        );
        assert_eq!(
            intent(&root.join("4/app"), &pick.checkout).unwrap(),
            "END-1: Fix the login\n\nKeep return_to."
        );
    }

    #[test]
    fn a_merged_ticket_is_finished_and_says_what_it_leaves_ready() {
        let root = tracker_fixture("merged");
        if !in_own_process(
            "a_merged_ticket_is_finished_and_says_what_it_leaves_ready",
            &root,
        ) {
            return;
        }
        let mut job = Job {
            landing: Some(Pick {
                id: "END-1".into(),
                checkout: root.join("main"),
                source: None,
            }),
            ..Job::new(Kind::Start, pool::stand_in(root.join("4/app")))
        };
        let mut log = String::new();
        assert!(land(&mut job, &mut log));
        assert_eq!(job.ready, ["END-2"]);
        assert_eq!(
            std::fs::read_to_string(root.join("ops")).unwrap(),
            "linear finish END-1\n"
        );
        assert_eq!(log, "marked END-1 done\nEND-1 merged; ready now: END-2\n");
        // A source without `finish = true` is left alone, and still says.
        let mut job = Job {
            landing: Some(Pick {
                id: "PER-1".into(),
                checkout: root.join("main"),
                source: Some("keeps".into()),
            }),
            ..Job::new(Kind::Start, pool::stand_in(root.join("4/app")))
        };
        assert!(land(&mut job, &mut log));
        assert_eq!(job.ready, ["PER-2"]);
        assert_eq!(
            std::fs::read_to_string(root.join("ops")).unwrap(),
            "linear finish END-1\n"
        );
    }
}
