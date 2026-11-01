//! `p`: a tree's port on the phone. The share command, `tether share`
//! unless the config names another, makes the port reachable from the
//! phone, as tether does over the tailnet, and prints the address it is
//! reached at, which treetop shows as a QR code to scan with the phone's
//! camera.

use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use qrcode::{Color, QrCode};

use crate::child::{self, End};

/// What a port is shared with when the config names nothing.
const DEFAULT: [&str; 2] = ["tether", "share"];

/// How long the share command has to print an address. `tether share`
/// answers in a second or two, but on a tailnet without Serve it prints a
/// link to enable it and waits, which treetop cannot click for you.
const GRACE: Duration = Duration::from_secs(30);

/// A share command running on a worker thread, so the screen keeps drawing
/// while it works.
pub struct Sharing {
    pub port: u16,
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
    let (program, args) = command(share, port);
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let _ = done.send(run(&program, &args, GRACE));
    });
    Sharing { port, result }
}

/// The program and arguments that share `port`: each `{port}` in the
/// configured words replaced, else the port added last.
fn command(share: &[String], port: u16) -> (String, Vec<String>) {
    let words: Vec<String> = if share.is_empty() {
        DEFAULT.map(String::from).to_vec()
    } else {
        share.to_vec()
    };
    let port = port.to_string();
    let mut words: Vec<String> = if words.iter().any(|word| word.contains("{port}")) {
        words
            .iter()
            .map(|word| word.replace("{port}", &port))
            .collect()
    } else {
        words.into_iter().chain([port]).collect()
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
            command(&[], 5173),
            ("tether".into(), words(&["share", "5173"]))
        );
        let custom = words(&["share-it", "--port={port}", "--https"]);
        assert_eq!(
            command(&custom, 5173),
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
}
