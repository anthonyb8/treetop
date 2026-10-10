//! Entering a tree outside tmux, the one thing that still takes the terminal
//! because a shell needs it, and opening a tree's port in the browser.
//! Returning and destroying run in `jobs`.

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

/// Opens `http://localhost:<port>` with the desktop's opener, `open` on macOS
/// and `xdg-open` elsewhere, detached from the terminal treetop draws on.
/// Returns what happened, for the status line.
pub fn browse(port: u16, tree: &str) -> Result<String> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let url = format!("http://localhost:{port}");
    let mut child = Command::new(opener)
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {opener}"))?;
    // Reaped off the UI thread, so a slow opener never holds up a frame.
    thread::spawn(move || child.wait());
    Ok(format!("opened {url} from tree {tree}"))
}
