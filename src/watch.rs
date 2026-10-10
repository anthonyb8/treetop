//! Which held trees have changed, from inotify, so git runs only for a tree
//! something has touched instead of for every tree every few seconds. Each
//! `git status` costs tens of milliseconds of CPU, and most trees sit still
//! most of the time.
//!
//! Each held tree has a watch on every directory holding a tracked file and on
//! its own git dir (HEAD, index), and the repository's shared refs are watched
//! once. A change in a tree means its git counts are read again; a change in
//! the refs, a commit, fetch or push, means every tree's unpushed count is.
//! Anything inotify cannot cover falls back to the old way: a tree it cannot
//! watch is checked every time, and every tree is checked every BACKSTOP.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::pool::{self, Tree, Work};

/// How often every held tree is checked whatever inotify says, for changes it
/// cannot see, such as files inside a directory that was untracked when the
/// tree was first watched. A round costs about as much as a minute of reading
/// every tree used to, so it is kept as rare as the pool listing's backstop.
const BACKSTOP: Duration = Duration::from_secs(120);

/// Writes, creations, deletions, renames and permission changes: what can
/// change `git status`. Plain reads and opens are left out.
const MASK: u32 = libc::IN_CLOSE_WRITE
    | libc::IN_CREATE
    | libc::IN_DELETE
    | libc::IN_MOVED_FROM
    | libc::IN_MOVED_TO
    | libc::IN_ATTRIB
    | libc::IN_ONLYDIR;

/// What a watch descriptor reports on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Watched {
    /// A tracked directory of the tree at this path.
    Tree(PathBuf),
    /// The tree's own git dir, where HEAD and the index live: a change there
    /// can be a checkout, which changes which directories are tracked.
    GitDir(PathBuf),
    Refs,
}

/// What a tree's git counts need reading for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// Uncommitted files and unpushed commits.
    Full,
    /// Only unpushed commits, keeping this many uncommitted files.
    Unpushed(usize),
}

pub struct Watcher {
    /// None when inotify is unavailable, which makes every tree polled.
    inotify: Option<OwnedFd>,
    watches: HashMap<i32, Watched>,
    /// Each watched tree's descriptors.
    trees: HashMap<PathBuf, Vec<i32>>,
    /// Trees inotify could not cover, checked every time.
    polled: HashSet<PathBuf>,
    /// Trees changed since the last `take`, and trees whose watched
    /// directories need listing again because their git dir changed.
    changed: HashSet<PathBuf>,
    relist: HashSet<PathBuf>,
    is_refs_changed: bool,
    /// Every tree is read again, as after an event queue overflow.
    is_everything_changed: bool,
    refs_watched: bool,
    last_backstop: Instant,
}

fn add_watch(inotify: RawFd, dir: &Path) -> Option<i32> {
    let path = CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: the descriptor is a live inotify instance and `path` is a
    // NUL-terminated string that outlives the call.
    let wd = unsafe { libc::inotify_add_watch(inotify, path.as_ptr(), MASK) };
    (wd >= 0).then_some(wd)
}

/// The tree's root and every directory holding a tracked file, from `git
/// ls-files`, so ignored directories such as `node_modules` are never watched.
fn tracked_dirs(tree: &Path) -> Option<Vec<PathBuf>> {
    Some(dirs_of(tree, &pool::git(tree, &["ls-files", "-z"])?))
}

/// The root and every parent directory of the NUL-separated `files`, which
/// are relative to `tree`.
fn dirs_of(tree: &Path, files: &str) -> Vec<PathBuf> {
    let mut dirs: HashSet<PathBuf> = HashSet::from([tree.to_path_buf()]);
    for file in files.split('\0').filter(|f| !f.is_empty()) {
        let mut dir = tree.join(file);
        while dir.pop() && dir.starts_with(tree) && dirs.insert(dir.clone()) {}
    }
    dirs.into_iter().collect()
}

/// Every directory under `dir`, `dir` included.
fn dirs_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = vec![dir.to_path_buf()];
    let mut next = 0;
    while let Some(current) = found.get(next).cloned() {
        next += 1;
        if let Ok(entries) = std::fs::read_dir(&current) {
            found.extend(
                entries
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.path()),
            );
        }
    }
    found
}

impl Watcher {
    pub fn new() -> Self {
        // SAFETY: inotify_init1 takes only flags and returns a new descriptor
        // or -1.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        Watcher {
            // SAFETY: a non-negative fd from inotify_init1 is owned by nobody else.
            inotify: (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) }),
            watches: HashMap::new(),
            trees: HashMap::new(),
            polled: HashSet::new(),
            changed: HashSet::new(),
            relist: HashSet::new(),
            is_refs_changed: false,
            is_everything_changed: false,
            refs_watched: false,
            last_backstop: Instant::now(),
        }
    }

    /// The inotify descriptor, None when inotify is unavailable.
    fn fd(&self) -> Option<RawFd> {
        self.inotify.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Reads the events queued since the last drain, without blocking.
    pub fn drain(&mut self) {
        let Some(inotify) = self.fd() else { return };
        let mut buffer = [0u8; 16 * 1024];
        loop {
            // SAFETY: the buffer is valid for writes of its full length.
            let read = unsafe { libc::read(inotify, buffer.as_mut_ptr().cast(), buffer.len()) };
            let Ok(len) = usize::try_from(read) else {
                return;
            };
            if len == 0 {
                return;
            }
            let mut at = 0;
            let header = std::mem::size_of::<libc::inotify_event>();
            while at + header <= len {
                // SAFETY: the kernel writes whole events, each a header
                // followed by `len` bytes of name, and `at` stays on event
                // boundaries; read_unaligned copes with the byte buffer.
                let event: libc::inotify_event =
                    unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(at).cast()) };
                self.note(event.wd, event.mask);
                at += header + event.len as usize;
            }
        }
    }

    fn note(&mut self, wd: i32, mask: u32) {
        if mask & libc::IN_Q_OVERFLOW != 0 {
            self.is_everything_changed = true;
            return;
        }
        let watched = if mask & libc::IN_IGNORED != 0 {
            self.watches.remove(&wd)
        } else {
            self.watches.get(&wd).cloned()
        };
        match watched {
            Some(Watched::Tree(tree)) => {
                self.changed.insert(tree);
            }
            Some(Watched::GitDir(tree)) => {
                self.changed.insert(tree.clone());
                self.relist.insert(tree);
            }
            Some(Watched::Refs) => self.is_refs_changed = true,
            None => {}
        }
    }

    fn unwatch(&mut self, wds: &[i32]) {
        let Some(inotify) = self.fd() else { return };
        for wd in wds {
            self.watches.remove(wd);
            // SAFETY: removing a watch only takes the two descriptors; a
            // watch the kernel already dropped returns an error, ignored.
            unsafe { libc::inotify_rm_watch(inotify, *wd) };
        }
    }

    /// Watches the tree's tracked directories and git dir, replacing any
    /// watches it had. A tree that cannot be watched in full is polled.
    fn watch_tree(&mut self, tree: &Tree) {
        if let Some(old) = self.trees.remove(&tree.path) {
            self.unwatch(&old);
        }
        let dirs = tracked_dirs(&tree.path).zip(pool::git_dir(&tree.path));
        let (Some(inotify), Some((dirs, git_dir))) = (self.fd(), dirs) else {
            self.polled.insert(tree.path.clone());
            return;
        };
        let targets = dirs
            .into_iter()
            .map(|dir| (dir, Watched::Tree(tree.path.clone())))
            .chain([(git_dir, Watched::GitDir(tree.path.clone()))]);
        let mut added = Vec::new();
        for (dir, watched) in targets {
            match add_watch(inotify, &dir) {
                Some(wd) => added.push((wd, watched)),
                // Out of watches, or a directory that vanished mid-listing.
                None if dir.exists() => {
                    let wds: Vec<i32> = added.iter().map(|(wd, _)| *wd).collect();
                    self.unwatch(&wds);
                    self.polled.insert(tree.path.clone());
                    return;
                }
                None => {}
            }
        }
        let wds = added.iter().map(|(wd, _)| *wd).collect();
        self.watches.extend(added);
        self.polled.remove(&tree.path);
        self.trees.insert(tree.path.clone(), wds);
    }

    /// Watches the repository's shared refs: its common dir, for
    /// `packed-refs` and `FETCH_HEAD`, and every directory under `refs/heads`
    /// and `refs/remotes`. Listed again after any change there, so a new
    /// branch directory is watched too.
    fn watch_refs(&mut self, trees: &[Tree]) {
        let Some(inotify) = self.fd() else { return };
        let Some(git_dir) = trees.iter().find_map(|t| pool::git_dir(&t.path)) else {
            return;
        };
        let common = pool::common_dir(&git_dir);
        let mut dirs = vec![common.clone()];
        dirs.extend(dirs_under(&common.join("refs/heads")));
        dirs.extend(dirs_under(&common.join("refs/remotes")));
        let wds: Vec<i32> = dirs.iter().filter_map(|d| add_watch(inotify, d)).collect();
        for wd in wds {
            self.watches.insert(wd, Watched::Refs);
        }
        self.refs_watched = true;
    }

    /// Brings the watches in line with the held trees and says which trees'
    /// git counts need reading, given the counts read so far. A tree with no
    /// counts yet, a polled one, and every tree at the backstop get a full
    /// read; consumes the changes noted since the last call.
    pub fn take(
        &mut self,
        trees: &[Tree],
        known: &HashMap<PathBuf, Option<Work>>,
    ) -> Vec<(PathBuf, Check)> {
        let held: Vec<&Tree> = trees.iter().filter(|t| t.is_held()).collect();
        let held_paths: HashSet<&PathBuf> = held.iter().map(|t| &t.path).collect();
        let gone: Vec<PathBuf> = self
            .trees
            .keys()
            .filter(|path| !held_paths.contains(path))
            .cloned()
            .collect();
        for path in gone {
            if let Some(wds) = self.trees.remove(&path) {
                self.unwatch(&wds);
            }
        }
        self.polled.retain(|path| held_paths.contains(path));

        let is_backstop = self.last_backstop.elapsed() >= BACKSTOP;
        if is_backstop {
            self.last_backstop = Instant::now();
        }
        // New trees are watched; a polled one is tried again at the backstop,
        // and one whose git dir changed has its directories listed again.
        // Each costs one `git ls-files`.
        let to_watch: Vec<&Tree> = held
            .iter()
            .copied()
            .filter(|t| {
                let is_new = !self.trees.contains_key(&t.path) && !self.polled.contains(&t.path);
                let is_retry = is_backstop && self.polled.contains(&t.path);
                is_new || is_retry || self.relist.contains(&t.path)
            })
            .collect();
        for tree in to_watch {
            self.watch_tree(tree);
        }
        if !self.refs_watched || self.is_refs_changed {
            self.watch_refs(trees);
        }
        let checks = plan(
            &held,
            known,
            &Signals {
                changed: &self.changed,
                polled: &self.polled,
                is_refs_changed: self.is_refs_changed,
                is_everything: self.is_everything_changed || is_backstop,
            },
        );
        self.changed.clear();
        self.relist.clear();
        self.is_refs_changed = false;
        self.is_everything_changed = false;
        checks
    }
}

/// What has happened since the last read, for `plan`.
struct Signals<'a> {
    changed: &'a HashSet<PathBuf>,
    polled: &'a HashSet<PathBuf>,
    is_refs_changed: bool,
    is_everything: bool,
}

/// Which held trees to read, and how much. A tree is read in full when
/// something in it changed, when it is polled, when it has no counts yet or
/// git could not read it last time, and when everything is; otherwise a refs
/// change rereads only its unpushed commits, and a still tree is skipped.
fn plan(
    held: &[&Tree],
    known: &HashMap<PathBuf, Option<Work>>,
    signals: &Signals,
) -> Vec<(PathBuf, Check)> {
    held.iter()
        .filter_map(|tree| {
            let last = known.get(&tree.path).copied().flatten();
            let is_full = signals.is_everything
                || signals.changed.contains(&tree.path)
                || signals.polled.contains(&tree.path);
            let check = match last {
                Some(work) if !is_full => signals
                    .is_refs_changed
                    .then_some(Check::Unpushed(work.changed))?,
                _ => Check::Full,
            };
            Some((tree.path.clone(), check))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;

    fn trees() -> Vec<Tree> {
        parse(
            r#"[
          {"name":"1","status":"leased","branch":"a","lease_holder":"me","path":"/p/1","leased_at":null,"processes":[]},
          {"name":"2","status":"leased","branch":"b","lease_holder":"me","path":"/p/2","leased_at":null,"processes":[]},
          {"name":"3","status":"leased","branch":"c","lease_holder":"me","path":"/p/3","leased_at":null,"processes":[]},
          {"name":"4","status":"leased","branch":"d","lease_holder":"me","path":"/p/4","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap()
    }

    fn work(changed: usize) -> Option<Work> {
        Some(Work {
            changed,
            unpushed: 0,
        })
    }

    fn paths(list: &[&str]) -> HashSet<PathBuf> {
        list.iter().map(PathBuf::from).collect()
    }

    fn checks(signals: &Signals) -> Vec<(String, Check)> {
        let trees = trees();
        let held: Vec<&Tree> = trees.iter().collect();
        // Tree 3 has never been read; tree 4 could not be read last time.
        let known = HashMap::from([
            (PathBuf::from("/p/1"), work(5)),
            (PathBuf::from("/p/2"), work(0)),
            (PathBuf::from("/p/4"), None),
        ]);
        plan(&held, &known, signals)
            .into_iter()
            .map(|(path, check)| (path.display().to_string(), check))
            .collect()
    }

    #[test]
    fn reads_only_changed_unread_and_polled_trees_when_nothing_else_moved() {
        let (changed, polled) = (paths(&["/p/1"]), paths(&["/p/2"]));
        let signals = Signals {
            changed: &changed,
            polled: &polled,
            is_refs_changed: false,
            is_everything: false,
        };
        assert_eq!(
            checks(&signals),
            [
                ("/p/1".into(), Check::Full),
                ("/p/2".into(), Check::Full),
                ("/p/3".into(), Check::Full),
                ("/p/4".into(), Check::Full),
            ]
        );
        let none = HashSet::new();
        let still = Signals {
            changed: &none,
            polled: &none,
            is_refs_changed: false,
            is_everything: false,
        };
        assert_eq!(
            checks(&still),
            [("/p/3".into(), Check::Full), ("/p/4".into(), Check::Full)],
            "a still tree with counts is skipped"
        );
    }

    #[test]
    fn a_refs_change_rereads_only_unpushed_and_keeps_uncommitted() {
        let none = HashSet::new();
        let signals = Signals {
            changed: &none,
            polled: &none,
            is_refs_changed: true,
            is_everything: false,
        };
        assert_eq!(
            checks(&signals)[..2],
            [
                ("/p/1".into(), Check::Unpushed(5)),
                ("/p/2".into(), Check::Unpushed(0))
            ]
        );
    }

    #[test]
    fn the_backstop_reads_every_tree_in_full() {
        let none = HashSet::new();
        let signals = Signals {
            changed: &none,
            polled: &none,
            is_refs_changed: true,
            is_everything: true,
        };
        assert!(
            checks(&signals)
                .iter()
                .all(|(_, check)| *check == Check::Full)
        );
    }

    #[test]
    fn lists_the_root_and_every_parent_of_a_tracked_file() {
        let mut dirs = dirs_of(Path::new("/t"), "README.md\0ui/src/pages/a.ts\0ui/b.ts\0");
        dirs.sort();
        assert_eq!(
            dirs,
            ["/t", "/t/ui", "/t/ui/src", "/t/ui/src/pages"].map(PathBuf::from)
        );
    }
}
