//! Returning and destroying, run with the TUI suspended so treehouse's own
//! prompts and output reach the terminal. Child stdout goes to stderr: stdout
//! is reserved for the path printed on Enter, which a shell wrapper captures.

use std::io::{self, BufRead, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use crate::app::{Action, Pending};
use crate::pool::Tree;

fn run(command: &mut Command) -> io::Result<ExitStatus> {
    command.stdout(Stdio::from(io::stderr())).status()
}

fn report(what: &str, result: io::Result<ExitStatus>) {
    match result {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("{what} exited with {status}"),
        Err(err) => eprintln!("{what}: {err}"),
    }
}

/// Stops the tree's stack, as `thr` does, so no dev server outlives the tree.
/// Without the harness on PATH there is no stack to stop.
fn stack_down(path: &Path) {
    match run(Command::new("harness")
        .args(["stack", "down"])
        .current_dir(path))
    {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        result => report("harness stack down", result),
    }
}

fn ask(prompt: &str) -> bool {
    eprint!("{prompt} [y/N] ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).is_ok() && matches!(answer.trim(), "y" | "Y" | "yes")
}

fn heading(tree: &Tree) {
    let branch = tree.branch.as_deref().unwrap_or("(detached)");
    eprintln!("\n== tree {}  {branch}", tree.name);
}

/// Releases the lease and hands the tree back to the pool. treehouse still
/// asks before it discards uncommitted changes.
fn return_tree(tree: &Tree) {
    heading(tree);
    stack_down(&tree.path);
    report(
        "treehouse return",
        run(Command::new("treehouse").arg("return").arg(&tree.path)),
    );
}

/// Removes the tree from disk. treehouse prints a dry run naming every risk
/// first, and nothing is removed unless that preview is confirmed here.
fn destroy_tree(tree: &Tree) {
    const INCLUDE: [&str; 3] = ["--include-leased", "--include-in-use", "--include-unlanded"];
    heading(tree);
    report(
        "treehouse destroy (preview)",
        run(Command::new("treehouse")
            .arg("destroy")
            .arg(&tree.path)
            .args(INCLUDE)),
    );
    if !ask(&format!(
        "Destroy tree {} as previewed? This cannot be undone.",
        tree.name
    )) {
        eprintln!("kept tree {}", tree.name);
        return;
    }
    stack_down(&tree.path);
    report(
        "treehouse destroy",
        run(Command::new("treehouse")
            .arg("destroy")
            .arg(&tree.path)
            .args(INCLUDE)
            .arg("--yes")),
    );
}

pub fn perform(pending: &Pending) {
    for tree in &pending.trees {
        match pending.action {
            Action::Return => return_tree(tree),
            Action::Destroy => destroy_tree(tree),
        }
    }
    eprint!("\nPress Enter to go back to treetop ");
    let _ = io::stderr().flush();
    let _ = io::stdin().lock().read_line(&mut String::new());
}
