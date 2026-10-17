//! treetop's commands that print and exit rather than open the screen:
//! `status`, `send` and `dispatch`. They are how the coordinator sees and
//! steers the other agents, and work as well from any shell. Each reads only
//! what it needs, so a call costs the coordinator one small JSON read.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, bail};
use serde_json::{Value, json};

use crate::agents;
use crate::app::report_state;
use crate::builtin;
use crate::cache;
use crate::chat::Chats;
use crate::coordinator;
use crate::dispatch::Recipient;
use crate::jobs::{self, Job, Kind, Launch};
use crate::live;
use crate::plugin;
use crate::pool::{self, Tree};
use crate::reports::{self, Report};
use crate::sidecar;
use crate::tickets::{Filters, Source};
use crate::watch::Check;

const USAGE: &str = "usage:
  treetop                          open the pool view
  treetop status                   every tree, agent and report, as JSON
  treetop send <tree|agent> <msg>  type a message into an agent's chat and send it
  treetop dispatch <new|main|N> <task>
                                   start a new agent on a task in a fresh tree,
                                   the main checkout or tree N
  treetop tickets [--mine] [words] this repository's open tickets, from its ticket source
  treetop plugins                  every plugin, built in or found, and what each is
  treetop plugin <name>            answer one request on stdin as a built-in plugin";

/// How long `send` waits for every message to go in.
const SEND_TIMEOUT: Duration = Duration::from_secs(45);

/// How long a chat is kept open after its message went in, so the Enter
/// reaches the session before the chat client is ended.
const SEND_GRACE: Duration = Duration::from_secs(1);

/// The size of the terminal a message is typed into, unseen.
const SEND_SIZE: (u16, u16) = (40, 120);

/// Answers `--help`, `--version` and `plugins`, which need no repository,
/// so they work from any directory. None for every other command.
pub fn answer_anywhere(args: &[String]) -> Option<String> {
    match args {
        [flag] if flag == "-h" || flag == "--help" || flag == "help" => Some(USAGE.into()),
        [flag] if flag == "-V" || flag == "--version" => {
            Some(format!("treetop {}", env!("CARGO_PKG_VERSION")))
        }
        [command] if command == "plugins" => Some(plugins()),
        _ => None,
    }
}

/// One line per file in the plugin directory: its name, kind, path and
/// ops, or why treetop cannot use it; then each built-in none replaces.
fn plugins() -> String {
    let Some(dir) = plugin::dir() else {
        return "no plugin directory: neither XDG_CONFIG_HOME nor HOME is set".into();
    };
    let found = plugin::discover(&dir);
    let mut lines: Vec<String> = found
        .iter()
        .map(|found| match &found.description {
            Ok(d) => format!(
                "{:<16} {:<8} {} ({})",
                d.name,
                d.kind.label(),
                found.path.display(),
                d.ops.join(", ")
            ),
            Err(err) => format!("{:<16} {:<8} {}: {err}", "-", "-", found.path.display()),
        })
        .collect();
    if found.is_empty() {
        lines.push(format!("no plugins in {}", dir.display()));
    }
    let replaced = |name: &str| {
        found
            .iter()
            .any(|f| f.description.as_ref().is_ok_and(|d| d.name == name))
    };
    for (name, kind) in builtin::KINDS {
        if !replaced(name) {
            lines.push(format!(
                "{name:<16} {:<8} built in (treetop plugin {name})",
                kind.label()
            ));
        }
    }
    lines.join("\n")
}

/// Runs the command `args` names, from the main checkout `checkout`.
pub fn run(args: &[String], checkout: &Path) -> Result<()> {
    match args {
        [command] if command == "status" => status(checkout),
        [command, target, message @ ..] if command == "send" && !message.is_empty() => {
            send(checkout, target, &message.join(" "))
        }
        [command, place, task @ ..] if command == "dispatch" && !task.is_empty() => {
            dispatch(checkout, place, &task.join(" "))
        }
        [command, rest @ ..] if command == "tickets" => tickets(checkout, rest),
        _ => bail!("{USAGE}"),
    }
}

/// The pool as treehouse lists it now, with the main checkout first.
fn listing(checkout: &Path) -> Result<Vec<Tree>> {
    let (mut trees, raw) = pool::status(checkout)?;
    cache::write(checkout, &raw);
    trees.insert(0, pool::main_tree(checkout));
    Ok(trees)
}

/// The last listing the screen saved, which is enough to find an agent's
/// tree and costs nothing; a live one when there is none.
fn cached_listing(checkout: &Path) -> Result<Vec<Tree>> {
    match cache::read(checkout) {
        Some(mut trees) => {
            trees.insert(0, pool::main_tree(checkout));
            Ok(trees)
        }
        None => listing(checkout),
    }
}

/// Fills in each tree's processes and agents.
fn read_agents(trees: &mut [Tree]) {
    let processes = live::processes(trees);
    let sidecars = sidecar::read_all(trees);
    let mut agents = agents::find(trees, &processes, &sidecars);
    for tree in trees.iter_mut() {
        tree.processes = processes.get(&tree.path).cloned().unwrap_or_default();
        tree.agents = agents.remove(&tree.path).unwrap_or_default();
        tree.sidecar = sidecars.get(&tree.path).cloned().map(Box::new);
    }
}

fn minutes_since(at: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(at).map_or(0, |age| age.as_secs() / 60)
}

fn report_json(report: &Report, trees: &[Tree], now: SystemTime) -> Value {
    json!({
        "job": report.job,
        "name": report.name,
        "state": report_state(report, trees).label(),
        "says": report.text,
        "minutes_ago": minutes_since(report.updated, now),
    })
}

/// Every tree that is held or has an agent or report in it, with its agents
/// and reports, then the reports from outside the pool. Available trees
/// with nothing in them are only counted. The coordinator is left out of
/// what it reads about.
fn status_json(trees: &[Tree], reports: &[Report], now: SystemTime) -> Value {
    let reports: Vec<&Report> = reports
        .iter()
        .filter(|r| r.name != coordinator::NAME)
        .collect();
    let holding = |report: &Report| reports::tree_of(report, trees).map(|tree| tree.path.clone());
    let mut listed = Vec::new();
    let mut available = 0;
    for tree in trees {
        let agents: Vec<Value> = tree
            .agents
            .iter()
            .filter(|a| a.name != coordinator::NAME)
            .map(|a| {
                json!({
                    "name": a.name,
                    "state": a.status.map(|s| s.label()),
                    "job": a.job,
                    "has_chat": a.attach.is_some(),
                })
            })
            .collect();
        let tree_reports: Vec<Value> = reports
            .iter()
            .filter(|r| holding(r).as_ref() == Some(&tree.path))
            .map(|r| report_json(r, trees, now))
            .collect();
        if !tree.is_held() && agents.is_empty() && tree_reports.is_empty() {
            available += 1;
            continue;
        }
        listed.push(json!({
            "tree": tree.name,
            "status": tree.status,
            "branch": tree.branch,
            "holder": tree.holder,
            "changed": tree.work.map(|w| w.changed),
            "unpushed": tree.work.map(|w| w.unpushed),
            "ports": tree.ports(),
            "agents": agents,
            "reports": tree_reports,
            "sidecar": tree.sidecar,
        }));
    }
    let elsewhere: Vec<Value> = reports
        .iter()
        .filter(|r| holding(r).is_none())
        .map(|r| {
            let mut value = report_json(r, trees, now);
            value["dir"] = json!(r.dir);
            value
        })
        .collect();
    json!({
        "trees": listed,
        "available_trees": available,
        "reports_outside_the_pool": elsewhere,
    })
}

fn status(checkout: &Path) -> Result<()> {
    let mut trees = listing(checkout)?;
    read_agents(&mut trees);
    let held: Vec<(PathBuf, Check)> = trees
        .iter()
        .filter(|t| t.is_held())
        .map(|t| (t.path.clone(), Check::Full))
        .collect();
    let mut work = live::work(held);
    for tree in &mut trees {
        tree.work = work.remove(&tree.path).flatten();
    }
    let reports = reports::Reader::default().read();
    let status = status_json(&trees, &reports, SystemTime::now());
    println!("{}", serde_json::to_string_pretty(&status)?);
    Ok(())
}

/// Whom `target` names: every agent with a chat in the tree of that number
/// (or `main`), else the agents of that name, else a stopped agent of that
/// name, which `claude attach` starts again. Never the coordinator itself.
fn recipients(trees: &[Tree], reports: &[Report], target: &str) -> Vec<Recipient> {
    let chats = |tree: &Tree, is_named: &dyn Fn(&str) -> bool| -> Vec<Recipient> {
        tree.agents
            .iter()
            .filter(|a| a.name != coordinator::NAME && is_named(&a.name))
            .filter_map(|a| Recipient::of(a, &tree.name))
            .collect()
    };
    if let Some(tree) = trees.iter().find(|t| t.name == target) {
        return chats(tree, &|_| true);
    }
    let named: Vec<Recipient> = trees
        .iter()
        .flat_map(|tree| chats(tree, &|name| name.eq_ignore_ascii_case(target)))
        .collect();
    if !named.is_empty() {
        return named;
    }
    reports
        .iter()
        .filter(|r| r.name != coordinator::NAME && r.name.eq_ignore_ascii_case(target))
        .max_by_key(|r| r.updated)
        .map(|r| {
            let tree = reports::tree_of(r, trees);
            let is_running = trees
                .iter()
                .flat_map(|t| &t.agents)
                .any(|a| a.job.as_deref() == Some(r.job.as_str()) && a.pid.is_some());
            Recipient::of_job(
                &r.name,
                tree.map_or_else(|| "outside the pool".into(), |t| t.name.clone()),
                &r.job,
                is_running,
            )
        })
        .into_iter()
        .collect()
}

fn send(checkout: &Path, target: &str, message: &str) -> Result<()> {
    let mut trees = cached_listing(checkout)?;
    read_agents(&mut trees);
    let reports = reports::Reader::default().read();
    let to = recipients(&trees, &reports, target);
    if to.is_empty() {
        bail!("no agent with a chat to type into at {target}; `treetop status` lists them");
    }
    let mut chats = Chats::default();
    let (mut delivered, mut failed) = (0, Vec::new());
    let mut typed = 0;
    for recipient in &to {
        let (agent, tree) = (recipient.agent.clone(), recipient.tree.clone());
        if let (true, Some(plugin), Some(id)) =
            (recipient.is_cold, &recipient.plugin, &recipient.job)
        {
            let message = jobs::Message {
                plugin: plugin.clone(),
                id: id.clone(),
                text: message.to_string(),
            };
            match jobs::send_cold(&message) {
                Ok(()) => {
                    println!("sent to {agent} in tree {tree}, starting it again");
                    delivered += 1;
                }
                Err(why) => failed.push((agent, tree, why)),
            }
            continue;
        }
        let hints = recipient.plugin.as_deref().and_then(plugin::screen_of);
        chats.deliver(&recipient.command, message, SEND_SIZE, (agent, tree), hints)?;
        typed += 1;
    }
    let started = Instant::now();
    let (mut typed_in, mut typed_failed) = (0, 0);
    while typed_in + typed_failed < typed && started.elapsed() < SEND_TIMEOUT {
        let pumped = chats.pump();
        for (agent, tree) in pumped.delivered {
            println!("sent to {agent} in tree {tree}");
            typed_in += 1;
        }
        typed_failed += pumped.failed.len();
        failed.extend(pumped.failed);
        thread::sleep(Duration::from_millis(20));
    }
    delivered += typed_in;
    // The Enter has been written; give the session a moment to take it
    // before the chat clients are ended.
    let grace = Instant::now();
    while grace.elapsed() < SEND_GRACE {
        chats.pump();
        thread::sleep(Duration::from_millis(20));
    }
    for (agent, tree, why) in &failed {
        eprintln!("not sent to {agent} in tree {tree}: {why}");
    }
    if delivered < to.len() {
        bail!("sent to {delivered} of {} agent(s)", to.len());
    }
    Ok(())
}

/// One line per open ticket of the repository's source: its id, state and
/// title. `--mine` keeps those assigned to you; other words search titles.
fn tickets(checkout: &Path, args: &[String]) -> Result<()> {
    let source = Source::of(checkout).map_err(anyhow::Error::msg)?;
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .filter(|a| *a != "--mine")
        .collect();
    let filters = Filters {
        mine: args.iter().any(|a| a == "--mine"),
        query: (!words.is_empty()).then(|| words.join(" ")),
        ..Filters::default()
    };
    let tickets = source.list(&filters).map_err(anyhow::Error::msg)?;
    if tickets.is_empty() {
        println!("no open tickets in {}", source.name);
    }
    for ticket in tickets {
        println!("{:<10} {:<14} {}", ticket.id, ticket.state, ticket.title);
    }
    Ok(())
}

fn dispatch(checkout: &Path, place: &str, task: &str) -> Result<()> {
    let (tree, is_new_tree) = match place {
        "new" => (pool::stand_in(PathBuf::from("(new tree)")), true),
        "main" => (pool::main_tree(checkout), false),
        number => match listing(checkout)?.into_iter().find(|t| t.name == number) {
            Some(tree) => (tree, false),
            None => bail!("no tree {number} in the pool; give new, main or a tree number"),
        },
    };
    let mut job = Job {
        launch: Some(Launch::agent(task.to_string(), is_new_tree)),
        ..Job::new(Kind::Start, tree)
    };
    let (is_ok, output) = jobs::run(&mut job);
    print!("{output}");
    if !is_ok {
        bail!("the agent did not start in tree {}", job.tree.name);
    }
    println!("started an agent in tree {}", job.tree.name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, Source, Status};
    use crate::reports::State;

    fn tree(name: &str, status: &str, agents: Vec<Agent>) -> Tree {
        Tree {
            name: name.into(),
            status: status.into(),
            branch: Some(format!("feature/{name}")),
            agents,
            ..pool::stand_in(PathBuf::from(format!("/p/{name}/app")))
        }
    }

    fn agent(name: &str, job: &str) -> Agent {
        Agent {
            name: name.into(),
            pid: Some(1),
            status: Some(Status::Busy),
            source: Source::Plugin,
            is_background: true,
            attach: Some(vec!["claude".into(), "attach".into(), job.into()]),
            job: Some(job.into()),
            plugin: None,
        }
    }

    fn report(job: &str, name: &str, state: State, dir: &str) -> Report {
        Report {
            job: job.into(),
            name: name.into(),
            state,
            text: format!("{name} says"),
            dir: Some(PathBuf::from(dir)),
            updated: SystemTime::UNIX_EPOCH,
            steps: Vec::new(),
        }
    }

    fn pool() -> (Vec<Tree>, Vec<Report>) {
        let trees = vec![
            tree("4", "leased", vec![agent("fix-login", "aa")]),
            tree("7", "available", vec![]),
            tree("8", "leased", vec![agent(coordinator::NAME, "cc")]),
        ];
        let reports = vec![
            report("aa", "fix-login", State::Blocked, "/p/4/app"),
            report("bb", "old-task", State::Stopped, "/home/me/elsewhere"),
            report("cc", coordinator::NAME, State::Working, "/p/8/app"),
        ];
        (trees, reports)
    }

    #[test]
    fn status_lists_what_is_held_counts_the_rest_and_leaves_out_the_coordinator() {
        let (trees, reports) = pool();
        let status = status_json(&trees, &reports, SystemTime::UNIX_EPOCH);
        let listed: Vec<&str> = status["trees"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["tree"].as_str().unwrap())
            .collect();
        assert_eq!(listed, ["4", "8"]);
        assert_eq!(status["available_trees"], 1);
        let four = &status["trees"][0];
        assert_eq!(four["agents"][0]["name"], "fix-login");
        assert_eq!(four["reports"][0]["state"], "needs you");
        assert_eq!(four["reports"][0]["says"], "fix-login says");
        assert_eq!(status["trees"][1]["agents"], json!([]));
        assert_eq!(status["trees"][1]["reports"], json!([]));
        assert_eq!(status["reports_outside_the_pool"][0]["name"], "old-task");
    }

    #[test]
    fn send_finds_a_tree_by_number_an_agent_by_name_or_a_stopped_one() {
        let (trees, reports) = pool();
        let by_tree = recipients(&trees, &reports, "4");
        assert_eq!(by_tree[0].agent, "fix-login");
        assert_eq!(by_tree[0].command, ["claude", "attach", "aa"]);
        assert_eq!(recipients(&trees, &reports, "FIX-LOGIN")[0].tree, "4");
        let stopped = recipients(&trees, &reports, "old-task");
        assert_eq!(stopped[0].command, ["claude", "attach", "bb"]);
        assert_eq!(stopped[0].tree, "outside the pool");
        assert!(recipients(&trees, &reports, "8").is_empty(), "never itself");
        assert!(recipients(&trees, &reports, coordinator::NAME).is_empty());
        assert!(recipients(&trees, &reports, "nobody").is_empty());
    }
}
