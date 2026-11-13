//! What treetop keeps between runs that is not a cache: scratch chats, and
//! per workspace, the asks you have already seen, so switching workspaces or
//! starting treetop again never opens one a second time. A file that cannot
//! be read counts as empty, and one that cannot be written is skipped: losing
//! it only means an ask may open once more.

use std::collections::HashMap;
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

/// The file the double of the workspace checked out at `checkout` is kept
/// in: the second window's tree and tab, or none for one window.
pub fn layout_file(checkout: &Path) -> Option<PathBuf> {
    Some(dir()?.join(format!("layout-{}.json", crate::cache::key(checkout))))
}

/// The file what the notifications put away is kept in, for every
/// workspace, since one lists them all.
pub fn put_away_file() -> Option<PathBuf> {
    Some(dir()?.join("put-away.json"))
}

/// What `file` holds, else the empty value, such as the seen asks.
pub fn read<T: serde::de::DeserializeOwned + Default>(file: &Path) -> T {
    fs::read_to_string(file)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// Writes `value` to `file` through a temporary file and a rename, so a reader
/// never sees half of it.
pub fn write<T: serde::Serialize>(file: &Path, value: &T) {
    let write = || -> std::io::Result<()> {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        let partial = file.with_extension("json.partial");
        fs::write(&partial, serde_json::to_string(value)?)?;
        fs::rename(partial, file)
    };
    let _ = write();
}

/// The titles agents were given after they started, by job, since Claude
/// Code's own name for a session cannot change once it runs.
pub fn titles() -> HashMap<String, String> {
    dir()
        .and_then(|dir| fs::read_to_string(dir.join("titles.json")).ok())
        .and_then(|raw| serde_json::from_str::<Vec<(String, String)>>(&raw).ok())
        .map(|titles| titles.into_iter().collect())
        .unwrap_or_default()
}

/// Records `title` for agent `job`, keeping the newest titles only, so the
/// file stays small.
pub fn add_title(job: &str, title: &str) {
    const KEPT: usize = 500;
    let Some(file) = dir().map(|dir| dir.join("titles.json")) else {
        return;
    };
    let mut titles: Vec<(String, String)> = fs::read_to_string(&file)
        .ok()
        .and_then(|raw| serde_json::from_str::<Vec<(String, String)>>(&raw).ok())
        .unwrap_or_default();
    titles.retain(|(j, _)| j != job);
    titles.push((job.to_string(), title.to_string()));
    let skip = titles.len().saturating_sub(KEPT);
    let titles = &titles[skip..];
    let write = || -> std::io::Result<()> {
        fs::create_dir_all(file.parent().unwrap_or(Path::new(".")))?;
        let partial = file.with_extension("json.partial");
        fs::write(&partial, serde_json::to_string(titles)?)?;
        fs::rename(partial, &file)
    };
    let _ = write();
}
