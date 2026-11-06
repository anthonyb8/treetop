//! The inbox for the phone: the same entries, order and marks as the one
//! down the right of the list, built by `crate::inbox` from the workspace's
//! `App`, so the two never disagree. Opening or dismissing an entry here
//! writes the one marks file the desktop keeps too.

use std::path::Path;
use std::sync::PoisonError;
use std::time::SystemTime;

use serde_json::{Value, json};
use tiny_http::Method;

use super::{Asked, Pool, Reply};
use crate::app::App;
use crate::inbox::{self, Kind};

/// `GET /api/inbox`, and `POST /api/inbox/seen` or `/api/inbox/dismiss`
/// with `{"key": "..."}`.
pub fn route(pool: &Pool, method: &Method, call: &str, asked: &Asked) -> Reply {
    let Some(file) = inbox::file() else {
        return Reply::error(500, "treetop has no state directory to keep the inbox in");
    };
    match (method, call) {
        (Method::Get, "") => {
            let mut app = pool.app.lock().unwrap_or_else(PoisonError::into_inner);
            let is_away_read = pool.read_away(&mut app);
            let is_reading = !is_away_read || !pool.is_read(&app);
            // The desktop, or another phone, may have opened or dismissed
            // entries since the last ask.
            app.keep_inbox_in(file);
            if let Some(asked) = crate::state::asked_file(&pool.checkout) {
                app.keep_asked_in(asked);
            }
            Reply::json(
                200,
                &inbox_json(&app, &pool.name, SystemTime::now(), is_reading),
            )
        }
        (Method::Post, "seen" | "dismiss") => {
            #[derive(serde::Deserialize)]
            struct Marked {
                key: String,
            }
            let Ok(marked) = serde_json::from_slice::<Marked>(asked.body) else {
                return Reply::error(400, "takes {\"key\": \"...\"}");
            };
            mark(&file, call == "dismiss", marked.key);
            Reply::json(200, &json!({}))
        }
        _ => Reply::error(404, "no such call"),
    }
}

/// Marks the entry `key` seen, or dismissed, in `file`.
fn mark(file: &Path, is_dismissed: bool, key: String) {
    inbox::change(file, |marks| {
        if is_dismissed {
            marks.dismissed.insert(key);
        } else {
            marks.seen.insert(key);
        }
    });
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

/// The inbox of workspace `here` as the phone draws it, in the desktop's
/// order, as of `now`; `is_reading` while the pools are still being read,
/// so the phone does not take what is not read yet for nothing waiting.
/// Each entry names its own workspace, `here`'s too, so it opens there
/// whichever workspace the phone has moved to since.
pub fn inbox_json(app: &App, here: &str, now: SystemTime, is_reading: bool) -> Value {
    let shown = inbox::shown(app);
    let unseen = shown.iter().filter(|(_, seen)| !seen).count();
    let entries: Vec<Value> = shown
        .into_iter()
        .map(|(entry, seen)| {
            json!({
                "key": entry.key,
                "kind": kind_name(entry.kind),
                "workspace": entry.away.map_or_else(|| here.to_string(), |a| a.name),
                "tree": entry.tree,
                "title": entry.title,
                // The desktop's names the key that resumes it, which the
                // phone does not have.
                "what": if entry.kind == Kind::Died { "its agent died".into() } else { entry.what },
                "ago": entry.at.and_then(|at| now.duration_since(at).ok()).map(|d| d.as_secs()),
                "seen": seen,
            })
        })
        .collect();
    json!({ "workspace": here, "entries": entries, "unseen": unseen, "is_reading": is_reading })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::Marks;
    use crate::serve::pool::tests::{declared, keys};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn the_phone_s_inbox_is_the_desktop_s_in_its_order_with_its_marks() {
        let mut app = crate::inbox::tests::app();
        let desktop: Vec<(String, bool)> = inbox::shown(&app)
            .into_iter()
            .map(|(e, seen)| (e.key, seen))
            .collect();
        let first = desktop[0].0.clone();
        app.inbox.seen.insert(first.clone());

        let json = inbox_json(&app, "app", UNIX_EPOCH + Duration::from_secs(1_000), false);
        assert_eq!(keys(&json), declared("Inbox"));
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(keys(&entries[0]), declared("InboxEntry"));
        let mut phone: Vec<(String, bool)> = entries
            .iter()
            .map(|e| {
                (
                    e["key"].as_str().unwrap().into(),
                    e["seen"].as_bool().unwrap(),
                )
            })
            .collect();
        phone.sort();
        let mut expected: Vec<(String, bool)> = inbox::shown(&app)
            .into_iter()
            .map(|(e, seen)| (e.key, seen))
            .collect();
        expected.sort();
        assert_eq!(phone, expected);
        assert_eq!(
            entries.last().unwrap()["key"],
            first.as_str(),
            "seen goes last"
        );
        assert_eq!(json["unseen"], desktop.len() - 1);
    }

    #[test]
    fn opening_and_dismissing_on_the_phone_land_in_the_desktop_s_marks() {
        let file =
            std::env::temp_dir().join(format!("treetop-serve-inbox-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&file);
        mark(&file, false, "opened".into());
        mark(&file, true, "gone".into());
        let mut app = crate::inbox::tests::app();
        app.keep_inbox_in(file.clone());
        assert!(app.inbox.seen.contains("opened"));
        assert!(app.inbox.dismissed.contains("gone"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn swipes_at_the_same_moment_all_stay_dismissed() {
        let file = std::env::temp_dir().join(format!(
            "treetop-serve-inbox-race-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&file);
        let swipes: Vec<_> = (0..40)
            .map(|n| {
                let file = file.clone();
                std::thread::spawn(move || mark(&file, true, format!("swipe {n}")))
            })
            .collect();
        for swipe in swipes {
            swipe.join().unwrap();
        }
        let kept: Marks = crate::state::read(&file);
        assert_eq!(kept.dismissed.len(), 40, "{kept:?}");
        let _ = std::fs::remove_file(&file);
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
        assert!(is_read(&pool));
    }

    #[test]
    fn every_entry_names_its_workspace_so_it_opens_there_from_any_other() {
        let app = crate::inbox::tests::app();
        let json = inbox_json(&app, "app", UNIX_EPOCH, false);
        assert_eq!(json["workspace"], "app");
        let named: Vec<&str> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["workspace"].as_str().expect("named"))
            .collect();
        assert!(named.contains(&"app"), "{named:?}");
    }
}
