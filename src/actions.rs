//! Entering a tree outside tmux: the one thing that still takes the terminal,
//! because a shell needs it. Returning and destroying run in `jobs`.

use std::path::Path;
use std::process::Command;

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
