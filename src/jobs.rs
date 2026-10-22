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
use crate::tickets::{self, Source};

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
}

impl Launch {
    /// An agent on ticket `id`, in a new tree; its task, name and branch come
    /// from the ticket when the job runs.
    pub fn pick(id: String, checkout: PathBuf) -> Self {
        Launch {
            ticket: Some(Pick {
                id: id.clone(),
                checkout,
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
                    let (is_ok, output) = run(&mut job);
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
/// stay theirs.
fn start_steps(job: &mut Job, log: &mut String, undone: &mut Vec<Undo>) -> Result<(), String> {
    let mut launch = job
        .launch
        .clone()
        .unwrap_or_else(|| Launch::agent(String::new(), false));
    if let Some(pick) = launch.ticket.clone() {
        take_up(&pick, &mut launch).map_err(|why| format!("ticket: {why}"))?;
        job.launch = Some(launch.clone());
    }
    let is_leasing = launch.is_new_tree || !job.tree.is_held();
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
    if is_leasing {
        check(&job.tree.path, log).map_err(|why| format!("check: {why}"))?;
    }
    if let Some(name) = &branch
        && resumes
    {
        exec(git(&job.tree.path).args(["switch", name]), GIT_LIMIT, log)
            .map_err(|why| format!("branch: {why}"))?;
        undone.push(Undo::Detach);
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
    }
    let starter = starter(&launch, &job.tree.path)?;
    if is_leasing {
        record.agent = Some(match &starter {
            Starter::Command(command) => agent_name(&command[0]),
            Starter::Assistant(_, plugin) | Starter::Plugin(plugin) => plugin.name().to_string(),
        });
        record.branch = checked_out(&job.tree.path);
        sidecar::write(&job.tree.path, &record)
            .map_err(|err| format!("sidecar: could not write {}: {err}", sidecar::FILE))?;
        undone.push(Undo::Sidecar);
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
            return Ok(());
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
    let _ = writeln!(log, "{} started {id}", plugin.name());
    job.attach = plugin
        .call("attach", json!({ "id": id }))
        .ok()
        .and_then(|reply| serde_json::from_value(reply["argv"].clone()).ok());
    if is_leasing {
        record.job = Some(id);
        if let Err(err) = sidecar::write(&job.tree.path, &record) {
            let _ = writeln!(log, "could not record the job in {}: {err}", sidecar::FILE);
        }
    }
    Ok(())
}

/// Fills `launch` from the ticket it picks up: the prompt from the config's
/// template, a session name led by the ticket's id, and the branch the
/// ticket system names, unless one was given.
fn take_up(pick: &Pick, launch: &mut Launch) -> Result<(), String> {
    let source = Source::of(&pick.checkout)?;
    let ticket = source.get(&pick.id)?;
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
    let marked = Source::of(&pick.checkout).and_then(|source| source.start(&pick.id));
    let _ = match marked {
        Ok(()) => writeln!(log, "marked {} started and assigned to you", pick.id),
        Err(why) => writeln!(log, "could not mark {} started: {why}", pick.id),
    };
}

/// Runs the start's steps and, when one fails, undoes the ones taken, then
/// says which failed and why on the last line, which the status line shows.
fn start(job: &mut Job, log: &mut String) -> bool {
    let mut undone = Vec::new();
    let Err(why) = start_steps(job, log, &mut undone) else {
        if let Some(pick) = job.launch.as_ref().and_then(|l| l.ticket.as_ref()) {
            mark_started(pick, log);
        }
        run_hook("on_dispatch", &hooks(log).on_dispatch, &job.tree.path, log);
        return true;
    };
    if !undone.is_empty() {
        let _ = writeln!(log, "rolling back");
    }
    for step in undone.into_iter().rev() {
        undo(step, &job.tree.path, log);
    }
    let _ = writeln!(log, "{why}");
    false
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

/// Runs a hook in `tree`. A failure goes to the log and nothing else.
fn run_hook(name: &str, command: &[String], tree: &Path, log: &mut String) {
    let Some((program, args)) = command.split_first() else {
        return;
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
    if let Err(why) = exec(&mut command, HOOK_LIMIT, log) {
        let _ = writeln!(log, "{name} hook failed: {why}");
    }
}

/// Ends every background agent a plugin reports in the tree, through that
/// plugin, since treehouse stops only processes standing in it, and a
/// session that moved in with `EnterWorktree` stands elsewhere. Each is
/// removed where its plugin can, so it is not found in the tree once the
/// tree is leased again, else stopped, cold ones too, so its plugin stops
/// keeping it awake.
fn end_agents(tree: &Tree, log: &mut String) {
    for agent in tree.agents.iter().filter(|a| a.is_background) {
        let (Some(name), Some(id)) = (&agent.plugin, &agent.job) else {
            continue;
        };
        let Some(plugin) = plugin::named(PluginKind::Agent, name) else {
            let _ = writeln!(
                log,
                "could not end {} ({id}): no agent plugin {name}",
                agent.name
            );
            continue;
        };
        let can_remove = plugin
            .description()
            .is_ok_and(|d| d.ops.iter().any(|op| op == "remove"));
        if can_remove {
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
        let ticket = Source::of(checkout)?.get(id)?;
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
    let source = match Source::of(&pick.checkout) {
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
        Kind::Preview => capture(
            treehouse("destroy", &job.tree.path).args(INCLUDE),
            TREEHOUSE_LIMIT,
            &mut log,
        ),
        Kind::Destroy => capture(
            treehouse("destroy", &job.tree.path)
                .args(INCLUDE)
                .arg("--yes"),
            TREEHOUSE_LIMIT,
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
}
