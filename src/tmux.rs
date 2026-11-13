//! Entering a tree inside tmux: the pane its agent runs in, else a window per
//! tree, named for it, so treetop stays open in its own window instead of
//! handing over the terminal.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::live;
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

/// A tmux pane, by the ids tmux targets it with.
#[derive(Debug, PartialEq, Eq)]
struct Pane {
    session: String,
    window: String,
    pane: String,
}

/// The pane in `panes` (`list-panes -a -F
/// '#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}'` output) whose
/// process is one of `lineage`: an agent runs as its pane's process or a child
/// or grandchild of the pane's shell. Matching by process rather than by a
/// pane id the agent recorded means a pane reused since, by a later agent or
/// anything else, never claims it.
fn pane_in(panes: &str, lineage: &[u32]) -> Option<Pane> {
    panes.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let (session, window, pane) = (fields.next()?, fields.next()?, fields.next()?);
        let pid: u32 = fields.next()?.parse().ok()?;
        lineage.contains(&pid).then(|| Pane {
            session: session.to_string(),
            window: window.to_string(),
            pane: pane.to_string(),
        })
    })
}

/// The pid, its parent and its grandparent.
fn lineage(pid: u32) -> Vec<u32> {
    let mut lineage = vec![pid];
    for _ in 0..2 {
        let Some(stat) = lineage.last().and_then(|&pid| live::stat(pid)) else {
            break;
        };
        lineage.push(stat.ppid);
    }
    lineage
}

/// Switches to the pane of the first agent in the tree that has one, in
/// whichever tmux session it is. None when no agent runs in a pane, as a
/// background session does not.
fn open_agent(tree: &Tree) -> Result<Option<String>> {
    let agents: Vec<_> = tree.agents.iter().filter(|a| !a.is_background).collect();
    if agents.is_empty() {
        return Ok(None);
    }
    let panes = tmux(&[
        "list-panes",
        "-a",
        "-F",
        "#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}",
    ])?;
    for agent in agents {
        let Some(pid) = agent.pid else { continue };
        if let Some(pane) = pane_in(&panes, &lineage(pid)) {
            // Switching the client is only needed, and only possible from a
            // client, when the agent is in another session.
            let current = tmux(&["display-message", "-p", "#{session_id}"])?;
            if current.trim() != pane.session {
                tmux(&["switch-client", "-t", &pane.session])?;
            }
            tmux(&["select-window", "-t", &pane.window])?;
            tmux(&["select-pane", "-t", &pane.pane])?;
            return Ok(Some(format!(
                "switched to {} in tree {}",
                agent.name, tree.name
            )));
        }
    }
    Ok(None)
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

/// Switches to the pane of the agent working in the tree, else to a window of
/// this session already in the tree, else opens one there. Returns what
/// happened, for the status line.
/// Shows `text` in the status line of every client, the way tmux reports a
/// bell in another window, so news reaches you in whatever window you are.
pub fn notify(text: &str) {
    let _ = tmux(&["display-message", "-d", "5000", text]);
}

/// The session options `show_waiting` changes, which it keeps first and
/// puts back as they were.
const KEPT: [&str; 2] = ["status-right", "status-right-length"];

/// The tmux user option holding the session's own value of `option` while
/// treetop's line is up, empty for none; set once kept, so a second treetop
/// or one started after another died keeps the first's copy.
fn kept(option: &str) -> String {
    format!("@treetop-{option}")
}

/// Reads an option of this session: its own value, or with `-g` the global
/// one, empty where it has none.
fn option(scope: &str, name: &str) -> String {
    tmux(&["show-options", scope, name])
        .unwrap_or_default()
        .trim_end()
        .to_string()
}

/// `line` before `right`, the status-right it goes in front of: what needs
/// action as a badge in the warning colours, what is done plain beside it.
fn status_text(line: &str, right: &str) -> String {
    let line = line.replace('#', "##");
    let (needs, done) = match line.split_once("  ") {
        Some((needs, done)) if needs.starts_with('●') => (needs, done),
        _ if line.starts_with('●') => (line.as_str(), ""),
        _ => ("", line.as_str()),
    };
    let badge =
        (!needs.is_empty()).then(|| format!("#[fg=black,bg=yellow,bold] {needs} #[default]"));
    // One space between each part shown, so it takes no more room than
    // `show_waiting` keeps for it.
    [badge.as_deref().unwrap_or(""), done, right]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Puts `line`, the counts of what needs action and what is done, at the
/// left of this session's status-right, what needs action in the warning
/// colours, where every window shows it until it is cleared; None clears
/// it, putting back what the session had.
pub fn show_waiting(line: Option<&str>) {
    let Some(line) = line else {
        if option("-qv", "@treetop-kept").is_empty() {
            return;
        }
        for name in KEPT {
            let own = option("-qv", &kept(name));
            let _ = if own.is_empty() {
                tmux(&["set-option", "-u", name])
            } else {
                tmux(&["set-option", name, &own])
            };
            let _ = tmux(&["set-option", "-u", &kept(name)]);
        }
        let _ = tmux(&["set-option", "-u", "@treetop-kept"]);
        return;
    };
    if option("-qv", "@treetop-kept").is_empty() {
        for name in KEPT {
            let _ = tmux(&["set-option", &kept(name), &option("-qv", name)]);
        }
        let _ = tmux(&["set-option", "@treetop-kept", "1"]);
    }
    // What the session showed before: its own, else the global one.
    let before = |name: &str| {
        Some(option("-qv", &kept(name)))
            .filter(|own| !own.is_empty())
            .unwrap_or_else(|| option("-gv", name))
    };
    let shown = status_text(line, &before("status-right"));
    // Room for the line on top of what was there, so tmux cuts neither.
    let room: usize = before("status-right-length").parse().unwrap_or(40);
    let room = (room + line.chars().count() + 3).to_string();
    let _ = tmux(&["set-option", "status-right-length", &room]);
    let _ = tmux(&["set-option", "status-right", &shown]);
}

/// treetop's own pane's `allow-passthrough`, as it takes effect there, from
/// the pane, its window or the global setting: its pane by `$TMUX_PANE`,
/// since without a target tmux reads the pane on screen.
pub fn allow_passthrough() -> String {
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let mut args = vec!["display-message", "-p"];
    if !pane.is_empty() {
        args.extend(["-t", pane.as_str()]);
    }
    args.push("#{allow-passthrough}");
    tmux(&args).unwrap_or_default().trim().to_string()
}

/// How long since anyone typed in any client of this tmux server, or with
/// every client detached, in any of its sessions; None where tmux cannot
/// say.
pub fn idle() -> Option<std::time::Duration> {
    let latest = |what: &str, format: &str| {
        tmux(&[what, "-F", format])
            .ok()?
            .lines()
            .filter_map(|at| at.trim().parse::<u64>().ok())
            .max()
    };
    let latest = latest("list-clients", "#{client_activity}")
        .or_else(|| latest("list-sessions", "#{session_activity}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(std::time::Duration::from_secs(now.saturating_sub(latest)))
}

pub fn open(tree: &Tree) -> Result<String> {
    if let Some(switched) = open_agent(tree)? {
        return Ok(switched);
    }
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

    #[test]
    fn the_status_shows_the_counts_what_needs_action_as_a_badge() {
        let right = "%Y-%m-%d";
        assert_eq!(
            status_text("● 2 needs action  ✓ 3 done", right),
            "#[fg=black,bg=yellow,bold] ● 2 needs action #[default] ✓ 3 done %Y-%m-%d"
        );
        assert_eq!(
            status_text("● 2 needs action", right),
            "#[fg=black,bg=yellow,bold] ● 2 needs action #[default] %Y-%m-%d"
        );
        // Only what is done: no badge, nothing loud.
        assert_eq!(status_text("✓ 3 done", right), "✓ 3 done %Y-%m-%d");
        // Never wider than the line and the three columns kept for it.
        let shown = |text: String| {
            text.replace("#[fg=black,bg=yellow,bold]", "")
                .replace("#[default]", "")
        };
        for line in ["● 2 needs action  ✓ 3 done", "● 2 needs action", "✓ 3 done"] {
            let width = shown(status_text(line, right)).chars().count();
            assert!(width <= line.chars().count() + 3 + right.len(), "{line}");
        }
    }
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

    #[test]
    fn finds_the_pane_whose_process_is_in_the_agents_lineage() {
        let panes = "$0\t@1\t%1\t100\n$2\t@6\t%6\t200\n$2\t@7\t%9\t300\n";
        let pane = |session: &str, window: &str, pane: &str| Pane {
            session: session.into(),
            window: window.into(),
            pane: pane.into(),
        };
        assert_eq!(
            pane_in(panes, &[4242, 200, 1]),
            Some(pane("$2", "@6", "%6"))
        );
        assert_eq!(
            pane_in(panes, &[300]),
            Some(pane("$2", "@7", "%9")),
            "the agent is the pane's process"
        );
        assert_eq!(
            pane_in(panes, &[4242, 555, 1]),
            None,
            "a pane its shell is not in"
        );
    }
}
