//! `treetop plugins` lists what a user dropped into the plugin directory,
//! from any directory, and says why it cannot use one rather than failing.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A config directory holding one working plugin, one that speaks a newer
/// protocol, one that crashes, and a file that is not executable.
fn config_dir() -> PathBuf {
    let config = std::env::temp_dir().join(format!("treetop-plugins-{}", std::process::id()));
    let _ = fs::remove_dir_all(&config);
    let plugins = config.join("treetop").join("plugins");
    fs::create_dir_all(&plugins).unwrap();
    let plugin = |name: &str, body: &str| {
        let path = plugins.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    plugin(
        "my-agent",
        r#"echo '{"ok":true,"result":{"protocol":1,"name":"my-agent","kind":"agent","ops":["start","find"]}}'"#,
    );
    plugin(
        "next-tickets",
        r#"echo '{"ok":true,"result":{"protocol":9,"name":"next","kind":"tickets"}}'"#,
    );
    plugin("broken", "echo 'missing LINEAR_API_KEY' >&2; exit 1");
    fs::write(plugins.join("README"), "notes").unwrap();
    config
}

fn plugins(config: &Path) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_treetop"))
        .arg("plugins")
        .current_dir(std::env::temp_dir())
        .env("XDG_CONFIG_HOME", config)
        .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn lists_each_plugin_and_why_one_cannot_be_used() {
    let listing = plugins(&config_dir());
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines.len(), 7, "{listing}");
    assert!(
        lines[0].contains("broken: failed: missing LINEAR_API_KEY"),
        "{listing}"
    );
    assert!(lines[1].starts_with("my-agent"), "{listing}");
    assert!(
        lines[1].contains("agent") && lines[1].ends_with("(start, find)"),
        "{listing}"
    );
    assert!(
        lines[2].contains("unsupported: speaks protocol 9, treetop speaks 1"),
        "{listing}"
    );
    assert!(
        lines[3].starts_with("claude") && lines[3].ends_with("built in (treetop plugin claude)"),
        "{listing}"
    );
    assert!(
        lines[4].starts_with("linear") && lines[4].contains("tickets"),
        "{listing}"
    );
    assert!(
        lines[5].starts_with("github") && lines[5].contains("ship"),
        "{listing}"
    );
    assert!(
        lines[6].starts_with("no-mistakes") && lines[6].contains("ship"),
        "{listing}"
    );
}

#[test]
fn says_where_it_looked_when_there_are_none_and_lists_the_built_ins() {
    let empty = std::env::temp_dir().join(format!("treetop-no-plugins-{}", std::process::id()));
    let listing = plugins(&empty);
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(
        lines[0],
        format!("no plugins in {}", empty.join("treetop/plugins").display())
    );
    assert!(lines[1].starts_with("claude"), "{listing}");
}

#[test]
fn a_built_in_answers_through_the_contract_on_stdin() {
    use std::io::Write;
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_treetop"))
        .args(["plugin", "claude"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"protocol":1,"op":"describe","config":{},"args":{}}"#)
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["result"]["kind"], "agent");
    assert_eq!(
        reply["result"]["ops"],
        serde_json::json!([
            "start", "find", "attach", "send", "stop", "remove", "keep", "clear"
        ])
    );
}
