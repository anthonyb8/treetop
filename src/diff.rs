//! The side-by-side diff: everything a tree's branch changes against its base,
//! the way a pull request shows it, plus work not yet committed. git does the
//! diffing; this parses its unified output and pairs removed lines with the
//! added lines that replace them, so the two columns line up row by row.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use serde::Deserialize;

use crate::pool;

/// Untracked files beyond this many are left out; past it they are almost
/// always generated output nobody meant to review.
const MAX_UNTRACKED: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Added,
    Deleted,
    Modified,
    Renamed,
}

/// One side of a row: a line number and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    pub number: usize,
    pub text: String,
    /// Removed on the left, added on the right; false for context.
    pub is_change: bool,
    /// The chars that differ from the line it is paired with, so the eye
    /// lands on what changed rather than on the whole line.
    pub emphasis: Option<(usize, usize)>,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Widens a char range to whole words, so a highlight never starts or stops
/// inside one.
fn to_words(chars: &[char], (mut from, mut to): (usize, usize)) -> (usize, usize) {
    while from > 0 && from < chars.len() && is_word(chars[from - 1]) && is_word(chars[from]) {
        from -= 1;
    }
    while to > 0 && to < chars.len() && is_word(chars[to - 1]) && is_word(chars[to]) {
        to += 1;
    }
    (from, to)
}

/// The differing middle of two paired lines, as char ranges in each: what is
/// left after their common start and common end, widened to whole words.
/// None when they share neither, since then the whole line is the change.
fn emphasis(old: &str, new: &str) -> Option<((usize, usize), (usize, usize))> {
    let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix + suffix > 0).then(|| {
        (
            to_words(&old, (prefix, old.len() - suffix)),
            to_words(&new, (prefix, new.len() - suffix)),
        )
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// `@@ -a,b +c,d @@` and the function git found it in.
    Hunk(String),
    /// A line of each side, either missing where the other side has more.
    Pair(Option<Side>, Option<Side>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The old path of a rename.
    pub from: Option<String>,
    pub change: Change,
    pub is_binary: bool,
    pub additions: usize,
    pub deletions: usize,
    pub rows: Vec<Row>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchDiff {
    /// The ref the branch is compared against, such as `origin/development`.
    pub base: String,
    pub files: Vec<FileDiff>,
}

#[derive(Deserialize)]
struct TreehouseConfig {
    base_branch: Option<String>,
}

/// The pool's base branch: `base_branch` in the repository's
/// `treehouse.toml`, which treehouse itself cuts trees from, else the branch
/// `origin/HEAD` points to.
pub fn base(checkout: &Path) -> Option<String> {
    let configured = fs::read_to_string(checkout.join("treehouse.toml"))
        .ok()
        .and_then(|text| toml::from_str::<TreehouseConfig>(&text).ok())
        .and_then(|config| config.base_branch);
    configured.or_else(|| {
        let head = pool::git(
            checkout,
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        )?;
        Some(head.trim().trim_start_matches("origin/").to_string())
    })
}

/// git output even when it exits 1, which `diff --no-index` does whenever the
/// files differ.
fn git_diff_output(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(["--no-optional-locks", "-c", "core.quotePath=false", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    matches!(out.status.code(), Some(0 | 1))
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads the tree's diff against the merge base with `base`, plus its
/// untracked files as additions.
pub fn read(dir: &Path, base: Option<&str>) -> Result<BranchDiff, String> {
    let base = base.ok_or("no base branch: set base_branch in treehouse.toml")?;
    let remote = format!("origin/{base}");
    let base_ref = [remote.as_str(), base]
        .into_iter()
        .find(|r| pool::git(dir, &["rev-parse", "--verify", "--quiet", r]).is_some())
        .ok_or_else(|| format!("base branch {base} not found in this tree"))?
        .to_string();
    let merge_base = pool::git(dir, &["merge-base", "HEAD", &base_ref])
        .ok_or("git cannot read this tree, or it shares no history with its base")?;
    let tracked = git_diff_output(
        dir,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "-M",
            "-U3",
            merge_base.trim(),
        ],
    )
    .ok_or("git diff failed in this tree")?;
    let mut files = parse(&tracked);
    let untracked =
        pool::git(dir, &["ls-files", "--others", "--exclude-standard", "-z"]).unwrap_or_default();
    for path in untracked
        .split('\0')
        .filter(|p| !p.is_empty())
        .take(MAX_UNTRACKED)
    {
        let added = git_diff_output(
            dir,
            &["diff", "--no-color", "--no-index", "--", "/dev/null", path],
        );
        files.extend(parse(&added.unwrap_or_default()));
    }
    Ok(BranchDiff {
        base: base_ref,
        files,
    })
}

fn clean(text: &str) -> String {
    text.trim_end_matches('\r').replace('\t', "    ")
}

/// `-12,7 +12,9` from a hunk header, as the first old and new line numbers.
fn hunk_start(header: &str) -> (usize, usize) {
    let mut numbers = header.split_whitespace().take(2).map(|range| {
        range
            .get(1..)
            .and_then(|r| r.split(',').next())
            .and_then(|n| n.parse().ok())
            .unwrap_or(1)
    });
    (numbers.next().unwrap_or(1), numbers.next().unwrap_or(1))
}

struct Builder {
    file: FileDiff,
    removed: Vec<Side>,
    added: Vec<Side>,
    old_line: usize,
    new_line: usize,
}

impl Builder {
    fn new(path: String) -> Self {
        Builder {
            file: FileDiff {
                path,
                from: None,
                change: Change::Modified,
                is_binary: false,
                additions: 0,
                deletions: 0,
                rows: Vec::new(),
            },
            removed: Vec::new(),
            added: Vec::new(),
            old_line: 0,
            new_line: 0,
        }
    }

    /// Pairs a run of removed lines with the run of added lines after it,
    /// row by row, as GitHub's split view does.
    fn flush(&mut self) {
        let rows = self.removed.len().max(self.added.len());
        let mut removed = std::mem::take(&mut self.removed).into_iter();
        let mut added = std::mem::take(&mut self.added).into_iter();
        for _ in 0..rows {
            let (mut old, mut new) = (removed.next(), added.next());
            if let (Some(old), Some(new)) = (old.as_mut(), new.as_mut())
                && let Some((left, right)) = emphasis(&old.text, &new.text)
            {
                (old.emphasis, new.emphasis) = (Some(left), Some(right));
            }
            self.file.rows.push(Row::Pair(old, new));
        }
    }

    fn finish(mut self) -> FileDiff {
        self.flush();
        self.file
    }
}

/// The path after ` b/` in a `diff --git a/x b/y` line, for files with no
/// `---`/`+++` lines, such as binaries.
fn git_header_path(line: &str) -> String {
    line.rsplit_once(" b/")
        .map_or_else(|| line.to_string(), |(_, path)| path.to_string())
}

/// Parses `git diff` output into files of paired rows.
pub fn parse(text: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut current: Option<Builder> = None;
    for raw in text.lines() {
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            files.extend(current.take().map(Builder::finish));
            current = Some(Builder::new(git_header_path(rest)));
            continue;
        }
        let Some(b) = current.as_mut() else { continue };
        if let Some(header) = raw.strip_prefix("@@ ") {
            b.flush();
            (b.old_line, b.new_line) = hunk_start(header);
            b.file.rows.push(Row::Hunk(format!("@@ {}", clean(header))));
        } else if raw.starts_with("new file mode") {
            b.file.change = Change::Added;
        } else if raw.starts_with("deleted file mode") {
            b.file.change = Change::Deleted;
        } else if let Some(from) = raw.strip_prefix("rename from ") {
            b.file.change = Change::Renamed;
            b.file.from = Some(from.to_string());
        } else if raw.starts_with("Binary files ") {
            b.file.is_binary = true;
            if raw.contains("/dev/null and") {
                b.file.change = Change::Added;
            } else if raw.ends_with("and /dev/null differ") {
                b.file.change = Change::Deleted;
            }
        } else if let Some(path) = raw.strip_prefix("+++ ") {
            if let Some(path) = path.strip_prefix("b/") {
                b.file.path = path.to_string();
            }
        } else if raw.starts_with("--- ") || raw.starts_with('\\') {
            // The old path is already known; "\ No newline" annotates a line.
        } else if let Some(text) = raw.strip_prefix('-') {
            b.removed.push(Side {
                number: b.old_line,
                text: clean(text),
                is_change: true,
                emphasis: None,
            });
            b.old_line += 1;
            b.file.deletions += 1;
        } else if let Some(text) = raw.strip_prefix('+') {
            b.added.push(Side {
                number: b.new_line,
                text: clean(text),
                is_change: true,
                emphasis: None,
            });
            b.new_line += 1;
            b.file.additions += 1;
        } else if let Some(text) = raw.strip_prefix(' ') {
            b.flush();
            let side = |number| {
                Some(Side {
                    number,
                    text: clean(text),
                    is_change: false,
                    emphasis: None,
                })
            };
            b.file
                .rows
                .push(Row::Pair(side(b.old_line), side(b.new_line)));
            b.old_line += 1;
            b.new_line += 1;
        }
    }
    files.extend(current.map(Builder::finish));
    files
}

/// One row of the single scroll through every file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewRow {
    File(usize),
    Line(usize, usize),
    Binary(usize),
    Spacer,
}

/// A loaded diff laid out as one scroll, with where each file starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub diff: Arc<BranchDiff>,
    pub rows: Vec<ViewRow>,
    pub file_starts: Vec<usize>,
}

pub fn layout(diff: BranchDiff) -> Layout {
    let mut rows = Vec::new();
    let mut file_starts = Vec::new();
    for (index, file) in diff.files.iter().enumerate() {
        if index > 0 {
            rows.push(ViewRow::Spacer);
        }
        file_starts.push(rows.len());
        rows.push(ViewRow::File(index));
        if file.is_binary {
            rows.push(ViewRow::Binary(index));
        }
        rows.extend((0..file.rows.len()).map(|row| ViewRow::Line(index, row)));
    }
    Layout {
        diff: Arc::new(diff),
        rows,
        file_starts,
    }
}

/// Reads diffs one at a time on its own thread, in the order asked.
pub struct Loader {
    requests: Sender<PathBuf>,
    pub results: Receiver<(PathBuf, Result<Layout, String>)>,
}

impl Loader {
    pub fn spawn(base: Option<String>) -> Self {
        let (requests, queue) = mpsc::channel::<PathBuf>();
        let (results_tx, results) = mpsc::channel();
        thread::spawn(move || {
            for path in queue {
                let diff = read(&path, base.as_deref()).map(layout);
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

    const MODIFIED: &str = "diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,5 +10,6 @@ fn main() {
 keep one
-old two
-old three
+new two
+new three
+new four
 keep five
\\ No newline at end of file
";

    fn side(number: usize, text: &str, is_change: bool) -> Option<Side> {
        Some(Side {
            number,
            text: text.into(),
            is_change,
            emphasis: None,
        })
    }

    fn emphasized(number: usize, text: &str, range: (usize, usize)) -> Option<Side> {
        Some(Side {
            emphasis: Some(range),
            ..side(number, text, true).unwrap()
        })
    }

    #[test]
    fn pairs_removed_lines_with_the_added_lines_that_replace_them() {
        let files = parse(MODIFIED);
        let file = &files[0];
        assert_eq!(
            (file.path.as_str(), file.change),
            ("src/a.rs", Change::Modified)
        );
        assert_eq!((file.additions, file.deletions), (3, 2));
        assert_eq!(
            file.rows,
            [
                Row::Hunk("@@ -10,5 +10,6 @@ fn main() {".into()),
                Row::Pair(side(10, "keep one", false), side(10, "keep one", false)),
                Row::Pair(
                    emphasized(11, "old two", (0, 3)),
                    emphasized(11, "new two", (0, 3))
                ),
                Row::Pair(
                    emphasized(12, "old three", (0, 3)),
                    emphasized(12, "new three", (0, 3))
                ),
                Row::Pair(None, side(13, "new four", true)),
                Row::Pair(side(13, "keep five", false), side(14, "keep five", false)),
            ]
        );
    }

    #[test]
    fn reads_new_deleted_renamed_and_binary_files() {
        let text = "diff --git a/new.md b/new.md
new file mode 100644
--- /dev/null
+++ b/new.md
@@ -0,0 +1 @@
+hello
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1 +0,0 @@
-bye
diff --git a/old/name.rs b/new/name.rs
similarity index 100%
rename from old/name.rs
rename to new/name.rs
diff --git a/logo.png b/logo.png
Binary files a/logo.png and b/logo.png differ
";
        let files = parse(text);
        let summary: Vec<(&str, Change, bool)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.change, f.is_binary))
            .collect();
        assert_eq!(
            summary,
            [
                ("new.md", Change::Added, false),
                ("gone.rs", Change::Deleted, false),
                ("new/name.rs", Change::Renamed, false),
                ("logo.png", Change::Modified, true),
            ]
        );
        assert_eq!(files[0].rows[1], Row::Pair(None, side(1, "hello", true)));
        assert_eq!(files[1].rows[1], Row::Pair(side(1, "bye", true), None));
        assert_eq!(files[2].from.as_deref(), Some("old/name.rs"));
    }

    #[test]
    fn lays_every_file_out_in_one_scroll() {
        let mut files = parse(MODIFIED);
        files.extend(parse(MODIFIED));
        let layout = layout(BranchDiff {
            base: "origin/main".into(),
            files,
        });
        assert_eq!(layout.file_starts, [0, 8]);
        assert_eq!(layout.rows[0], ViewRow::File(0));
        assert_eq!(layout.rows[7], ViewRow::Spacer);
        assert_eq!(layout.rows[8], ViewRow::File(1));
        assert_eq!(layout.rows.len(), 15);
    }

    #[test]
    fn emphasizes_only_the_chars_that_differ() {
        let old = "unless `client/tests/anon-access.test.ts` lists it.";
        let new = "unless `api/tests/domains/anon-access.test.ts` lists it.";
        let ((a, b), (c, d)) = emphasis(old, new).unwrap();
        let old_mid: String = old.chars().skip(a).take(b - a).collect();
        let new_mid: String = new.chars().skip(c).take(d - c).collect();
        assert_eq!(
            (old_mid.as_str(), new_mid.as_str()),
            ("client/tests", "api/tests/domains")
        );
        assert_eq!(
            emphasis("abc", "xyz"),
            None,
            "nothing shared, the whole line is the change"
        );
    }

    #[test]
    fn expands_tabs_so_columns_line_up() {
        let files = parse("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-\tone\n+\ttwo\n");
        assert_eq!(
            files[0].rows[1],
            Row::Pair(
                emphasized(1, "    one", (4, 7)),
                emphasized(1, "    two", (4, 7))
            )
        );
    }
}
