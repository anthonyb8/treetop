//! Scratch chats: Claude Code sessions for thinking out loud, tied to no
//! tree. They all run in one folder under treetop's state directory, outside
//! every repository, so they never land on a tree, never open as an ask, and
//! are the same list in every workspace. One folder means Claude Code's trust
//! prompt is accepted once for all of them. Each keeps a record beside the
//! others, `<n>.json`; `C` lists them, starts a new one and deletes one.
//!
//! ```json
//! { "job": "0908e7f3", "name": "scratch 3", "created": 1792227120 }
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::agents::claude;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scratch {
    /// The Claude Code session it runs as, which `claude attach` opens.
    pub job: String,
    pub name: String,
    /// When it was started, in seconds since the epoch.
    pub created: u64,
    /// A planning chat's workspace: the checkout it plans for, which it can
    /// read and runs `keel` from. None for a scratch chat, and for a record
    /// written before planning chats, which shows in every workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
    /// A scratch chat `C` started, for no workspace: never on Plan. A
    /// record without it, written before planning chats, can be either.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scratch: bool,
    /// The Linear issues a planning chat has created, as its transcript
    /// shows `keel linear create` saying, in the order it made them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
    /// Its record's file; not written, since it is where the record is.
    #[serde(skip)]
    pub record: PathBuf,
}

impl Scratch {
    /// The command that opens its chat, starting it again if it stopped.
    pub fn attach(&self) -> Vec<String> {
        vec!["claude".into(), "attach".into(), self.job.clone()]
    }
}

/// `$XDG_STATE_HOME/treetop/scratch`, else `~/.local/state/treetop/scratch`:
/// where every scratch chat runs and keeps its record.
pub fn root() -> Option<PathBuf> {
    Some(crate::state::dir()?.join("scratch"))
}

/// Every scratch chat whose record can be read, the newest first.
pub fn list() -> Vec<Scratch> {
    root().map(|root| list_in(&root)).unwrap_or_default()
}

fn list_in(root: &Path) -> Vec<Scratch> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut chats: Vec<Scratch> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .filter_map(|record| {
            let text = fs::read_to_string(&record).ok()?;
            let chat: Scratch = serde_json::from_str(&text).ok()?;
            Some(Scratch { record, ..chat })
        })
        .collect();
    chats.sort_by(|a, b| b.created.cmp(&a.created).then(b.job.cmp(&a.job)));
    chats
}

/// The number of the next chat under `root`: one past the highest record.
fn next_number(root: &Path) -> u32 {
    fs::read_dir(root)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| e.path().file_stem()?.to_str()?.parse::<u32>().ok())
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0)
        + 1
}

/// The planning chats of the workspace at `checkout`, the newest first:
/// those started for it, and those whose record names no workspace.
pub fn planning(checkout: &Path) -> Vec<Scratch> {
    root()
        .map(|root| planning_in(&root, checkout))
        .unwrap_or_default()
}

/// The jobs of every planning chat, in any workspace.
pub fn planning_jobs() -> std::collections::HashSet<String> {
    list()
        .into_iter()
        .filter(|chat| chat.checkout.is_some())
        .map(|chat| chat.job)
        .collect()
}

fn planning_in(root: &Path, checkout: &Path) -> Vec<Scratch> {
    list_in(root)
        .into_iter()
        .filter(|chat| match &chat.checkout {
            Some(c) => c == checkout,
            None => !chat.scratch,
        })
        .collect()
}

/// Starts a new scratch chat, idle, and waits until Claude Code lists it,
/// or with `checkout` a planning chat for that workspace, which can read it.
/// Slow: it runs off the screen's thread.
pub fn start(checkout: Option<&Path>) -> Result<Scratch, String> {
    let root = root().ok_or("no home directory for scratch chats")?;
    fs::create_dir_all(&root).map_err(|err| format!("{}: {err}", root.display()))?;
    let number = next_number(&root);
    let name = match checkout {
        Some(_) => format!("plan {number}"),
        None => format!("scratch {number}"),
    };
    let mut command = Command::new("claude");
    command.args(["--bg", "--name", &name]);
    if let Some(checkout) = checkout {
        command.arg("--add-dir").arg(checkout);
    }
    let out = command
        .current_dir(&root)
        .output()
        .map_err(|err| format!("claude: {err}"))?;
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let said = if said.is_empty() {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        } else {
            said
        };
        // Claude Code runs a background session only in a folder whose
        // trust prompt was accepted, which only you can do.
        if said.contains("not trusted") {
            return Err(format!(
                "Claude Code does not trust {} yet: run `claude` there once and accept, then C again",
                root.display()
            ));
        }
        return Err(format!("claude: {said}"));
    }
    let out = String::from_utf8_lossy(&out.stdout);
    let job = claude::attach_id(&out)
        .ok_or("claude printed no session id")?
        .to_string();
    claude::confirm(&job).map_err(|err| err.message)?;
    let chat = Scratch {
        job,
        name,
        created: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
        record: root.join(format!("{number}.json")),
        checkout: checkout.map(Path::to_path_buf),
        scratch: checkout.is_none(),
        issues: Vec::new(),
    };
    save(&chat)?;
    Ok(chat)
}

/// Writes the chat's record again, as it now stands.
pub fn save(chat: &Scratch) -> Result<(), String> {
    let text = serde_json::to_string(chat).map_err(|err| err.to_string())?;
    fs::write(&chat.record, text).map_err(|err| format!("{}: {err}", chat.record.display()))
}

/// Deletes a scratch chat: its session, with `claude rm`, then its record.
pub fn remove(chat: &Scratch) -> Result<(), String> {
    let out = Command::new("claude")
        .args(["rm", &chat.job])
        .output()
        .map_err(|err| format!("claude: {err}"))?;
    // A session Claude Code no longer knows is as good as removed.
    if !out.status.success() {
        let said = String::from_utf8_lossy(&out.stderr);
        if !said.to_lowercase().contains("not found") {
            return Err(format!("claude rm: {}", said.trim()));
        }
    }
    fs::remove_file(&chat.record).map_err(|err| format!("{}: {err}", chat.record.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_lists_its_workspaces_chats_and_old_ones_but_not_scratch_chats() {
        let root = std::env::temp_dir().join(format!("treetop-planning-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let records = [
            ("1", r#""checkout":"/code/here""#),
            ("2", r#""checkout":"/code/there""#),
            ("3", ""),
            ("4", r#""scratch":true"#),
        ];
        for (n, extra) in records {
            let sep = if extra.is_empty() { "" } else { "," };
            let text = format!(r#"{{"job":"j{n}","name":"chat {n}","created":{n}{sep}{extra}}}"#);
            fs::write(root.join(format!("{n}.json")), text).unwrap();
        }
        let jobs: Vec<String> = planning_in(&root, Path::new("/code/here"))
            .into_iter()
            .map(|c| c.job)
            .collect();
        assert_eq!(
            jobs,
            ["j3", "j1"],
            "here's, and the old one that names none"
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn lists_the_readable_chats_newest_first_and_numbers_the_next() {
        let root = std::env::temp_dir().join(format!("treetop-scratch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (number, job, created) in [("1", "aaaa1111", 10), ("2", "bbbb2222", 20)] {
            let text =
                format!(r#"{{"job":"{job}","name":"scratch {number}","created":{created}}}"#);
            fs::write(root.join(format!("{number}.json")), text).unwrap();
        }
        fs::write(root.join("3.json"), "not json").unwrap();
        fs::write(root.join("notes.txt"), "kept by a chat").unwrap();
        let chats = list_in(&root);
        let jobs: Vec<&str> = chats.iter().map(|c| c.job.as_str()).collect();
        assert_eq!(
            jobs,
            ["bbbb2222", "aaaa1111"],
            "an unreadable record is left out"
        );
        assert_eq!(chats[0].record, root.join("2.json"));
        assert_eq!(chats[0].attach(), ["claude", "attach", "bbbb2222"]);
        assert_eq!(next_number(&root), 4, "past every record, readable or not");
        fs::remove_dir_all(&root).unwrap();
    }
}
