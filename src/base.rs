//! Keeps the repository's main checkout on its upstream, so the base every
//! tree is cut from and the checkout you work in are never weeks behind. It
//! only ever fast-forwards, and only a checkout on a branch with an upstream,
//! with no tracked changes, no commits of its own and no agent busy in it.
//! Anything else is left as it is and reported as behind, for the GIT column
//! to show as `⇣N`.

use std::path::Path;
use std::process::Command;

use crate::pool;

/// Where the main checkout stands against its upstream after a catch-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// Level with its upstream, or it has none to follow.
    Current,
    /// Fast-forwarded by this many commits.
    Forwarded(usize),
    /// This many commits behind, and left there: it has changes or commits
    /// of its own, or an agent is busy in it.
    Behind(usize),
}

impl Standing {
    pub fn behind(self) -> usize {
        match self {
            Standing::Behind(n) => n,
            Standing::Current | Standing::Forwarded(_) => 0,
        }
    }
}

/// Fetches the checkout's upstream remote, quietly. False when it could not,
/// as offline, which only leaves the catch-up to work from older refs.
pub fn fetch(checkout: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["fetch", "--quiet"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Fast-forwards the checkout to its upstream where that is safe. `is_busy`
/// says an agent is mid-turn in it, which a moving tree would confuse.
pub fn catch_up(checkout: &Path, is_busy: bool) -> Standing {
    let Some(counts) = pool::git(
        checkout,
        &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
    ) else {
        return Standing::Current;
    };
    let mut counts = counts.split_whitespace().map(str::parse::<usize>);
    let (Some(Ok(ahead)), Some(Ok(behind))) = (counts.next(), counts.next()) else {
        return Standing::Current;
    };
    if behind == 0 {
        return Standing::Current;
    }
    let is_clean = || {
        pool::git(checkout, &["status", "--porcelain", "--untracked-files=no"])
            .is_some_and(|status| status.trim().is_empty())
    };
    if ahead > 0 || is_busy || !is_clean() {
        return Standing::Behind(behind);
    }
    // git itself refuses a fast-forward that would overwrite an untracked
    // file, which leaves the checkout as it was.
    match pool::git(checkout, &["merge", "--ff-only", "--quiet", "@{upstream}"]) {
        Some(_) => Standing::Forwarded(behind),
        None => Standing::Behind(behind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn run(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    /// An origin, the main checkout cloned from it, and another clone that
    /// pushes `behind` commits the checkout has not seen.
    fn repos(name: &str, behind: usize) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("treetop-base-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        run(
            &root,
            &["init", "--quiet", "--bare", "-b", "main", "origin.git"],
        );
        run(&root, &["clone", "--quiet", "origin.git", "other"]);
        let other = root.join("other");
        fs::write(other.join("a"), "0").unwrap();
        run(&other, &["add", "a"]);
        run(&other, &["commit", "--quiet", "-m", "0"]);
        run(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        run(&root, &["clone", "--quiet", "origin.git", "main"]);
        for n in 1..=behind {
            fs::write(other.join("a"), n.to_string()).unwrap();
            run(&other, &["commit", "--quiet", "-am", &n.to_string()]);
        }
        run(&other, &["push", "--quiet", "origin", "HEAD:main"]);
        (root.clone(), root.join("main"))
    }

    #[test]
    fn a_clean_checkout_catches_up_with_what_was_pushed() {
        let (root, main) = repos("clean", 3);
        assert_eq!(
            catch_up(&main, false),
            Standing::Current,
            "nothing fetched yet"
        );
        assert!(fetch(&main));
        assert_eq!(catch_up(&main, false), Standing::Forwarded(3));
        assert_eq!(fs::read_to_string(main.join("a")).unwrap(), "3");
        assert_eq!(catch_up(&main, false), Standing::Current);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn a_checkout_with_work_of_its_own_or_a_busy_agent_is_left_behind() {
        let (root, main) = repos("kept", 2);
        assert!(fetch(&main));
        assert_eq!(
            catch_up(&main, true),
            Standing::Behind(2),
            "an agent is busy"
        );

        fs::write(main.join("a"), "mine").unwrap();
        assert_eq!(
            catch_up(&main, false),
            Standing::Behind(2),
            "a tracked change"
        );
        assert_eq!(fs::read_to_string(main.join("a")).unwrap(), "mine");

        run(&main, &["commit", "--quiet", "-am", "mine"]);
        assert_eq!(
            catch_up(&main, false),
            Standing::Behind(2),
            "a commit of its own"
        );
        let _ = fs::remove_dir_all(root);
    }
}
