//! Which coding agent is working in each tree, read from what agents already
//! leave behind, so nothing has to wrap or configure them. Three sources, each
//! answering which agents are alive and which directory each works in:
//!
//! - `holder`: an `agent.json` any agent or wrapper may write into the tree's
//!   git directory, the one open convention.
//! - `claude`: Claude Code's own session files, for sessions that move into a
//!   tree mid-session, which leaves their process's cwd where it started.
//! - `procs`: a known agent's process standing in the tree, for any agent
//!   started inside it.
//!
//! A source that cannot read its files yields nothing rather than an error.

mod claude;
mod holder;
mod procs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::pool::{Process, Tree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Busy,
    Idle,
    /// Waiting on you: a question, a permission prompt, a finished turn.
    Waiting,
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "busy" => Some(Status::Busy),
            "idle" => Some(Status::Idle),
            "waiting" => Some(Status::Waiting),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Busy => "busy",
            Status::Idle => "idle",
            Status::Waiting => "waiting",
        }
    }
}

/// Where an agent was seen, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Holder,
    Claude,
    Process,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Holder => "agent.json",
            Source::Claude => "claude code",
            Source::Process => "process",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    pub pid: u32,
    /// None where the source cannot say, as a bare process cannot.
    pub status: Option<Status>,
    pub source: Source,
    /// Runs with no terminal of its own, so there is no pane to switch to.
    pub is_background: bool,
    /// The command that opens its chat in a terminal, which `c` runs inside
    /// treetop; None when the agent offers none.
    pub attach: Option<Vec<String>>,
}

/// An agent and the directory it works in, before it is placed in a tree.
type Sighting = (PathBuf, Agent);

fn is_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The agents working in each tree. `processes` is the fast clock's latest
/// read, so the process scan looks only at processes already in a tree.
pub fn find(
    trees: &[Tree],
    processes: &HashMap<PathBuf, Vec<Process>>,
) -> HashMap<PathBuf, Vec<Agent>> {
    let mut sightings = holder::find(trees);
    sightings.extend(claude::find());
    sightings.extend(procs::find(processes));
    place(trees, sightings)
}

/// Puts each sighting in the tree its directory is in. A pid seen by two
/// sources counts once, where the more specific one saw it: a Claude Code
/// session that started in one tree and moved to another still stands in the
/// first, but works in the second.
fn place(trees: &[Tree], sightings: Vec<Sighting>) -> HashMap<PathBuf, Vec<Agent>> {
    let mut seen = HashSet::new();
    let mut by_tree: HashMap<PathBuf, Vec<Agent>> = HashMap::new();
    for (dir, agent) in sightings {
        let Some(tree) = trees.iter().find(|t| t.contains(&dir)) else {
            continue;
        };
        if seen.insert(agent.pid) {
            by_tree.entry(tree.path.clone()).or_default().push(agent);
        }
    }
    by_tree
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;

    fn agent(name: &str, pid: u32, source: Source) -> Agent {
        Agent {
            name: name.into(),
            pid,
            status: None,
            source,
            is_background: false,
            attach: None,
        }
    }

    #[test]
    fn a_pid_counts_once_where_the_most_specific_source_saw_it() {
        let trees = parse(
            r#"[
          {"name":"4","status":"leased","branch":"b","lease_holder":"me","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"9","status":"leased","branch":"c","lease_holder":"me","path":"/p/9/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap();
        let sightings = vec![
            (
                PathBuf::from("/p/9/app"),
                agent("fix-login", 7, Source::Claude),
            ),
            (
                PathBuf::from("/home/me"),
                agent("elsewhere", 8, Source::Claude),
            ),
            (
                PathBuf::from("/p/4/app/ui"),
                agent("claude", 7, Source::Process),
            ),
            (
                PathBuf::from("/p/4/app"),
                agent("codex", 12, Source::Process),
            ),
        ];
        let by_tree = place(&trees, sightings);
        let names = |path: &str| -> Vec<String> {
            by_tree[Path::new(path)]
                .iter()
                .map(|a| a.name.clone())
                .collect()
        };
        assert_eq!(names("/p/9/app"), ["fix-login"]);
        assert_eq!(names("/p/4/app"), ["codex"]);
        assert_eq!(by_tree.len(), 2);
    }
}
