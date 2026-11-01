//! What the phone can do to the pool: start an agent, start a stopped one
//! again, and give a finished tree back. Each runs as treetop's own keys
//! run it, through `jobs`, so the phone and the terminal never differ in
//! what an action does; the pool is listed again once it is done.

use serde::Deserialize;
use serde_json::json;

use crate::jobs::{self, Job, Kind, Launch};
use crate::pool::{Tree, stand_in};

use super::{Pool, Reply};

#[derive(Deserialize)]
struct Dispatch {
    task: String,
    /// The tree to start in; None leases a fresh one.
    #[serde(default)]
    tree: Option<String>,
    /// The branch a fresh tree is cut from; None for the pool's base.
    #[serde(default)]
    base: Option<String>,
}

/// Starts an agent on a task, in a fresh tree or the one named, as `n` and
/// `N` do.
pub fn dispatch(pool: &Pool, body: &[u8]) -> Reply {
    let Ok(asked) = serde_json::from_slice::<Dispatch>(body) else {
        return Reply::error(
            400,
            "dispatch takes {\"task\": \"...\", \"tree\": null, \"base\": null}",
        );
    };
    let task = asked.task.trim();
    if task.is_empty() {
        return Reply::error(400, "an agent needs a task");
    }
    let tree = match &asked.tree {
        None => stand_in("(new tree)".into()),
        Some(name) => match pool.tree(name) {
            Some(tree) if !tree.is_main => tree,
            _ => return Reply::error(404, &format!("no tree {name} in {}", pool.name)),
        },
    };
    let mut launch = Launch::agent(task.to_string(), asked.tree.is_none());
    launch.base = asked.base.filter(|b| !b.is_empty() && asked.tree.is_none());
    let mut job = Job {
        launch: Some(launch),
        ..Job::new(Kind::Start, tree)
    };
    let (is_ok, output) = jobs::run(&mut job);
    pool.refresh();
    if is_ok {
        Reply::json(200, &json!({ "tree": job.tree.name }))
    } else {
        Reply::error(502, &last_line(&output, "the agent did not start"))
    }
}

/// Starts the tree's stopped or failed agent again, with its conversation
/// and the flags it was started with, as `R` does.
pub fn resume(pool: &Pool, name: &str) -> Reply {
    let Some(tree) = pool.tree(name) else {
        return Reply::error(404, &format!("no tree {name} in {}", pool.name));
    };
    let mut job = Job::new(Kind::Resume, tree);
    let (is_ok, output) = jobs::run(&mut job);
    pool.refresh();
    if is_ok {
        Reply::json(200, &json!({}))
    } else {
        Reply::error(502, &last_line(&output, "nothing was resumed"))
    }
}

/// Gives the tree back to the pool as `r` does: its agents are stopped
/// first. Anything else running in it, such as a dev server, keeps it, since
/// the phone cannot show what stopping it would cost.
pub fn give_back(pool: &Pool, name: &str) -> Reply {
    let Some(tree) = pool.tree(name) else {
        return Reply::error(404, &format!("no tree {name} in {}", pool.name));
    };
    if let Err(why) = may_give_back(&tree) {
        return Reply::error(409, &why);
    }
    let mut job = Job::new(Kind::Return, tree);
    let (is_ok, output) = jobs::run(&mut job);
    pool.refresh();
    if is_ok {
        Reply::json(200, &json!({}))
    } else {
        Reply::error(
            502,
            &last_line(&output, &format!("tree {name} was not returned")),
        )
    }
}

/// Whether the phone may return `tree`: a held tree with nothing but its
/// agents running in it.
pub fn may_give_back(tree: &Tree) -> Result<(), String> {
    if tree.is_main {
        return Err("the main checkout is never returned".into());
    }
    if !tree.is_held() {
        return Err(format!("tree {} is already available", tree.name));
    }
    let agents: Vec<u32> = tree.agents.iter().filter_map(|a| a.pid).collect();
    let others: Vec<String> = tree
        .processes
        .iter()
        .filter(|p| !agents.contains(&p.pid))
        .map(|p| format!("{} ({})", p.name, p.pid))
        .collect();
    if others.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "tree {} has {} running in it; stop it, or return the tree from treetop",
            tree.name,
            others.join(", ")
        ))
    }
}

/// The last line a job wrote, which says why it failed; `otherwise` when it
/// wrote none.
fn last_line(output: &str, otherwise: &str) -> String {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(otherwise)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{Agent, Source, Status};
    use crate::pool::Process;
    use crate::serve::pool::tests::tree;

    fn process(pid: u32, name: &str) -> Process {
        Process {
            pid,
            name: name.into(),
            ports: Vec::new(),
        }
    }

    #[test]
    fn a_tree_goes_back_with_its_agents_but_not_from_under_anything_else() {
        let agent = Agent {
            name: "login".into(),
            pid: Some(40),
            status: Some(Status::Idle),
            source: Source::Plugin,
            is_background: true,
            attach: None,
            job: Some("j".into()),
            plugin: Some("claude".into()),
            active_at: None,
            context: None,
            spend: None,
        };
        let with = |processes: Vec<Process>| Tree {
            agents: vec![agent.clone()],
            processes,
            ..tree("17", "me/END-1-x")
        };
        assert_eq!(may_give_back(&with(vec![])), Ok(()));
        assert_eq!(may_give_back(&with(vec![process(40, "claude")])), Ok(()));
        let served = may_give_back(&with(vec![process(40, "claude"), process(41, "vite")]));
        assert_eq!(
            served,
            Err(
                "tree 17 has vite (41) running in it; stop it, or return the tree from treetop"
                    .into()
            )
        );
        let free = Tree {
            status: "available".into(),
            ..tree("3", "x")
        };
        assert!(
            may_give_back(&free)
                .unwrap_err()
                .contains("already available")
        );
        let main = Tree {
            is_main: true,
            ..tree("main", "main")
        };
        assert!(may_give_back(&main).unwrap_err().contains("never returned"));
    }
}
