//! Entering a tree outside tmux, the one thing that still takes the terminal
//! because a shell needs it, and opening a tree's port in the browser.
//! Returning and destroying run in `jobs`; an agent's chat runs in `chat`.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};

/// Runs the user's shell in the tree. treetop's own cwd stays in the main
/// checkout, so a later return or destroy of this tree never hits treetop.
pub fn shell(path: &Path) {
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "sh".into());
    eprintln!("treetop: shell in {}; exit to come back", path.display());
    match Command::new(shell).current_dir(path).status() {
        Ok(status) if !status.success() => eprintln!("shell exited with {status}"),
        Ok(_) => {}
        Err(err) => eprintln!("shell: {err}"),
    }
}

/// Opens `http://localhost:<port>`: with the config's `browser` when it
/// names one, else in the desktop's browser when there is a desktop, else,
/// as on a server reached over SSH, by copying the URL to the clipboard of
/// the computer the terminal runs on.
pub fn browse(port: u16, tree: &str) -> Opening {
    let url = format!("http://localhost:{port}");
    open(&url, format!(" from tree {tree}"))
}

/// Opens `url` the way `browse` opens a port.
pub fn open_url(url: &str) -> Opening {
    open(url, String::new())
}

fn open(url: &str, from: String) -> Opening {
    let browser = crate::config::load()
        .map(|config| config.browser)
        .unwrap_or_default();
    open_with(&browser, url, from)
}

/// What opening a URL came to, for the status line: said at once, or a
/// configured browser still running on a worker thread.
pub enum Opening {
    Said(String),
    Pending(Pending),
}

/// A configured browser deciding off the UI thread whether it opened the
/// URL, which a forwarder such as `tether open` takes seconds to know.
pub struct Pending {
    url: String,
    program: String,
    from: String,
    result: mpsc::Receiver<Result<(), String>>,
}

impl Opening {
    /// What to say at once: the outcome, or that the browser is running.
    pub fn said_now(&self) -> String {
        match self {
            Opening::Said(said) => said.clone(),
            Opening::Pending(pending) => format!(
                "opening {} in {}{}",
                pending.url, pending.program, pending.from
            ),
        }
    }
}

impl Pending {
    /// None while the browser runs; then what happened. A browser that
    /// failed has the URL copied to the clipboard instead, from this thread,
    /// which is the one drawing the terminal.
    pub fn poll(&self) -> Option<String> {
        let result = match self.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(format!("{} vanished", self.program)),
        };
        let (url, from) = (&self.url, &self.from);
        Some(match result {
            Ok(()) => format!("opened {url} in {}{from}", self.program),
            Err(err) => match copy(url) {
                Ok(()) => format!("{err}; copied {url}"),
                Err(copy_err) => format!("{err}; {copy_err:#}"),
            },
        })
    }
}

/// Opens `url` with `browser`, the configured command, on a worker thread
/// when it names one; else with the desktop's opener, or the clipboard.
fn open_with(browser: &[String], url: &str, from: String) -> Opening {
    if let Some((program, args)) = browser_command(browser, url) {
        let (done, result) = mpsc::channel();
        let name = program.clone();
        thread::spawn(move || {
            let ran = run_browser(&name, &args, BROWSER_GRACE);
            let _ = done.send(ran.map_err(|err| format!("{err:#}")));
        });
        return Opening::Pending(Pending {
            url: url.to_string(),
            program,
            from,
            result,
        });
    }
    if has_desktop() && open_in_browser(url).is_ok() {
        return Opening::Said(format!("opened {url}{from}"));
    }
    Opening::Said(match copy(url) {
        Ok(()) => format!("copied {url} to the clipboard{from}"),
        Err(err) => format!("{err:#}"),
    })
}

/// Sets the terminal's clipboard to `url`.
fn copy(url: &str) -> Result<()> {
    let mut stdout = std::io::stdout();
    stdout
        .write_all(osc52(url).as_bytes())
        .and_then(|()| stdout.flush())
        .context("copying the URL")
}

/// The program and arguments that open `url` with the configured `browser`:
/// each `{url}` in its words replaced, else the URL added last. None when
/// no browser is configured.
fn browser_command(browser: &[String], url: &str) -> Option<(String, Vec<String>)> {
    let (program, rest) = browser.split_first()?;
    let mut args: Vec<String> = rest.iter().map(|arg| arg.replace("{url}", url)).collect();
    if !browser.iter().any(|word| word.contains("{url}")) {
        args.push(url.to_string());
    }
    Some((program.replace("{url}", url), args))
}

/// A desktop session to open a browser in: macOS, or a graphical session's
/// display on Linux.
fn has_desktop() -> bool {
    cfg!(target_os = "macos")
        || ["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()))
}

/// Opens `url` with the desktop's opener, `open` on macOS and `xdg-open`
/// elsewhere, detached from the terminal treetop draws on.
fn open_in_browser(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    spawn_detached(opener, &[url.to_string()])
}

/// How long a configured browser has to fail before it counts as opened:
/// `tether open` gives up within 3 seconds when no laptop answers, and a
/// browser that keeps running is left to run. It runs on a worker thread,
/// so the wait never holds up a frame.
const BROWSER_GRACE: Duration = Duration::from_secs(5);

/// Runs the configured browser, an error when it exits unsuccessfully within
/// `grace`, with the first line it wrote to stderr as the reason.
fn run_browser(program: &str, args: &[String], grace: Duration) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    let deadline = Instant::now() + grace;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let mut said = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut said);
                }
                let reason = said.lines().map(str::trim).find(|l| !l.is_empty());
                // A reason that already names the program, as
                // `tether: no browser connected` does, is said as it is.
                let named = Path::new(program)
                    .file_name()
                    .map(|name| format!("{}:", name.to_string_lossy()));
                return Err(match reason {
                    Some(reason) if named.is_some_and(|n| reason.starts_with(&n)) => {
                        anyhow!("{reason}")
                    }
                    Some(reason) => anyhow!("{program} failed: {reason}"),
                    None => anyhow!("{program} failed ({status})"),
                });
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            // Still running, as a browser it started itself does: leave it,
            // reaped off the UI thread. Its stderr is dropped, so a full pipe
            // never stalls it.
            Ok(None) => {
                drop(child.stderr.take());
                thread::spawn(move || child.wait());
                return Ok(());
            }
            Err(err) => return Err(err).with_context(|| format!("waiting on {program}")),
        }
    }
}

/// Starts `program` with `args`, detached from the terminal treetop draws
/// on.
fn spawn_detached(program: &str, args: &[String]) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("could not run {program}"))?;
    // Reaped off the UI thread, so a slow opener never holds up a frame.
    thread::spawn(move || child.wait());
    Ok(())
}

/// The OSC 52 sequence that sets the terminal's clipboard to `text`. tmux
/// passes it on to the terminal it runs in when `set-clipboard` is on, and
/// over SSH that terminal is the one on the computer being typed at.
fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Standard base64 with padding, for OSC 52.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let triple = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(triple >> (18 - 6 * i) & 0x3f) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64_with_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(
            base64(b"http://localhost:5223"),
            "aHR0cDovL2xvY2FsaG9zdDo1MjIz"
        );
    }

    #[test]
    fn a_configured_browser_gets_the_url_where_it_says_else_last() {
        let words = |list: &[&str]| list.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        let url = "http://localhost:5223";
        assert_eq!(browser_command(&[], url), None);
        assert_eq!(
            browser_command(&words(&["chromium", "--user-data-dir=/p"]), url),
            Some(("chromium".into(), words(&["--user-data-dir=/p", url])))
        );
        assert_eq!(
            browser_command(&words(&["browse", "--app={url}", "--new"]), url),
            Some((
                "browse".into(),
                words(&["--app=http://localhost:5223", "--new"])
            ))
        );
    }

    /// What `opening` came to, once its browser has decided.
    fn settle(opening: Opening) -> String {
        let Opening::Pending(pending) = opening else {
            panic!("a configured browser runs off the UI thread");
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(said) = pending.poll() {
                return said;
            }
            assert!(Instant::now() < deadline, "the browser never settled");
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn opens_the_url_with_the_configured_browser_even_without_a_desktop() {
        let dir = std::env::temp_dir().join(format!("treetop-browser-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let got = dir.join("url");
        let browser = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!("printf %s \"$0\" > {}", got.display()),
        ];
        let opening = open_with(&browser, "http://localhost:5223", " from tree 3".into());
        assert_eq!(
            opening.said_now(),
            "opening http://localhost:5223 in sh from tree 3"
        );
        assert_eq!(
            settle(opening),
            "opened http://localhost:5223 in sh from tree 3"
        );
        assert_eq!(
            std::fs::read_to_string(&got).unwrap(),
            "http://localhost:5223"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_browser_that_fails_slowly_falls_back_to_the_clipboard_without_holding_the_screen() {
        // As `tether open` does with no laptop answering: it gives up after
        // its two-second wait.
        let browser = [
            "sh",
            "-c",
            "sleep 2; echo 'tether: no browser connected' >&2; exit 1",
        ]
        .map(String::from);
        let started = Instant::now();
        let opening = open_with(&browser, "http://localhost:5223", String::new());
        assert!(started.elapsed() < Duration::from_millis(500));
        assert_eq!(
            settle(opening),
            "sh failed: tether: no browser connected; copied http://localhost:5223"
        );
    }

    #[test]
    fn a_reason_that_names_the_browser_is_said_as_it_is() {
        let dir = std::env::temp_dir().join(format!("treetop-named-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tether = dir.join("tether");
        std::fs::write(
            &tether,
            "#!/bin/sh\necho 'tether: no browser connected' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&tether, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let err = run_browser(tether.to_str().unwrap(), &[], Duration::from_secs(5)).unwrap_err();
        assert_eq!(format!("{err:#}"), "tether: no browser connected");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_browser_that_keeps_running_counts_as_opened() {
        let browser = ["-c".to_string(), "sleep 5".to_string()];
        let started = Instant::now();
        run_browser("sh", &browser, Duration::from_millis(200)).unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_browser_that_will_not_start_falls_back_to_the_clipboard() {
        let browser = vec!["/nonexistent/browser".to_string()];
        let said = settle(open_with(&browser, "http://localhost:5223", String::new()));
        assert!(
            said.starts_with("could not run /nonexistent/browser"),
            "{said}"
        );
        assert!(said.ends_with("; copied http://localhost:5223"), "{said}");
    }

    #[test]
    fn wraps_the_url_in_an_osc52_clipboard_sequence() {
        assert_eq!(osc52("foo"), "\x1b]52;c;Zm9v\x07");
    }
}
