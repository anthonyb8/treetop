//! The repositories treetop has been opened in, each a workspace `w` can
//! switch to. treetop adds the checkout it opens on every start, so the list
//! fills as it is used; it never scans treehouse's own directories, keeping
//! to treehouse's CLI as the only source of pool state. Like the listing
//! cache, an unreadable list is treated as empty, never as an error.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cache;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The repository's main checkout.
    pub checkout: PathBuf,
    /// When treetop last opened it, in seconds since the epoch.
    pub opened: u64,
}

impl Workspace {
    /// The repository's name, as the picker and the summary badge say it.
    pub fn name(&self) -> String {
        name(&self.checkout)
    }
}

/// The name of the repository checked out at `checkout`: its directory's.
pub fn name(checkout: &Path) -> String {
    checkout
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| checkout.display().to_string())
}

fn file(dir: &Path) -> PathBuf {
    dir.join("workspaces.json")
}

fn read_from(dir: &Path) -> Vec<Workspace> {
    fs::read_to_string(file(dir))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Written through a temporary file and a rename, so two treetops never
/// leave half a list.
fn write_to(dir: &Path, list: &[Workspace]) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let target = file(dir);
    let partial = target.with_extension("json.partial");
    fs::write(&partial, serde_json::to_string_pretty(list)?)?;
    fs::rename(partial, target)
}

/// `list` with `checkout` opened `at`: moved to the front if it was there.
fn with_opened(mut list: Vec<Workspace>, checkout: &Path, at: u64) -> Vec<Workspace> {
    list.retain(|w| w.checkout != checkout);
    list.insert(
        0,
        Workspace {
            checkout: checkout.to_path_buf(),
            opened: at,
        },
    );
    list
}

/// Every workspace whose checkout is still there, most recently opened first.
fn existing(mut list: Vec<Workspace>) -> Vec<Workspace> {
    list.retain(|w| w.checkout.is_dir());
    list.sort_by_key(|w| std::cmp::Reverse(w.opened));
    list
}

/// Notes that treetop opened `checkout` now.
pub fn record(checkout: &Path) {
    let Some(dir) = cache::dir() else { return };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let _ = write_to(&dir, &with_opened(read_from(&dir), checkout, now));
}

/// Every workspace still on disk, most recently opened first.
pub fn list() -> Vec<Workspace> {
    cache::dir()
        .map(|dir| existing(read_from(&dir)))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("treetop-workspaces-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn opening_moves_a_workspace_to_the_front_once() {
        let list = with_opened(Vec::new(), Path::new("/repo/app"), 1);
        let list = with_opened(list, Path::new("/repo/harness"), 2);
        let list = with_opened(list, Path::new("/repo/app"), 3);
        let order: Vec<(String, u64)> = list.iter().map(|w| (w.name(), w.opened)).collect();
        assert_eq!(order, [("app".into(), 3), ("harness".into(), 2)]);
    }

    #[test]
    fn reads_back_what_it_wrote_newest_first_and_drops_what_is_gone() {
        let dir = scratch("roundtrip");
        let here = dir.join("here");
        fs::create_dir_all(&here).unwrap();
        let list = vec![
            Workspace {
                checkout: dir.join("gone"),
                opened: 9,
            },
            Workspace {
                checkout: here.clone(),
                opened: 5,
            },
        ];
        write_to(&dir, &list).unwrap();
        assert_eq!(read_from(&dir), list);
        let shown = existing(read_from(&dir));
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].checkout, here);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_corrupt_list_reads_as_empty() {
        let dir = scratch("corrupt");
        fs::create_dir_all(&dir).unwrap();
        fs::write(file(&dir), "{not json").unwrap();
        assert!(read_from(&dir).is_empty());
        fs::remove_dir_all(dir).unwrap();
    }
}
