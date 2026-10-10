//! Agents started inside a tree: a process standing in it whose name is a
//! known agent's. It sees any agent, but only by name, never its status.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::{Agent, Sighting, Source};
use crate::live;
use crate::pool::Process;

/// Agents treetop knows by name. `TREETOP_AGENTS`, comma-separated, adds more.
const KNOWN: [&str; 8] = [
    "claude",
    "codex",
    "aider",
    "opencode",
    "gemini",
    "goose",
    "amp",
    "cursor-agent",
];

fn known() -> Vec<String> {
    let extra = std::env::var("TREETOP_AGENTS").unwrap_or_default();
    KNOWN
        .iter()
        .map(|name| (*name).to_string())
        .chain(
            extra
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(str::to_string),
        )
        .collect()
}

/// The agent a process is, by its `comm` or the file name, less extension, of
/// its first two arguments. The arguments cover agents run by an interpreter,
/// whose `comm` is `node` or `python3` and whose script is argv[1].
fn agent_name(comm: &str, args: &[String], known: &[String]) -> Option<String> {
    let stem = |arg: &String| {
        Path::new(arg)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
    };
    std::iter::once(Some(comm.to_string()))
        .chain(args.iter().take(2).map(stem))
        .flatten()
        .find(|name| known.contains(name))
}

fn cmdline(pid: u32) -> Vec<String> {
    fs::read(format!("/proc/{pid}/cmdline"))
        .map(|raw| {
            raw.split(|b| *b == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Keeps the agents whose parent is not itself an agent, so an agent's own
/// helper processes do not count as a second agent. Each entry is the agent
/// and its parent's pid.
fn outermost(found: Vec<(Agent, Option<u32>)>) -> Vec<Agent> {
    let pids: Vec<u32> = found.iter().map(|(agent, _)| agent.pid).collect();
    found
        .into_iter()
        .filter(|(_, ppid)| ppid.is_none_or(|ppid| !pids.contains(&ppid)))
        .map(|(agent, _)| agent)
        .collect()
}

pub fn find(processes: &HashMap<PathBuf, Vec<Process>>) -> Vec<Sighting> {
    let known = known();
    processes
        .iter()
        .flat_map(|(tree, processes)| {
            let found = processes
                .iter()
                .filter_map(|p| {
                    let name = agent_name(&p.name, &cmdline(p.pid), &known)?;
                    let agent = Agent {
                        name,
                        pid: p.pid,
                        status: None,
                        source: Source::Process,
                        is_background: false,
                    };
                    Some((agent, live::stat(p.pid).map(|s| s.ppid)))
                })
                .collect();
            outermost(found)
                .into_iter()
                .map(|agent| (tree.clone(), agent))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| (*a).to_string()).collect()
    }

    #[test]
    fn names_an_agent_by_comm_or_by_the_script_an_interpreter_runs() {
        let known = args(&["claude", "codex", "my-agent"]);
        assert_eq!(agent_name("claude", &[], &known).as_deref(), Some("claude"));
        assert_eq!(
            agent_name(
                "node",
                &args(&["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"]),
                &known
            )
            .as_deref(),
            Some("codex")
        );
        assert_eq!(
            agent_name("MainThread", &args(&["/opt/my-agent", "--yes"]), &known).as_deref(),
            Some("my-agent")
        );
        assert_eq!(
            agent_name("node", &args(&["node", "vite.js"]), &known),
            None
        );
        assert_eq!(
            agent_name("zsh", &args(&["zsh", "-c", "codex"]), &known),
            None
        );
    }

    #[test]
    fn an_agents_own_helpers_are_not_second_agents() {
        let agent = |pid| Agent {
            name: "claude".into(),
            pid,
            status: None,
            source: Source::Process,
            is_background: false,
        };
        let found = vec![
            (agent(10), Some(1)),
            (agent(11), Some(10)),
            (agent(20), None),
        ];
        let pids: Vec<u32> = outermost(found).iter().map(|a| a.pid).collect();
        assert_eq!(pids, [10, 20]);
    }
}
