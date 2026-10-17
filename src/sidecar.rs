//! `treetop.json`, treetop's record of a tree it leased: which agent it
//! started there, under which job, for which ticket, on which branch, and
//! when. It sits in the tree's own git directory, as `agent.json` does, so it
//! never shows in `git status`. treetop writes it at dispatch and removes it
//! on return, so a tree that has been returned and handed out again never
//! carries the last agent's record.
//!
//! ```json
//! { "version": 1, "agent": "claude", "job": "0908e7f3", "ticket": "END-123",
//!   "branch": "anthony/fix-login", "lease_id": null, "leased_at": 1792227120 }
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::pool;

/// The format this treetop writes. A file from a newer one is left alone.
pub const VERSION: u32 = 1;

pub const FILE: &str = "treetop.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sidecar {
    pub version: u32,
    /// The agent plugin that started the tree's agent.
    pub agent: Option<String>,
    /// The agent's id, as its plugin knows it.
    pub job: Option<String>,
    pub ticket: Option<String>,
    pub branch: Option<String>,
    /// treehouse's identity for the lease, when it gave one.
    pub lease_id: Option<String>,
    /// When treetop leased the tree, in seconds since the epoch.
    pub leased_at: u64,
}

impl Sidecar {
    /// A record of a lease taken now.
    pub fn leased_now() -> Self {
        Sidecar {
            version: VERSION,
            agent: None,
            job: None,
            ticket: None,
            branch: None,
            lease_id: None,
            leased_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
        }
    }
}

fn file(tree: &Path) -> Option<PathBuf> {
    pool::git_dir(tree).map(|dir| dir.join(FILE))
}

/// The tree's record, if treetop leased it and can read what it wrote.
pub fn read(tree: &Path) -> Option<Sidecar> {
    let text = fs::read_to_string(file(tree)?).ok()?;
    let sidecar: Sidecar = serde_json::from_str(&text).ok()?;
    (sidecar.version <= VERSION).then_some(sidecar)
}

/// Written through a temporary file and a rename, so a reader never sees
/// half a record.
pub fn write(tree: &Path, sidecar: &Sidecar) -> io::Result<()> {
    let target = file(tree).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} has no git directory", tree.display()),
        )
    })?;
    let partial = target.with_extension("json.partial");
    fs::write(&partial, serde_json::to_string_pretty(sidecar)?)?;
    fs::rename(partial, target)
}

/// Removes the tree's record; one that is already gone is no error.
pub fn remove(tree: &Path) -> io::Result<()> {
    let Some(target) = file(tree) else {
        return Ok(());
    };
    match fs::remove_file(target) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree laid out as a pool worktree is: a `.git` file pointing at its
    /// own git directory elsewhere.
    fn tree(test: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("treetop-sidecar-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let (tree, git_dir) = (root.join("7/app"), root.join("main/.git/worktrees/app7"));
        fs::create_dir_all(&tree).unwrap();
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", git_dir.display()),
        )
        .unwrap();
        tree
    }

    #[test]
    fn a_record_lives_in_the_git_dir_and_goes_on_return() {
        let tree = tree("round-trip");
        assert_eq!(read(&tree), None);
        let sidecar = Sidecar {
            agent: Some("claude".into()),
            ticket: Some("END-123".into()),
            ..Sidecar::leased_now()
        };
        write(&tree, &sidecar).unwrap();
        assert!(!tree.join(FILE).exists());
        assert_eq!(read(&tree), Some(sidecar));
        remove(&tree).unwrap();
        assert_eq!(read(&tree), None);
        remove(&tree).unwrap();
    }

    #[test]
    fn a_record_from_a_newer_treetop_or_a_broken_one_is_not_read() {
        let tree = tree("unreadable");
        let path = file(&tree).unwrap();
        fs::write(&path, r#"{"version":2,"leased_at":1}"#).unwrap();
        assert_eq!(read(&tree), None);
        fs::write(&path, "{").unwrap();
        assert_eq!(read(&tree), None);
        fs::write(&path, r#"{"version":1,"leased_at":1,"extra":true}"#).unwrap();
        assert_eq!(read(&tree).map(|s| s.leased_at), Some(1));
    }
}
