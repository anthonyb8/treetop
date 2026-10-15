//! `--help` and `--version` answer from any directory, the way a user checks
//! what is installed, inside a repository or not.

use std::process::Command;

/// A directory in no git repository.
fn outside_any_repo() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("treetop-anywhere-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn treetop(arg: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_treetop"))
        .arg(arg)
        .current_dir(outside_any_repo())
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap()
}

#[test]
fn version_answers_outside_a_repository() {
    let out = treetop("--version");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("treetop {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_answers_outside_a_repository() {
    let out = treetop("--help");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("treetop status"));
}
