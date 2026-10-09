//! What changes by the second, read directly the way htop does: which processes
//! stand in each tree, from /proc, and each held tree's git state. Both cost
//! milliseconds, where a `treehouse status` costs seconds.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use crate::pool::{self, Process, Tree, Work};

/// The pid and cwd of every process whose cwd is readable. One that exits
/// mid-read, or belongs to another user, is skipped.
fn running() -> Vec<(PathBuf, u32)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            Some((fs::read_link(entry.path().join("cwd")).ok()?, pid))
        })
        .collect()
}

/// Groups pids under the tree their cwd is in, which is how treehouse counts
/// them: the two agree process for process.
fn assign(trees: &[Tree], running: &[(PathBuf, u32)]) -> HashMap<PathBuf, Vec<u32>> {
    let mut by_tree: HashMap<PathBuf, Vec<u32>> = HashMap::new();
    for (cwd, pid) in running {
        if let Some(tree) = trees.iter().find(|t| t.contains(cwd)) {
            by_tree.entry(tree.path.clone()).or_default().push(*pid);
        }
    }
    for pids in by_tree.values_mut() {
        pids.sort_unstable();
    }
    by_tree
}

/// The processes standing in each tree, named from /proc. A tree with none is
/// absent from the map.
pub fn processes(trees: &[Tree]) -> HashMap<PathBuf, Vec<Process>> {
    assign(trees, &running())
        .into_iter()
        .map(|(path, pids)| {
            let processes = pids
                .into_iter()
                .filter_map(|pid| {
                    let name = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
                    Some(Process {
                        pid,
                        name: name.trim_end().to_string(),
                    })
                })
                .collect();
            (path, processes)
        })
        .collect()
}

fn work_in(dir: &Path) -> Option<Work> {
    let changed = pool::git(dir, &["status", "--porcelain"])?.lines().count();
    let unpushed = pool::git(dir, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?
        .trim()
        .parse()
        .ok()?;
    Some(Work { changed, unpushed })
}

/// Git state for every held tree, checked in parallel so one slow repository
/// does not hold up the rest. None for a tree git cannot read.
pub fn work(trees: &[Tree]) -> HashMap<PathBuf, Option<Work>> {
    thread::scope(|scope| {
        let checks: Vec<_> = trees
            .iter()
            .filter(|t| t.is_held())
            .map(|t| scope.spawn(move || (t.path.clone(), work_in(&t.path))))
            .collect();
        checks
            .into_iter()
            .filter_map(|check| check.join().ok())
            .collect()
    })
}

/// The repository's worktrees with their HEADs and branches. It changes when a
/// tree is leased onto a branch, returned or destroyed, and costs milliseconds,
/// so it tells treetop when the slow pool listing is worth running.
pub fn worktrees(checkout: &Path) -> Option<String> {
    pool::git(checkout, &["worktree", "list", "--porcelain"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;

    fn trees() -> Vec<Tree> {
        parse(
            r#"[
          {"name":"4","status":"leased","branch":"b","lease_holder":"me","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"40","status":"leased","branch":"c","lease_holder":"me","path":"/p/40/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap()
    }

    fn process(pid: u32, cwd: &str) -> (PathBuf, u32) {
        (PathBuf::from(cwd), pid)
    }

    #[test]
    fn assigns_processes_by_cwd_to_the_tree_they_stand_in() {
        let running = [
            process(9, "/p/4/app/ui"),
            process(3, "/p/4/app"),
            process(5, "/p/40/app/src"),
            process(7, "/home/me"),
        ];
        let by_tree = assign(&trees(), &running);
        let pids = |path: &str| by_tree[Path::new(path)].clone();
        assert_eq!(pids("/p/4/app"), [3, 9]);
        assert_eq!(pids("/p/40/app"), [5]);
        assert_eq!(by_tree.len(), 2);
    }
}
