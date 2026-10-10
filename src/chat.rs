//! An agent's chat inside treetop. The chat runs in a pseudo-terminal treetop
//! owns, its output parsed into a screen treetop draws in its own frame, so
//! the real terminal is never handed over: whatever modes the chat switches
//! on (mouse tracking, extended keys, bracketed paste) land in the parser,
//! and treetop sees every key first, keeping Ctrl+] to leave.
//!
//! A chat left with Ctrl+] keeps running in the background, its screen kept
//! current, so opening it again is instant.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Lines of scrollback kept; the chat draws its own history, so none.
const SCROLLBACK: usize = 0;

/// How long a new chat's output must pause before it is shown: its first
/// draw arrives in bursts, and showing each would flash half-drawn frames.
const SETTLE: Duration = Duration::from_millis(80);

/// The longest a new chat stays behind its "opening" line, however busy it is.
const REVEAL: Duration = Duration::from_secs(2);

/// The longest a synchronized update is waited on before drawing anyway, in
/// case its end marker never comes.
const SYNC_WAIT: Duration = Duration::from_millis(100);

/// The synchronized-output markers (DEC mode 2026) a program wraps a redraw
/// in, so a terminal can show the whole frame at once.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";

/// Chats kept running after they are left, the most recently left kept.
const KEPT: usize = 8;

pub struct Chat {
    pub agent: String,
    pub tree: String,
    /// The command it runs, which also identifies it: opening the same
    /// command again shows this chat.
    command: Vec<String>,
    master: File,
    parser: vt100::Parser,
    output: Receiver<Vec<u8>>,
    child: Option<Child>,
    is_ended: bool,
    opened_at: Instant,
    last_output: Option<Instant>,
    /// Its first draw has settled and it is shown; until then, "opening".
    is_ready: bool,
    /// When the synchronized update in progress began, if one is.
    sync_since: Option<Instant>,
}

/// Where the last synchronized-output marker in `bytes` leaves the chat:
/// inside an update (true), out of one (false), or as it was (None).
fn sync_state(bytes: &[u8]) -> Option<bool> {
    let last = |marker: &[u8]| bytes.windows(marker.len()).rposition(|w| w == marker);
    match (last(SYNC_BEGIN), last(SYNC_END)) {
        (Some(begin), Some(end)) => Some(begin > end),
        (Some(_), None) => Some(true),
        (None, Some(_)) => Some(false),
        (None, None) => None,
    }
}

fn winsize(rows: u16, cols: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

fn close_on_exec(fd: &OwnedFd) -> Result<()> {
    // SAFETY: fcntl on a descriptor we own, setting only FD_CLOEXEC.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error()).context("marking the terminal close-on-exec");
    }
    Ok(())
}

impl Chat {
    /// Starts `command` in a new pseudo-terminal of `rows` by `cols`.
    pub fn open(
        command: &[String],
        rows: u16,
        cols: u16,
        agent: String,
        tree: String,
    ) -> Result<Self> {
        let Some((program, args)) = command.split_first() else {
            bail!("the agent's chat command is empty");
        };
        let (mut master, mut slave) = (-1, -1);
        let size = winsize(rows, cols);
        // SAFETY: openpty writes two descriptors into the out-pointers; the
        // name and termios pointers may be null.
        let opened = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null(),
                &size,
            )
        };
        if opened < 0 {
            return Err(std::io::Error::last_os_error()).context("opening a terminal for the chat");
        }
        // SAFETY: openpty succeeded, so both are fresh descriptors nobody else owns.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
        // Neither original may leak into the child; it gets the slave as
        // stdin, stdout and stderr only.
        close_on_exec(&master)?;
        close_on_exec(&slave)?;
        let mut child_command = Command::new(program);
        child_command
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec: a new
        // session, with the terminal on stdin as its controlling terminal,
        // so the chat gets its own Ctrl+C and window size.
        unsafe {
            child_command.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = child_command
            .spawn()
            .with_context(|| format!("running {program}"))?;
        let master = File::from(master);
        let (sender, output) = mpsc::channel();
        let mut reader = master.try_clone().context("reading the chat")?;
        // Ends when the chat exits: reading the master then fails with EIO.
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(read @ 1..) = reader.read(&mut buffer) {
                if sender.send(buffer[..read].to_vec()).is_err() {
                    return;
                }
            }
        });
        Ok(Chat {
            agent,
            tree,
            command: command.to_vec(),
            master,
            parser: vt100::Parser::new(rows, cols, SCROLLBACK),
            output,
            child: Some(child),
            is_ended: false,
            opened_at: Instant::now(),
            last_output: None,
            is_ready: false,
            sync_since: None,
        })
    }

    /// Feeds whatever the chat has written since the last call to the
    /// screen. Returns whether the screen should be drawn again: not while
    /// a synchronized update is half done, nor before the first draw has
    /// settled, except to reveal it.
    pub fn pump(&mut self) -> bool {
        let mut is_new = false;
        loop {
            match self.output.try_recv() {
                Ok(bytes) => {
                    self.parser.process(&bytes);
                    match sync_state(&bytes) {
                        Some(true) => self.sync_since = self.sync_since.or(Some(Instant::now())),
                        Some(false) => self.sync_since = None,
                        None => {}
                    }
                    self.last_output = Some(Instant::now());
                    is_new = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.is_ended = true;
                    return true;
                }
            }
        }
        if !self.is_ready {
            let has_settled = self
                .last_output
                .is_some_and(|at| at.elapsed() >= SETTLE && self.sync_since.is_none());
            self.is_ready = has_settled || self.opened_at.elapsed() >= REVEAL;
            return self.is_ready;
        }
        let is_mid_update = self.sync_since.is_some_and(|at| at.elapsed() < SYNC_WAIT);
        is_new && !is_mid_update
    }

    /// Its first draw has settled, so its screen is shown rather than the
    /// "opening" line.
    pub fn is_ready(&self) -> bool {
        self.is_ready
    }

    /// The chat's program has exited.
    pub fn is_ended(&self) -> bool {
        self.is_ended
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    fn write(&mut self, bytes: &[u8]) {
        // A chat that has just exited cannot take input; the next pump ends it.
        let _ = self.master.write_all(bytes);
    }

    pub fn send_key(&mut self, key: KeyEvent) {
        let bytes = encode(key, self.screen().application_cursor());
        self.write(&bytes);
    }

    /// Pastes `text`, wrapped in bracketed-paste markers when the chat asked
    /// for them, so it arrives as one paste rather than as typed keys.
    pub fn paste(&mut self, text: &str) {
        if self.screen().bracketed_paste() {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let size = winsize(rows, cols);
        // SAFETY: TIOCSWINSZ on our own master with a valid winsize; the
        // kernel signals the chat to redraw at the new size.
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) };
        self.parser.screen_mut().set_size(rows, cols);
    }
}

impl Drop for Chat {
    /// Ends the chat's program and reaps it off the UI thread. Ending a chat
    /// client leaves the agent's session running.
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        if let Ok(group) = libc::pid_t::try_from(child.id()) {
            // SAFETY: the child leads its own session and process group.
            unsafe { libc::kill(-group, libc::SIGTERM) };
        }
        thread::spawn(move || child.wait());
    }
}

/// What happened to the open and background chats since the last pump.
pub struct Pumped {
    /// The open chat's screen should be drawn again.
    pub is_stale: bool,
    /// The open chat's program exited, which closes it: its agent and tree.
    pub ended: Option<(String, String)>,
}

/// The chat on screen, if one is, and the ones left running behind it.
#[derive(Default)]
pub struct Chats {
    open: Option<Chat>,
    /// Left with Ctrl+], oldest first.
    background: Vec<Chat>,
}

impl Chats {
    pub fn open(&self) -> Option<&Chat> {
        self.open.as_ref()
    }

    pub fn open_mut(&mut self) -> Option<&mut Chat> {
        self.open.as_mut()
    }

    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Shows the chat running `command`: the one left running for it if
    /// there is one, at once, else a new one.
    pub fn show(
        &mut self,
        command: &[String],
        rows: u16,
        cols: u16,
        agent: String,
        tree: String,
    ) -> Result<()> {
        let chat = match self.background.iter().position(|c| c.command == command) {
            Some(index) => {
                let mut chat = self.background.remove(index);
                if chat.screen().size() != (rows, cols) {
                    chat.resize(rows, cols);
                }
                chat
            }
            None => Chat::open(command, rows, cols, agent, tree)?,
        };
        self.open = Some(chat);
        Ok(())
    }

    /// Leaves the open chat running behind treetop, ending the oldest left
    /// one beyond KEPT.
    pub fn hide(&mut self) {
        if let Some(chat) = self.open.take() {
            self.background.push(chat);
        }
        while self.background.len() > KEPT {
            self.background.remove(0);
        }
    }

    /// Pumps every chat. A background chat whose program exited is dropped;
    /// the open one closes and is reported.
    pub fn pump(&mut self) -> Pumped {
        for chat in &mut self.background {
            chat.pump();
        }
        self.background.retain(|c| !c.is_ended());
        let Some(open) = &mut self.open else {
            return Pumped {
                is_stale: false,
                ended: None,
            };
        };
        let is_stale = open.pump();
        let ended = open
            .is_ended()
            .then(|| self.open.take())
            .flatten()
            .map(|chat| (chat.agent.clone(), chat.tree.clone()));
        Pumped { is_stale, ended }
    }

    /// Ends the background chats whose agent is gone.
    pub fn retain(&mut self, is_live: impl Fn(&[String]) -> bool) {
        self.background.retain(|c| is_live(&c.command));
    }

    /// Resizes every chat, so one opened later is already the right size.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        for chat in self.open.iter_mut().chain(&mut self.background) {
            chat.resize(rows, cols);
        }
    }
}

/// Ctrl+\\, which goes to the next agent's chat. Without the terminal's
/// extended key modes, crossterm reports the byte it sends (0x1c) as Ctrl+4.
pub fn is_next(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('\\' | '4'))
}

/// Ctrl+], which leaves the chat. Without the terminal's extended key modes,
/// crossterm reports the byte Ctrl+] sends (0x1d) as Ctrl+5.
pub fn is_exit(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(']' | '5'))
}

/// The bytes a terminal sends for `key`, in the xterm encoding a program
/// running under `TERM=xterm-256color` expects. Arrows, Home and End use the
/// application form when the chat has switched application cursor mode on.
pub fn encode(key: KeyEvent, application_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm's modifier parameter: 1 plus shift 1, alt 2, ctrl 4.
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let cursor = |letter: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{letter}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{letter}").into_bytes()
        } else {
            format!("\x1b[{letter}").into_bytes()
        }
    };
    let tilde = |code: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{code};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{code}~").into_bytes()
        }
    };
    let bytes = match key.code {
        KeyCode::Char(c) if ctrl => match c.to_ascii_lowercase() {
            letter @ 'a'..='z' => vec![letter as u8 - b'a' + 1],
            ' ' | '@' | '2' => vec![0],
            '[' | '3' => vec![0x1b],
            '\\' | '4' => vec![0x1c],
            ']' | '5' => vec![0x1d],
            '^' | '6' => vec![0x1e],
            '_' | '/' | '7' => vec![0x1f],
            other => other.to_string().into_bytes(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return cursor('A'),
        KeyCode::Down => return cursor('B'),
        KeyCode::Right => return cursor('C'),
        KeyCode::Left => return cursor('D'),
        KeyCode::Home => return cursor('H'),
        KeyCode::End => return cursor('F'),
        KeyCode::Insert => return tilde(2),
        KeyCode::Delete => return tilde(3),
        KeyCode::PageUp => return tilde(5),
        KeyCode::PageDown => return tilde(6),
        KeyCode::F(n @ 1..=4) => format!("\x1bO{}", char::from(b'P' + n - 1)).into_bytes(),
        KeyCode::F(n @ 5..=12) => {
            let codes = [15, 17, 18, 19, 20, 21, 23, 24];
            return tilde(codes[usize::from(n - 5)]);
        }
        _ => Vec::new(),
    };
    if alt && !bytes.is_empty() {
        [vec![0x1b], bytes].concat()
    } else {
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn plain(code: KeyCode) -> Vec<u8> {
        encode(key(code, KeyModifiers::NONE), false)
    }

    #[test]
    fn encodes_text_and_editing_keys() {
        assert_eq!(plain(KeyCode::Char('é')), "é".as_bytes());
        assert_eq!(plain(KeyCode::Enter), b"\r");
        assert_eq!(plain(KeyCode::Backspace), [0x7f]);
        assert_eq!(plain(KeyCode::Tab), b"\t");
        assert_eq!(plain(KeyCode::BackTab), b"\x1b[Z");
        assert_eq!(plain(KeyCode::Esc), [0x1b]);
        assert_eq!(plain(KeyCode::Delete), b"\x1b[3~");
        assert_eq!(plain(KeyCode::PageUp), b"\x1b[5~");
        assert_eq!(plain(KeyCode::F(1)), b"\x1bOP");
        assert_eq!(plain(KeyCode::F(12)), b"\x1b[24~");
    }

    #[test]
    fn encodes_control_and_alt() {
        let ctrl = |c| encode(key(KeyCode::Char(c), KeyModifiers::CONTROL), false);
        assert_eq!(ctrl('c'), [3]);
        assert_eq!(ctrl('Z'), [26]);
        assert_eq!(ctrl(' '), [0]);
        assert_eq!(ctrl('5'), [0x1d]);
        assert_eq!(
            encode(key(KeyCode::Char('b'), KeyModifiers::ALT), false),
            b"\x1bb"
        );
        assert_eq!(
            encode(key(KeyCode::Enter, KeyModifiers::ALT), false),
            b"\x1b\r"
        );
    }

    #[test]
    fn arrows_follow_application_cursor_mode_and_modifiers() {
        assert_eq!(plain(KeyCode::Up), b"\x1b[A");
        assert_eq!(
            encode(key(KeyCode::Up, KeyModifiers::NONE), true),
            b"\x1bOA"
        );
        assert_eq!(
            encode(key(KeyCode::Left, KeyModifiers::CONTROL), true),
            b"\x1b[1;5D"
        );
        assert_eq!(
            encode(key(KeyCode::Home, KeyModifiers::SHIFT), false),
            b"\x1b[1;2H"
        );
        assert_eq!(
            encode(key(KeyCode::Delete, KeyModifiers::CONTROL), false),
            b"\x1b[3;5~"
        );
    }

    #[test]
    fn the_last_sync_marker_in_a_chunk_decides_where_the_chat_is() {
        assert_eq!(sync_state(b"\x1b[?2026hdraw"), Some(true));
        assert_eq!(sync_state(b"\x1b[?2026hdraw\x1b[?2026l"), Some(false));
        assert_eq!(sync_state(b"\x1b[?2026l\x1b[?2026hnext"), Some(true));
        assert_eq!(sync_state(b"plain output"), None);
    }

    #[test]
    fn ctrl_backslash_goes_to_the_next_chat_however_it_is_reported() {
        assert!(is_next(key(KeyCode::Char('\\'), KeyModifiers::CONTROL)));
        assert!(is_next(key(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(!is_next(key(KeyCode::Char('\\'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_right_bracket_leaves_however_it_is_reported() {
        assert!(is_exit(key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(is_exit(key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
        assert!(!is_exit(key(KeyCode::Char(']'), KeyModifiers::NONE)));
        assert!(!is_exit(key(KeyCode::Char('z'), KeyModifiers::CONTROL)));
    }
}
