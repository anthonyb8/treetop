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

fn write_script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
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
    assert!(!screen.text().contains("needs action"), "{}", screen.text());

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
    while !screen.text().contains("1 needs action") {
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
            "[1] Waiting",
            "[2] Trees",
            "[3] Ship",
            "[4] News",
            "[0] Tree 1 · here-work",
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
    screen.press("2");
    screen.wait_for("panel 2 with the keys", |text| {
        bar(text).starts_with(" PANEL 2 ") && bar(text).contains("j/k move")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-103: in the panels layout `]` shows the next workspace's trees on
/// [2] Trees at once, only to look at, and Enter switches there.
#[test]
fn a_bracket_shows_another_workspace_on_the_trees_panel_and_enter_switches_there() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("tabs", "layout = \"panels\"\n");
    screen.wait_on("here");
    screen.wait_for("here's tree in the window", |text| {
        text.contains("[0] Tree 1 · here-work")
    });
    screen.wait_for("both workspaces as tabs", |text| {
        text.lines()
            .any(|l| l.contains("[2] Trees") && l.contains("here") && l.contains("there"))
    });

    screen.press("]");
    screen.wait_for("there's trees, only looked at", |text| {
        text.contains("there-work") && text.contains("there is only looked at here")
    });
    // Still here: nothing switched, and here's tree is not on the panel.
    assert!(
        screen
            .text()
            .lines()
            .last()
            .unwrap()
            .contains("treetop · here")
    );

    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("there's tree in the window", |text| {
        text.contains("[0] Tree 1 · there-work")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-104: in the panels layout an agent asking in another workspace is
/// listed in [1] Waiting with where it is, and Enter on it switches there
/// with its tree in the window.
#[test]
fn an_ask_in_another_workspace_is_listed_in_waiting_and_enter_goes_there() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("waiting", "layout = \"panels\"\n");
    screen.wait_on("here");
    screen.wait_for("here's tree in the window", |text| {
        text.contains("[0] Tree 1 · here-work")
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
    screen.wait_for("there's ask in [1] Waiting", |text| {
        text.lines()
            .any(|l| l.contains("there:1") && l.contains("asks you"))
    });
    screen.press("1");
    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("there's tree in the window", |text| {
        text.contains("[0] Tree 1 · there-work")
    });
    drop(asker);
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-105: in the panels layout an agent starting to ask in another
/// workspace is a line in [4] News, which stays however many keys go by.
#[test]
fn an_ask_in_another_workspace_is_news_that_stays() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("news", "layout = \"panels\"\n");
    screen.wait_on("here");
    screen.wait_for("nothing new yet", |text| {
        text.contains("nothing new since treetop opened")
    });
    // Its first read is done before the agent asks, so the ask is new.
    std::thread::sleep(std::time::Duration::from_secs(5));
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
    let is_news = |text: &str| {
        text.lines()
            .skip_while(|l| !l.contains("[4] News"))
            .any(|l| l.contains("there:1") && l.contains("asks you"))
    };
    screen.wait_for("there's ask in [4] News", is_news);
    for key in ["j", "k", "3", "2"] {
        screen.press(key);
    }
    assert!(is_news(&screen.text()), "{}", screen.text());
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
        text.contains("[1] Planning chats")
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
        screen.text().contains("[1] Planning chats"),
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
                && text.contains("[3] Ready to pick up 1")
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
    screen.press("3");
    screen.press("p");
    screen.wait_for("the new tree in the window, on the Trees screen", |text| {
        text.contains("[2] Trees")
            && !text.contains("[1] Planning chats")
            && text.contains("[0] Tree 20 · anb-90 · ANB-90 from plan 1 (I)")
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
    screen.press("]");
    screen.press("\r");
    screen.wait_on("there");
    screen.press("]");
    screen.press("\r");
    screen.wait_on("here");
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
    while !screen.text().contains("1 needs action") {
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

/// ANB-106: a planning chat opens in [0] as a tree's chat tab does, with
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
    // Enter opens it in [0], in Normal: the panels stay, nothing floats.
    screen.press("\r");
    screen.wait_for("the chat in [0]", |text| {
        text.contains("chat j1 is open")
            && text.contains("┐┌ [0] plan 1 ─")
            && text.contains("[2] Made from it")
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
    // Another chat under the cursor is another chat in [0].
    screen.press("j");
    screen.wait_for("[0] on the other chat", |text| {
        text.contains("┐┌ [0] plan 2 ─") && !text.contains("chat j1 is open")
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
            && text.starts_with(" treetop    Trees   Plan   Issues ")
            && !text.contains("chat j2 is open")
    });
    screen.press("\t");
    screen.wait_for("Trees, the planning chat gone", |text| {
        text.contains("[2] Trees") && !text.contains("chat j2 is open") && bar_is(" PANEL 2 ")(text)
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-103: on another workspace's tab a capital, which a terminal sends
/// with Shift, acts on no tree of this one's.
#[test]
fn a_capital_on_another_workspaces_tab_acts_on_nothing_here() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("capital", "");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    screen.press("]");
    screen.wait_for("there's trees", |text| text.contains("there-work"));
    // N would open a new agent's form for this workspace, which takes the
    // keys; `?` after it opens the help only if nothing did.
    screen.press("N");
    screen.press("?");
    screen.wait_for("the help, so no form had opened", |text| {
        text.contains("show every key") || text.contains("Keys")
    });
    assert!(!screen.text().contains("New agent"), "{}", screen.text());
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
    screen.wait_for("the last chat", |text| text.contains("┌ [0] plan 1 ─"));
    screen.press("n");
    screen.wait_for("the new chat, ready to type to", |text| {
        text.contains("┌ [0] plan 4 ─")
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

/// A second tree, 2, on `branch`, in the pool of the workspace `name`, in
/// its cached listing and what the stand-in treehouse says.
fn add_tree(root: &Path, name: &str, branch: &str) {
    let checkout = root.join(name);
    let tree = root.join(format!("{name}-trees/2"));
    git(
        &checkout,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            tree.to_str().unwrap(),
        ],
    );
    let cache = root.join(format!("cache/treetop/{}.json", cache_key(&checkout)));
    let before = fs::read_to_string(&cache).unwrap();
    let mut trees: serde_json::Value = serde_json::from_str(&before).unwrap();
    let mut second = trees[0].clone();
    second["name"] = "2".into();
    second["branch"] = branch.into();
    second["path"] = tree.to_str().unwrap().into();
    trees.as_array_mut().unwrap().push(second);
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

/// ANB-103: Enter on another workspace's tree lands on that tree.
#[test]
fn enter_on_another_workspaces_tree_lands_on_it() {
    let Workspaces {
        root, mut screen, ..
    } = two_workspaces("land", "");
    add_tree(&root, "there", "there-second");
    screen.wait_for("the panels", |text| text.contains("[2] Trees"));
    // Away and back, so there's listing is read with its second tree.
    screen.press("]");
    screen.wait_for("there's trees", |text| text.contains("there-work"));
    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("both its trees", |text| text.contains("there-second"));
    screen.press("]");
    screen.wait_for("here's trees", |text| text.contains("here-work"));
    screen.press("\r");
    screen.wait_on("here");
    screen.press("]");
    screen.wait_for("there's trees again", |text| text.contains("there-second"));
    screen.press("j");
    screen.press("\r");
    screen.wait_on("there");
    screen.wait_for("tree 2 in the window", |text| {
        text.contains("[0] Tree 2 · there-second")
    });
    // Nor does what is read next move it.
    std::thread::sleep(Duration::from_secs(5));
    assert!(
        screen.text().contains("[0] Tree 2 · there-second"),
        "{}",
        screen.text()
    );
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}

/// ANB-104, ANB-105: `F` on [3] Ship acts on the PR under its cursor, and a
/// failure that never reached its agent waits on you again.
#[test]
fn a_failure_sent_from_ship_that_never_arrives_waits_again() {
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
    screen.press("]");
    screen.wait_for("there's trees", |text| text.contains("there-work"));
    screen.press("\r");
    screen.wait_on("there");
    screen.press("]");
    screen.wait_for("here's trees", |text| text.contains("here-work"));
    screen.press("\r");
    screen.wait_on("here");
    screen.wait_for("the failed PR, on you and in Ship", |text| {
        text.contains("1 needs action") && text.contains("#65 x e2e") && text.contains("F send it")
    });
    screen.press("3");
    screen.press("F");
    screen.wait_for("the send failing", |text| {
        text.contains("not sent to fixer in 1")
    });
    screen.wait_for("the failure on you again", |text| {
        text.contains("1 needs action") && text.contains("checks failed")
    });
    drop(screen);
    let _ = fs::remove_dir_all(&root);
}
