//! `p`: a tree's port on the phone. The share command, `tether share`
//! unless the config names another, makes the port reachable from the
//! phone, as tether does over the tailnet, and prints the address it is
//! reached at, which treetop shows as a QR code to scan with the phone's
//! camera.
//!
//! `P`: the same port on the internet, for anyone with the link. The publish
//! command, `tether share --public` unless the config names another, puts
//! it there with Tailscale Funnel, and the unpublish command, `tether
//! unshare`, takes it down again.
//!
//! Which ports are shared either way is read back from the shares command,
//! `tether shares` unless the config names another, so the PORTS column
//! marks them whoever shared them and whenever: a share outlives treetop,
//! since it lives in Tailscale, not here.

use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use qrcode::{Color, QrCode};

use crate::child::{self, End};

/// What a port is shared with when the config names nothing.
const DEFAULT: &[&str] = &["tether", "share"];

/// What a port is published with when the config names nothing.
const DEFAULT_PUBLISH: &[&str] = &["tether", "share", "--public"];

/// What a published port is taken down with when the config names nothing.
const DEFAULT_UNPUBLISH: &[&str] = &["tether", "unshare"];

/// What the shared ports are listed with when the config names nothing.
const DEFAULT_SHARES: &[&str] = &["tether", "shares"];

/// How long the shares command has to list them. It only reads Tailscale's
/// serve config, so it answers at once or not at all; the PORTS column waits
/// on nothing.
const LISTING: Duration = Duration::from_secs(5);

/// How long the share command has to print an address. `tether share`
/// answers in a second or two, but on a tailnet without Serve it prints a
/// link to enable it and waits, which treetop cannot click for you.
const GRACE: Duration = Duration::from_secs(30);

/// How many modules of light margin a QR code wants round it to scan, as
/// the standard asks; the popup draws less where the screen has no room.
pub const QUIET_ZONE: usize = 4;

const BLACK_ON_WHITE: &str = "\x1b[38;5;16;48;5;231m";
const RESET: &str = "\x1b[0m";

/// A share command running on a worker thread, so the screen keeps drawing
/// while it works.
pub struct Sharing {
    pub port: u16,
    /// On the internet, for anyone with the link, not only the tailnet.
    pub is_public: bool,
    result: mpsc::Receiver<Result<String, String>>,
}

impl Sharing {
    /// None while the command runs; then the address, or why there is none.
    pub fn poll(&self) -> Option<Result<String, String>> {
        match self.result.try_recv() {
            Ok(result) => Some(result),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err("the share command vanished".into())),
        }
    }
}

/// Shares `port` with `share`, the configured command, off this thread.
pub fn start(share: &[String], port: u16) -> Sharing {
    spawn(command(share, DEFAULT, port), port, false)
}

/// Publishes `port` to the internet with `publish`, the configured
/// command, off this thread.
pub fn publish(publish: &[String], port: u16) -> Sharing {
    spawn(command(publish, DEFAULT_PUBLISH, port), port, true)
}

fn spawn((program, args): (String, Vec<String>), port: u16, is_public: bool) -> Sharing {
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(run(&program, &args, GRACE));
    });
    Sharing {
        port,
        is_public,
        result,
    }
}

/// Takes `port` off the phone and the internet both with `unpublish`, the
/// configured command, off this thread; the receiver gets what came of it,
/// to show. One command does both because `tether unshare` does: a port
/// taken off the internet loses its tailnet share with it.
pub fn unshare(unpublish: &[String], port: u16) -> mpsc::Receiver<String> {
    let (program, args) = command(unpublish, DEFAULT_UNPUBLISH, port);
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let said = match stop(&program, &args, GRACE) {
            Ok(()) => format!("unshared :{port}"),
            Err(err) => format!("could not unshare :{port}: {err}"),
        };
        let _ = done.send(said);
    });
    result
}

/// Runs the unpublish command to its end.
fn stop(program: &str, args: &[String], grace: Duration) -> Result<(), String> {
    let mut command = Command::new(program);
    command.args(args);
    let ran = child::run(&mut command, None, grace)
        .map_err(|err| format!("could not run {program}: {err}"))?;
    let status = match ran.end {
        End::Exited(status) => status,
        End::TimedOut => {
            return Err(format!(
                "{program} did not finish within {}s",
                grace.as_secs()
            ));
        }
        End::HeldOpen(_) => return Err(format!("{program} left its output open")),
    };
    if status.success() {
        Ok(())
    } else {
        Err(crate::actions::failure(
            program,
            &String::from_utf8_lossy(&ran.stderr),
            &status.to_string(),
        ))
    }
}

/// A port this machine shares: the port a dev server listens on, and
/// whether it is on the internet, as `P` puts it there, rather than only
/// the tailnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shared {
    pub port: u16,
    pub is_public: bool,
    /// The HTTPS address it is reached at, which `Q` draws as a QR code
    /// without asking the share command for it again.
    pub address: String,
}

/// Every port this machine shares, as `listing`, the configured command,
/// lists them; `tether shares` where it names none. Shares made outside
/// treetop, or before it started, are in it too: Tailscale holds them, not
/// treetop.
pub fn shares(listing: &[String]) -> Result<Vec<Shared>, String> {
    let (program, args) = parts(listing, DEFAULT_SHARES);
    let mut command = Command::new(&program);
    command.args(&args);
    let ran = child::run(&mut command, None, LISTING)
        .map_err(|err| format!("could not run {program}: {err}"))?;
    match ran.end {
        End::Exited(status) if status.success() => {}
        End::Exited(status) => {
            return Err(crate::actions::failure(
                &program,
                &String::from_utf8_lossy(&ran.stderr),
                &status.to_string(),
            ));
        }
        End::TimedOut => {
            return Err(format!(
                "{program} did not finish within {}s",
                LISTING.as_secs()
            ));
        }
        End::HeldOpen(_) => return Err(format!("{program} left its output open")),
    }
    Ok(listed(&String::from_utf8_lossy(&ran.stdout)))
}

/// The ports in what the shares command printed: a line an address, as
/// `https://host:5173 -> http://localhost:5173`, with ` (public)` on the
/// ones Funnel puts on the internet. A port shared both ways is public,
/// which is the reach worth showing.
fn listed(printed: &str) -> Vec<Shared> {
    let mut shares: Vec<Shared> = Vec::new();
    for line in printed.lines() {
        let Some((address, target)) = line.split_once("->") else {
            continue;
        };
        let (target, is_public) = match target.trim().strip_suffix("(public)") {
            Some(rest) => (rest.trim(), true),
            None => (target.trim(), false),
        };
        let Some(port) = target_port(target) else {
            continue;
        };
        let share = Shared {
            port,
            is_public,
            address: address.trim().to_string(),
        };
        match shares.iter_mut().find(|seen| seen.port == port) {
            // The public address is the one worth keeping: it is what
            // anyone with the link opens.
            Some(seen) if is_public && !seen.is_public => *seen = share,
            Some(_) => {}
            None => shares.push(share),
        }
    }
    shares
}

/// The port a share forwards to: the one its target names, as
/// `http://localhost:5173` or `localhost:5173/app` name 5173.
fn target_port(target: &str) -> Option<u16> {
    let rest = target.split_once("://").map_or(target, |(_, rest)| rest);
    let (_, port) = rest.split('/').next()?.rsplit_once(':')?;
    port.parse().ok()
}

/// Shares `port` with `share`, the configured command, and returns its
/// address, or why there is none, once the command has answered.
pub fn share(share: &[String], port: u16) -> Result<String, String> {
    let (program, args) = command(share, DEFAULT, port);
    run(&program, &args, GRACE)
}

/// The program and arguments that act on `port`: the configured words, or
/// `default` where there are none, with each `{port}` replaced, else the
/// port added last.
fn command(configured: &[String], default: &[&str], port: u16) -> (String, Vec<String>) {
    let (program, args) = parts(configured, default);
    let port = port.to_string();
    let all: Vec<String> = std::iter::once(program).chain(args).collect();
    let mut all: Vec<String> = if all.iter().any(|word| word.contains("{port}")) {
        all.iter()
            .map(|word| word.replace("{port}", &port))
            .collect()
    } else {
        all.into_iter().chain([port]).collect()
    };
    let program = all.remove(0);
    (program, all)
}

/// The program and arguments of a command that takes no port: the
/// configured words, or `default` where there are none.
fn parts(configured: &[String], default: &[&str]) -> (String, Vec<String>) {
    let mut words: Vec<String> = if configured.is_empty() {
        default.iter().map(|word| word.to_string()).collect()
    } else {
        configured.to_vec()
    };
    let program = words.remove(0);
    (program, words)
}

/// Runs the share command and takes the first `http` line it prints as the
/// address; else an error with what it wrote to stderr.
fn run(program: &str, args: &[String], grace: Duration) -> Result<String, String> {
    let mut command = Command::new(program);
    command.args(args);
    let ran = child::run(&mut command, None, grace)
        .map_err(|err| format!("could not run {program}: {err}"))?;
    let stderr = String::from_utf8_lossy(&ran.stderr);
    let status = match ran.end {
        End::Exited(status) => status,
        End::TimedOut => {
            // All of it, on one line: a link to follow may be on any.
            let said = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
            let late = format!("{program} printed no address within {}s", grace.as_secs());
            return Err(if said.is_empty() {
                late
            } else {
                format!("{late}: {said}")
            });
        }
        End::HeldOpen(_) => return Err(format!("{program} left its output open")),
    };
    if !status.success() {
        return Err(crate::actions::failure(
            program,
            &stderr,
            &status.to_string(),
        ));
    }
    String::from_utf8_lossy(&ran.stdout)
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("http://") || line.starts_with("https://"))
        .map(String::from)
        .ok_or_else(|| format!("{program} printed no address"))
}

/// A QR code's modules, without the quiet zone round it, which is drawn
/// with the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code {
    pub width: usize,
    modules: Vec<bool>,
}

impl Code {
    /// The QR code of `text`.
    pub fn new(text: &str) -> Result<Code, String> {
        let code = QrCode::new(text.as_bytes()).map_err(|err| format!("no QR code: {err}"))?;
        Ok(Code {
            width: code.width(),
            modules: code
                .to_colors()
                .into_iter()
                .map(|color| color == Color::Dark)
                .collect(),
        })
    }

    /// Whether the module at column `x`, row `y` is dark; outside the code
    /// is the light quiet zone.
    pub fn is_dark(&self, x: isize, y: isize) -> bool {
        let (Ok(x), Ok(y)) = (usize::try_from(x), usize::try_from(y)) else {
            return false;
        };
        x < self.width && y < self.width && self.modules[y * self.width + x]
    }

    /// The code for a terminal, two modules to a character, with its quiet
    /// zone. Black on white whatever the terminal's colours, as the popup
    /// draws it, which every camera reads.
    pub fn text(&self) -> String {
        let quiet = QUIET_ZONE as isize;
        let side = self.width as isize + quiet;
        let mut text = String::new();
        for y in (-quiet..side).step_by(2) {
            text.push_str(BLACK_ON_WHITE);
            for x in -quiet..side {
                text.push(match (self.is_dark(x, y), self.is_dark(x, y + 1)) {
                    (false, false) => ' ',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (true, true) => '█',
                });
            }
            text.push_str(RESET);
            text.push('\n');
        }
        text
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn shares_with_tether_unless_the_config_names_a_command() {
        assert_eq!(
            command(&[], DEFAULT, 5173),
            ("tether".into(), words(&["share", "5173"]))
        );
        assert_eq!(
            command(&[], DEFAULT_PUBLISH, 5173),
            ("tether".into(), words(&["share", "--public", "5173"]))
        );
        assert_eq!(
            command(&[], DEFAULT_UNPUBLISH, 5173),
            ("tether".into(), words(&["unshare", "5173"]))
        );
        let custom = words(&["share-it", "--port={port}", "--https"]);
        assert_eq!(
            command(&custom, DEFAULT, 5173),
            ("share-it".into(), words(&["--port=5173", "--https"]))
        );
    }

    #[test]
    fn the_address_is_the_first_url_the_command_prints() {
        let script = "echo 'Serving on your tailnet'; echo '  https://box.tail1.ts.net:5173  '; \
                      echo 'https://other'";
        let got = run("sh", &words(&["-c", script]), GRACE);
        assert_eq!(got, Ok("https://box.tail1.ts.net:5173".into()));
    }

    #[test]
    fn a_failing_command_says_why() {
        let script = "echo 'tailscale is not running' >&2; exit 1";
        let got = run("sh", &words(&["-c", script]), GRACE);
        assert_eq!(got, Err("sh failed: tailscale is not running".into()));
        let got = run("sh", &words(&["-c", "echo done"]), GRACE);
        assert_eq!(got, Err("sh printed no address".into()));
        let got = run("/nonexistent/tether", &[], GRACE).unwrap_err();
        assert!(
            got.starts_with("could not run /nonexistent/tether"),
            "{got}"
        );
    }

    #[test]
    fn the_shared_ports_are_read_back_with_which_are_public() {
        assert_eq!(
            parts(&[], DEFAULT_SHARES),
            ("tether".into(), words(&["shares"]))
        );
        let printed = "https://box.tail1.ts.net:5173 -> http://localhost:5173\n\
                       https://box.tail1.ts.net -> http://localhost:3000 (public)\n";
        assert_eq!(
            listed(printed),
            vec![
                Shared {
                    port: 5173,
                    is_public: false,
                    address: "https://box.tail1.ts.net:5173".into(),
                },
                Shared {
                    port: 3000,
                    is_public: true,
                    address: "https://box.tail1.ts.net".into(),
                },
            ]
        );
    }

    #[test]
    fn a_port_shared_both_ways_is_public_and_a_line_without_a_port_is_skipped() {
        // `tether shares` says so on stderr when there are none, and a share
        // can forward somewhere that is not a local port.
        let printed = "https://box.tail1.ts.net:5173 -> http://localhost:5173\n\
                       https://box.tail1.ts.net -> http://localhost:5173 (public)\n\
                       https://box.tail1.ts.net -> text proxy\n";
        assert_eq!(
            listed(printed),
            vec![Shared {
                port: 5173,
                is_public: true,
                // The public address, not the tailnet one it also has.
                address: "https://box.tail1.ts.net".into(),
            }]
        );
        assert_eq!(listed(""), vec![]);
    }

    #[test]
    fn a_shares_command_that_fails_says_why() {
        let got = shares(&words(&[
            "sh",
            "-c",
            "echo 'tailscale is down' >&2; exit 1",
        ]));
        assert_eq!(got, Err("sh failed: tailscale is down".into()));
        let script = "echo 'https://box.tail1.ts.net -> http://localhost:5173 (public)'";
        assert_eq!(
            shares(&words(&["sh", "-c", script])),
            Ok(vec![Shared {
                port: 5173,
                is_public: true,
                address: "https://box.tail1.ts.net".into(),
            }])
        );
    }

    #[test]
    fn unsharing_says_whether_the_port_came_down() {
        let said = |script: &str| {
            unshare(&words(&["sh", "-c", script]), 5173)
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
        };
        assert_eq!(said("exit 0"), "unshared :5173");
        assert_eq!(
            said("echo 'tailscale is not running' >&2; exit 1"),
            "could not unshare :5173: sh failed: tailscale is not running"
        );
    }

    #[test]
    fn a_command_that_waits_is_stopped_with_what_it_asked() {
        // As `tether share` does on a tailnet without Serve: it prints a link
        // to enable it and waits.
        let script = "echo 'Serve is not enabled. Visit:' >&2; \
                      echo 'https://login.tailscale.com/f/serve' >&2; sleep 30";
        let started = Instant::now();
        let got = run("sh", &words(&["-c", script]), Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            got,
            Err(
                "sh printed no address within 1s: Serve is not enabled. Visit: \
                 https://login.tailscale.com/f/serve"
                    .into()
            )
        );
    }

    #[test]
    fn the_code_is_the_qr_code_of_the_address() {
        let url = "https://box.tail1.ts.net:5173";
        let code = Code::new(url).unwrap();
        let qr = QrCode::new(url.as_bytes()).unwrap();
        assert_eq!(code.width, qr.width());
        // The finder pattern's dark corner, and the quiet zone round it.
        assert!(code.is_dark(0, 0));
        assert!(!code.is_dark(-1, 0));
        assert!(!code.is_dark(0, code.width as isize));
    }

    #[test]
    fn the_terminal_code_reads_back_as_the_code() {
        let code = Code::new("https://box.tail1.ts.net:7777").unwrap();
        let text = code.text();
        let rows: Vec<Vec<char>> = text
            .lines()
            .map(|line| {
                let line = line.strip_prefix(BLACK_ON_WHITE).unwrap();
                line.strip_suffix(RESET).unwrap().chars().collect()
            })
            .collect();
        let quiet = QUIET_ZONE as isize;
        // Every module of the code and its quiet zone, top then bottom half
        // of the character it falls in.
        for y in -quiet..code.width as isize + quiet {
            for x in -quiet..code.width as isize + quiet {
                let row = usize::try_from((y + quiet) / 2).unwrap();
                let shown = rows[row][usize::try_from(x + quiet).unwrap()];
                let dark = if (y + quiet) % 2 == 0 {
                    "▀█"
                } else {
                    "▄█"
                };
                assert_eq!(dark.contains(shown), code.is_dark(x, y), "({x}, {y})");
            }
        }
    }
}
