//! The notifications dismissed, kept per workspace so they stay dismissed
//! when treetop opens again. One file per workspace, written through a
//! temporary file and a rename, so a read never sees half of it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cache;

/// How many dismissed notifications are remembered: plenty for a day's.
const KEPT: usize = 500;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// The notifications dismissed, by key, the latest last.
    #[serde(default)]
    pub dismissed: Vec<String>,
}

/// `$XDG_STATE_HOME/treetop`, else `~/.local/state/treetop`.
fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))?;
    Some(base.join("treetop"))
}

/// The state file of the workspace checked out at `checkout`.
pub fn path(checkout: &Path) -> Option<PathBuf> {
    Some(dir()?.join(format!("dismissed-{}.json", cache::key(checkout))))
}

/// The state at `path`, or none yet; an unreadable file reads as none.
pub fn read_from(path: &Path) -> State {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_to(path: &Path, state: &State) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("json.partial");
    fs::write(&partial, serde_json::to_string_pretty(state)?)?;
    fs::rename(partial, path)
}

/// Reads the state at `path`, changes it with `change`, and writes it back.
pub fn update(path: &Path, change: impl FnOnce(&mut State)) -> io::Result<()> {
    let mut state = read_from(path);
    change(&mut state);
    write_to(path, &state)
}

impl State {
    /// Remembers that notification `key` was dismissed.
    pub fn dismiss(&mut self, key: &str) {
        if !self.dismissed.iter().any(|k| k == key) {
            self.dismissed.push(key.to_string());
        }
        let over = self.dismissed.len().saturating_sub(KEPT);
        self.dismissed.drain(..over);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dismissals_survive_a_write_and_a_read() {
        let dir = std::env::temp_dir().join(format!("treetop-dismissed-{}", std::process::id()));
        let path = dir.join("dismissed.json");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(read_from(&path), State::default(), "none yet");
        update(&path, |s| {
            s.dismiss("job@1");
            s.dismiss("job@1");
        })
        .unwrap();
        assert_eq!(read_from(&path).dismissed, ["job@1"], "once each");
        let _ = fs::remove_dir_all(&dir);
    }
}
