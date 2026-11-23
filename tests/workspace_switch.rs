//! A shell opened in a tree keeps running while `w` is on another workspace,
//! the way a user leaves a repository for another and comes back: treetop
//! runs in a terminal of its own against two pooled repositories, a
//! stand-in `treehouse` that lists each one's tree, and a stand-in `$SHELL`
//! that writes down its process id.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const ROWS: u16 = 50;
const COLS: u16 = 200;

/// Writes a stand-in tool, put in place whole: treetop may be running the
/// one it replaces.
fn write_script(path: &Path, body: &str) {
    let new = path.with_extension("new");
    fs::write(&new, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&new, fs::Permissions::from_mode(0o755)).unwrap();
    fs::rename(&new, path).unwrap();
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository at `root/<name>` with tree 1 of its pool leased at
/// `root/<name>-trees/1`, as `treehouse status --json` lists it.
fn repository(root: &Path, name: &str) -> (PathBuf, String) {
    let checkout = root.join(name);
    fs::create_dir_all(&checkout).unwrap();
    git(&checkout, &["init", "-q", "-b", "main"]);
    git(&checkout, &["commit", "-q", "--allow-empty", "-m", "start"]);
    let tree = root.join(format!("{name}-trees/1"));
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("{name}-work"),
            tree.to_str().unwrap(),
        ],
    );
    let status = serde_json::json!([{
        "name": "1",
        "status": "leased",
        "branch": format!("{name}-work"),
        "lease_holder": "me",
        "path": tree,
        "leased_at": null,
        "processes": [],
    }]);
    (checkout, status.to_string())
}

/// The name treetop's cache gives a checkout's listing: its path's FNV-1a.
fn cache_key(checkout: &Path) -> String {
    let hash = checkout
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
        });
    format!("{hash:016x}")
}

/// treetop in a pseudo-terminal, its screen parsed as it draws.
struct Screen {
    child: Child,
    input: File,
    parser: Arc<Mutex<vt100::Parser>>,
}

impl Screen {
    fn start(command: &mut Command) -> Screen {
        let (mut master, mut slave) = (0, 0);
        let size = libc::winsize {
            ws_row: ROWS,
            ws_col: COLS,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty writes two fresh descriptors into the out-pointers.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        };
        assert_eq!(opened, 0, "openpty");
        // SAFETY: both are fresh descriptors nobody else owns.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        let slave = File::from(slave);
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        let master = File::from(master);
        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let mut reader = master.try_clone().unwrap();
        let screen = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    return;
                }
                screen.lock().unwrap().process(&buffer[..read]);
            }
        });
        Screen {
            child,
            input: master,
            parser,
        }
    }

    fn text(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn press(&mut self, keys: &str) {
        self.input.write_all(keys.as_bytes()).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Waits until `is_there` holds of the screen, failing with what it shows.
    fn wait_for(&self, what: &str, is_there: impl Fn(&str) -> bool) {
        let started = Instant::now();
        loop {
            let text = self.text();
            if is_there(&text) {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "waited for {what}:\n{text}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Goes to the workspace `to`, the other of the two, as `w` then Enter does.
    fn switch_to(&mut self, to: &str) {
        self.press("w");
        self.wait_for("the workspaces", |text| text.contains(" Workspaces "));
        self.press("\r");
        self.wait_on(to);
    }

    /// Waits until the bottom bar's badge names `workspace`.
    fn wait_on(&self, workspace: &str) {
        let badge = format!("treetop · {workspace}");
        self.wait_for(&badge, |text| {
            text.lines().last().is_some_and(|bar| bar.contains(&badge))
        });
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn is_running(pid: i32) -> bool {
    // SAFETY: signal 0 only asks whether the process is there.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The process ids the stand-in shell wrote down, one per start.
fn shells(file: &Path) -> Vec<i32> {
    fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

/// Two pooled repositories, `here` and `there`, each with tree 1 leased,
/// both workspaces with `here` opened last, and treetop started on `here`
/// in a terminal of its own, against stand-ins for everything it reads,
/// with `config` as its config file.
struct Workspaces {
    root: PathBuf,
    /// Where the stand-in shell writes down each one it starts.
    started: PathBuf,
    screen: Screen,
}

fn two_workspaces(name: &str, config: &str) -> Workspaces {
    two_workspaces_with(name, config, |_| {})
}

/// `two_workspaces`, once `prepare` has had its root, before treetop starts:
/// a pool changed after that races treetop's own first listing of it.
fn two_workspaces_with(name: &str, config: &str, prepare: impl FnOnce(&Path)) -> Workspaces {
    let root = std::env::temp_dir().join(format!("treetop-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let bin = root.join("bin");
    let cache = root.join("cache/treetop");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let (here, here_status) = repository(&root, "here");
    let (there, there_status) = repository(&root, "there");
    // Each pool as an earlier run's listing left it, so the other
    // workspace is known before it is opened.
    for (checkout, status) in [(&here, &here_status), (&there, &there_status)] {
        fs::write(cache.join(format!("{}.json", cache_key(checkout))), status).unwrap();
    }

    write_script(
        &bin.join("treehouse"),
        &format!(
            "case \"$(git rev-parse --path-format=absolute --git-common-dir)\" in\n\
             {here}/*) echo '{here_status}' ;;\n\
             {there}/*) echo '{there_status}' ;;\n\
             *) exit 1 ;;\nesac\n",
            here = here.display(),
            there = there.display(),
        ),
    );
    // What treetop reads besides the pool finds nothing, rather than the
    // machine's own agents, PRs and runs.
    for tool in ["claude", "gh", "keel", "tether", "tmux", "no-mistakes"] {
        write_script(&bin.join(tool), "exit 1\n");
    }
    let started = root.join("shells");
    let shell = bin.join("shell");
    write_script(
        &shell,
        &format!("echo $$ >> {}\nexec sleep 600\n", started.display()),
    );
    // Both are workspaces, here opened last.
    // Opened this past minute, so each is recent enough to be read.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    fs::write(
        cache.join("workspaces.json"),
        serde_json::json!([
            { "checkout": here, "opened": now },
            { "checkout": there, "opened": now - 1 },
        ])
        .to_string(),
    )
    .unwrap();

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    fs::create_dir_all(root.join("config/treetop")).unwrap();
    fs::write(root.join("config/treetop/config.toml"), config).unwrap();
    prepare(&root);
    let screen = Screen::start(
        Command::new(env!("CARGO_BIN_EXE_treetop"))
            .current_dir(root.join("here-trees/1"))
            .env_clear()
            .env("PATH", path)
            .env("HOME", root.join("home"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_STATE_HOME", root.join("state"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("TERM", "xterm-256color")
            .env("SHELL", &shell),
    );
    Workspaces {
        root,
        started,
        screen,
    }
}

#[test]
fn a_shell_left_for_another_workspace_is_the_same_shell_on_coming_back() {
    let Workspaces {
        root,
        started,
        mut screen,
        ..
    } = two_workspaces("switch", "layout = \"table\"\n");
    screen.wait_on("here");
    screen.wait_for("tree 1's window", |text| {
        text.contains("Tree 1 · here-work")
    });

    // The window on tree 1 steps to its shell tab, which starts its shell.
    let opened = Instant::now();
    while shells(&started).is_empty() {
        assert!(
            opened.elapsed() < Duration::from_secs(20),
            "{}",
            screen.text()
        );
        screen.press("]");
        std::thread::sleep(Duration::from_millis(700));
    }
    let [first] = shells(&started)[..] else {
        panic!("one shell: {:?}", shells(&started));
    };

    // `w`, then the other workspace in the list, and back.
    screen.press("w");
    screen.press("j\r");
    screen.wait_on("there");
    screen.wait_for("there's tree 1", |text| {
        text.contains("Tree 1 · there-work")
    });
    assert!(is_running(first), "the shell ran on while away");
    screen.press("w");
    screen.press("j\r");
    screen.wait_on("here");
    screen.wait_for("tree 1's window", |text| {
        text.contains("Tree 1 · here-work")
    });

    // Round every tab once, the shell's among them, which shows the shell
    // left running or starts another in its place.
    for _ in 0..6 {
        screen.press("]");
        std::thread::sleep(Duration::from_millis(700));
    }
    assert!(is_running(first), "the shell runs on after coming back");
    assert_eq!(shells(&started), [first], "no shell started in its place");

    // Quitting treetop ends it.
    screen.press("\x03");
    let quit = Instant::now();
    while is_running(first) {
        assert!(
            quit.elapsed() < Duration::from_secs(10),
            "{first} still runs"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// The process ids the stand-in agent runs as; killed when the test ends.
struct Asker(Child);

impl Drop for Asker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// ANB-101: an agent that starts asking in another workspace shows in
/// treetop as quickly as one in the open workspace, rather than at that
/// workspace's next read of its PRs, minutes later.
#[test]
fn an_agent_asking_in_another_workspace_shows_within_seconds() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("asking", "layout = \"table\"\n");
    screen.wait_on("here");
    screen.wait_for("tree 1's window", |text| {
        text.contains("Tree 1 · here-work")
    });
    // There is read once it has been opened, which lists its pool: over
    // with `w`, and back.
    screen.press("w");
    screen.press("j\r");
    screen.wait_on("there");
    screen.press("w");
    screen.press("j\r");
    screen.wait_on("here");
    screen.wait_for("tree 1's window", |text| {
        text.contains("Tree 1 · here-work")
    });
    // Its first read is done, its agent not yet asking.
    std::thread::sleep(Duration::from_secs(1));
    assert!(!screen.text().contains("needs you"), "{}", screen.text());

    // An agent in there's tree 1 starts asking, by the open `agent.json`
    // convention any agent can follow.
    let asker = Asker(Command::new("sleep").arg("600").spawn().unwrap());
    let git_dir = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .current_dir(root.join("there-trees/1"))
        .output()
        .unwrap();
    let git_dir = PathBuf::from(String::from_utf8(git_dir.stdout).unwrap().trim());
    fs::write(
        git_dir.join("agent.json"),
        serde_json::json!({ "name": "asker", "pid": asker.0.id(), "status": "waiting" })
            .to_string(),
    )
    .unwrap();

    let asked = Instant::now();
    while !screen.text().contains("1 needs you") {
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "there's ask did not show within 5 s:\n{}",
            screen.text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(asker);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-102, ANB-108: with no `layout` in the config, treetop opens on its
/// numbered panels with the tree under the cursor in the window, and the
/// number keys move the keys between them, the bar saying which has them.
#[test]
fn with_no_layout_set_treetop_opens_on_numbered_panels_and_number_keys_move_the_keys() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("panels", "");
    screen.wait_for("the numbered panels and the window", |text| {
        [
            "[2] Trees",
            "[3] Linear",
            "[4] Keel",
            "[1] Tree 1 · here-work",
        ]
        .iter()
        .all(|title| text.contains(title))
    });
    let bar = |text: &str| text.lines().last().unwrap_or_default().to_string();
    // Waited for too: the bar is the last row drawn.
    screen.wait_for("panel 2 with the keys", |text| {
        bar(text).starts_with(" PANEL 2 ")
    });
    screen.press("3");
    screen.wait_for("panel 3 with the keys", |text| {
        bar(text).starts_with(" PANEL 3 ")
    });
    // The window's tree has no agent, so its info tab has the keys.
    screen.press("]");
    screen.press("1");
    screen.wait_for("the window with the keys", |text| {
        bar(text).starts_with(" NORMAL ")
    });
    screen.press("2");
    screen.wait_for("panel 2 with the keys", |text| {
        bar(text).starts_with(" PANEL 2 ") && bar(text).contains("j/k move")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-113: Plan's steps follow what was typed to the chat, its record keeps
/// where it is, and with no chats the window explains them.
#[test]
fn plans_steps_follow_the_chat_and_are_kept_with_it() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("plan-steps", "");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.press("\t");
    screen.wait_for("the steps explained", |text| {
        text.contains("No planning chats for here yet.")
            && text.contains("4 Pick one up            p gives one a fresh tree and an agent")
    });
    screen.press("\t\t");
    screen.wait_for("Trees", |text| text.contains("[2] Trees"));
    let here = root.join("here").canonicalize().unwrap();
    let records = root.join("state/treetop/scratch");
    fs::create_dir_all(&records).unwrap();
    let record = records.join("1.json");
    fs::write(
        &record,
        serde_json::json!({"job": "j1", "name": "plan 1", "created": 1, "checkout": here})
            .to_string(),
    )
    .unwrap();
    // A chat with an input box, emptied as each message is sent.
    write_script(
        &root.join("bin/claude"),
        "[ \"$1\" = attach ] || exit 1\n\
         box() { printf '\\033[2J\\033[H%s\\n❯ \\n\\n\\n\\n\\n%s\\n\\033[2;3H' ────── ──────; }\n\
         box\nwhile IFS= read -r line; do box; done\n",
    );
    screen.press("\t");
    screen.wait_for("the first step", |text| {
        text.contains("▸ 1 Talk it through ─ 2 Draft the issues")
    });
    for (key, step, kept) in [
        ("D", "✓ 1 Talk it through ─ ▸ 2 Draft the issues", "draft"),
        (
            "S",
            "✓ 2 Draft the issues ─ ▸ 3 Create them in Linear",
            "create",
        ),
    ] {
        screen.press(key);
        screen.wait_for(step, |text| text.contains(step));
        let record = fs::read_to_string(&record).unwrap();
        assert!(record.contains(&format!(r#""asked":"{kept}""#)), "{record}");
    }
    // Away and back reads its record again.
    screen.press("\t\t");
    screen.wait_for("Trees", |text| text.contains("[2] Trees"));
    screen.press("\t");
    screen.wait_for("the same step", |text| {
        text.contains("▸ 3 Create them in Linear")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-112: the Linear panel shows the issue of the tree under the cursor, and
/// the next tree's as the cursor moves, with no Enter; Enter reads it in the
/// window and Esc gives the keys back.
#[test]
fn the_linear_panel_follows_the_cursor_and_its_issue_is_read_in_the_window() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces_with("linear", "", |root| {
        add_tree(root, "here", "2", Some("me/anb-91-second"));
        add_tree(root, "here", "3", Some("here-third"));
        // Linear as a ticket plugin, which can be read and not changed.
        let plugins = root.join("config/treetop/plugins");
        fs::create_dir_all(&plugins).unwrap();
        write_script(
            &plugins.join("tix"),
            r#"read -r request
case "$request" in
*'"describe"'*) echo '{"ok":true,"result":{"protocol":1,"name":"tix","kind":"tickets","ops":["list","get"]}}' ;;
*'"ANB-90"'*) echo '{"ok":true,"result":{"id":"ANB-90","title":"Show the rail","state":"In Progress","project":"Inbox","labels":["Bug"],"description":"Draw it as a rail."}}' ;;
*'"ANB-91"'*) echo '{"ok":true,"result":{"id":"ANB-91","title":"Keep the rail","state":"Todo"}}' ;;
*) echo '{"ok":true,"result":{"tickets":[]}}' ;;
esac
"#,
        );
        let here = root.join("here").canonicalize().unwrap();
        fs::write(
            root.join("config/treetop/config.toml"),
            format!(
                "[issues.tix]\nplugin = \"tix\"\nteam = \"ANB\"\n[repos.\"{}\"]\nissues = \"tix\"\n",
                here.display()
            ),
        )
        .unwrap();
        // Tree 1 was picked up from an issue; tree 2's branch names one.
        let git_dir = Command::new("git")
            .args(["rev-parse", "--path-format=absolute", "--git-dir"])
            .current_dir(root.join("here-trees/1"))
            .output()
            .unwrap();
        let git_dir = PathBuf::from(String::from_utf8(git_dir.stdout).unwrap().trim());
        fs::write(
            git_dir.join("treetop.json"),
            serde_json::json!({ "version": 1, "ticket": "ANB-90", "source": "tix", "leased_at": 1 })
                .to_string(),
        )
        .unwrap();
    });
    let linear = |text: &str| -> String {
        let rows = text.lines().skip_while(|l| !l.contains("[3] Linear"));
        rows.take(7)
            .map(|l| l.chars().take(72).collect::<String>() + "\n")
            .collect()
    };
    let bar_is =
        |mode: &'static str| move |text: &str| text.lines().last().unwrap_or("").starts_with(mode);
    screen.wait_for("tree 1's issue", |text| {
        let rows = linear(text);
        rows.contains("[3] Linear · ANB-90 ")
            && rows.contains("Show the rail")
            && rows.contains("· bug")
            && rows.contains("In Progress")
            && rows.contains("Inbox")
    });
    // Back and forth, each at once: no key but the cursor's.
    for (tree, has) in [("2", "Keep the rail"), ("3", "issue    none")] {
        screen.press("j");
        screen.wait_for(tree, |text| {
            text.contains(&format!("[1] Tree {tree} ·")) && linear(text).contains(has)
        });
    }
    screen.press("kk");
    screen.press("3");
    screen.press("\r");
    screen.wait_for("it in the window, with the keys", |text| {
        text.contains("ANB-90  Show the rail")
            && text.contains("Draw it as a rail.")
            && bar_is(" NORMAL ")(text)
    });
    screen.press("l");
    screen.wait_for("its project", |text| {
        text.contains("this source's plugin cannot read projects")
    });
    screen.press("\x1b");
    screen.wait_for("the keys back on [3]", bar_is(" PANEL 3 "));
    screen.press("s");
    screen.wait_for("why not", |text| {
        text.lines()
            .last()
            .is_some_and(|bar| bar.contains("can list and read issues, not change them"))
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-111: the Keel panel shows the run of the tree under the cursor, and the
/// next tree's as the cursor moves, with no Enter; a tree's keys there act on
/// none, and Enter shows the run in the window.
#[test]
fn the_keel_panel_follows_the_cursor_from_tree_to_tree() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces_with("keel", "", |root| {
        add_tree(root, "here", "2", Some("here-second"));
        let steps: Vec<_> = (1..12)
            .map(|n| {
                let state = match n {
                    1..5 => "done",
                    5 => "doing",
                    _ => "pending",
                };
                serde_json::json!({ "title": format!("Step number {n}"), "state": state })
            })
            .collect();
        let runs = serde_json::json!([{ "version": 1, "tree": root.join("here-trees/1"),
            "playbook": "issue", "goal": "ANB-41", "state": "active", "steps": steps,
            "current": 5, "attention": 1, "stale": false }]);
        write_script(&root.join("bin/keel"), &format!("echo '{runs}'\n"));
    });
    let keel = |text: &str| -> String {
        let rows = text.lines().skip_while(|l| !l.contains("[4] Keel"));
        rows.map(|l| l.chars().take(72).collect::<String>() + "\n")
            .collect()
    };
    screen.wait_for("tree 1's run", |text| {
        let rows = keel(text);
        text.contains("[1] Tree 1 · here-work")
            && rows.contains("[4] Keel · tree 1 ")
            && rows.contains("playbook issue · step 5 of 11")
            && rows.contains("state    1 decision flagged")
            && rows.contains("✓ Step number 4")
            && rows.contains("▸ Step number 5")
    });
    screen.press("j");
    screen.wait_for("tree 2, with none", |text| {
        let rows = keel(text);
        rows.contains("[4] Keel · tree 2 ") && rows.contains("run      none")
    });
    screen.press("k");
    screen.press("4");
    // Enter is taken after the rest, so what they did shows by then.
    for key in ["r", "F", "x", "\r"] {
        screen.press(key);
    }
    screen.wait_for("the run in the window", |text| {
        text.contains("keel run · issue · ANB-41")
            && text.lines().last().unwrap_or("").starts_with(" NORMAL ")
    });
    let shown = screen.text();
    assert!(!shown.contains("backburner"), "x put it there:\n{shown}");
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-110: `w` lists the workspaces from the panels and from a window in
/// Normal, `/` narrows them, Enter switches, and the next key acts on the tree
/// under the cursor there.
#[test]
fn w_lists_the_workspaces_and_enter_switches_to_the_one_picked() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces_with("list", "", |root| {
        add_tree(root, "there", "2", None);
    });
    screen.wait_for("here's project in the title", |text| {
        text.contains("[2] Trees · here ─") && text.contains("[1] Tree 1 · here-work")
    });
    // From a window in Normal, and Esc closes it.
    screen.press("]");
    screen.press("1");
    screen.wait_for("the window with the keys", |text| {
        text.lines().last().unwrap_or("").starts_with(" NORMAL ")
    });
    screen.press("w");
    screen.wait_for("both workspaces", |text| {
        text.contains(" Workspaces ") && text.contains("1 held · 1 free")
    });
    screen.press("\x1b");
    screen.wait_for("it closed", |text| !text.contains(" Workspaces "));
    // From another panel, whose keys are not what the next workspace opens on.
    screen.press("4");
    screen.wait_for("Keel with the keys", |text| text.contains("PANEL 4"));
    screen.press("w");
    screen.wait_for("both again", |text| text.contains(" Workspaces "));
    screen.press("/the");
    screen.wait_for("there alone", |text| text.contains(" Workspaces /the "));
    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("there's tree in the window", |text| {
        text.contains("[2] Trees · there ─") && text.contains("[1] Tree 1 · there-work")
    });
    screen.press("r");
    screen.wait_for("the box to return there's tree", |text| {
        text.contains("there-work") && text.contains("Return")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-104: in the panels layout an agent asking in another workspace is
/// counted on the top row and listed by `e` with where it is, and Enter on
/// it switches there with its tree in the window.
#[test]
fn an_ask_in_another_workspace_is_listed_by_e_and_enter_goes_there() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("waiting", "layout = \"panels\"\n");
    screen.wait_on("here");
    screen.wait_for("here's tree in the window", |text| {
        text.contains("[1] Tree 1 · here-work")
    });
    let asker = Asker(Command::new("sleep").arg("600").spawn().unwrap());
    let git_dir = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .current_dir(root.join("there-trees/1"))
        .output()
        .unwrap();
    let git_dir = PathBuf::from(String::from_utf8(git_dir.stdout).unwrap().trim());
    fs::write(
        git_dir.join("agent.json"),
        serde_json::json!({ "name": "asker", "pid": asker.0.id(), "status": "waiting" })
            .to_string(),
    )
    .unwrap();
    screen.wait_for("there's ask on the top row", |text| {
        text.lines().next().is_some_and(|top| {
            top.split_whitespace()
                .eq(["Trees", "Plan", "Issues", "●", "1", "needs", "you"])
        })
    });
    screen.press("e");
    screen.wait_for("it in the list", |text| text.contains("● there 1"));
    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("there's tree in the window", |text| {
        text.contains("[1] Tree 1 · there-work")
    });
    drop(asker);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-106: in the panels layout Tab goes to the Plan screen, where `n`
/// starts a planning chat for this workspace in no tree, able to read the
/// repository; one Claude Code will not start says so on the bar, on Plan.
#[test]
fn n_on_the_plan_screen_starts_a_planning_chat_that_can_read_the_repository() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("plan", "layout = \"panels\"\n");
    screen.wait_on("here");
    // Claude Code as a stand-in that writes down what it was asked, then
    // will not start.
    let asked = root.join("claude-asked");
    write_script(
        &root.join("bin/claude"),
        &format!(
            "echo \"$@\" >> {}\necho 'no session' >&2\nexit 1\n",
            asked.display()
        ),
    );
    screen.press("\t");
    screen.wait_for("the Plan screen", |text| {
        text.contains("[2] Planning chats")
            && text.lines().last().unwrap_or("").starts_with(" PLAN ")
    });
    screen.press("n");
    screen.wait_for("the start's failure on the bar", |text| {
        text.lines()
            .last()
            .is_some_and(|bar| bar.contains("could not start a planning chat"))
    });
    let said = fs::read_to_string(&asked).unwrap();
    let here = root.join("here").canonicalize().unwrap();
    assert!(
        said.contains("--bg --name plan ")
            && said.contains(&format!("--add-dir {}", here.display())),
        "{said}"
    );
    assert!(
        screen.text().contains("[2] Planning chats"),
        "still on Plan"
    );
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn what_a_planning_chat_made_fills_its_bar_and_p_picks_up_the_ready_one_on_trees() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("planned", "layout = \"panels\"\n");
    screen.wait_on("here");
    let here = root.join("here").canonicalize().unwrap();
    // Linear as a ticket plugin: ANB-90 in Backlog, ANB-91 blocked by it.
    let plugins = root.join("config/treetop/plugins");
    fs::create_dir_all(&plugins).unwrap();
    write_script(
        &plugins.join("tix"),
        r#"read -r request
case "$request" in
*'"describe"'*) echo '{"ok":true,"result":{"protocol":1,"name":"tix","kind":"tickets","ops":["list","get"]}}' ;;
*'"ANB-90"'*) echo '{"ok":true,"result":{"id":"ANB-90","title":"Read what chats made","state":"Backlog","project":"treetop panels"}}' ;;
*'"ANB-91"'*) echo '{"ok":true,"result":{"id":"ANB-91","title":"Pick up from Plan","state":"Backlog","project":"treetop panels","links":[{"id":"ANB-90","kind":"blocked by"}]}}' ;;
*) echo '{"ok":true,"result":{"tickets":[]}}' ;;
esac
"#,
    );
    fs::write(
        root.join("config/treetop/config.toml"),
        format!(
            "layout = \"panels\"\n[issues.tix]\nplugin = \"tix\"\n[repos.\"{}\"]\nissues = \"tix\"\n",
            here.display()
        ),
    )
    .unwrap();
    // A planning chat for this workspace, whose transcript shows
    // `keel linear create` making both, and a create line it only read.
    let records = root.join("state/treetop/scratch");
    fs::create_dir_all(&records).unwrap();
    fs::write(
        records.join("1.json"),
        serde_json::json!({"job": "j1", "name": "plan 1", "created": 1, "checkout": here})
            .to_string(),
    )
    .unwrap();
    let claude = root.join("home/.claude");
    fs::create_dir_all(claude.join("jobs/j1")).unwrap();
    fs::write(claude.join("jobs/j1/state.json"), r#"{"sessionId":"s1"}"#).unwrap();
    fs::create_dir_all(claude.join("projects/here")).unwrap();
    let call = |id: &str, command: &str, output: &str| {
        let used = serde_json::json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": command}}
        ]}});
        let result = serde_json::json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": id, "content": output}
        ]}});
        format!("{used}\n{result}\n")
    };
    let made =
        |id: &str| format!("{id} https://linear.app/a/issue/{id}/x (team ANB, workspace anb)");
    fs::write(
        claude.join("projects/here/s1.jsonl"),
        call("t1", "cat notes.md", &made("ANB-7"))
            + &call("t2", "keel linear create --title a", &made("ANB-90"))
            + &call("t3", "keel linear create --title b", &made("ANB-91")),
    )
    .unwrap();

    screen.press("\t");
    screen.wait_for(
        "the chat's issues on [2] and the ready one on [3]",
        |text| {
            text.contains("project treetop panels")
                && text.contains("░░░░░░░░░░░░░░░░ 0 done · 0 going · 2")
                && text.contains("⊘ ANB-90")
                && text.contains("[4] Ready to pick up 1")
        },
    );
    let shown = screen.text();
    assert!(
        !shown.contains("ANB-7 "),
        "a create line only read: {shown}"
    );
    let ready_row = shown
        .lines()
        .find(|l| l.contains("Read what chats made") && l.contains("ready"));
    assert!(ready_row.is_some(), "{shown}");
    // The record keeps what the transcript showed.
    let record = fs::read_to_string(records.join("1.json")).unwrap();
    assert!(
        record.contains(r#""issues":["ANB-90","ANB-91"]"#),
        "{record}"
    );

    // It goes to Trees, leases a fresh tree for it and starts its agent,
    // and lands on that tree once it is listed.
    let stub = root.join("bin/treehouse");
    let said = fs::read_to_string(&stub).unwrap();
    let before =
        fs::read_to_string(root.join(format!("cache/treetop/{}.json", cache_key(&here)))).unwrap();
    let tree = root.join("here-trees/20");
    let mut trees: serde_json::Value = serde_json::from_str(&before).unwrap();
    let mut leased = trees[0].clone();
    leased["name"] = "20".into();
    leased["branch"] = "anb-90".into();
    leased["path"] = tree.to_str().unwrap().into();
    trees.as_array_mut().unwrap().push(leased);
    let after = trees.to_string();
    write_script(
        &stub,
        &format!(
            "if [ \"$1\" = get ]; then\n\
             git -C {here} worktree add -q -b anb-90 {tree} >&2\n\
             echo '{{\"path\":\"{tree}\",\"lease_id\":\"L1\"}}'; exit 0\nfi\n\
             if [ -d {tree} ]; then case \"$(git rev-parse --path-format=absolute --git-common-dir)\" in\n\
             {here}/*) echo '{after}'; exit 0 ;;\nesac; fi\n{rest}",
            here = here.display(),
            tree = tree.display(),
            rest = said.trim_start_matches("#!/bin/sh\n"),
        ),
    );
    write_script(
        &root.join("bin/claude"),
        "case \"$1\" in\n--bg) echo 'claude attach new20' ;;\n\
         agents) echo '[{\"id\":\"new20\"}]' ;;\n*) exit 1 ;;\nesac\n",
    );
    screen.press("4");
    screen.press("p");
    screen.wait_for("the new tree in the window, on the Trees screen", |text| {
        text.contains("[2] Trees")
            && !text.contains("[2] Planning chats")
            && text.contains("[1] Tree 20 · anb-90 · ANB-90 from plan 1 (I)")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_dialog_open_over_the_panels_has_a_bar_of_its_mode_alone() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("dialog", "");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.press("G");
    screen.press("n");
    screen.wait_for("the new agent dialog", |text| {
        text.lines()
            .last()
            .is_some_and(|bar| bar.starts_with(" INSERT "))
    });
    // What the bar would offer, [2] Trees' keys, would only type into it.
    let bar = screen.text().lines().last().unwrap_or("").to_string();
    assert!(
        !bar.contains("j/k move") && !bar.contains("1-4 panels"),
        "{bar}"
    );
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-101: a slow read of another workspace's PRs holds up none of its
/// agents' reads.
#[test]
fn an_agents_ask_shows_within_seconds_while_its_workspaces_prs_are_slow_to_read() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("slow-prs", "");
    // Asked before the first read starts, GitHub takes its time to say no.
    write_script(&root.join("bin/gh"), "sleep 9\nexit 1\n");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    // Off to there and back starts the reader again, with the slow PRs.
    screen.switch_to("there");
    screen.switch_to("here");
    std::thread::sleep(Duration::from_millis(1500));
    let asker = Asker(Command::new("sleep").arg("600").spawn().unwrap());
    let git_dir = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .current_dir(root.join("there-trees/1"))
        .output()
        .unwrap();
    let git_dir = PathBuf::from(String::from_utf8(git_dir.stdout).unwrap().trim());
    fs::write(
        git_dir.join("agent.json"),
        serde_json::json!({ "name": "asker", "pid": asker.0.id(), "status": "waiting" })
            .to_string(),
    )
    .unwrap();
    let asked = Instant::now();
    while !screen.text().contains("1 needs you") {
        assert!(
            asked.elapsed() < Duration::from_secs(5),
            "there's ask did not show within 5 s:\n{}",
            screen.text()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(asker);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-106: a planning chat opens in [1] as a tree's chat tab does, with
/// Normal and Insert, and leaving Plan leaves nothing of it on Trees.
#[test]
fn a_planning_chat_opens_in_the_window_with_normal_and_insert() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("plan-chat", "");
    let here = root.join("here").canonicalize().unwrap();
    let records = root.join("state/treetop/scratch");
    fs::create_dir_all(&records).unwrap();
    for n in [1, 2] {
        fs::write(
            records.join(format!("{n}.json")),
            serde_json::json!({"job": format!("j{n}"), "name": format!("plan {n}"),
                "created": 10 - n, "checkout": here})
            .to_string(),
        )
        .unwrap();
    }
    write_script(
        &root.join("bin/claude"),
        "[ \"$1\" = attach ] || exit 1\necho \"chat $2 is open\"\n\
         while read -r line; do echo \"$2 heard $line\"; done\n",
    );
    let bar_is =
        |mode: &'static str| move |text: &str| text.lines().last().unwrap_or("").starts_with(mode);
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.press("\t");
    screen.wait_for("the planning chats", |text| text.contains("plan 1"));
    // Enter opens it in [1], in Normal: the panels stay, nothing floats.
    screen.press("\r");
    screen.wait_for("the chat in [1]", |text| {
        text.contains("chat j1 is open")
            && text.contains("┐┌ [1] plan 1 ─")
            && text.contains("[3] Made from it")
            && !text.contains("back to treetop")
    });
    screen.wait_for("Normal", bar_is(" NORMAL "));
    screen.press("i");
    screen.wait_for("Insert", bar_is(" INSERT   Ctrl+Q normal"));
    // Capitals, digits and Tab are the chat's, not Plan's keys.
    screen.press("Split 2 S\r");
    screen.wait_for("what was typed, heard", |text| {
        text.contains("j1 heard Split 2 S")
    });
    screen.press("\x11");
    screen.wait_for("Normal again", bar_is(" NORMAL "));
    screen.press("\x1b");
    screen.wait_for("Plan's own keys", bar_is(" PLAN "));
    // Another chat under the cursor is another chat in [1].
    screen.press("j");
    screen.wait_for("[1] on the other chat", |text| {
        text.contains("┐┌ [1] plan 2 ─") && !text.contains("chat j1 is open")
    });
    screen.press("i");
    screen.wait_for("its chat, in Insert", |text| {
        text.contains("chat j2 is open") && bar_is(" INSERT ")(text)
    });
    screen.press("\x11");
    screen.wait_for("Normal in it", bar_is(" NORMAL "));
    // On to Issues, under the same top row, and back on Trees.
    screen.press("\t");
    screen.wait_for("Issues", |text| {
        text.contains("no issue source for this repository")
            && text.starts_with("  Trees   Plan   Issues ")
            && !text.contains("chat j2 is open")
    });
    screen.press("\t");
    screen.wait_for("Trees, the planning chat gone", |text| {
        text.contains("[2] Trees") && !text.contains("chat j2 is open") && bar_is(" PANEL 2 ")(text)
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-106: the chat `n` starts is the one that opens, wherever the cursor
/// was, and what is typed next goes to it.
#[test]
fn n_on_plan_opens_the_chat_it_started_not_the_one_under_the_cursor() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("plan-new", "");
    let here = root.join("here").canonicalize().unwrap();
    let records = root.join("state/treetop/scratch");
    fs::create_dir_all(&records).unwrap();
    for n in [1, 2, 3] {
        fs::write(
            records.join(format!("{n}.json")),
            serde_json::json!({"job": format!("j{n}"), "name": format!("plan {n}"),
                "created": n, "checkout": here})
            .to_string(),
        )
        .unwrap();
    }
    write_script(
        &root.join("bin/claude"),
        "case \"$1\" in\n--bg) echo 'claude attach new4' ;;\n\
         agents) echo '[{\"id\":\"new4\"}]' ;;\n\
         attach) echo \"chat $2 is open\"; while read -r l; do echo \"$2 heard $l\"; done ;;\n\
         *) exit 1 ;;\nesac\n",
    );
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.press("\t");
    screen.wait_for("the planning chats", |text| text.contains("plan 1"));
    // Down to the last of them, the oldest.
    screen.press("jj");
    screen.wait_for("the last chat", |text| text.contains("┌ [1] plan 1 ─"));
    screen.press("n");
    screen.wait_for("the new chat, ready to type to", |text| {
        text.contains("┌ [1] plan 4 ─")
            && text.contains("chat new4 is open")
            && text.lines().last().unwrap_or("").starts_with(" INSERT ")
    });
    screen.press("hello\r");
    screen.wait_for("the new chat hearing it", |text| {
        text.contains("new4 heard hello")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// Another tree, `number`, in the pool of the workspace `name`, in its cached
/// listing and what the stand-in treehouse says: held on `branch`, or free.
fn add_tree(root: &Path, name: &str, number: &str, branch: Option<&str>) {
    let checkout = root.join(name);
    let tree = root.join(format!("{name}-trees/{number}"));
    let path = tree.to_str().unwrap();
    match branch {
        Some(branch) => git(&checkout, &["worktree", "add", "-q", "-b", branch, path]),
        None => git(&checkout, &["worktree", "add", "-q", "--detach", path]),
    }
    let cache = root.join(format!("cache/treetop/{}.json", cache_key(&checkout)));
    let before = fs::read_to_string(&cache).unwrap();
    let mut trees: serde_json::Value = serde_json::from_str(&before).unwrap();
    let mut added = trees[0].clone();
    added["name"] = number.into();
    added["branch"] = branch.into();
    added["path"] = path.into();
    if branch.is_none() {
        added["status"] = "available".into();
        added["lease_holder"] = serde_json::Value::Null;
    }
    trees.as_array_mut().unwrap().push(added);
    fs::write(&cache, trees.to_string()).unwrap();
    let stub = root.join("bin/treehouse");
    let said = fs::read_to_string(&stub).unwrap();
    assert!(said.contains(&before), "{said}\n{before}");
    write_script(
        &stub,
        &said
            .replace("#!/bin/sh\n", "")
            .replace(&before, &trees.to_string()),
    );
}

/// ANB-104, ANB-105: `F` on [2] Trees sends the failure of the PR of the tree
/// under its cursor, which the bar offers, and a failure that never reached
/// its agent waits on you again.
#[test]
fn a_failure_sent_that_never_arrives_waits_again() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("unsent", "");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    let here = root.join("here").canonicalize().unwrap();
    // A PR that failed its checks on tree 1, and an agent there whose chat
    // ends as soon as it is opened.
    let plugins = root.join("config/treetop/plugins");
    fs::create_dir_all(&plugins).unwrap();
    write_script(
        &plugins.join("shp"),
        r##"read -r request
case "$request" in
*'"describe"'*) echo '{"ok":true,"result":{"protocol":1,"name":"shp","kind":"ship","ops":["status","logs"]}}' ;;
*'"op":"logs"'*) echo '{"ok":true,"result":{"text":"e2e broke"}}' ;;
*) echo '{"ok":true,"result":{"branches":[{"branch":"here-work","stage":"ci_failed","label":"#65 x e2e"}]}}' ;;
esac
"##,
    );
    write_script(
        &plugins.join("ag"),
        &format!(
            r#"read -r request
case "$request" in
*'"describe"'*) echo '{{"ok":true,"result":{{"protocol":1,"name":"ag","kind":"agent","ops":["find"]}}}}' ;;
*) echo '{{"ok":true,"result":{{"agents":[{{"id":"a1","name":"fixer","dir":"{}","live":true,"status":"idle","background":true,"attach":["sh","-c","exit 1"]}}]}}}}' ;;
esac
"#,
            root.join("here-trees/1").display()
        ),
    );
    fs::write(
        root.join("config/treetop/config.toml"),
        format!("[repos.\"{}\"]\nship = [\"shp\"]\n", here.display()),
    )
    .unwrap();
    // Away and back reads the plugins.
    screen.switch_to("there");
    screen.switch_to("here");
    screen.wait_for("the failed PR, on you and in Trees", |text| {
        text.contains("1 needs you")
            && text.contains("ci failed")
            && text.contains("PR #65 x e2e")
            && text.contains("F send failure")
    });
    screen.press("F");
    screen.wait_for("the send failing", |text| {
        text.contains("not sent to fixer in 1")
    });
    screen.wait_for("the failure on you again", |text| {
        text.contains("1 needs you") && text.contains("ci failed")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-104: a failure still to be typed in when you switch workspace waits on
/// you again, as it dies with its chat.
#[test]
fn a_failure_still_unsent_as_you_switch_workspace_waits_again() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("unsent-switch", "");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    let here = root.join("here").canonicalize().unwrap();
    // A PR that failed its checks on tree 1, and an agent there whose chat
    // ends as soon as it is opened.
    let plugins = root.join("config/treetop/plugins");
    fs::create_dir_all(&plugins).unwrap();
    write_script(
        &plugins.join("shp"),
        r##"read -r request
case "$request" in
*'"describe"'*) echo '{"ok":true,"result":{"protocol":1,"name":"shp","kind":"ship","ops":["status","logs"]}}' ;;
*'"op":"logs"'*) echo '{"ok":true,"result":{"text":"e2e broke"}}' ;;
*) echo '{"ok":true,"result":{"branches":[{"branch":"here-work","stage":"ci_failed","label":"#65 x e2e"}]}}' ;;
esac
"##,
    );
    write_script(
        &plugins.join("ag"),
        &format!(
            r#"read -r request
case "$request" in
*'"describe"'*) echo '{{"ok":true,"result":{{"protocol":1,"name":"ag","kind":"agent","ops":["find"]}}}}' ;;
*) echo '{{"ok":true,"result":{{"agents":[{{"id":"a1","name":"fixer","dir":"{}","live":true,"status":"idle","background":true,"attach":["sleep","600"]}}]}}}}' ;;
esac
"#,
            root.join("here-trees/1").display()
        ),
    );
    fs::write(
        root.join("config/treetop/config.toml"),
        format!("[repos.\"{}\"]\nship = [\"shp\"]\n", here.display()),
    )
    .unwrap();
    // Away and back reads the plugins.
    screen.switch_to("there");
    screen.switch_to("here");
    screen.wait_for("the failed PR, on you and in Trees", |text| {
        text.contains("1 needs you")
            && text.contains("ci failed")
            && text.contains("PR #65 x e2e")
            && text.contains("F send failure")
    });
    screen.press("F");
    // A switch waits for the failure to be read; then it is being typed in.
    screen.wait_for("the failure read, and sent", |text| {
        text.lines()
            .map(|l| l.chars().take(50).collect::<String>())
            .any(|l| l.contains("idle") && l.contains("here-work"))
    });
    screen.switch_to("there");
    screen.wait_for("the failure on you again, and why, on the bar", |text| {
        text.contains("1 needs you")
            && text
                .lines()
                .last()
                .is_some_and(|bar| bar.contains("not sent to fixer in here:1, you switched"))
    });
    screen.press("e");
    screen.wait_for("which it is", |text| text.contains("✗ here 1"));
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-102: the next key clears what the bar says, so it shows the keys of
/// what has them again, on Trees and on Plan.
#[test]
fn a_message_on_the_bar_gives_way_to_the_keys_at_the_next_one() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("message", "");
    let bar_has =
        |what: &'static str| move |text: &str| text.lines().last().unwrap_or("").contains(what);
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    // Tree 1 has no PR whose failure F could send.
    screen.press("F");
    screen.wait_for("why not", bar_has("nothing has failed on tree 1's PR"));
    screen.press("4");
    screen.wait_for("[4]'s keys", bar_has("j/k steps"));
    screen.press("\t");
    screen.wait_for("Plan", bar_has("new chat"));
    screen.press("D");
    screen.wait_for("why not", bar_has("no planning chat yet"));
    screen.press("j");
    screen.wait_for("Plan's keys", bar_has("draft issues"));
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-106: a planning chat left starting with Esc is on Plan's list once it
/// is up, to open from there.
#[test]
fn a_planning_chat_left_starting_joins_the_list_on_plan() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("plan-esc", "");
    write_script(
        &root.join("bin/claude"),
        "case \"$1\" in\n--bg) sleep 2; echo 'claude attach new1' ;;\n\
         agents) echo '[{\"id\":\"new1\"}]' ;;\n\
         attach) echo \"chat $2 is open\"; exec sleep 600 ;;\n*) exit 1 ;;\nesac\n",
    );
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.press("\t");
    screen.wait_for("Plan", |text| text.contains("none yet: n starts one"));
    screen.press("n");
    screen.wait_for("it starting", |text| {
        text.contains("starting") || text.contains("opening")
    });
    screen.press("\x1b");
    screen.wait_for("it in the list, said so", |text| {
        text.contains("○ plan 1")
            && text
                .lines()
                .last()
                .is_some_and(|bar| bar.contains("plan 1 is ready; Enter on it opens it"))
    });
    screen.press("\r");
    screen.wait_for("its chat", |text| text.contains("chat new1 is open"));
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-108: a palette command is on the tree it names, whichever panel has
/// the keys. A rename, since treetop returns no tree it was started in.
#[test]
fn the_palettes_rename_is_of_the_tree_it_names_from_any_panel() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("palette", "");
    screen.wait_for("tree 1 in the window", |text| {
        text.contains("[1] Tree 1 · here-work")
    });
    let bar_is =
        |mode: String| move |text: &str| text.lines().last().unwrap_or("").starts_with(&mode);
    for panel in ["4", "3", "2"] {
        screen.press(panel);
        screen.press("\x10");
        screen.wait_for("the palette", |text| {
            text.contains("Rename the branch of tree 1")
        });
        screen.press("rename");
        screen.press("\r");
        screen.wait_for("the box to rename it", bar_is(" INSERT ".into()));
        screen.press("\x1b");
        screen.wait_for("the panel's keys again", bar_is(format!(" PANEL {panel} ")));
    }
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// The top row's count of `n` that need you.
fn count(n: usize) -> String {
    format!("{n} need{} you", if n == 1 { "s" } else { "" })
}

/// An agent in the tree at `tree` starts asking, until the file returned is
/// removed.
fn asking_in(tree: &Path, asker: &Asker) -> PathBuf {
    let git_dir = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-dir"])
        .current_dir(tree)
        .output()
        .unwrap();
    let file = PathBuf::from(String::from_utf8(git_dir.stdout).unwrap().trim()).join("agent.json");
    fs::write(
        &file,
        serde_json::json!({ "name": "asker", "pid": asker.0.id(), "status": "waiting" })
            .to_string(),
    )
    .unwrap();
    file
}

/// ANB-104: the list's cursor stays on its item as one above it resolves,
/// so Enter goes to what was picked.
#[test]
fn the_lists_cursor_stays_on_what_it_was_on_as_another_resolves() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces_with("hold", "", |root| {
        add_tree(root, "there", "2", Some("there-second"));
        add_tree(root, "there", "3", Some("there-third"));
    });
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    // Away and back, so there's trees are known.
    screen.switch_to("there");
    screen.wait_for("all three", |text| text.contains("there-third"));
    screen.switch_to("here");
    // One process each: one is one agent, wherever it is said to be.
    let askers = [(); 3].map(|()| Asker(Command::new("sleep").arg("600").spawn().unwrap()));
    let files = [1, 2, 3].map(|n| {
        let file = asking_in(&root.join(format!("there-trees/{n}")), &askers[n - 1]);
        screen.wait_for("the ask", |text| text.contains(&count(n)));
        file
    });
    // Onto the second of them, whichever it is in the list.
    let on = |text: &str, n: usize| {
        text.lines()
            .filter(|l| l.contains("● there "))
            .nth(n)
            .unwrap_or("")
            .to_string()
    };
    screen.press("e");
    screen.press("j");
    screen.wait_for("the list", |text| text.contains("● there "));
    let picked = ["there 1", "there 2", "there 3"]
        .into_iter()
        .find(|place| on(&screen.text(), 1).contains(place))
        .unwrap();
    let above = ["there 1", "there 2", "there 3"]
        .into_iter()
        .find(|place| on(&screen.text(), 0).contains(place))
        .unwrap();
    // The one above it resolves.
    let number: usize = above.trim_start_matches("there ").parse().unwrap();
    fs::remove_file(&files[number - 1]).unwrap();
    screen.wait_for("it gone", |text| text.contains("2 need you"));
    screen.press("\r");
    screen.wait_on("there");
    let number = picked.trim_start_matches("there ");
    let title = format!("[1] Tree {number} ·");
    screen.wait_for("the tree picked", |text| text.contains(&title));
    drop(askers);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-104: gone to from the list, another workspace's item has its tree under
/// the cursor there, so the next key acts on that tree.
#[test]
fn the_key_after_going_to_another_workspaces_item_acts_on_its_tree() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces_with("jump", "", |root| {
        add_tree(root, "there", "2", Some("there-second"));
    });
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.switch_to("there");
    screen.wait_for("both", |text| text.contains("there-second"));
    screen.switch_to("here");
    let askers = [(); 2].map(|()| Asker(Command::new("sleep").arg("600").spawn().unwrap()));
    for n in [1, 2] {
        asking_in(&root.join(format!("there-trees/{n}")), &askers[n - 1]);
        screen.wait_for("the ask", |text| text.contains(&count(n)));
    }
    let rows = |text: &str| -> Vec<String> {
        text.lines()
            .filter_map(|l| Some(l[l.find("● there ")?..].chars().skip(8).take(1).collect()))
            .collect()
    };
    screen.press("e");
    screen.wait_for("both in the list", |text| rows(text).len() == 2);
    let last = rows(&screen.text())[1].clone();
    screen.press("j");
    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("the tree picked", |text| {
        text.contains(&format!("[1] Tree {last} ·"))
    });
    std::thread::sleep(Duration::from_secs(1));
    screen.press("x");
    screen.wait_for("that tree shelved", |text| {
        text.lines()
            .last()
            .is_some_and(|bar| bar.contains(&format!("tree {last} is on the Backburner")))
    });
    drop(askers);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-104: `W` goes round everything that needs you, in every workspace.
#[test]
fn shift_w_reaches_everything_that_needs_you_across_workspaces() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces_with("round", "", |root| {
        add_tree(root, "there", "2", Some("there-second"));
        add_tree(root, "here", "2", Some("here-second"));
    });
    let askers = [(); 4].map(|()| Asker(Command::new("sleep").arg("600").spawn().unwrap()));
    let trees = [
        "here-trees/1",
        "here-trees/2",
        "there-trees/1",
        "there-trees/2",
    ];
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    // Away and back, so there's second tree is known.
    screen.switch_to("there");
    screen.wait_for("both", |text| text.contains("there-second"));
    for (tree, asker) in trees.iter().zip(&askers) {
        // With a chat, as an agent has, for `W` to open.
        let file = asking_in(&root.join(tree), asker);
        fs::write(
            file,
            serde_json::json!({ "name": "asker", "pid": asker.0.id(), "status": "waiting",
                "attach": ["sh", "-c", "echo asks; exec sleep 600"] })
            .to_string(),
        )
        .unwrap();
    }
    screen.wait_for("all four", |text| text.contains("4 need you"));
    let place = |text: &str| -> Option<String> {
        let title = text.lines().find(|l| l.contains("[1] Tree "))?;
        let at = title.find("[1] Tree ")?;
        Some(title[at..].chars().take(40).collect())
    };
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..4 {
        screen.press("W");
        std::thread::sleep(Duration::from_secs(6));
        screen.wait_for("all four still waiting", |text| text.contains("4 need you"));
        let title = place(&screen.text()).unwrap();
        let branch = ["here-work", "here-second", "there-work", "there-second"]
            .into_iter()
            .find(|b| title.contains(&format!("· {b} ")))
            .unwrap();
        seen.insert(branch);
    }
    assert_eq!(seen.len(), 4, "{seen:?}");
    drop(askers);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}
