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

#[test]
fn a_shell_left_for_another_workspace_is_the_same_shell_on_coming_back() {
    let root = std::env::temp_dir().join(format!("treetop-switch-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let bin = root.join("bin");
    let cache = root.join("cache/treetop");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(&cache).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let (here, here_status) = repository(&root, "here");
    let (there, there_status) = repository(&root, "there");

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
    fs::write(
        cache.join("workspaces.json"),
        serde_json::json!([
            { "checkout": here, "opened": 2 },
            { "checkout": there, "opened": 1 },
        ])
        .to_string(),
    )
    .unwrap();

    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut screen = Screen::start(
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
