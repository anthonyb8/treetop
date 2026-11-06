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
            pool.read_away(&mut app);
            // The desktop, or another phone, may have opened or dismissed
            // entries since the last ask.
            app.keep_inbox_in(file);
            if let Some(asked) = crate::state::asked_file(&pool.checkout) {
                app.keep_asked_in(asked);
            }
            Reply::json(200, &inbox_json(&app, SystemTime::now()))
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

/// The inbox as the phone draws it, in the desktop's order, as of `now`.
pub fn inbox_json(app: &App, now: SystemTime) -> Value {
    let shown = inbox::shown(app);
    let unseen = shown.iter().filter(|(_, seen)| !seen).count();
    let entries: Vec<Value> = shown
        .into_iter()
        .map(|(entry, seen)| {
            json!({
                "key": entry.key,
                "kind": kind_name(entry.kind),
                "workspace": entry.away.map(|a| a.name),
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
    json!({ "entries": entries, "unseen": unseen })
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

        let json = inbox_json(&app, UNIX_EPOCH + Duration::from_secs(1_000));
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
}
