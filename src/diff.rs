//! What a tree would lose, file by file and commit by commit: the diff pane.
//! Read off the UI thread by one loader, so a large repository never stalls a
//! keystroke, and turned into styled lines here so the layout is testable.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::pool;

/// Lines kept per section. A tree with thousands of changed files would
/// otherwise build a pane nobody scrolls through.
const CAP: usize = 300;

/// Raw git output for one tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    /// `git status --short`.
    pub status: String,
    /// `git diff --stat HEAD`; empty when HEAD does not exist yet.
    pub stat: String,
    /// `git log --oneline HEAD --not --remotes`.
    pub unpushed: String,
}

/// None when git cannot read the tree at all.
fn read(dir: &Path) -> Option<Diff> {
    Some(Diff {
        status: pool::git(dir, &["status", "--short"])?,
        stat: pool::git(dir, &["diff", "--stat", "HEAD"]).unwrap_or_default(),
        unpushed: pool::git(dir, &["log", "--oneline", "HEAD", "--not", "--remotes"])
            .unwrap_or_default(),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Heading,
    Added,
    Modified,
    Deleted,
    Plain,
    Note,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub tone: Tone,
    pub text: String,
}

fn line(tone: Tone, text: impl Into<String>) -> DiffLine {
    DiffLine {
        tone,
        text: text.into(),
    }
}

/// The colour of one `git status --short` entry, from its two status letters.
fn status_tone(entry: &str) -> Tone {
    let code = entry.get(..2).unwrap_or("");
    if code.contains('D') {
        Tone::Deleted
    } else if code == "??" || code.contains('A') {
        Tone::Added
    } else {
        Tone::Modified
    }
}

/// One section: a heading with its count, then its lines up to CAP.
fn section(lines: &mut Vec<DiffLine>, heading: &str, body: &str, tone: impl Fn(&str) -> Tone) {
    let entries: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    if entries.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(line(Tone::Plain, ""));
    }
    lines.push(line(
        Tone::Heading,
        format!("{heading} ({})", entries.len()),
    ));
    lines.extend(
        entries
            .iter()
            .take(CAP)
            .map(|e| line(tone(e), format!("  {e}"))),
    );
    if entries.len() > CAP {
        lines.push(line(
            Tone::Note,
            format!("  ... and {} more", entries.len() - CAP),
        ));
    }
}

/// The pane's lines for a tree's diff.
pub fn lines(diff: &Diff) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    section(&mut lines, "Uncommitted files", &diff.status, status_tone);
    // The stat's last line is its own summary ("3 files changed, ..."), so the
    // count in the heading would only repeat it.
    if !diff.stat.trim().is_empty() {
        if !lines.is_empty() {
            lines.push(line(Tone::Plain, ""));
        }
        lines.push(line(Tone::Heading, "Changed lines"));
        let stat: Vec<&str> = diff.stat.lines().collect();
        lines.extend(
            stat.iter()
                .take(CAP)
                .map(|l| line(Tone::Plain, format!("  {}", l.trim()))),
        );
        if stat.len() > CAP {
            lines.push(line(
                Tone::Note,
                format!("  ... and {} more", stat.len() - CAP),
            ));
        }
    }
    section(&mut lines, "Unpushed commits", &diff.unpushed, |_| {
        Tone::Plain
    });
    if lines.is_empty() {
        lines.push(line(
            Tone::Note,
            "Nothing to lose: no uncommitted files and no unpushed commits.",
        ));
    }
    lines
}

/// Reads diffs one at a time on its own thread, in the order asked.
pub struct Loader {
    requests: Sender<PathBuf>,
    pub results: Receiver<(PathBuf, Option<Diff>)>,
}

impl Loader {
    pub fn spawn() -> Self {
        let (requests, queue) = mpsc::channel::<PathBuf>();
        let (results_tx, results) = mpsc::channel();
        thread::spawn(move || {
            for path in queue {
                let diff = read(&path);
                if results_tx.send((path, diff)).is_err() {
                    return;
                }
            }
        });
        Loader { requests, results }
    }

    pub fn request(&self, path: PathBuf) {
        let _ = self.requests.send(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(lines: &[DiffLine]) -> Vec<&str> {
        lines.iter().map(|l| l.text.as_str()).collect()
    }

    #[test]
    fn lists_files_stat_and_unpushed_commits_in_sections() {
        let diff = Diff {
            status: " M src/a.rs\n?? notes.md\n D old.rs\n".into(),
            stat: " src/a.rs | 4 ++--\n 1 file changed, 2 insertions(+), 2 deletions(-)\n".into(),
            unpushed: "abc1234 Add a\n".into(),
        };
        let lines = lines(&diff);
        assert_eq!(
            texts(&lines),
            [
                "Uncommitted files (3)",
                "   M src/a.rs",
                "  ?? notes.md",
                "   D old.rs",
                "",
                "Changed lines",
                "  src/a.rs | 4 ++--",
                "  1 file changed, 2 insertions(+), 2 deletions(-)",
                "",
                "Unpushed commits (1)",
                "  abc1234 Add a",
            ]
        );
        let tones: Vec<Tone> = lines[1..4].iter().map(|l| l.tone).collect();
        assert_eq!(tones, [Tone::Modified, Tone::Added, Tone::Deleted]);
    }

    #[test]
    fn a_clean_tree_says_there_is_nothing_to_lose() {
        let lines = lines(&Diff::default());
        assert_eq!(lines.len(), 1);
        assert!(lines[0].text.starts_with("Nothing to lose"));
    }

    #[test]
    fn caps_a_long_section_and_says_how_many_were_left_out() {
        let status: String = (0..CAP + 5).map(|i| format!("?? f{i}\n")).collect();
        let lines = lines(&Diff {
            status,
            ..Diff::default()
        });
        assert_eq!(lines.len(), CAP + 2);
        assert_eq!(lines.last().unwrap().text, "  ... and 5 more");
    }
}
