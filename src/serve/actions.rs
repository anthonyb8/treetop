//! What the phone can do to the pool: start an agent, start a stopped one
//! again, give a finished tree back, set a tree aside, and reach a port a
//! tree serves. Each runs as treetop's own keys run it, through `jobs`,
//! `sidecar` and `phone`, so the phone and the terminal never differ in what
//! an action does; the pool is listed again once it is done.

use std::sync::PoisonError;

use serde::Deserialize;
use serde_json::json;

use crate::jobs::{self, Job, Kind, Launch};
use crate::phone::{self, Shared};
use crate::pool::{Tree, stand_in};
use crate::sidecar::{self, Shelf, Sidecar};

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

/// Starts an agent on a task, in a fresh tree or the one named, as `N` and
/// `n` do.
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

/// Starts an agent on ticket `id` of issue source `source` in a fresh tree, as
/// `p` does on the Issues screen, its task, name and branch from the ticket.
/// A ticket already in a tree is not picked up twice: the answer names that
/// tree.
pub fn pick(pool: &Pool, id: &str, source: Option<String>, body: &[u8]) -> Reply {
    #[derive(Deserialize)]
    struct Picked {
        /// The branch the tree is cut from; None for the pool's base.
        #[serde(default)]
        base: Option<String>,
    }
    let Ok(asked) = serde_json::from_slice::<Picked>(body) else {
        return Reply::error(400, "pick takes {\"base\": null}");
    };
    let held = {
        let app = pool.app.lock().unwrap_or_else(PoisonError::into_inner);
        app.tree_of_ticket(id).map(|tree| tree.name.clone())
    };
    if let Some(tree) = held {
        return Reply::json(200, &json!({ "tree": tree }));
    }
    let mut job = Job {
        launch: Some(pick_launch(pool, id, source, asked.base)),
        ..Job::new(Kind::Start, stand_in("(new tree)".into()))
    };
    let (is_ok, output) = jobs::run(&mut job);
    pool.refresh();
    if is_ok {
        Reply::json(200, &json!({ "tree": job.tree.name }))
    } else {
        Reply::error(502, &last_line(&output, "the agent did not start"))
    }
}

/// What picking up ticket `id` starts: a fresh tree cut from `base`, or from
/// the pool's own where none was chosen.
fn pick_launch(pool: &Pool, id: &str, source: Option<String>, base: Option<String>) -> Launch {
    let mut launch = Launch::pick(id.to_string(), pool.checkout.clone(), source);
    launch.base = base.filter(|b| !b.is_empty());
    launch
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

/// Sets the tree aside on `shelf`, Parked or Backburner, or brings it back
/// from there, as `z` and `x` do. The phone says which it wants rather than
/// asking for a flip, so a second tap on a screen that had not caught up
/// changes nothing: `{"parked": bool}` for Parked, `{"backburner": bool}`
/// for Backburner.
pub fn shelve(pool: &Pool, name: &str, shelf: Shelf, body: &[u8]) -> Reply {
    #[derive(Deserialize)]
    struct Asked {
        parked: Option<bool>,
        backburner: Option<bool>,
    }
    let asked = serde_json::from_slice::<Asked>(body).ok();
    let Some(on) = asked.and_then(|asked| match shelf {
        Shelf::Parked => asked.parked,
        Shelf::Backburner => asked.backburner,
    }) else {
        return Reply::error(
            400,
            match shelf {
                Shelf::Parked => "park takes {\"parked\": true | false}",
                Shelf::Backburner => "backburner takes {\"backburner\": true | false}",
            },
        );
    };
    let Some(tree) = pool.tree(name) else {
        return Reply::error(404, &format!("no tree {name} in {}", pool.name));
    };
    if tree.is_main || !tree.is_held() {
        return Reply::error(
            409,
            &format!("tree {name} is not leased: nothing to set aside"),
        );
    }
    match set_shelf(&tree, shelf, on) {
        Ok(record) => {
            let mut app = pool.app.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(tree) = app.trees.iter_mut().find(|t| t.name == name) {
                tree.sidecar = Some(Box::new(record));
            }
            drop(app);
            pool.refresh();
            Reply::json(200, &json!({}))
        }
        Err(err) => Reply::error(500, &format!("could not record it: {err}")),
    }
}

/// Puts the tree on `shelf` when `on`, else takes it off that shelf, in its
/// `treetop.json`, over what the file says now rather than what the pool
/// last read, so nothing written since is lost. Taking it off a shelf it is
/// not on leaves it where it is.
fn set_shelf(tree: &Tree, shelf: Shelf, on: bool) -> std::io::Result<Sidecar> {
    let mut record = sidecar::read(&tree.path)
        .or_else(|| tree.sidecar.as_deref().cloned())
        .unwrap_or_else(Sidecar::leased_now);
    if on {
        record.shelve(Some(shelf));
    } else if record.shelf() == Some(shelf) {
        record.shelve(None);
    }
    sidecar::write(&tree.path, &record)?;
    Ok(record)
}

/// Shares a port one of the pool's trees serves with the phone, as `p` does,
/// and says where to reach it. Only those: nothing else listening on the
/// server goes onto the tailnet from a tap.
pub fn share_port(pool: &Pool, share: &[String], port: u16) -> Reply {
    if !pool.serves(port) {
        return Reply::error(404, &format!("no tree in {} serves port {port}", pool.name));
    }
    match phone::share(share, port) {
        Ok(url) => Reply::json(200, &json!({ "url": url })),
        Err(why) => Reply::error(502, &why),
    }
}

/// Puts a port one of the pool's trees serves on the internet, as `P` does,
/// and says its public address, to copy or send. Only those, as with
/// sharing: nothing else listening on the server goes public from a tap.
pub fn publish_port(pool: &Pool, publish: &[String], port: u16) -> Reply {
    if !pool.serves(port) {
        return Reply::error(404, &format!("no tree in {} serves port {port}", pool.name));
    }
    match phone::publish_now(publish, port) {
        Ok(url) => {
            pool.set_share(
                port,
                Some(Shared {
                    port,
                    is_public: true,
                    address: url.clone(),
                }),
            );
            Reply::json(200, &json!({ "url": url }))
        }
        Err(why) => Reply::error(502, &why),
    }
}

/// Takes a port one of the pool's trees serves off the internet and the
/// tailnet, as `x` does in `P`'s popup. Only those, so a tap cannot take
/// treetop serve's own port off the tailnet and the phone with it.
pub fn unshare_port(pool: &Pool, unpublish: &[String], port: u16) -> Reply {
    if !pool.serves(port) {
        return Reply::error(404, &format!("no tree in {} serves port {port}", pool.name));
    }
    match phone::unshare_now(unpublish, port) {
        Ok(()) => {
            pool.set_share(port, None);
            Reply::json(200, &json!({}))
        }
        Err(why) => Reply::error(502, &why),
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
    use std::collections::HashMap;
    use std::sync::Mutex;

    use serde_json::Value;

    use crate::agents::{Agent, Source, Status};
    use crate::pool::Process;
    use crate::serve::pool::tests::{pool as app_of_trees, tree};
    use crate::sidecar::Sidecar;
    use std::fs;
    use std::path::PathBuf;

    fn process(pid: u32, name: &str) -> Process {
        Process {
            pid,
            name: name.into(),
            ports: Vec::new(),
        }
    }

    /// A tap puts a port a tree serves on the internet, the Tree screen shows
    /// it public at once, and a second takes it down; a port no tree serves
    /// never reaches the command.
    #[test]
    fn a_port_a_tree_serves_goes_public_and_comes_down_again() {
        let mut app = app_of_trees();
        let tree17 = app.trees.iter_mut().find(|t| t.name == "17").unwrap();
        tree17.processes = vec![Process {
            pid: 42,
            name: "vite".into(),
            ports: vec![5173],
        }];
        let pool = Pool {
            name: "app".into(),
            checkout: PathBuf::from("/nowhere"),
            kick: Mutex::new(std::sync::mpsc::channel().0),
            push: None,
            seen: Mutex::default(),
            ready: Mutex::default(),
            away: Mutex::new(None),
            app: Mutex::new(app),
        };
        let sh = |script: &str| vec!["sh".to_string(), "-c".into(), script.into()];
        let public = |pool: &Pool| {
            let app = pool.app.lock().unwrap();
            let tree = app.trees.iter().find(|t| t.name == "17").unwrap();
            crate::serve::tree::tree_json(&app, tree, 0)["ports"][0]["public"].clone()
        };
        let body = |reply: &Reply| serde_json::from_slice::<Value>(&reply.body).unwrap();

        let reply = publish_port(
            &pool,
            &sh("echo 'Available on the internet:'; echo https://box.ts.net/"),
            5173,
        );
        assert_eq!(
            (reply.status, body(&reply)["url"].as_str()),
            (200, Some("https://box.ts.net/"))
        );
        assert_eq!(public(&pool), "https://box.ts.net/");

        let reply = unshare_port(&pool, &sh("exit 0"), 5173);
        assert_eq!(reply.status, 200);
        assert_eq!(public(&pool), Value::Null);

        let reply = publish_port(&pool, &sh("echo 'Funnel is not enabled' >&2; exit 1"), 5173);
        assert_eq!(reply.status, 502);
        assert_eq!(body(&reply)["error"], "sh failed: Funnel is not enabled");
        assert_eq!(public(&pool), Value::Null);

        let mark = std::env::temp_dir().join(format!("treetop-published-{}", std::process::id()));
        let _ = fs::remove_file(&mark);
        let touch = vec!["touch".to_string(), mark.display().to_string()];
        assert_eq!(publish_port(&pool, &touch, 7777).status, 404);
        assert_eq!(unshare_port(&pool, &touch, 7777).status, 404);
        assert!(!mark.exists(), "no command ran for a port no tree serves");
    }

    /// ANB-29. The source is one no config has, so that a pick that gets as
    /// far as the job stops there, having started nothing.
    #[test]
    fn an_issue_already_in_a_tree_answers_with_it_and_any_other_goes_to_the_job() {
        let mut app = app_of_trees();
        let mut record = Sidecar::leased_now();
        record.ticket = Some("END-1".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/pool/17"), record)]));
        let pool = Pool {
            name: "app".into(),
            checkout: PathBuf::from("/nowhere"),
            kick: Mutex::new(std::sync::mpsc::channel().0),
            push: None,
            seen: Mutex::default(),
            ready: Mutex::default(),
            away: Mutex::new(None),
            app: Mutex::new(app),
        };
        let ask = |id: &str, body: &[u8]| {
            let reply = pick(&pool, id, Some("no-such-source".into()), body);
            let json: Value = serde_json::from_slice(&reply.body).unwrap();
            (reply.status, json)
        };
        let (status, held) = ask("END-1", br#"{"base": null}"#);
        assert_eq!((status, held["tree"].as_str()), (200, Some("17")));
        let (status, free) = ask("END-2", br#"{"base": null}"#);
        assert_eq!(
            (status, free["error"].as_str()),
            (
                502,
                Some("ticket: no issue source no-such-source in the config")
            )
        );
        assert_eq!(ask("END-2", b"not json").0, 400);
        let base =
            |asked: Option<&str>| pick_launch(&pool, "END-2", None, asked.map(str::to_string)).base;
        assert_eq!(base(Some("release")).as_deref(), Some("release"));
        assert_eq!(base(Some("")), None, "no choice is the pool's own");
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
            answered_at: None,
            context: None,
            spend: None,
            label: None,
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

    /// A tree on disk as a pool worktree is, with its own git directory.
    fn tree_on_disk(test: &str) -> (PathBuf, Tree) {
        let root = std::env::temp_dir().join(format!("treetop-park-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let path = root.join("7/app");
        fs::create_dir_all(root.join("gitdir")).unwrap();
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(".git"),
            format!("gitdir: {}\n", root.join("gitdir").display()),
        )
        .unwrap();
        let tree = Tree {
            path,
            ..tree("7", "me/END-7-x")
        };
        (root, tree)
    }

    #[test]
    fn parking_keeps_what_the_record_says_now_and_undoes_itself() {
        let (root, tree) = tree_on_disk("keeps");
        // Written after the pool last read the tree.
        let on_disk = Sidecar {
            job: Some("written-since".into()),
            ..Sidecar::leased_now()
        };
        sidecar::write(&tree.path, &on_disk).unwrap();
        let parked = set_shelf(&tree, Shelf::Parked, true).unwrap();
        assert!(parked.parked);
        assert_eq!(parked.job.as_deref(), Some("written-since"));
        assert_eq!(sidecar::read(&tree.path), Some(parked));
        // Asked twice over, it stays what was asked.
        assert!(set_shelf(&tree, Shelf::Parked, true).unwrap().parked);
        assert!(!set_shelf(&tree, Shelf::Parked, false).unwrap().parked);
        assert!(!sidecar::read(&tree.path).unwrap().parked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_tree_sits_on_one_shelf_and_leaving_another_keeps_it_there() {
        let (root, tree) = tree_on_disk("shelves");
        let record = set_shelf(&tree, Shelf::Backburner, true).unwrap();
        assert_eq!(record.shelf(), Some(Shelf::Backburner));
        let record = set_shelf(&tree, Shelf::Parked, false).unwrap();
        assert_eq!(
            record.shelf(),
            Some(Shelf::Backburner),
            "not parked, so unparking leaves it"
        );
        let record = set_shelf(&tree, Shelf::Parked, true).unwrap();
        assert!(record.parked && !record.backburner);
        assert_eq!(
            sidecar::read(&tree.path).unwrap().shelf(),
            Some(Shelf::Parked)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_held_tree_treetop_has_no_record_of_can_still_be_parked() {
        let (root, tree) = tree_on_disk("unrecorded");
        assert!(set_shelf(&tree, Shelf::Parked, true).unwrap().parked);
        assert!(sidecar::read(&tree.path).unwrap().parked);
        let _ = fs::remove_dir_all(root);
    }
}
