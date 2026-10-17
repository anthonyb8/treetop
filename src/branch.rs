//! The branch a new agent works on. treehouse hands trees out on a detached
//! HEAD, and an agent left there commits to no branch at all, so every
//! dispatch names one: the ticket's branch when there is a ticket, else
//! `<user>/<slug of the task>`. The name shows in the compose box, where it
//! can be changed before the agent starts.

use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The longest slug a task is cut to, so branch names stay readable.
const LONGEST: usize = 40;

/// Who the branches belong to: `git config user.name`, else `$USER`, as a
/// slug. Empty when neither is set.
pub fn user() -> String {
    let configured = Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned());
    let name = configured
        .filter(|name| !name.trim().is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default();
    slug(&name)
}

/// `text` as a branch-safe slug: lowercase words of letters and digits
/// joined by `-`, cut at a word boundary to LONGEST characters.
pub fn slug(text: &str) -> String {
    let lowered = text.to_lowercase();
    let words = lowered
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty());
    let mut slug = String::new();
    for word in words {
        if !slug.is_empty() && slug.len() + 1 + word.len() > LONGEST {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(word);
    }
    slug.truncate(LONGEST);
    slug
}

/// The branch a task starts on when nobody names one: `<user>/<slug>`, or
/// just the slug when there is no user. None for a task with no words.
pub fn for_task(user: &str, task: &str) -> Option<String> {
    let slug = slug(task);
    if slug.is_empty() {
        return None;
    }
    Some(if user.is_empty() {
        slug
    } else {
        format!("{user}/{slug}")
    })
}

/// The branch a launch starts on when nobody named one: the task's, else,
/// for an agent started idle, one named for the moment it started.
pub fn for_launch(user: &str, task: &str) -> String {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    for_task(user, task)
        .or_else(|| for_task(user, &format!("agent {at}")))
        .unwrap_or_else(|| format!("agent-{at}"))
}

/// Why git would refuse `name` as a branch, checked before anything is
/// leased so a typo costs nothing. Follows `git check-ref-format`'s rules.
pub fn problem(name: &str) -> Option<&'static str> {
    let bad_char = |c: char| c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c);
    if name.is_empty() {
        Some("the branch has no name")
    } else if name.starts_with('-') {
        Some("a branch cannot start with -")
    } else if name.chars().any(bad_char) {
        Some("a branch cannot hold spaces or any of ~ ^ : ? * [ \\")
    } else if name.contains("..") || name.contains("@{") || name.contains("//") {
        Some("a branch cannot hold .., @{ or //")
    } else if name.ends_with('/') || name.ends_with('.') || name.ends_with(".lock") {
        Some("a branch cannot end in /, . or .lock")
    } else if name.split('/').any(|part| part.starts_with('.')) {
        Some("no part of a branch can start with .")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_becomes_a_short_slug_under_the_user() {
        assert_eq!(
            for_task(
                "anthony",
                "END-123: Fix the login redirect, then add a test"
            )
            .as_deref(),
            Some("anthony/end-123-fix-the-login-redirect-then-add")
        );
        assert_eq!(for_task("", "Fix it").as_deref(), Some("fix-it"));
        assert_eq!(for_task("anthony", "  ... "), None);
    }

    #[test]
    fn a_user_name_is_slugged_too() {
        assert_eq!(slug("Anthony B."), "anthony-b");
    }

    #[test]
    fn a_name_git_would_refuse_says_why() {
        assert_eq!(problem("anthony/fix-login"), None);
        assert_eq!(problem("feature/END-1.2"), None);
        assert!(problem("fix login").is_some());
        assert!(problem("-x").is_some());
        assert!(problem("a..b").is_some());
        assert!(problem("a/").is_some());
        assert!(problem("a/.hidden").is_some());
        assert!(problem("x.lock").is_some());
        assert!(problem("").is_some());
    }
}
