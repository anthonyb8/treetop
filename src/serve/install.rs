//! `treetop serve --install`: the server as a systemd user service, so the
//! phone app is there after a reboot, a crash or a closed terminal.

use std::fs;
use std::io::ErrorKind;
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};

use crate::child::{self, End};

/// The service's name, which `systemctl --user restart` is given to pick up
/// a new binary.
const SERVICE: &str = "treetop-serve";

/// The first line of a unit treetop wrote, which tells it from one written
/// by hand.
const MARKER: &str =
    "# Written by `treetop serve --install`; `treetop serve --uninstall` removes it.";

/// How long a `systemctl` or `loginctl` call may take.
const LIMIT: Duration = Duration::from_secs(30);

/// How long a started service has to take its port before the install says
/// it did not come up.
const START: Duration = Duration::from_secs(15);

/// What the service runs, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    pub binary: PathBuf,
    pub workspace: PathBuf,
    /// The `--port` asked for, which the service is started with; None
    /// leaves the port to the config.
    pub port: Option<u16>,
    /// The `PATH` installed from. systemd's own has neither `~/.local/bin`
    /// nor `~/.cargo/bin`, where the commands the server runs usually are:
    /// the share command, `treehouse`, and an agent's.
    pub path: String,
}

impl Unit {
    /// The unit file.
    ///
    /// `Restart=always` because `on-failure` counts SIGTERM as a clean exit,
    /// and a killed server has to come back; `systemctl stop` still stops it.
    /// `StartLimitIntervalSec=0` because a port taken for a while is worth
    /// outlasting, not giving up on. `KillMode=process` because the server
    /// can be the one that starts Claude Code's daemon, which the default
    /// would end with it, and the running agents with that.
    pub fn text(&self) -> String {
        let mut exec = quoted(&self.binary.to_string_lossy());
        exec.push_str(" serve");
        if let Some(port) = self.port {
            exec.push_str(&format!(" --port {port}"));
        }
        format!(
            "{MARKER}\n\
             [Unit]\n\
             Description=treetop for your phone\n\
             StartLimitIntervalSec=0\n\
             \n\
             [Service]\n\
             ExecStart={exec}\n\
             WorkingDirectory={workspace}\n\
             Environment={path}\n\
             Restart=always\n\
             RestartSec=2\n\
             KillMode=process\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            workspace = self.workspace.to_string_lossy().replace('%', "%%"),
            path = quoted(&format!("PATH={}", self.path)),
        )
    }
}

/// `word` as one systemd word, with its specifiers and quotes escaped.
fn quoted(word: &str) -> String {
    let word = word
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{word}\"")
}

/// Where systemd reads the user's units from.
fn unit_path() -> Result<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .context("neither XDG_CONFIG_HOME nor HOME is set")?;
    Ok(config
        .join("systemd")
        .join("user")
        .join(format!("{SERVICE}.service")))
}

/// Whether `text` is a unit this module wrote.
fn is_ours(text: &str) -> bool {
    text.lines().next() == Some(MARKER)
}

/// Whether there is a unit at `path`, refusing one written by another hand.
fn is_installed(path: &Path) -> Result<bool> {
    match fs::read_to_string(path) {
        Ok(text) if is_ours(&text) => Ok(true),
        Ok(_) => bail!(
            "{} was not written by treetop, so it is left alone: move it away to go on",
            path.display()
        ),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("reading {}", path.display())),
    }
}

/// Runs `program` with `args` and returns what it printed.
fn ask(program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);
    let ran = child::run(&mut command, None, LIMIT).map_err(|err| match err.kind() {
        ErrorKind::NotFound => format!("{program} is not installed"),
        _ => format!("could not run {program}: {err}"),
    })?;
    match ran.end {
        End::Exited(status) if status.success() => {
            Ok(String::from_utf8_lossy(&ran.stdout).into_owned())
        }
        End::Exited(status) => Err(crate::actions::failure(
            program,
            &String::from_utf8_lossy(&ran.stderr),
            &status.to_string(),
        )),
        End::TimedOut | End::HeldOpen(_) => {
            Err(format!("{program} did not answer in {}s", LIMIT.as_secs()))
        }
    }
}

fn systemctl(args: &[&str]) -> Result<()> {
    let args: Vec<&str> = ["--user"].iter().chain(args).copied().collect();
    ask("systemctl", &args)
        .map(|_| ())
        .map_err(|why| anyhow!(why))
}

/// Writes the unit, then enables and starts the service. A service already
/// running is restarted, so it runs what the unit now says. Returns once it
/// listens on `port`.
pub fn install(unit: &Unit, port: u16) -> Result<()> {
    let path = unit_path()?;
    // Before anything is written, so a machine it cannot work on is left as
    // it was.
    systemctl(&["show-environment"])
        .map_err(|why| anyhow!("treetop serve --install needs systemd's user manager: {why}"))?;
    is_installed(&path)?;
    let dir = path.parent().context("a unit's path has a directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    fs::write(&path, unit.text()).with_context(|| format!("writing {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", SERVICE])?;
    systemctl(&["restart", SERVICE])?;
    if !listens(port, START) {
        bail!(
            "{SERVICE} did not start listening on 127.0.0.1:{port}; \
             `journalctl --user -u {SERVICE}` says why"
        );
    }
    println!("{SERVICE} is running: {}", path.display());
    Ok(())
}

/// Restarts the service when treetop installed it, so it runs the binary now
/// on disk. False when there is none to restart.
pub fn restart_if_installed() -> Result<bool> {
    if !is_installed(&unit_path()?)? {
        return Ok(false);
    }
    systemctl(&["restart", SERVICE])?;
    Ok(true)
}

/// Whether something answers on the port within `limit`.
fn listens(port: u16, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// What lingering means for the service. Without it a user's services stop
/// when they log out, and start again only when they log back in.
pub fn lingering() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    match ask(
        "loginctl",
        &["show-user", &user, "--property=Linger", "--value"],
    ) {
        Ok(answer) if answer.trim() == "yes" => {
            "Lingering is on, so it runs at boot and while you are logged out.".into()
        }
        Ok(_) => "Lingering is off, so it stops when you log out and does not start at boot: \
                  `loginctl enable-linger` keeps it running."
            .into(),
        Err(why) => format!(
            "Could not read lingering ({why}): without `loginctl enable-linger` \
             it stops when you log out."
        ),
    }
}

/// Stops, disables and removes the service.
pub fn uninstall() -> Result<()> {
    let path = unit_path()?;
    if !is_installed(&path)? {
        println!("{SERVICE} is not installed");
        return Ok(());
    }
    systemctl(&["disable", "--now", SERVICE])?;
    fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    systemctl(&["daemon-reload"])?;
    println!(
        "{SERVICE} is stopped and removed. Its port stays shared on your tailnet, \
         answering nothing, until `tether unshare <port>`."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> Unit {
        Unit {
            binary: "/home/me/.local/bin/treetop".into(),
            workspace: "/home/me/code/app".into(),
            port: None,
            path: "/home/me/.local/bin:/usr/bin".into(),
        }
    }

    /// The value of `key` in the unit, the way systemd reads a line.
    fn value(text: &str, key: &str) -> String {
        let start = format!("{key}=");
        text.lines()
            .find_map(|line| line.strip_prefix(&start))
            .unwrap_or_else(|| panic!("no {key} in {text}"))
            .to_string()
    }

    #[test]
    fn the_unit_carries_a_port_only_when_one_was_asked_for() {
        let exec = |port| value(&Unit { port, ..unit() }.text(), "ExecStart");
        assert_eq!(exec(None), "\"/home/me/.local/bin/treetop\" serve");
        assert_eq!(
            exec(Some(8123)),
            "\"/home/me/.local/bin/treetop\" serve --port 8123"
        );
    }

    #[test]
    fn a_killed_server_comes_back_and_a_restart_leaves_the_agents() {
        let text = unit().text();
        // `on-failure` would leave a killed server down, and the default
        // kill mode would take Claude Code's daemon with a restart.
        assert_eq!(value(&text, "Restart"), "always");
        assert_eq!(value(&text, "KillMode"), "process");
    }

    #[test]
    fn a_path_systemd_would_read_differently_is_escaped() {
        let text = Unit {
            binary: "/home/me/100% $done/treetop".into(),
            workspace: "/home/me/100% done".into(),
            port: None,
            path: "/home/me/\"odd\\x\"$:/usr/bin".into(),
        }
        .text();
        assert_eq!(
            value(&text, "ExecStart"),
            "\"/home/me/100%% $done/treetop\" serve"
        );
        assert_eq!(value(&text, "WorkingDirectory"), "/home/me/100%% done");
        assert_eq!(
            value(&text, "Environment"),
            "\"PATH=/home/me/\\\"odd\\\\x\\\"$:/usr/bin\""
        );
    }
}
