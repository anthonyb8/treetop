//! `treetop return`, against a real treehouse pool, the way an agent in one
//! tree reaches for another: a tree is given back only by whoever holds it,
//! or once its work is finished, and never from under a live process.
//!
//! Needs `treehouse` on PATH. Its config and Claude Code's are pointed at
//! scratch directories, so no hook runs and no real agent is touched.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

use serde_json::Value;

/// A repository with one commit on `main`, pushed to a bare origin, whose
/// pool lives beside it.
struct Pool {
    root: PathBuf,
    repo: PathBuf,
}

impl Pool {
    fn new(test: &str) -> Pool {
        let root =
            std::env::temp_dir().join(format!("treetop-guard-{}-{test}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repo = root.join("repo");
        for dir in [&repo, &root.join("home/.config"), &root.join("claude")] {
            fs::create_dir_all(dir).unwrap();
        }
        let pool = Pool { root, repo };
        fs::write(pool.repo.join("treehouse.toml"), "base_branch = \"main\"\n").unwrap();
        pool.git(&pool.repo, &["init", "-q", "-b", "main"]);
        pool.git(&pool.repo, &["add", "."]);
        pool.commit(&pool.repo, "first");
        let origin = pool.root.join("origin.git");
        pool.git(
            &pool.repo,
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        pool.git(
            &pool.repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        pool.git(&pool.repo, &["push", "-q", "-u", "origin", "main"]);
        pool
    }

    /// `program` run in `dir` with this pool's environment.
    fn command(&self, program: &str, dir: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(dir)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("home/.config"))
            .env("XDG_STATE_HOME", self.root.join("home/.state"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("claude"))
            .env("TREEHOUSE_ROOT", self.root.join("pool"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t");
        command
    }

    fn run(&self, program: &str, dir: &Path, args: &[&str]) -> Output {
        self.command(program, dir).args(args).output().unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let out = self.run("git", dir, args);
        assert!(out.status.success(), "git {args:?}: {}", text(&out.stderr));
    }

    fn commit(&self, dir: &Path, message: &str) {
        self.git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    }

    /// Leases a tree for `holder`, as a session's `EnterWorktree` does.
    fn lease(&self, holder: &str) -> PathBuf {
        let out = self.run(
            "treehouse",
            &self.repo,
            &["get", "--lease", "--lease-holder", holder],
        );
        assert!(out.status.success(), "lease: {}", text(&out.stderr));
        PathBuf::from(text(&out.stdout).trim())
    }

    /// The tree at `path`, as `treehouse status --json` lists it.
    fn entry(&self, path: &Path) -> Value {
        let out = self.run("treehouse", &self.repo, &["status", "--json"]);
        assert!(out.status.success(), "status: {}", text(&out.stderr));
        let trees: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap();
        trees
            .into_iter()
            .find(|t| t["path"].as_str() == path.to_str())
            .unwrap_or_else(|| panic!("{} is not in the pool", path.display()))
    }

    fn name(&self, path: &Path) -> String {
        self.entry(path)["name"].as_str().unwrap().to_string()
    }

    fn is_leased(&self, path: &Path) -> bool {
        self.entry(path)["status"] != "available"
    }

    /// What the guard's hook answers Claude Code for a session standing in
    /// `cwd` that is about to run `command`: a refusal's reason, or None.
    fn hook(&self, cwd: &Path, command: &str) -> Option<String> {
        let mut child = self
            .command(env!("CARGO_BIN_EXE_treetop"), cwd)
            .args(["hook", "pre-tool-use"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let event = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
            "cwd": cwd,
        });
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{}", text(&out.stderr));
        if out.stdout.is_empty() {
            return None;
        }
        let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
        let output = &answer["hookSpecificOutput"];
        assert_eq!(output["permissionDecision"], "deny", "{answer}");
        Some(
            output["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .to_string(),
        )
    }

    /// `treetop return <name>` run from `dir`.
    fn give_back(&self, dir: &Path, name: &str) -> Output {
        self.run(env!("CARGO_BIN_EXE_treetop"), dir, &["return", name])
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A process standing in `dir`, as a tree's own session does.
fn session_in(dir: &Path) -> Child {
    Command::new("sleep")
        .arg("300")
        .current_dir(dir)
        .spawn()
        .unwrap()
}

#[test]
fn a_session_cannot_return_a_tree_another_session_is_working_in() {
    let pool = Pool::new("foreign");
    let theirs = pool.lease("claude:feature/theirs");
    let mine = pool.lease("claude:feature/mine");
    pool.git(&theirs, &["switch", "-q", "-c", "feature/theirs"]);
    pool.commit(&theirs, "work never pushed");
    let mut session = session_in(&theirs);
    let name = pool.name(&theirs);

    let out = pool.give_back(&mine, &name);

    assert!(!out.status.success(), "{}", text(&out.stdout));
    let why = text(&out.stderr);
    assert!(
        why.contains(&session.id().to_string()),
        "names the live pid: {why}"
    );
    assert!(pool.is_leased(&theirs), "the tree is still theirs");
    assert!(
        session.try_wait().unwrap().is_none(),
        "their session runs on"
    );
    assert_eq!(
        pool.run("git", &theirs, &["branch", "--show-current"])
            .stdout,
        b"feature/theirs\n",
        "their branch is untouched"
    );

    // With the session gone, the tree is still not the caller's, and its
    // work is not finished.
    session.kill().unwrap();
    session.wait().unwrap();
    let out = pool.give_back(&mine, &name);
    assert!(!out.status.success(), "{}", text(&out.stdout));
    let why = text(&out.stderr);
    assert!(why.contains("1 unpushed"), "{why}");
    assert!(pool.is_leased(&theirs));

    // Whoever stands in it may give it back.
    let out = pool.give_back(&theirs, &name);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!pool.is_leased(&theirs));
}

#[test]
fn a_finished_tree_nobody_runs_in_may_be_returned_from_anywhere() {
    let pool = Pool::new("finished");
    let done = pool.lease("claude:done");
    let name = pool.name(&done);

    let out = pool.give_back(&pool.repo, &name);

    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(!pool.is_leased(&done));
}

/// What happened to tree 28: the agent of another tree stopped its stack,
/// then returned it under the holder name it read off the pool, while the
/// tree's own session ran on.
#[test]
fn an_agent_is_kept_from_another_trees_stack_and_lease_by_the_hook() {
    let pool = Pool::new("hook");
    fs::create_dir_all(pool.root.join("home/.config/treetop")).unwrap();
    fs::write(
        pool.root.join("home/.config/treetop/config.toml"),
        "[guard]\nin_tree = [\"devstack down\"]\n",
    )
    .unwrap();
    let theirs = pool.lease("claude:feature/theirs");
    let mine = pool.lease("claude:feature/mine");
    let mut session = session_in(&theirs);
    let (there, pid) = (theirs.display(), session.id());

    let why = pool
        .hook(
            &mine,
            &format!(
                "cd {there} && devstack down && treehouse return {there} \
                 --if-lease-holder \"claude:feature/theirs\" --force"
            ),
        )
        .expect("refused");
    assert!(why.contains("no `devstack down`"), "{why}");
    let why = pool
        .hook(
            &mine,
            &format!("treehouse return {there} --if-lease-holder claude:feature/theirs"),
        )
        .expect("refused");
    assert!(why.contains(&pid.to_string()), "names the live pid: {why}");
    let why = pool.hook(&mine, "treehouse return --all").expect("refused");
    assert!(why.contains("every tree in the pool"), "{why}");

    assert_eq!(pool.hook(&mine, "devstack down"), None, "its own tree");
    assert_eq!(pool.hook(&mine, "ls && treehouse status"), None);
    assert!(pool.is_leased(&theirs));
    session.kill().unwrap();
    session.wait().unwrap();
}

#[test]
fn guard_says_what_this_caller_may_do_to_a_tree() {
    let pool = Pool::new("check");
    let theirs = pool.lease("claude:feature/theirs");
    let mine = pool.lease("claude:feature/mine");
    pool.git(&theirs, &["switch", "-q", "-c", "feature/theirs"]);
    let name = pool.name(&theirs);
    let guard = |dir: &Path, args: &[&str]| {
        let mut all = vec!["guard", name.as_str()];
        all.extend(args);
        pool.run(env!("CARGO_BIN_EXE_treetop"), dir, &all)
    };

    let out = guard(&mine, &[]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("has no merged PR"),
        "{}",
        text(&out.stderr)
    );
    assert!(!guard(&mine, &["--in-tree"]).status.success());
    assert!(guard(&theirs, &["--in-tree"]).status.success());
    assert!(guard(&theirs, &[]).status.success());
    assert!(pool.is_leased(&theirs), "a check changes nothing");
}
