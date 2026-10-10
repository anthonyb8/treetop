//! What changes by the second, read directly the way htop does: which processes
//! stand in each tree and the ports they listen on, from /proc, and each held
//! tree's git state. Both cost milliseconds, where a `treehouse status` costs
//! seconds.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use crate::pool::{self, Process, Tree, Work};
use crate::watch::Check;

/// The pid and cwd of every process whose cwd is readable. One that exits
/// mid-read, or belongs to another user, is skipped.
fn running() -> Vec<(PathBuf, u32)> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            Some((fs::read_link(entry.path().join("cwd")).ok()?, pid))
        })
        .collect()
}

/// Groups pids under the tree their cwd is in, which is how treehouse counts
/// them: the two agree process for process.
fn assign(trees: &[Tree], running: &[(PathBuf, u32)]) -> HashMap<PathBuf, Vec<u32>> {
    let mut by_tree: HashMap<PathBuf, Vec<u32>> = HashMap::new();
    for (cwd, pid) in running {
        if let Some(tree) = trees.iter().find(|t| t.contains(cwd)) {
            by_tree.entry(tree.path.clone()).or_default().push(*pid);
        }
    }
    for pids in by_tree.values_mut() {
        pids.sort_unstable();
    }
    by_tree
}

/// The processes standing in each tree, named from /proc, with the ports they
/// listen on. A tree with none is absent from the map.
pub fn processes(trees: &[Tree]) -> HashMap<PathBuf, Vec<Process>> {
    let listening = listening();
    assign(trees, &running())
        .into_iter()
        .map(|(path, pids)| {
            let processes = pids
                .into_iter()
                .filter_map(|pid| {
                    let name = fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
                    let ports = if listening.is_empty() {
                        Vec::new()
                    } else {
                        ports(fd_links(pid), &listening)
                    };
                    Some(Process {
                        pid,
                        name: name.trim_end().to_string(),
                        ports,
                    })
                })
                .collect();
            (path, processes)
        })
        .collect()
}

/// Listening TCP sockets by inode, from the text of `/proc/net/tcp` or `tcp6`.
/// A row's fields are slot, local address as `hex-ip:hex-port`, remote
/// address, state (`0A` is LISTEN), and its inode tenth; every other state is
/// dropped.
fn parse_listen(table: &str) -> HashMap<u64, u16> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.get(3) != Some(&"0A") {
                return None;
            }
            let port = u16::from_str_radix(fields.get(1)?.rsplit(':').next()?, 16).ok()?;
            Some((fields.get(9)?.parse().ok()?, port))
        })
        .collect()
}

/// Every listening TCP socket on the machine, IPv4 and IPv6, by inode.
fn listening() -> HashMap<u64, u16> {
    ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|table| fs::read_to_string(table).ok())
        .flat_map(|table| parse_listen(&table))
        .collect()
}

/// Where each of a process's file descriptors points; a socket reads as
/// `socket:[<inode>]`.
fn fd_links(pid: u32) -> Vec<PathBuf> {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|fds| {
            fds.flatten()
                .filter_map(|fd| fs::read_link(fd.path()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// The listening ports among a process's fd `links`, lowest first.
fn ports(links: Vec<PathBuf>, listening: &HashMap<u64, u16>) -> Vec<u16> {
    let mut ports: Vec<u16> = links
        .iter()
        .filter_map(|link| {
            let inode = link
                .to_str()?
                .strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse()
                .ok()?;
            listening.get(&inode).copied()
        })
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// The fields of `/proc/<pid>/stat` treetop needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub ppid: u32,
    /// When the process started, in clock ticks since boot: with the pid, it
    /// tells a live process from a later one that reused its pid.
    pub start: u64,
}

/// Parses `/proc/<pid>/stat`. The command name in field 2 may hold spaces and
/// parentheses, so fields are counted from the last `)`: ppid is field 4 and
/// the start time field 22.
fn parse_stat(text: &str) -> Option<Stat> {
    let fields: Vec<&str> = text
        .get(text.rfind(')')? + 1..)?
        .split_whitespace()
        .collect();
    Some(Stat {
        ppid: fields.get(1)?.parse().ok()?,
        start: fields.get(19)?.parse().ok()?,
    })
}

/// The process's parent and start time; None once it has exited.
pub fn stat(pid: u32) -> Option<Stat> {
    parse_stat(&fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

fn work_in(dir: &Path, check: Check) -> Option<Work> {
    let changed = match check {
        Check::Full => pool::git(dir, &["status", "--porcelain"])?.lines().count(),
        Check::Unpushed(changed) => changed,
    };
    let unpushed = pool::git(dir, &["rev-list", "--count", "HEAD", "--not", "--remotes"])?
        .trim()
        .parse()
        .ok()?;
    Some(Work { changed, unpushed })
}

/// Git state for each tree in `checks`, read as far as its check says, in
/// parallel so one slow repository does not hold up the rest. None for a tree
/// git cannot read.
pub fn work(checks: Vec<(PathBuf, Check)>) -> HashMap<PathBuf, Option<Work>> {
    thread::scope(|scope| {
        let checks: Vec<_> = checks
            .into_iter()
            .map(|(path, check)| {
                scope.spawn(move || {
                    let work = work_in(&path, check);
                    (path, work)
                })
            })
            .collect();
        checks
            .into_iter()
            .filter_map(|check| check.join().ok())
            .collect()
    })
}

/// The repository's worktrees and their branches. It changes when a tree is
/// leased onto a branch, returned or destroyed, and costs milliseconds, so it
/// tells treetop when the slow pool listing is worth running.
pub fn worktrees(checkout: &Path) -> Option<String> {
    pool::git(checkout, &["worktree", "list", "--porcelain"]).map(|list| without_heads(&list))
}

/// `git worktree list --porcelain` without its `HEAD <commit>` lines: a commit
/// moves a HEAD but changes nothing about the pool, and would otherwise cost a
/// pool listing every time an agent commits.
fn without_heads(list: &str) -> String {
    list.lines()
        .filter(|line| !line.starts_with("HEAD "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;

    fn trees() -> Vec<Tree> {
        parse(
            r#"[
          {"name":"4","status":"leased","branch":"b","lease_holder":"me","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"40","status":"leased","branch":"c","lease_holder":"me","path":"/p/40/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap()
    }

    fn process(pid: u32, cwd: &str) -> (PathBuf, u32) {
        (PathBuf::from(cwd), pid)
    }

    #[test]
    fn assigns_processes_by_cwd_to_the_tree_they_stand_in() {
        let running = [
            process(9, "/p/4/app/ui"),
            process(3, "/p/4/app"),
            process(5, "/p/40/app/src"),
            process(7, "/home/me"),
        ];
        let by_tree = assign(&trees(), &running);
        let pids = |path: &str| by_tree[Path::new(path)].clone();
        assert_eq!(pids("/p/4/app"), [3, 9]);
        assert_eq!(pids("/p/40/app"), [5]);
        assert_eq!(by_tree.len(), 2);
    }

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 111 1 0 100 0 0 10 0
   1: 0100007F:9C40 0100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 222 1 0 20 4 30 10 -1
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:1467 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 497207588 1 0 100 0 0 10 0
";

    #[test]
    fn keeps_only_listening_sockets_by_inode() {
        assert_eq!(parse_listen(TCP), HashMap::from([(111, 8080)]));
        assert_eq!(parse_listen(TCP6), HashMap::from([(497207588, 5223)]));
    }

    #[test]
    fn matches_fd_links_to_listening_inodes() {
        let listening = HashMap::from([(111, 8080), (497207588, 5223), (5, 5223)]);
        let links = [
            "socket:[497207588]",
            "/dev/null",
            "socket:[999]",
            "socket:[111]",
            "socket:[5]",
        ]
        .map(PathBuf::from)
        .to_vec();
        assert_eq!(ports(links, &listening), [5223, 8080]);
    }

    #[test]
    fn a_commit_leaves_the_worktree_list_unchanged_and_a_new_branch_does_not() {
        let before =
            "worktree /p/4\nHEAD aaa\nbranch refs/heads/x\n\nworktree /p/7\nHEAD bbb\ndetached\n";
        let commit =
            "worktree /p/4\nHEAD ccc\nbranch refs/heads/x\n\nworktree /p/7\nHEAD bbb\ndetached\n";
        let branch = "worktree /p/4\nHEAD aaa\nbranch refs/heads/x\n\nworktree /p/7\nHEAD bbb\nbranch refs/heads/y\n";
        assert_eq!(without_heads(before), without_heads(commit));
        assert_ne!(without_heads(before), without_heads(branch));
    }

    #[test]
    fn reads_stat_fields_after_a_command_name_with_spaces() {
        let text = "1245653 (node (Main) x) S 1245642 1215215 1215215 0 -1 4194560 1 0 0 0 5 1 0 0 20 0 11 0 163482243 1 2 3";
        assert_eq!(
            parse_stat(text),
            Some(Stat {
                ppid: 1245642,
                start: 163482243
            })
        );
        assert_eq!(parse_stat("garbage"), None);
    }
}
