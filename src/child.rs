//! Running a child process with a deadline. Every command treetop runs on a
//! job or for a plugin goes through here, so one that hangs costs its own
//! timeout and never blocks the jobs behind it. The child leads its own
//! process group, and the whole group is killed when it overruns, so nothing
//! it started is left behind.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

/// How often a running child is checked for having exited.
const POLL: Duration = Duration::from_millis(10);

/// How a child ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum End {
    Exited(ExitStatus),
    /// It ran past its deadline, and its group was killed.
    TimedOut,
    /// It exited, but something it started kept its output open past the
    /// deadline, so that was killed and its output is lost.
    HeldOpen(ExitStatus),
}

#[derive(Debug)]
pub struct Ran {
    pub end: End,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Ran {
    /// stdout then stderr, as text, for a job's log.
    pub fn text(&self) -> String {
        let mut text = String::from_utf8_lossy(&self.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&self.stderr));
        text
    }
}

/// Reads a pipe to its end on a thread of its own, so a child that writes
/// more than a pipe holds is never stuck waiting to be read.
fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    rx
}

fn kill_group(child: &mut Child) {
    // The child leads its own group, so this reaches whatever it started.
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.wait();
}

/// Starts `command` in a process group of its own. A program file still open
/// for writing, because it is being installed or because another thread
/// forked while writing it, is busy for a moment, so that is retried until
/// `deadline`.
fn spawn(command: &mut Command, deadline: Instant) -> io::Result<Child> {
    loop {
        match command.process_group(0).spawn() {
            Err(err) if err.raw_os_error() == Some(libc::ETXTBSY) && Instant::now() < deadline => {
                thread::sleep(POLL);
            }
            spawned => return spawned,
        }
    }
}

/// Runs `command` with `input` on its stdin, or none, capturing its output,
/// and kills it and everything it started once `limit` has passed.
pub fn run(command: &mut Command, input: Option<Vec<u8>>, limit: Duration) -> io::Result<Ran> {
    let deadline = Instant::now() + limit;
    let stdin = if input.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    command
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = spawn(command, deadline)?;
    if let (Some(mut pipe), Some(input)) = (child.stdin.take(), input) {
        // Written on a thread too: a child that never reads its input must
        // not hold treetop past the deadline.
        thread::spawn(move || {
            let _ = pipe.write_all(&input);
        });
    }
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        kill_group(&mut child);
        return Err(io::Error::other("no pipes to the child"));
    };
    let (stdout, stderr) = (drain(stdout), drain(stderr));
    let status = loop {
        match child.try_wait()? {
            Some(status) => break status,
            None if Instant::now() < deadline => thread::sleep(POLL),
            None => {
                kill_group(&mut child);
                return Ok(Ran {
                    end: End::TimedOut,
                    stdout: stdout.recv_timeout(POLL).unwrap_or_default(),
                    stderr: stderr.recv_timeout(POLL).unwrap_or_default(),
                });
            }
        }
    };
    let left = deadline.saturating_duration_since(Instant::now());
    let Ok(out) = stdout.recv_timeout(left) else {
        kill_group(&mut child);
        return Ok(Ran {
            end: End::HeldOpen(status),
            stdout: Vec::new(),
            stderr: stderr.recv_timeout(POLL).unwrap_or_default(),
        });
    };
    Ok(Ran {
        end: End::Exited(status),
        stdout: out,
        stderr: stderr.recv_timeout(left.max(POLL)).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn captures_output_and_feeds_input() {
        let ran = run(
            &mut sh("cat; echo err >&2"),
            Some(b"hi".to_vec()),
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(matches!(ran.end, End::Exited(status) if status.success()));
        assert_eq!(ran.text(), "hierr\n");
    }

    #[test]
    fn a_hang_is_killed_with_its_children() {
        let dir = std::env::temp_dir().join(format!("treetop-child-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("survived");
        let script = format!("(sleep 1; touch {}) & sleep 30", marker.display());
        let started = Instant::now();
        let ran = run(&mut sh(&script), None, Duration::from_millis(200)).unwrap();
        assert_eq!(ran.end, End::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(2));
        thread::sleep(Duration::from_millis(1500));
        assert!(!marker.exists(), "the child's child outlived the timeout");
    }

    #[test]
    fn a_child_left_holding_the_output_is_killed() {
        let ran = run(
            &mut sh("sleep 30 & echo hi"),
            None,
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(matches!(ran.end, End::HeldOpen(status) if status.success()));
    }
}
