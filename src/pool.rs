//! The pool as treehouse reports it. treetop never reads treehouse's own files:
//! `treehouse status --json` is the only source of pool state, so a treehouse
//! upgrade cannot leave it reading stale state. Processes and git state, which
//! change by the second, come from `live` instead.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::agents::Agent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub name: String,
    /// The TCP ports it listens on, lowest first.
    pub ports: Vec<u16>,
}

/// Uncommitted files and commits on no remote: the work a destroy loses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Work {
    pub changed: usize,
    pub unpushed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    pub name: String,
    pub status: String,
    pub branch: Option<String>,
    pub holder: Option<String>,
    pub path: PathBuf,
    pub leased_at: Option<String>,
    pub processes: Vec<Process>,
    /// The coding agents working in the tree, the most specific source first.
    pub agents: Vec<Agent>,
    /// None for an available tree, which treehouse keeps clean, and when git fails.
    pub work: Option<Work>,
}

impl Tree {
    /// Somebody holds it, so `treehouse return` has something to release.
    pub fn is_held(&self) -> bool {
        self.status != "available"
    }

    /// `dir` is this tree or inside it, compared by path component.
    pub fn contains(&self, dir: &Path) -> bool {
        dir.starts_with(&self.path)
    }

    /// Every port a process in the tree listens on, lowest first, each once:
    /// a server on both 127.0.0.1 and ::1 holds two sockets on one port.
    pub fn ports(&self) -> Vec<u16> {
        let mut ports: Vec<u16> = self
            .processes
            .iter()
            .flat_map(|p| p.ports.iter().copied())
            .collect();
        ports.sort_unstable();
        ports.dedup();
        ports
    }

    /// Held, with no agent working in it: the tree most likely to be forgotten.
    pub fn is_orphan(&self) -> bool {
        self.is_held() && self.agents.is_empty()
    }
}

#[derive(Deserialize)]
struct RawProcess {
    pid: u32,
    name: String,
}

#[derive(Deserialize)]
struct RawTree {
    name: String,
    status: String,
    branch: Option<String>,
    lease_holder: Option<String>,
    path: PathBuf,
    leased_at: Option<String>,
    #[serde(default)]
    processes: Vec<RawProcess>,
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

/// Parses `treehouse status --json`, ordered by pool number the way
/// `treehouse status` prints it.
pub fn parse(json: &str) -> Result<Vec<Tree>> {
    let raw: Vec<RawTree> =
        serde_json::from_str(json).context("reading treehouse status --json")?;
    let mut trees: Vec<Tree> = raw
        .into_iter()
        .map(|t| Tree {
            name: t.name,
            status: t.status,
            branch: non_empty(t.branch),
            holder: non_empty(t.lease_holder),
            path: t.path,
            leased_at: t.leased_at,
            processes: t
                .processes
                .into_iter()
                .map(|p| Process {
                    pid: p.pid,
                    name: p.name,
                    ports: Vec::new(),
                })
                .collect(),
            agents: Vec::new(),
            work: None,
        })
        .collect();
    trees.sort_by(|a, b| {
        let key = |t: &Tree| (t.name.parse::<u64>().unwrap_or(u64::MAX), t.name.clone());
        key(a).cmp(&key(b))
    });
    Ok(trees)
}

/// The pool of the repository `dir` belongs to, and the raw JSON it was read
/// from, for the cache.
pub fn status(dir: &Path) -> Result<(Vec<Tree>, String)> {
    let out = Command::new("treehouse")
        .args(["status", "--json"])
        .current_dir(dir)
        .output()
        .context("running treehouse status")?;
    if !out.status.success() {
        bail!(
            "treehouse status: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let raw = String::from_utf8_lossy(&out.stdout).into_owned();
    Ok((parse(&raw)?, raw))
}

/// Runs git in `dir`. `--no-optional-locks` keeps a status check, run every
/// couple of seconds, from taking the index lock out from under whoever is
/// working in the tree.
pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The tree's own git directory: `.git` itself in a main checkout, else the
/// directory a linked worktree's `.git` file names. Read without running git,
/// because it is asked for every few seconds.
pub fn git_dir(tree: &Path) -> Option<PathBuf> {
    let dot_git = tree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let pointer = std::fs::read_to_string(&dot_git).ok()?;
    let dir = PathBuf::from(pointer.strip_prefix("gitdir:")?.trim());
    Some(if dir.is_relative() {
        tree.join(dir)
    } else {
        dir
    })
}

/// The directory a git dir shares with every worktree of its repository,
/// which holds the refs: named by its `commondir` file, else the git dir
/// itself.
pub fn common_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => git_dir.join(common.trim()),
        Err(_) => git_dir.to_path_buf(),
    }
}

/// The main checkout of the repository `dir` is in. treetop runs from there,
/// because `treehouse return` terminates every process standing in the tree it
/// returns, and treetop must not be one of them.
pub fn main_checkout(dir: &Path) -> Result<PathBuf> {
    let common = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .context("not inside a git repository")?;
    let common = PathBuf::from(common.trim());
    common
        .parent()
        .map(Path::to_path_buf)
        .context("git common dir has no parent")
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"[
      {"name":"10","status":"available","branch":"","lease_holder":"","path":"/p/10/app",
       "leased_at":null,"processes":[],"flavor":"git","detached":true,"lease_id":""},
      {"name":"4","status":"leased","branch":"feature/end-1-x","lease_holder":"claude:abc",
       "path":"/p/4/app","leased_at":"2026-09-24T15:23:37-04:00",
       "processes":[{"pid":42,"name":"node"}],"flavor":null,"detached":null,"lease_id":"l1"}
    ]"#;

    #[test]
    fn parses_and_orders_by_pool_number() {
        let trees = parse(STATUS).unwrap();
        assert_eq!(
            trees.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["4", "10"]
        );
        assert_eq!(trees[0].branch.as_deref(), Some("feature/end-1-x"));
        assert_eq!(
            trees[0].processes,
            [Process {
                pid: 42,
                name: "node".into(),
                ports: vec![]
            }]
        );
        assert!(trees[0].is_held());
    }

    #[test]
    fn empty_branch_and_holder_read_as_none() {
        let trees = parse(STATUS).unwrap();
        assert_eq!(trees[1].branch, None);
        assert_eq!(trees[1].holder, None);
        assert!(!trees[1].is_held());
    }

    #[test]
    fn ports_are_listed_once_lowest_first_across_processes() {
        let mut tree = parse(STATUS).unwrap().remove(0);
        let process = |pid, ports: &[u16]| Process {
            pid,
            name: "node".into(),
            ports: ports.to_vec(),
        };
        tree.processes = vec![process(1, &[5223, 9229]), process(2, &[5223, 3000])];
        assert_eq!(tree.ports(), [3000, 5223, 9229]);
    }

    #[test]
    fn contains_compares_whole_path_components() {
        let tree = &parse(STATUS).unwrap()[0];
        assert!(tree.contains(Path::new("/p/4/app/ui/src")));
        assert!(!tree.contains(Path::new("/p/40/app")));
    }
}
