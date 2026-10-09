//! The last `treehouse status --json` per repository, so treetop draws the pool
//! the moment it opens instead of after a listing that takes seconds. It is a
//! convenience: an unreadable or unparsable cache is ignored, never an error.

use std::fs;
use std::path::{Path, PathBuf};

use crate::pool::{self, Tree};

/// `$XDG_CACHE_HOME/treetop`, else `~/.cache/treetop`.
fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".cache")))?;
    Some(base.join("treetop"))
}

/// FNV-1a: a hash that stays the same across Rust releases, unlike std's,
/// so a cache file keeps its name after treetop is rebuilt.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn file(dir: &Path, checkout: &Path) -> PathBuf {
    dir.join(format!(
        "{:016x}.json",
        fnv1a(checkout.as_os_str().as_encoded_bytes())
    ))
}

fn read_from(dir: &Path, checkout: &Path) -> Option<Vec<Tree>> {
    pool::parse(&fs::read_to_string(file(dir, checkout)).ok()?).ok()
}

/// Written through a temporary file and a rename, so a reader never sees half
/// a listing.
fn write_to(dir: &Path, checkout: &Path, raw: &str) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    let target = file(dir, checkout);
    let partial = target.with_extension("json.partial");
    fs::write(&partial, raw)?;
    fs::rename(partial, target)
}

pub fn read(checkout: &Path) -> Option<Vec<Tree>> {
    read_from(&dir()?, checkout)
}

pub fn write(checkout: &Path, raw: &str) {
    if let Some(dir) = dir() {
        let _ = write_to(&dir, checkout, raw);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATUS: &str = r#"[{"name":"4","status":"leased","branch":"b","lease_holder":"me",
        "path":"/p/4/app","leased_at":null,"processes":[]}]"#;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("treetop-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn reads_back_what_it_wrote_per_checkout() {
        let dir = scratch("roundtrip");
        write_to(&dir, Path::new("/repo/a"), STATUS).unwrap();
        assert_eq!(read_from(&dir, Path::new("/repo/a")).unwrap()[0].name, "4");
        assert!(read_from(&dir, Path::new("/repo/b")).is_none());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn ignores_a_corrupt_cache() {
        let dir = scratch("corrupt");
        write_to(&dir, Path::new("/repo/a"), "{not json").unwrap();
        assert!(read_from(&dir, Path::new("/repo/a")).is_none());
        fs::remove_dir_all(dir).unwrap();
    }
}
