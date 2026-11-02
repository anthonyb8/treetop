//! What treetop keeps between runs that is not a cache: scratch chats, and
//! per workspace, the asks you have already seen, so switching workspaces or
//! starting treetop again never opens one a second time. A file that cannot
//! be read counts as empty, and one that cannot be written is skipped: losing
//! it only means an ask may open once more.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// `$XDG_STATE_HOME/treetop`, else `~/.local/state/treetop`.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))?;
    Some(base.join("treetop"))
}

/// The file the seen asks of the workspace checked out at `checkout` are kept
/// in.
pub fn asked_file(checkout: &Path) -> Option<PathBuf> {
    Some(dir()?.join(format!("asked-{}.json", crate::cache::key(checkout))))
}

pub fn read_asked(file: &Path) -> BTreeSet<String> {
    fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Written through a temporary file and a rename, so a reader never sees half
/// of it.
pub fn write_asked(file: &Path, asked: &BTreeSet<String>) {
    let write = || -> std::io::Result<()> {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        let partial = file.with_extension("json.partial");
        fs::write(&partial, serde_json::to_string(asked)?)?;
        fs::rename(partial, file)
    };
    let _ = write();
}
