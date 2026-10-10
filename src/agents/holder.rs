//! The holder file: `agent.json` in a tree's own git directory, an open
//! convention any agent or wrapper can follow to say it is working there.
//! The git directory keeps it out of `git status`. It counts only while its
//! pid is alive, so an agent that crashes leaves nothing stale on screen.
//!
//! ```json
//! { "name": "fix-login", "pid": 4242, "status": "waiting", "attach": ["my-agent", "attach", "fix-login"] }
//! ```
//!
//! `status`, one of `busy`, `idle` or `waiting`, and `attach`, the command
//! that opens the agent's chat in a terminal, are optional. treetop runs
//! `attach` only when `c` is pressed on the tree; the git dir is never cloned,
//! so only someone who can already write to the repository can set it.

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::{Agent, Sighting, Source, Status, is_alive};
use crate::pool::{self, Tree};

#[derive(Deserialize)]
struct Holder {
    name: String,
    pid: u32,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    attach: Option<Vec<String>>,
}

fn parse(text: &str) -> Option<Agent> {
    let holder: Holder = serde_json::from_str(text).ok()?;
    Some(Agent {
        name: holder.name,
        pid: holder.pid,
        status: holder.status.as_deref().and_then(Status::parse),
        source: Source::Holder,
        is_background: false,
        attach: holder.attach.filter(|command| !command.is_empty()),
    })
}

fn read(tree: &Path) -> Option<Agent> {
    let text = fs::read_to_string(pool::git_dir(tree)?.join("agent.json")).ok()?;
    parse(&text).filter(|agent| is_alive(agent.pid))
}

pub fn find(trees: &[Tree]) -> Vec<Sighting> {
    trees
        .iter()
        .filter(|t| t.is_held())
        .filter_map(|t| Some((t.path.clone(), read(&t.path)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A scratch worktree whose `.git` file points at a git dir beside it.
    fn worktree(name: &str, holder: Option<&str>) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("treetop-holder-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (tree, git) = (root.join("tree"), root.join("gitdir"));
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&git).unwrap();
        fs::write(tree.join(".git"), format!("gitdir: {}\n", git.display())).unwrap();
        if let Some(text) = holder {
            fs::write(git.join("agent.json"), text).unwrap();
        }
        tree
    }

    #[test]
    fn reads_a_live_holder_through_the_git_file() {
        let pid = std::process::id();
        let tree = worktree(
            "live",
            Some(&format!(
                r#"{{"name":"fix-login","pid":{pid},"status":"waiting","extra":1}}"#
            )),
        );
        let agent = read(&tree).unwrap();
        assert_eq!((agent.name.as_str(), agent.pid), ("fix-login", pid));
        assert_eq!(agent.status, Some(Status::Waiting));
    }

    #[test]
    fn ignores_a_dead_pid_malformed_json_and_no_file() {
        let dead = worktree("dead", Some(r#"{"name":"gone","pid":4294967295}"#));
        assert_eq!(read(&dead), None);
        let malformed = worktree("malformed", Some("{name: oops"));
        assert_eq!(read(&malformed), None);
        let absent = worktree("absent", None);
        assert_eq!(read(&absent), None);
    }

    #[test]
    fn reads_an_attach_command_and_treats_an_empty_one_as_none() {
        let agent = parse(r#"{"name":"a","pid":1,"attach":["my-agent","attach","a"]}"#).unwrap();
        assert_eq!(
            agent.attach,
            Some(vec!["my-agent".into(), "attach".into(), "a".into()])
        );
        assert_eq!(
            parse(r#"{"name":"a","pid":1,"attach":[]}"#).unwrap().attach,
            None
        );
        assert_eq!(parse(r#"{"name":"a","pid":1}"#).unwrap().attach, None);
    }

    #[test]
    fn an_unknown_status_reads_as_none() {
        let agent = parse(r#"{"name":"a","pid":1,"status":"thinking"}"#).unwrap();
        assert_eq!(agent.status, None);
    }
}
