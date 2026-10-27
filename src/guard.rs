//! Who may give a tree back from the command line. treehouse's own
//! `--if-lease-holder` compares only the name it is handed, so any caller
//! can pass any holder's; treetop decides from what it can see instead:
//!
//! - nobody returns a tree a live process works in, other than the caller's
//!   own: a process standing in it, or an agent placed in it;
//! - the caller returns a tree it holds: the one its agent works in, or for
//!   a caller that is no agent, the one it stands in;
//! - anyone returns a tree whose work is finished: its PR merged, or it is on
//!   no branch, with nothing uncommitted and nothing unpushed.
//!
//! Everything else is the user's to return, from treetop's return dialog,
//! which names what would be lost.
//!
//! A command that acts on the tree it runs in, such as stopping its dev
//! stack, the caller may run only in a tree it holds, or in one back in the
//! pool that nothing runs in. `treetop return` asks here itself; an agent's
//! shell commands come here through `hook`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::live;
use crate::pool::{self, Tree};
use crate::ship::Stage;

/// Whoever is asking: its own process and every ancestor, and where it
/// stands.
pub struct Caller {
    pub ancestry: HashSet<u32>,
    pub cwd: PathBuf,
}

impl Caller {
    /// This process, run in `cwd`.
    pub fn this(cwd: &Path) -> Caller {
        let mut ancestry = HashSet::new();
        let mut pid = std::process::id();
        while pid > 1 && ancestry.insert(pid) {
            let Some(stat) = live::stat(pid) else {
                break;
            };
            pid = stat.ppid;
        }
        Caller {
            ancestry,
            cwd: cwd.to_path_buf(),
        }
    }
}

/// Every live pid in the tree that is not one of `except`, the caller's
/// own: processes standing in it and agents placed in it, lowest first.
pub fn live_pids(tree: &Tree, except: &HashSet<u32>) -> Vec<u32> {
    let mut pids: Vec<u32> = tree
        .processes
        .iter()
        .map(|p| p.pid)
        .chain(tree.agents.iter().filter_map(|a| a.pid))
        .filter(|pid| !except.contains(pid))
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

/// Whether the caller holds `tree`: its agent, the nearest one among its
/// ancestors that treetop placed in any tree, works there; or, when no
/// ancestor is such an agent, it stands there.
pub fn holds(trees: &[Tree], tree: &Tree, caller: &Caller) -> bool {
    let agent_tree = trees.iter().find(|t| {
        t.agents
            .iter()
            .any(|a| a.pid.is_some_and(|pid| caller.ancestry.contains(&pid)))
    });
    match agent_tree {
        Some(home) => home.path == tree.path,
        None => pool::holding(trees, &caller.cwd).is_some_and(|t| t.path == tree.path),
    }
}

/// Why the tree's work is not finished, or None when it is: its PR merged
/// or it has no branch, and git reads nothing uncommitted or unpushed.
pub fn unfinished(tree: &Tree) -> Option<String> {
    let mut why = Vec::new();
    let is_merged = tree.ship.as_ref().is_some_and(|s| s.stage == Stage::Merged);
    if let (Some(branch), false) = (&tree.branch, is_merged) {
        why.push(format!("{branch} has no merged PR"));
    }
    match tree.work {
        None => why.push("git cannot read its changes".into()),
        Some(work) => {
            if work.changed > 0 {
                why.push(format!("{} uncommitted", work.changed));
            }
            if work.unpushed > 0 {
                why.push(format!("{} unpushed", work.unpushed));
            }
        }
    }
    (!why.is_empty()).then(|| why.join(", "))
}

/// Whether the caller may return `tree`, and why not when it may not.
pub fn may_return(trees: &[Tree], tree: &Tree, caller: &Caller) -> Result<(), String> {
    if tree.is_main {
        return Err("the main checkout is never returned".into());
    }
    if !tree.is_held() {
        return Err(format!("tree {} is already available", tree.name));
    }
    let live = live_pids(tree, &caller.ancestry);
    if !live.is_empty() {
        let pids: Vec<String> = live.iter().map(u32::to_string).collect();
        return Err(format!(
            "tree {} has live processes ({}); only the user can return it, from treetop",
            tree.name,
            pids.join(", ")
        ));
    }
    if holds(trees, tree, caller) {
        return Ok(());
    }
    match unfinished(tree) {
        None => Ok(()),
        Some(why) => Err(format!(
            "tree {} is not yours and its work is not finished ({why}); \
             only the user can return it, from treetop",
            tree.name
        )),
    }
}

/// Whether the caller may run a command that acts on `tree`, the tree it
/// runs in, and why not when it may not.
pub fn may_act_in(trees: &[Tree], tree: &Tree, caller: &Caller) -> Result<(), String> {
    if holds(trees, tree, caller) {
        return Ok(());
    }
    let live = live_pids(tree, &caller.ancestry);
    if !tree.is_main && !tree.is_held() && live.is_empty() {
        return Ok(());
    }
    let pids: Vec<String> = live.iter().map(u32::to_string).collect();
    let live = match pids.as_slice() {
        [] => String::new(),
        pids => format!(", and live processes ({}) work in it", pids.join(", ")),
    };
    Err(format!(
        "tree {} is not yours{live}; only the user can run this in it",
        tree.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, Source};
    use crate::pool::{Process, Work, parse};
    use crate::ship::Ship;

    fn trees() -> Vec<Tree> {
        let mut trees = parse(
            r#"[
          {"name":"27","status":"leased","branch":"feature/mine","lease_holder":"claude:mine","path":"/p/27/app","leased_at":null,"processes":[]},
          {"name":"28","status":"leased","branch":"feature/theirs","lease_holder":"claude:theirs","path":"/p/28/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap();
        for tree in &mut trees {
            tree.work = Some(Work::default());
        }
        trees
    }

    fn agent(pid: u32) -> Agent {
        Agent {
            name: "claude".into(),
            pid: Some(pid),
            status: None,
            source: Source::Plugin,
            is_background: true,
            attach: None,
            job: None,
            plugin: None,
            active_at: None,
            context: None,
            spend: None,
        }
    }

    fn caller(ancestry: &[u32], cwd: &str) -> Caller {
        Caller {
            ancestry: ancestry.iter().copied().collect(),
            cwd: PathBuf::from(cwd),
        }
    }

    fn merged(tree: &mut Tree) {
        tree.ship = Some(Box::new(Ship {
            branch: tree.branch.clone().unwrap_or_default(),
            stage: Stage::Merged,
            label: "#9 merged".into(),
            url: None,
            head: None,
            detail: Vec::new(),
            plugin: "github".into(),
        }));
    }

    #[test]
    fn an_agent_holds_the_tree_it_works_in_wherever_its_shell_stands() {
        let mut trees = trees();
        trees[0].agents.push(agent(100));
        let from_27 = caller(&[300, 200, 100], "/p/28/app");
        assert!(holds(&trees, &trees[0], &from_27));
        assert!(!holds(&trees, &trees[1], &from_27), "cd is not a lease");
        let person = caller(&[400], "/p/28/app/src");
        assert!(holds(&trees, &trees[1], &person));
        assert!(!holds(&trees, &trees[0], &person));
    }

    #[test]
    fn a_live_process_or_agent_in_the_tree_refuses_anyone_but_itself() {
        let mut trees = trees();
        trees[1].processes.push(Process {
            pid: 51,
            name: "node".into(),
            ports: Vec::new(),
        });
        trees[1].agents.push(agent(52));
        let outsider = caller(&[9], "/p/27/app");
        let why = may_return(&trees, &trees[1], &outsider).unwrap_err();
        assert!(why.contains("51, 52"), "{why}");
        let itself = caller(&[60, 52, 51], "/p/28/app");
        assert_eq!(may_return(&trees, &trees[1], &itself), Ok(()));
    }

    #[test]
    fn a_command_on_a_tree_runs_only_in_one_the_caller_holds_or_one_left_empty() {
        let mut trees = trees();
        trees[0].agents.push(agent(100));
        let from_27 = caller(&[300, 100], "/p/28/app");
        assert_eq!(may_act_in(&trees, &trees[0], &from_27), Ok(()));
        let why = may_act_in(&trees, &trees[1], &from_27).unwrap_err();
        assert!(why.starts_with("tree 28 is not yours"), "{why}");
        trees[1].status = "available".into();
        assert_eq!(
            may_act_in(&trees, &trees[1], &from_27),
            Ok(()),
            "a returned tree's leftovers"
        );
        trees[1].processes.push(Process {
            pid: 7,
            name: "node".into(),
            ports: Vec::new(),
        });
        let why = may_act_in(&trees, &trees[1], &from_27).unwrap_err();
        assert!(why.contains("live processes (7)"), "{why}");
    }

    #[test]
    fn a_tree_not_held_is_returned_only_once_its_work_is_finished() {
        let mut trees = trees();
        let outsider = caller(&[9], "/repo");
        let why = may_return(&trees, &trees[1], &outsider).unwrap_err();
        assert!(why.contains("feature/theirs has no merged PR"), "{why}");
        merged(&mut trees[1]);
        assert_eq!(may_return(&trees, &trees[1], &outsider), Ok(()));
        trees[1].work = Some(Work {
            changed: 2,
            unpushed: 1,
            ..Work::default()
        });
        let why = may_return(&trees, &trees[1], &outsider).unwrap_err();
        assert!(why.contains("2 uncommitted, 1 unpushed"), "{why}");
        trees[1].work = None;
        assert!(
            may_return(&trees, &trees[1], &outsider).is_err(),
            "unknown is not clean"
        );
        trees[1].branch = None;
        trees[1].ship = None;
        trees[1].work = Some(Work::default());
        assert_eq!(
            may_return(&trees, &trees[1], &outsider),
            Ok(()),
            "a clean tree on no branch"
        );
    }
}
