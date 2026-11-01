//! `treetop serve --install` and `--uninstall` against a stand-in
//! `systemctl`, which writes down what it was asked and whether the unit was
//! there to act on, so a machine's real services are never touched. Real
//! systemd is the journey in the README.

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A home with a git workspace, a config and stand-ins on `PATH`.
struct Home {
    root: PathBuf,
}

impl Home {
    fn new(name: &str) -> Home {
        let root = std::env::temp_dir().join(format!(
            "treetop-serve-install-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir_all(root.join("work")).unwrap();
        fs::create_dir_all(root.join(".config/treetop")).unwrap();
        fs::write(
            root.join(".config/treetop/config.toml"),
            "share = [\"share-it\"]\n[mobile]\nowner = \"me@example.com\"\n",
        )
        .unwrap();
        let home = Home { root };
        home.stand_in(
            "systemctl",
            &format!(
                "[ -f {unit} ] && on=unit || on='no unit'\necho \"$* with $on\" >> {calls}\n",
                unit = home.unit().display(),
                calls = home.calls().display(),
            ),
        );
        home.stand_in("loginctl", "echo no\n");
        home.stand_in("share-it", "echo https://box.tail1.ts.net:$1\n");
        let git = Command::new("git")
            .args(["init", "-q"])
            .arg(home.root.join("work"))
            .status()
            .unwrap();
        assert!(git.success());
        home
    }

    fn stand_in(&self, name: &str, script: &str) {
        let path = self.root.join("bin").join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn calls(&self) -> PathBuf {
        self.root.join("systemctl-calls")
    }

    fn unit(&self) -> PathBuf {
        self.root.join(".config/systemd/user/treetop-serve.service")
    }

    fn treetop(&self, args: &[&str]) -> Output {
        self.treetop_in(&self.root.join("work"), args)
    }

    fn treetop_in(&self, dir: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.root.join("bin").display(),
            std::env::var("PATH").unwrap()
        );
        Command::new(env!("CARGO_BIN_EXE_treetop"))
            .args(["serve"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .env("HOME", &self.root)
            .env_remove("XDG_CONFIG_HOME")
            .env("PATH", path)
            .output()
            .unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

/// A port something is listening on, as the started service would be, for
/// as long as the listener is kept.
fn listening() -> (TcpListener, String) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    (listener, port)
}

#[test]
fn install_writes_the_unit_and_starts_it_then_uninstall_takes_it_away() {
    let home = Home::new("cycle");
    let (_listener, port) = listening();

    let out = home.treetop(&["--install", "--port", &port]);
    assert!(out.status.success(), "{}", text(&out));
    let said = text(&out);
    assert!(said.contains("Lingering is off"), "{said}");
    assert!(
        said.contains(&format!("https://box.tail1.ts.net:{port}")),
        "{said}"
    );
    let unit = read(&home.unit());
    assert!(
        unit.contains(&format!(
            "ExecStart=\"{}\" serve --port {port}\n",
            env!("CARGO_BIN_EXE_treetop")
        )),
        "{unit}"
    );
    let workspace = home.root.join("work").canonicalize().unwrap();
    assert!(
        unit.contains(&format!("WorkingDirectory={}\n", workspace.display())),
        "{unit}"
    );
    // The commands it starts come from the PATH it was installed with.
    assert!(
        unit.contains(&format!(
            "Environment=\"PATH={}:",
            home.root.join("bin").display()
        )),
        "{unit}"
    );
    assert_eq!(
        read(&home.calls()),
        "--user show-environment with no unit\n\
         --user daemon-reload with unit\n\
         --user enable treetop-serve with unit\n\
         --user restart treetop-serve with unit\n"
    );

    // Over its own unit an install goes ahead, which is how the port changes.
    fs::remove_file(home.calls()).unwrap();
    let (_moved, moved) = listening();
    let out = home.treetop(&["--install", "--port", &moved]);
    assert!(out.status.success(), "{}", text(&out));
    let unit = read(&home.unit());
    assert!(unit.contains(&format!(" serve --port {moved}\n")), "{unit}");
    assert_eq!(
        read(&home.calls()),
        "--user show-environment with unit\n\
         --user daemon-reload with unit\n\
         --user enable treetop-serve with unit\n\
         --user restart treetop-serve with unit\n"
    );

    // From outside any repository, since the service may outlast its workspace.
    fs::remove_file(home.calls()).unwrap();
    let out = home.treetop_in(&home.root, &["--uninstall"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!home.unit().exists());
    assert_eq!(
        read(&home.calls()),
        "--user disable --now treetop-serve with unit\n\
         --user daemon-reload with no unit\n"
    );
}

#[test]
fn a_unit_from_another_hand_is_not_touched() {
    let home = Home::new("foreign");
    fs::create_dir_all(home.unit().parent().unwrap()).unwrap();
    fs::write(home.unit(), "[Service]\nExecStart=/bin/mine\n").unwrap();

    for args in [&["--install"][..], &["--uninstall"][..]] {
        let out = home.treetop(args);
        assert!(!out.status.success());
        assert!(
            text(&out).contains("was not written by treetop"),
            "{}",
            text(&out)
        );
        assert_eq!(read(&home.unit()), "[Service]\nExecStart=/bin/mine\n");
    }
    // The install's check that systemd is there, and nothing after it.
    assert_eq!(read(&home.calls()), "--user show-environment with unit\n");
}

#[test]
fn install_without_systemd_says_so_and_writes_nothing() {
    let home = Home::new("no-systemd");
    home.stand_in(
        "systemctl",
        "echo 'Failed to connect to bus: No such file' >&2; exit 1\n",
    );

    let out = home.treetop(&["--install"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("needs systemd's user manager"),
        "{}",
        text(&out)
    );
    assert!(
        text(&out).contains("Failed to connect to bus"),
        "{}",
        text(&out)
    );
    assert!(!home.unit().exists());
}

#[test]
fn install_without_an_owner_says_so_and_writes_nothing() {
    let home = Home::new("no-owner");
    fs::write(home.root.join(".config/treetop/config.toml"), "").unwrap();

    let out = home.treetop(&["--install"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("answers only its owner"),
        "{}",
        text(&out)
    );
    assert!(!home.unit().exists());
}
