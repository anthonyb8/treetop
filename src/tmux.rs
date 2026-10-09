//! Entering a tree inside tmux: a window per tree, named for it, so treetop
//! stays open in its own window instead of handing over the terminal.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::pool::Tree;

/// treetop is running inside a tmux client.
pub fn is_inside() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

/// The branch's last segment, which carries the issue ID and slug, else the
/// pool number.
fn window_name(tree: &Tree) -> String {
    match tree.branch.as_deref().and_then(|b| b.rsplit('/').next()) {
        Some(leaf) if !leaf.is_empty() => leaf.to_string(),
        _ => format!("tree {}", tree.name),
    }
}

/// The first window in `panes` (`list-panes -F '#{window_id}\t#{pane_current_path}'`
/// output) with a pane standing in `tree`.
fn window_in(panes: &str, tree: &Path) -> Option<String> {
    panes.lines().find_map(|line| {
        let (id, path) = line.split_once('\t')?;
        Path::new(path).starts_with(tree).then(|| id.to_string())
    })
}

fn tmux(args: &[&str]) -> Result<String> {
    let out = Command::new("tmux")
        .args(args)
        .output()
        .context("running tmux")?;
    if !out.status.success() {
        bail!(
            "tmux {}: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Switches to a window of this session already in the tree, else opens one
/// there. Returns what happened, for the status line.
pub fn open(tree: &Tree) -> Result<String> {
    let panes = tmux(&[
        "list-panes",
        "-s",
        "-F",
        "#{window_id}\t#{pane_current_path}",
    ])?;
    if let Some(id) = window_in(&panes, &tree.path) {
        tmux(&["select-window", "-t", &id])?;
        return Ok(format!(
            "switched to the window already in tree {}",
            tree.name
        ));
    }
    let name = window_name(tree);
    let path = tree.path.to_string_lossy();
    tmux(&["new-window", "-n", &name, "-c", &path])?;
    Ok(format!("opened tree {} in window {name}", tree.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;

    fn trees() -> Vec<Tree> {
        parse(
            r#"[
          {"name":"4","status":"leased","branch":"feature/end-1-a","lease_holder":"anthony","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"7","status":"leased","branch":"","lease_holder":"anthony","path":"/p/7/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap()
    }

    #[test]
    fn names_the_window_for_the_branch_leaf_or_the_pool_number() {
        let trees = trees();
        assert_eq!(window_name(&trees[0]), "end-1-a");
        assert_eq!(window_name(&trees[1]), "tree 7");
    }

    #[test]
    fn finds_a_window_with_a_pane_inside_the_tree() {
        let panes = "@1\t/home/me\n@3\t/p/40/app\n@5\t/p/4/app/ui/src\n";
        assert_eq!(window_in(panes, Path::new("/p/4/app")), Some("@5".into()));
        assert_eq!(window_in(panes, Path::new("/p/7/app")), None);
    }
}
