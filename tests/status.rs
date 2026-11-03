//! `treetop status` prints the snapshot an open treetop or `treetop serve`
//! keeps while it is fresh, without reading the pool, which takes seconds;
//! a stale one is never printed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// The name the cache files of the workspace at `checkout` take, as treetop
/// derives it: FNV-1a of the path, which stays the same across releases.
fn key(checkout: &Path) -> String {
    let hash = checkout
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{hash:016x}")
}

/// A repository, a cache directory, and a `treehouse` on PATH that fails,
/// so a status that reads the pool says so.
fn workspace() -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("treetop-status-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let (repo, cache, bin) = (root.join("app"), root.join("cache"), root.join("bin"));
    for dir in [&repo, &cache, &bin] {
        fs::create_dir_all(dir).unwrap();
    }
    assert!(
        Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    let treehouse = bin.join("treehouse");
    fs::write(
        &treehouse,
        "#!/bin/sh\necho 'treehouse was asked' >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(
        &treehouse,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    (fs::canonicalize(repo).unwrap(), cache, bin)
}

fn status(repo: &Path, cache: &Path, bin: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_treetop"))
        .arg("status")
        .current_dir(repo)
        .env("XDG_CACHE_HOME", cache)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .output()
        .unwrap()
}

#[test]
fn status_prints_a_fresh_snapshot_and_reads_the_pool_past_a_stale_one() {
    let (repo, cache, bin) = workspace();
    let snapshot = cache
        .join("treetop")
        .join(format!("{}.status.json", key(&repo)));
    fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
    fs::write(&snapshot, "{\"trees\": [], \"available_trees\": 3}\n").unwrap();

    let fresh = status(&repo, &cache, &bin);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&fresh.stdout),
        "{\"trees\": [], \"available_trees\": 3}\n"
    );

    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&snapshot)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let stale = status(&repo, &cache, &bin);
    assert!(!stale.status.success(), "a stale snapshot is not printed");
    assert!(String::from_utf8_lossy(&stale.stderr).contains("treehouse was asked"));
}
