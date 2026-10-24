//! The built-in Claude Code plugin, run as `treetop plugin claude` the way
//! treetop calls it, against a Claude Code config directory laid out as
//! Claude Code writes it and a stand-in `claude` on PATH that records what it
//! was asked.

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};

/// A fresh scratch directory for one test.
fn scratch(test: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("treetop-agents-{}-{test}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("claude/jobs")).unwrap();
    fs::create_dir_all(root.join("claude/sessions")).unwrap();
    fs::create_dir_all(root.join("bin")).unwrap();
    fs::create_dir_all(root.join("tree")).unwrap();
    // Stands in for `claude`: says where it ran and with what, and prints
    // the id `claude --bg` prints, which for `--resume` is the resumed job's
    // unless `new-job` names another.
    let fake = root.join("bin/claude");
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$(pwd) $*\" >> {calls}\n\
             if [ \"$1\" = agents ]; then echo '[{{\"id\":\"0000aaaa\"}}]'; exit 0; fi\n\
             id=0000aaaa\n\
             if [ \"$2\" = --resume ]; then id=$(echo \"$3\" | cut -c1-8); fi\n\
             if [ -f {new} ]; then id=$(cat {new}); fi\n\
             echo \"  claude attach $id    open in this terminal\"\n",
            calls = root.join("calls").display(),
            new = root.join("new-job").display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    root
}

/// A background job that finished its turn in `dir`, last written `age` ago,
/// with its conversation saved.
fn finished_job(root: &Path, job: &str, dir: &Path, age: Duration) {
    job_in(root, job, "done", dir, age);
    let projects = root.join("claude/projects/-tree");
    fs::create_dir_all(&projects).unwrap();
    fs::write(
        projects.join(format!("{job}-1111-2222-3333-444444444444.jsonl")),
        "{}\n",
    )
    .unwrap();
}

/// A background job in `state` in `dir`, last written `age` ago, with no
/// conversation saved, as `claude --bg --resume` leaves one it could not
/// resume.
fn job_in(root: &Path, job: &str, state: &str, dir: &Path, age: Duration) {
    let path = root.join("claude/jobs").join(job);
    fs::create_dir_all(&path).unwrap();
    let state = json!({
        "state": state,
        "detail": "fixed the login redirect",
        "name": format!("agent {job}"),
        "cwd": dir,
        "sessionId": format!("{job}-1111-2222-3333-444444444444"),
        "resumeSessionId": format!("{job}-1111-2222-3333-444444444444"),
    });
    fs::write(path.join("state.json"), state.to_string()).unwrap();
    fs::File::options()
        .write(true)
        .open(path.join("state.json"))
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

/// One request to `treetop plugin claude`, and its reply.
fn call(root: &Path, op: &str, args: Value) -> Value {
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_treetop"))
        .args(["plugin", "claude"])
        .current_dir(std::env::temp_dir())
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("PATH", path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = json!({ "protocol": 1, "op": op, "config": {}, "args": args });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["ok"], true, "{reply}");
    reply["result"].clone()
}

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

#[test]
fn a_finished_agent_is_still_found_days_after_it_last_spoke() {
    let root = scratch("aged");
    let tree = root.join("tree");
    finished_job(&root, "0aaaaaaa", &tree, 2 * DAY);
    let found = call(&root, "find", json!({ "dirs": [tree] }));
    let agents = found["agents"].as_array().unwrap();
    assert_eq!(agents.len(), 1, "{found}");
    assert_eq!(agents[0]["id"], "0aaaaaaa");
    assert_eq!(agents[0]["live"], false);
    assert_eq!(agents[0]["dir"], json!(tree));
}

/// What the stand-in `claude` was asked, one line per run: where it ran,
/// then its arguments.
fn calls(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn pins(root: &Path) -> Vec<String> {
    serde_json::from_str(&fs::read_to_string(root.join("claude/jobs/pins.json")).unwrap()).unwrap()
}

/// What the stand-in `claude` was asked, without where it ran.
fn asked(root: &Path) -> Vec<String> {
    calls(root)
        .iter()
        .map(|c| c.split_once(' ').unwrap().1.to_string())
        .collect()
}

/// A running background session for `job`, standing in this test's own
/// process, which is alive.
fn running(root: &Path, job: &str, dir: &Path) {
    let pid = std::process::id();
    let session = json!({
        "pid": pid,
        "sessionId": format!("{job}-1111-2222-3333-444444444444"),
        "cwd": dir,
        "kind": "bg",
        "status": "idle",
        "jobId": job,
    });
    fs::write(
        root.join("claude/sessions").join(format!("{pid}.json")),
        session.to_string(),
    )
    .unwrap();
}

#[test]
fn keeping_an_agent_pins_it_and_wakes_it_where_it_worked_if_cold() {
    let root = scratch("keep");
    let tree = root.join("tree");
    finished_job(&root, "0ccccccc", &tree, 3 * DAY);
    finished_job(&root, "0bbbbbbb", &tree, Duration::ZERO);
    running(&root, "0bbbbbbb", &tree);
    fs::write(root.join("claude/jobs/pins.json"), r#"["0fffffff"]"#).unwrap();

    let kept = call(&root, "keep", json!({ "ids": ["0ccccccc", "0bbbbbbb"] }));

    assert_eq!(
        kept["woken"],
        json!(["0ccccccc"]),
        "only the cold one wakes"
    );
    assert_eq!(pins(&root), ["0fffffff", "0ccccccc", "0bbbbbbb"]);
    assert_eq!(
        calls(&root),
        [format!(
            "{} --bg --resume 0ccccccc-1111-2222-3333-444444444444",
            tree.display()
        )],
        "woken in its tree, with no new message"
    );
    assert!(
        !root.join("claude/jobs/pins.json.lock").exists(),
        "the lock is let go"
    );
}

#[test]
fn stopping_a_cold_agent_unpins_it_without_starting_it() {
    let root = scratch("stop");
    let tree = root.join("tree");
    finished_job(&root, "0ccccccc", &tree, DAY);
    fs::write(
        root.join("claude/jobs/pins.json"),
        r#"["0fffffff", "0ccccccc"]"#,
    )
    .unwrap();

    call(&root, "stop", json!({ "id": "0ccccccc" }));

    assert_eq!(pins(&root), ["0fffffff"]);
    assert!(calls(&root).is_empty(), "{:?}", calls(&root));
}

#[test]
fn removing_an_agent_deletes_it_so_nothing_finds_it_again() {
    let root = scratch("remove");
    let tree = root.join("tree");
    finished_job(&root, "0ddddddd", &tree, DAY);
    running(&root, "0eeeeeee", &tree);
    fs::write(
        root.join("claude/jobs/pins.json"),
        r#"["0ddddddd", "0eeeeeee", "0fffffff"]"#,
    )
    .unwrap();

    let described = call(&root, "describe", json!({}));
    assert!(
        described["ops"]
            .as_array()
            .unwrap()
            .contains(&json!("remove")),
        "{described}"
    );
    call(&root, "remove", json!({ "id": "0ddddddd" }));
    call(&root, "remove", json!({ "id": "0eeeeeee" }));

    assert_eq!(pins(&root), ["0fffffff"], "no longer kept awake");
    let asked: Vec<String> = calls(&root)
        .iter()
        .map(|c| c.split_once(' ').unwrap().1.to_string())
        .collect();
    assert_eq!(asked, ["rm 0ddddddd", "rm 0eeeeeee"], "cold or running");
}

#[test]
fn an_agent_started_in_a_tree_of_its_own_is_told_to_stay_in_it() {
    let root = scratch("isolated");
    let tree = root.join("tree");
    let started = call(
        &root,
        "start",
        json!({ "dir": tree, "name": "fix login", "prompt": "fix it", "isolated": true }),
    );
    assert_eq!(started["id"], "0000aaaa");
    let start = &calls(&root)[0];
    assert!(
        start.starts_with(&format!(
            "{} --bg --name fix login --append-system-prompt You are already isolated: {} is",
            tree.display(),
            tree.display()
        )),
        "{start}"
    );
    assert!(start.contains("Do not call EnterWorktree"), "{start}");
    assert!(start.ends_with("-- fix it"), "{start}");

    let root = scratch("in-place");
    call(
        &root,
        "start",
        json!({ "dir": root.join("tree"), "prompt": "fix it", "isolated": false }),
    );
    assert!(
        !calls(&root)[0].contains("--append-system-prompt"),
        "an agent in the main checkout may take a tree of its own"
    );
}

#[test]
fn a_cold_agent_with_no_conversation_is_neither_woken_nor_pinned() {
    let root = scratch("unresumable");
    let tree = root.join("tree");
    // A job `claude --bg --resume` started in place of one it could not
    // resume: waking it would start another such job, and so on.
    job_in(&root, "0aaaaaaa", "done", &tree, DAY);
    fs::write(root.join("claude/jobs/pins.json"), "[]").unwrap();

    let kept = call(&root, "keep", json!({ "ids": ["0aaaaaaa"] }));

    assert_eq!(kept["woken"], json!([]));
    assert!(calls(&root).is_empty(), "{:?}", calls(&root));
    assert!(pins(&root).is_empty(), "{:?}", pins(&root));
}

#[test]
fn a_wake_that_comes_up_as_another_job_removes_that_job() {
    let root = scratch("respawned");
    let tree = root.join("tree");
    finished_job(&root, "0ccccccc", &tree, DAY);
    fs::write(root.join("new-job"), "0ddddddd").unwrap();

    let kept = call(&root, "keep", json!({ "ids": ["0ccccccc"] }));

    assert_eq!(kept["woken"], json!([]), "0ccccccc is still cold");
    assert_eq!(
        asked(&root),
        [
            "--bg --resume 0ccccccc-1111-2222-3333-444444444444",
            "rm 0ddddddd"
        ]
    );
}

#[test]
fn clearing_a_tree_removes_every_job_that_worked_in_it_failed_ones_too() {
    let root = scratch("clear");
    let tree = root.join("tree");
    let other = root.join("other");
    finished_job(&root, "0aaaaaaa", &tree, DAY);
    job_in(&root, "0bbbbbbb", "failed", &tree, DAY);
    job_in(&root, "0ccccccc", "failed", &tree.join("sub"), DAY);
    job_in(&root, "0ddddddd", "failed", &other, DAY);
    fs::write(
        root.join("claude/jobs/pins.json"),
        r#"["0aaaaaaa", "0bbbbbbb", "0ddddddd"]"#,
    )
    .unwrap();

    let described = call(&root, "describe", json!({}));
    assert!(
        described["ops"]
            .as_array()
            .unwrap()
            .contains(&json!("clear")),
        "{described}"
    );
    let cleared = call(&root, "clear", json!({ "dir": tree }));

    assert_eq!(
        cleared["removed"],
        json!(["0aaaaaaa", "0bbbbbbb", "0ccccccc"])
    );
    assert_eq!(asked(&root), ["rm 0aaaaaaa", "rm 0bbbbbbb", "rm 0ccccccc"]);
    assert_eq!(pins(&root), ["0ddddddd"], "another tree's job stays");
}
