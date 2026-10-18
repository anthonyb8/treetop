//! Which coding agent is working in each tree, read from what agents already
//! leave behind, so nothing has to wrap or configure them. Three sources, each
//! answering which agents are alive and which directory each works in:
//!
//! - `holder`: an `agent.json` any agent or wrapper may write into the tree's
//!   git directory, the one open convention.
//! - agent plugins' `find`, Claude Code's built in (`claude`), which also
//!   report cold agents: ones not running now that can be started again.
//! - `procs`: a known agent's process standing in the tree, for any agent
//!   started inside it.
//!
//! A source that cannot read its files yields nothing rather than an error.

pub mod claude;
mod holder;
mod procs;

pub use claude::config_dir;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::plugin::{self, Kind, Seen};
use crate::pool::{self, Process, Tree};
use crate::sidecar::Sidecar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Busy,
    Idle,
    /// Waiting on you: a question, a permission prompt, a finished turn.
    Waiting,
    /// Not running: its session ended, and opening it starts it again.
    Cold,
}

impl Status {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "busy" => Some(Status::Busy),
            "idle" => Some(Status::Idle),
            "waiting" => Some(Status::Waiting),
            "cold" => Some(Status::Cold),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Busy => "busy",
            Status::Idle => "idle",
            Status::Waiting => "waiting",
            Status::Cold => "cold",
        }
    }
}

/// Where an agent was seen, most specific first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Holder,
    /// An agent plugin, Claude Code's built in among them.
    Plugin,
    Process,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Holder => "agent.json",
            Source::Plugin => "plugin",
            Source::Process => "process",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub name: String,
    /// None for an agent with no process now: a cold one, which can be
    /// started again.
    pub pid: Option<u32>,
    /// None where the source cannot say, as a bare process cannot.
    pub status: Option<Status>,
    pub source: Source,
    /// Runs with no terminal of its own, so there is no pane to switch to.
    pub is_background: bool,
    /// The command that opens its chat in a terminal, which `c` runs inside
    /// treetop; None when the agent offers none.
    pub attach: Option<Vec<String>>,
    /// The background job Claude Code keeps its reports under, which joins
    /// the agent to its entry in the inbox.
    pub job: Option<String>,
    /// The agent plugin that reported it, which `send` and `stop` go
    /// through; None for one seen another way.
    pub plugin: Option<String>,
}

/// An agent and the directory it works in, before it is placed in a tree.
type Sighting = (PathBuf, Agent);

fn is_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// An agent plugin's sighting as an agent, where it works.
fn from_plugin(plugin: &str, seen: Seen) -> Sighting {
    let status = if seen.live {
        seen.status.as_deref().and_then(Status::parse)
    } else {
        Some(Status::Cold)
    };
    let agent = Agent {
        name: seen.name,
        pid: seen.pid.filter(|_| seen.live),
        status,
        source: Source::Plugin,
        is_background: seen.background,
        attach: seen.attach.filter(|command| !command.is_empty()),
        job: seen.id,
        plugin: Some(plugin.to_string()),
    };
    (seen.dir, agent)
}

/// What every agent plugin sees in `dirs`. A plugin that fails or answers
/// in a shape treetop cannot read adds nothing.
/// The agents plugins report, and when each cold one last worked, by its
/// job.
fn from_plugins(dirs: &[PathBuf]) -> (Vec<Sighting>, HashMap<String, u64>) {
    #[derive(Deserialize)]
    struct Found {
        #[serde(default)]
        agents: Vec<Seen>,
    }
    let mut active = HashMap::new();
    let sightings = plugin::of_kind(Kind::Agent)
        .iter()
        .filter_map(|plugin| {
            let result = plugin.call("find", json!({ "dirs": dirs })).ok()?;
            let found: Found = serde_json::from_value(result).ok()?;
            Some((plugin.name().to_string(), found.agents))
        })
        .flat_map(|(name, agents)| agents.into_iter().map(move |seen| (name.clone(), seen)))
        .map(|(name, seen)| {
            if let (Some(id), Some(at), false) = (&seen.id, seen.active_at, seen.live) {
                active.insert(id.clone(), at);
            }
            from_plugin(&name, seen)
        })
        .collect();
    (sightings, active)
}

/// The agents working in each tree. `processes` is the fast clock's latest
/// read, so the process scan looks only at processes already in a tree, and
/// `sidecars` the record of each tree treetop leased, which is how a cold
/// agent finds its tree.
pub fn find(
    trees: &[Tree],
    processes: &HashMap<PathBuf, Vec<Process>>,
    sidecars: &HashMap<PathBuf, Sidecar>,
) -> HashMap<PathBuf, Vec<Agent>> {
    let dirs: Vec<PathBuf> = trees.iter().map(|t| t.path.clone()).collect();
    let mut sightings = holder::find(trees);
    let (seen, active) = from_plugins(&dirs);
    sightings.extend(seen);
    sightings.extend(procs::find(processes));
    place(trees, sightings, sidecars, &active)
}

/// When the tree's current lease began, in seconds since the epoch: as
/// treetop recorded it, else as treehouse did. The main checkout is never
/// handed out, so it has been its own all along; an available tree has no
/// lease.
fn lease_began(tree: &Tree, sidecars: &HashMap<PathBuf, Sidecar>) -> Option<u64> {
    if tree.is_main {
        return Some(0);
    }
    if !tree.is_held() {
        return None;
    }
    sidecars
        .get(&tree.path)
        .map(|s| s.leased_at)
        .or_else(|| tree.leased_at.as_deref().and_then(pool::seconds))
}

/// Puts each sighting in the tree its directory is in. A pid seen by two
/// sources counts once, where the more specific one saw it: a Claude Code
/// session that started in one tree and moved to another still stands in the
/// first, but works in the second. A cold agent counts in the tree whose
/// sidecar names its job, else in the tree it last worked in, if that was
/// since the tree's lease began, by `active`, when each last worked: a tree
/// that has been returned and handed out again is not still the old agent's.
fn place(
    trees: &[Tree],
    sightings: Vec<Sighting>,
    sidecars: &HashMap<PathBuf, Sidecar>,
    active: &HashMap<String, u64>,
) -> HashMap<PathBuf, Vec<Agent>> {
    let mut seen = HashSet::new();
    let mut by_tree: HashMap<PathBuf, Vec<Agent>> = HashMap::new();
    for (dir, agent) in sightings {
        let tree = if agent.status == Some(Status::Cold) {
            let named = trees.iter().find(|t| {
                agent.job.is_some()
                    && sidecars.get(&t.path).and_then(|s| s.job.as_ref()) == agent.job.as_ref()
            });
            // A tree whose sidecar names another job is that job's.
            let worked_in = pool::holding(trees, &dir).filter(|t| {
                let is_another_jobs = sidecars.get(&t.path).is_some_and(|s| s.job.is_some());
                let last = agent.job.as_ref().and_then(|job| active.get(job));
                !is_another_jobs
                    && last
                        .zip(lease_began(t, sidecars))
                        .is_some_and(|(at, began)| *at >= began)
            });
            named.or(worked_in)
        } else {
            pool::holding(trees, &dir)
        };
        let Some(tree) = tree else {
            continue;
        };
        if agent.pid.is_none_or(|pid| seen.insert(pid)) {
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
            pid: Some(pid),
            status: None,
            source,
            is_background: false,
            attach: None,
            job: None,
            plugin: None,
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
                agent("fix-login", 7, Source::Plugin),
            ),
            (
                PathBuf::from("/home/me"),
                agent("elsewhere", 8, Source::Plugin),
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
        let by_tree = place(&trees, sightings, &HashMap::new(), &HashMap::new());
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

    #[test]
    fn a_cold_agent_counts_only_in_the_tree_whose_sidecar_names_its_job() {
        let trees = parse(
            r#"[
          {"name":"4","status":"leased","branch":"b","lease_holder":"treetop","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"9","status":"leased","branch":"c","lease_holder":"treetop","path":"/p/9/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap();
        let cold = |name: &str, job: &str| {
            from_plugin(
                "claude",
                Seen {
                    id: Some(job.into()),
                    name: name.into(),
                    // Both last worked in tree 4; tree 4 has since been handed
                    // to a new agent, recorded in its sidecar.
                    dir: PathBuf::from("/p/4/app"),
                    live: false,
                    pid: Some(5),
                    status: Some("busy".into()),
                    background: true,
                    attach: None,
                    active_at: None,
                },
            )
        };
        let sidecars = HashMap::from([(
            PathBuf::from("/p/9/app"),
            Sidecar {
                job: Some("aaaa1111".into()),
                ..Sidecar::leased_now()
            },
        )]);
        let by_tree = place(
            &trees,
            vec![cold("moved", "aaaa1111"), cold("stale", "bbbb2222")],
            &sidecars,
            &HashMap::new(),
        );
        let agents = &by_tree[Path::new("/p/9/app")];
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "moved");
        assert_eq!(agents[0].status, Some(Status::Cold));
        assert_eq!(agents[0].pid, None, "a cold agent has no process");
        assert!(!by_tree.contains_key(Path::new("/p/4/app")));
    }

    #[test]
    fn a_cold_agent_stays_in_the_tree_it_worked_in_since_its_lease_began() {
        // Tree 4 was leased by someone other than treetop, so it has no
        // sidecar; the main checkout is never handed out again.
        let trees = parse(
            r#"[
          {"name":"4","status":"leased","branch":"b","lease_holder":"claude:b","path":"/p/4/app","leased_at":"2026-10-17T12:00:00.5-04:00","processes":[]},
          {"name":"5","status":"available","branch":"","lease_holder":null,"path":"/p/5/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap();
        let cold = |name: &str, job: &str, dir: &str| {
            from_plugin(
                "claude",
                Seen {
                    id: Some(job.into()),
                    name: name.into(),
                    dir: PathBuf::from(dir),
                    live: false,
                    pid: None,
                    status: None,
                    background: true,
                    attach: None,
                    active_at: None,
                },
            )
        };
        let at = |time: &str| pool::seconds(time).unwrap();
        let active = HashMap::from([
            ("now0000".to_string(), at("2026-10-17T16:30:00Z")),
            ("then000".to_string(), at("2026-10-17T15:59:00Z")),
            ("idle000".to_string(), at("2026-10-17T16:30:00Z")),
        ]);
        let by_tree = place(
            &trees,
            vec![
                cold("this lease's", "now0000", "/p/4/app/src"),
                cold("an earlier lease's", "then000", "/p/4/app"),
                cold("in a returned tree", "idle000", "/p/5/app"),
            ],
            &HashMap::new(),
            &active,
        );
        let names: Vec<&str> = by_tree[Path::new("/p/4/app")]
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["this lease's"]);
        assert!(
            !by_tree.contains_key(Path::new("/p/5/app")),
            "a returned tree keeps none"
        );
    }
}
