//! Entering a tree outside tmux, the one thing that still takes the terminal
//! because a shell needs it, and opening a tree's port in the browser.
//! Returning and destroying run in `jobs`; an agent's chat runs in `chat`.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result};

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

/// Opens `http://localhost:<port>`: in the desktop's browser when there is
/// a desktop, else, as on a server reached over SSH, by copying the URL to
/// the clipboard of the computer the terminal runs on. Returns what
/// happened, for the status line.
pub fn browse(port: u16, tree: &str) -> Result<String> {
    let url = format!("http://localhost:{port}");
    if has_desktop() && open_in_browser(&url).is_ok() {
        return Ok(format!("opened {url} from tree {tree}"));
    }
    let mut stdout = std::io::stdout();
    stdout
        .write_all(osc52(&url).as_bytes())
        .and_then(|()| stdout.flush())
        .context("copying the URL")?;
    Ok(format!("copied {url} from tree {tree} to the clipboard"))
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
    let mut child = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {opener}"))?;
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
    fn wraps_the_url_in_an_osc52_clipboard_sequence() {
        assert_eq!(osc52("foo"), "\x1b]52;c;Zm9v\x07");
    }
}
