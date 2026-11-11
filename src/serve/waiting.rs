//! What waits on you, for the phone's Waiting tab: the same as the
//! desktop's banner, built by `crate::inbox` from the workspace's `App`, so
//! the two never disagree, with what finished or merged under it.

use std::sync::PoisonError;
use std::time::SystemTime;

use serde_json::{Value, json};
use tiny_http::Method;

use super::{Pool, Reply};
use crate::app::App;
use crate::inbox::{self, Kind};

/// `GET /api/waiting`. The old inbox's calls, from a page opened before
/// this treetop, answer too: its list as this one, and its marks with
/// nothing done, so it fails quietly until it is opened again.
pub fn route(pool: &Pool, method: &Method, call: &str) -> Reply {
    match (method, call) {
        (Method::Get, "") => {
            let mut app = pool.app.lock().unwrap_or_else(PoisonError::into_inner);
            let is_away_read = pool.read_away(&mut app);
            let is_reading = !is_away_read || !pool.is_read(&app);
            Reply::json(
                200,
                &waiting_json(&app, &pool.name, SystemTime::now(), is_reading),
            )
        }
        (Method::Post, "seen" | "dismiss") => Reply::json(200, &json!({})),
        _ => Reply::error(404, "no such call"),
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::NeedsYou => "needs_you",
        Kind::Failed => "failed",
        Kind::Died => "died",
        Kind::Done => "done",
        Kind::Merged => "merged",
    }
}

/// What waits in every workspace, as the desktop's banner orders it, and
/// what has finished or merged, as workspace `here`'s phone draws them as
/// of `now`; `is_reading` while the pools are still being read, so the
/// phone does not take what is not read yet for nothing waiting. Each names
/// its own workspace, `here`'s too, so it opens there whichever workspace
/// the phone has moved to since.
pub fn waiting_json(app: &App, here: &str, now: SystemTime, is_reading: bool) -> Value {
    let workspace = |entry: &inbox::Entry| {
        entry
            .away
            .as_ref()
            .map_or_else(|| here.to_string(), |a| a.name.clone())
    };
    let entries: Vec<Value> = inbox::waiting(app)
        .into_iter()
        .map(|entry| {
            json!({
                "key": format!("{}:{}", kind_name(entry.kind), entry.path.display()),
                "kind": kind_name(entry.kind),
                "workspace": workspace(&entry),
                "tree": entry.tree,
                "title": entry.title,
                // The desktop's names the key that resumes it, which the
                // phone does not have.
                "what": if entry.kind == Kind::Died { "its agent died".into() } else { entry.what },
                "ago": entry.at.and_then(|at| now.duration_since(at).ok()).map(|d| d.as_secs()),
            })
        })
        .collect();
    let also: Vec<Value> = inbox::entries(app)
        .into_iter()
        .filter(|e| matches!(e.kind, Kind::Done | Kind::Merged))
        .map(|entry| {
            json!({
                "kind": kind_name(entry.kind),
                "workspace": workspace(&entry),
                "tree": entry.tree,
                "title": entry.title,
            })
        })
        .collect();
    json!({ "workspace": here, "entries": entries, "also": also, "is_reading": is_reading })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::pool::tests::{declared, keys};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn the_phone_waits_on_what_the_desktop_does_in_its_order_with_the_rest_below() {
        let app = crate::inbox::tests::app();
        let json = waiting_json(&app, "app", UNIX_EPOCH + Duration::from_secs(1_000), false);
        assert_eq!(keys(&json), declared("Waiting"));
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(keys(&entries[0]), declared("WaitingEntry"));
        let trees: Vec<&str> = entries
            .iter()
            .map(|e| e["tree"].as_str().unwrap())
            .collect();
        let desktop: Vec<String> = inbox::waiting(&app).into_iter().map(|e| e.tree).collect();
        assert_eq!(trees, desktop);
        let also = json["also"].as_array().unwrap();
        assert_eq!(keys(&also[0]), declared("Finished"));
        assert_eq!(also[0]["tree"], "2");
        assert_eq!(also[0]["kind"], "done");
    }

    #[test]
    fn a_pool_still_being_read_is_not_taken_for_one_with_nothing_waiting() {
        let mut app = crate::inbox::tests::app();
        app.is_agents_read = false;
        let pool = Pool {
            name: "app".into(),
            checkout: std::path::PathBuf::from("/nowhere"),
            kick: std::sync::Mutex::new(std::sync::mpsc::channel().0),
            push: None,
            seen: std::sync::Mutex::default(),
            ready: std::sync::Mutex::default(),
            away: std::sync::Mutex::new(None),
            app: std::sync::Mutex::new(app),
        };
        let is_read = |pool: &Pool| pool.is_read(&pool.app.lock().unwrap());
        assert!(!is_read(&pool), "nothing read yet");
        pool.app.lock().unwrap().is_agents_read = true;
        assert!(!is_read(&pool), "agents read, reports not yet");
        pool.seen.lock().unwrap().has_reports = true;
        assert!(
            !is_read(&pool),
            "PRs not read yet, so a failed one would be missing"
        );
        pool.seen.lock().unwrap().has_ship = true;
        assert!(is_read(&pool));
    }

    #[test]
    fn every_entry_names_its_workspace_so_it_opens_there_from_any_other() {
        let app = crate::inbox::tests::app();
        let json = waiting_json(&app, "app", UNIX_EPOCH, false);
        assert_eq!(json["workspace"], "app");
        let named: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["workspace"].as_str().expect("named"))
            .collect();
        assert!(
            named.contains(&"app") && named.contains(&"endodeals-web"),
            "{named:?}"
        );
    }
}
