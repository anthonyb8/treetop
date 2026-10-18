//! An agent's chat inside treetop. The chat runs in a pseudo-terminal treetop
//! owns, its output parsed into a screen treetop draws in its own frame, so
//! the real terminal is never handed over: whatever modes the chat switches
//! on (mouse tracking, extended keys, bracketed paste) land in the parser,
//! and treetop sees every key first, keeping Ctrl+Q to leave.
//!
//! A chat left with Ctrl+Q keeps running in the background, its screen kept
//! current, so opening it again is instant.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

use crate::plugin::Screen;

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

/// For a chat whose screen treetop cannot read, how long after its first
/// draw a message is typed: Claude Code draws its input box before it takes
/// keys, and typing sooner loses them.
const TYPE_AFTER: Duration = Duration::from_millis(1500);

/// How long after the text is pasted Enter follows, so the chat reads the
/// paste as a paste and the Enter as sending it.
const SUBMIT_AFTER: Duration = Duration::from_millis(300);

/// How long a chat gets to show an empty input box before a message to it
/// fails.
const INPUT_LIMIT: Duration = Duration::from_secs(20);

/// How long a paste gets to show in the input box, and an Enter to empty
/// it again, before the message fails.
const LAND_LIMIT: Duration = Duration::from_secs(5);

/// The last lines of a chat's screen read for its hints' `away` markers,
/// where the footer of a screen of other sessions sits, so a transcript that
/// quotes one higher up does not count.
const AWAY_LINES: usize = 4;

/// Where a message waiting to be typed into a chat stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Not typed yet, since the time it was queued.
    Waiting(Instant),
    /// Pasted at this time; Enter follows once the paste shows.
    Pasted(Instant),
    /// Enter pressed at this time; sent once the input box empties.
    Submitted(Instant),
}

/// A line waiting to be typed into a chat.
struct Outgoing {
    text: String,
    stage: Stage,
}

/// What a chat's screen says about typing a message into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// The input box is there and empty.
    Empty,
    /// The input box holds this text.
    Holding(String),
    /// A dialog has the keys, so a message would answer it.
    Dialog,
    /// No input box yet.
    Missing,
}

/// The chat's input box as `hints` describe it, read from `text`, the
/// screen's rows.
pub fn input(text: &str, hints: &Screen) -> Input {
    let lines: Vec<&str> = text.lines().collect();
    let is_rule = |line: &str| line.trim_start().starts_with(&hints.rule);
    let opening = (1..lines.len())
        .rev()
        .find(|&at| lines[at].trim_start().starts_with(&hints.input) && is_rule(lines[at - 1]));
    let closing = opening.and_then(|at| (at + 1..lines.len()).find(|&end| is_rule(lines[end])));
    let (Some(at), Some(end)) = (opening, closing) else {
        let is_dialog = hints
            .dialogs
            .iter()
            .any(|marker| text.contains(marker.as_str()));
        return if is_dialog {
            Input::Dialog
        } else {
            Input::Missing
        };
    };
    let first = lines[at].trim_start()[hints.input.len()..].trim();
    let held: Vec<&str> = std::iter::once(first)
        .chain(lines[at + 1..end].iter().map(|line| line.trim()))
        .filter(|line| !line.is_empty())
        .collect();
    if held.is_empty() {
        Input::Empty
    } else {
        Input::Holding(held.join(" "))
    }
}

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
    /// When its first draw was shown, which starts TYPE_AFTER.
    ready_at: Option<Instant>,
    /// Lines to type and send once it takes keys, oldest first.
    outbox: VecDeque<Outgoing>,
    /// How its screen shows it takes a message; None types on a timer.
    hints: Option<Screen>,
}

/// Whether `key` on a chat showing `text` would leave its agent: ← with no
/// modifiers in an empty input box, for a chat whose hints say it can leave.
fn leaves(key: KeyEvent, text: &str, hints: &Screen) -> bool {
    !hints.away.is_empty()
        && key.code == KeyCode::Left
        && key.modifiers.is_empty()
        && input(text, hints) == Input::Empty
}

/// Whether `text`, a chat's screen, is a screen of other sessions rather
/// than its agent's chat.
fn is_away(text: &str, hints: &Screen) -> bool {
    text.lines()
        .rev()
        .filter(|line| !line.trim().is_empty())
        .take(AWAY_LINES)
        .any(|line| {
            hints
                .away
                .iter()
                .any(|marker| line.contains(marker.as_str()))
        })
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
            ready_at: None,
            outbox: VecDeque::new(),
            hints: None,
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
            if self.is_ready {
                self.ready_at = Some(Instant::now());
            }
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

    /// Passes a key on, except ← in an empty input box of a chat that
    /// leaves its agent on it, so the chat stays the agent's.
    pub fn send_key(&mut self, key: KeyEvent) {
        if self
            .hints
            .as_ref()
            .is_some_and(|hints| leaves(key, &self.screen().contents(), hints))
        {
            return;
        }
        let bytes = encode(key, self.screen().application_cursor());
        self.write(&bytes);
    }

    /// The chat has left its agent for a screen of other sessions.
    fn is_away(&self) -> bool {
        self.hints
            .as_ref()
            .is_some_and(|hints| is_away(&self.screen().contents(), hints))
    }

    /// Passes a mouse event on, in the mode and encoding the chat asked for.
    /// `area` is where the chat is drawn on screen, inside its popup.
    pub fn send_mouse(&mut self, event: MouseEvent, area: Rect) {
        let screen = self.screen();
        let bytes = encode_mouse(
            event,
            area,
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
            screen.application_cursor(),
        );
        if let Some(bytes) = bytes {
            self.write(&bytes);
        }
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

    /// Types the waiting lines in, one at a time. With hints, each waits for
    /// an empty input box, is pasted, gets Enter once the paste shows, and
    /// counts as sent once the box empties again; a dialog or a draft in the
    /// box fails it rather than typing into either. With none, the text is
    /// pasted TYPE_AFTER after the first draw and Enter follows SUBMIT_AFTER
    /// later. Returns how many were sent, and why each that failed did.
    fn flush(&mut self) -> (usize, Vec<String>) {
        let (mut sent, mut failed) = (0, Vec::new());
        let Some(hints) = self.hints.clone() else {
            return (self.flush_blind(), failed);
        };
        while let Some(next) = self.outbox.front() {
            let state = input(&self.screen().contents(), &hints);
            let (stage, text) = (next.stage, next.text.clone());
            let fail = match (stage, state) {
                (Stage::Waiting(_), Input::Empty) if self.is_ready => {
                    self.paste(&text);
                    self.set_stage(Stage::Pasted(Instant::now()));
                    return (sent, failed);
                }
                (Stage::Waiting(_), Input::Holding(_)) => {
                    Some("its input box already holds a draft; open its chat")
                }
                (Stage::Waiting(_), Input::Dialog) => {
                    Some("a dialog is open in its chat; answer it there")
                }
                (Stage::Waiting(since), _) if since.elapsed() >= INPUT_LIMIT => {
                    Some("its input box never showed")
                }
                (Stage::Pasted(_), Input::Holding(_)) => {
                    self.write(b"\r");
                    self.set_stage(Stage::Submitted(Instant::now()));
                    return (sent, failed);
                }
                (Stage::Pasted(at), _) if at.elapsed() >= LAND_LIMIT => {
                    Some("the paste never showed in its input box")
                }
                (Stage::Submitted(_), Input::Empty) => {
                    self.outbox.pop_front();
                    sent += 1;
                    continue;
                }
                (Stage::Submitted(at), _) if at.elapsed() >= LAND_LIMIT => {
                    Some("Enter did not send it; it may still be in the input box")
                }
                _ => return (sent, failed),
            };
            if let Some(why) = fail {
                self.outbox.pop_front();
                failed.push(why.to_string());
            }
        }
        (sent, failed)
    }

    /// `flush`, adding who each message went to, or failed to reach and
    /// why, to the lists.
    fn flush_into(
        &mut self,
        delivered: &mut Vec<(String, String)>,
        failed: &mut Vec<(String, String, String)>,
    ) {
        let (sent, reasons) = self.flush();
        delivered.extend((0..sent).map(|_| (self.agent.clone(), self.tree.clone())));
        failed.extend(
            reasons
                .into_iter()
                .map(|why| (self.agent.clone(), self.tree.clone(), why)),
        );
    }

    fn set_stage(&mut self, stage: Stage) {
        if let Some(next) = self.outbox.front_mut() {
            next.stage = stage;
        }
    }

    /// `flush` for a chat whose screen treetop cannot read.
    fn flush_blind(&mut self) -> usize {
        if self.ready_at.is_none_or(|at| at.elapsed() < TYPE_AFTER) {
            return 0;
        }
        let mut sent = 0;
        while let Some(next) = self.outbox.front_mut() {
            match next.stage {
                Stage::Waiting(_) => {
                    next.stage = Stage::Pasted(Instant::now());
                    let text = next.text.clone();
                    self.paste(&text);
                    return sent;
                }
                Stage::Pasted(at) | Stage::Submitted(at) if at.elapsed() >= SUBMIT_AFTER => {
                    self.write(b"\r");
                    self.outbox.pop_front();
                    sent += 1;
                }
                Stage::Pasted(_) | Stage::Submitted(_) => return sent,
            }
        }
        sent
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
    /// The open chat left its agent and was opened on it again: its agent
    /// and tree.
    pub kept: Option<(String, String)>,
    /// Agent and tree of each message typed in and sent.
    pub delivered: Vec<(String, String)>,
    /// Agent, tree and why, of each message that could not be sent.
    pub failed: Vec<(String, String, String)>,
}

/// The chat on screen, if one is, and the ones left running behind it.
#[derive(Default)]
pub struct Chats {
    open: Option<Chat>,
    /// Left with Ctrl+Q, oldest first.
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
    /// there is one, at once, else a new one. `hints` say how its screen
    /// shows that it takes a message, or has left its agent.
    pub fn show(
        &mut self,
        command: &[String],
        (rows, cols): (u16, u16),
        agent: String,
        tree: String,
        hints: Option<Screen>,
    ) -> Result<()> {
        let mut chat = match self.background.iter().position(|c| c.command == command) {
            Some(index) => {
                let mut chat = self.background.remove(index);
                if chat.screen().size() != (rows, cols) {
                    chat.resize(rows, cols);
                }
                chat
            }
            None => Chat::open(command, rows, cols, agent, tree)?,
        };
        chat.hints = chat.hints.take().or(hints);
        self.open = Some(chat);
        Ok(())
    }

    /// Leaves the open chat running behind treetop, ending the oldest left
    /// one beyond KEPT.
    pub fn hide(&mut self) {
        if let Some(chat) = self.open.take() {
            self.background.push(chat);
        }
        self.trim();
    }

    /// Ends the oldest background chats beyond KEPT, never one still holding
    /// a message to send.
    fn trim(&mut self) {
        while self.background.len() > KEPT {
            match self.background.iter().position(|c| c.outbox.is_empty()) {
                Some(index) => drop(self.background.remove(index)),
                None => break,
            }
        }
    }

    /// Queues `text` to be typed into the chat running `command` and sent:
    /// the one on screen or left running if there is one, else a new one
    /// opened behind treetop for it. `hints` say how its screen shows that
    /// it takes a message.
    pub fn deliver(
        &mut self,
        command: &[String],
        text: &str,
        (rows, cols): (u16, u16),
        (agent, tree): (String, String),
        hints: Option<Screen>,
    ) -> Result<()> {
        let outgoing = Outgoing {
            text: text.to_string(),
            stage: Stage::Waiting(Instant::now()),
        };
        let running = self
            .open
            .iter_mut()
            .chain(&mut self.background)
            .find(|c| c.command == command);
        match running {
            Some(chat) => {
                chat.hints = chat.hints.take().or(hints);
                chat.outbox.push_back(outgoing);
            }
            None => {
                let mut chat = Chat::open(command, rows, cols, agent, tree)?;
                chat.hints = hints;
                chat.outbox.push_back(outgoing);
                self.background.push(chat);
                self.trim();
            }
        }
        Ok(())
    }

    /// Pumps every chat and types in what is waiting. A background chat whose
    /// program exited is dropped; the open one closes and is reported.
    pub fn pump(&mut self) -> Pumped {
        let mut delivered = Vec::new();
        let mut failed = Vec::new();
        for chat in &mut self.background {
            chat.pump();
            chat.flush_into(&mut delivered, &mut failed);
        }
        let ended = |chat: &Chat| {
            (
                chat.agent.clone(),
                chat.tree.clone(),
                "its chat ended".into(),
            )
        };
        self.background.retain(|c| {
            if c.is_ended() && !c.outbox.is_empty() {
                failed.push(ended(c));
            }
            !c.is_ended()
        });
        let Some(open) = &mut self.open else {
            return Pumped {
                is_stale: false,
                ended: None,
                kept: None,
                delivered,
                failed,
            };
        };
        let mut is_stale = open.pump();
        let mut kept = None;
        if open.is_away() && open.outbox.is_empty() {
            is_stale = true;
            match self.reopen() {
                Ok(agent) => kept = Some(agent),
                Err(agent) => {
                    return Pumped {
                        is_stale,
                        ended: Some(agent),
                        kept,
                        delivered,
                        failed,
                    };
                }
            }
        }
        let Some(open) = &mut self.open else {
            return Pumped {
                is_stale,
                ended: None,
                kept,
                delivered,
                failed,
            };
        };
        open.flush_into(&mut delivered, &mut failed);
        if open.is_ended() && !open.outbox.is_empty() {
            failed.push(ended(open));
        }
        let ended = open
            .is_ended()
            .then(|| self.open.take())
            .flatten()
            .map(|chat| (chat.agent.clone(), chat.tree.clone()));
        Pumped {
            is_stale,
            ended,
            kept,
            delivered,
            failed,
        }
    }

    /// Opens the open chat on its agent again, once it has left for a
    /// screen of other sessions, so the popup only ever shows the agent it
    /// was opened on. Returns that agent and tree, as an error when it could
    /// not be opened again, which closes it.
    fn reopen(&mut self) -> Result<(String, String), (String, String)> {
        let Some(old) = self.open.take() else {
            return Err(Default::default());
        };
        let (rows, cols) = old.screen().size();
        let who = (old.agent.clone(), old.tree.clone());
        let hints = old.hints.clone();
        // The old client ends first, so the agent has one chat attached.
        let command = old.command.clone();
        drop(old);
        let mut chat = Chat::open(&command, rows, cols, who.0.clone(), who.1.clone())
            .map_err(|_| who.clone())?;
        chat.hints = hints;
        self.open = Some(chat);
        Ok(who)
    }

    /// Ends the background chats whose agent is gone, except one still
    /// delivering a message: attaching to a stopped agent starts it again.
    pub fn retain(&mut self, is_live: impl Fn(&[String]) -> bool) {
        self.background
            .retain(|c| !c.outbox.is_empty() || is_live(&c.command));
    }

    /// Resizes every chat, so one opened later is already the right size.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        for chat in self.open.iter_mut().chain(&mut self.background) {
            chat.resize(rows, cols);
        }
    }
}

/// Lines a wheel step scrolls. Claude Code moves one line per wheel event,
/// so each step is passed on this many times, as terminals scroll their own
/// scrollback; a chat that has not asked for the mouse gets as many arrows.
const WHEEL_LINES: usize = 3;

/// The bytes a terminal sends for a mouse event, when the chat's `mode`
/// asks for that kind of event, in its `encoding`. Rows are shifted by
/// the corner of `area`, where the chat is drawn; events outside it, on the
/// popup's frame or the table round it, are dropped.
/// A chat that has not asked for the mouse gets the wheel as arrow keys,
/// which scroll most programs, the way terminals do on an alternate screen.
fn encode_mouse(
    event: MouseEvent,
    area: Rect,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
    application_cursor: bool,
) -> Option<Vec<u8>> {
    if !area.contains(Position::new(event.column, event.row)) {
        return None;
    }
    let (col, row) = (event.column - area.x, event.row - area.y);
    if mode == MouseProtocolMode::None {
        let arrow = match event.kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            _ => return None,
        };
        let bytes = encode(KeyEvent::new(arrow, KeyModifiers::NONE), application_cursor);
        return Some(bytes.repeat(WHEEL_LINES));
    }
    let button_code = |button: MouseButton| match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let reports_release = mode != MouseProtocolMode::Press;
    let reports_drag = matches!(
        mode,
        MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion
    );
    let (code, is_release) = match event.kind {
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
        MouseEventKind::Down(button) => (button_code(button), false),
        MouseEventKind::Up(button) if reports_release => (button_code(button), true),
        MouseEventKind::Drag(button) if reports_drag => (button_code(button) + 32, false),
        MouseEventKind::Moved if mode == MouseProtocolMode::AnyMotion => (35, false),
        _ => return None,
    };
    let modifiers = u16::from(event.modifiers.contains(KeyModifiers::SHIFT)) * 4
        + u16::from(event.modifiers.contains(KeyModifiers::ALT)) * 8
        + u16::from(event.modifiers.contains(KeyModifiers::CONTROL)) * 16;
    let (code, col, row) = (code + modifiers, col + 1, row + 1);
    let is_wheel = matches!(
        event.kind,
        MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
    );
    let bytes = match encoding {
        MouseProtocolEncoding::Sgr => {
            let end = if is_release { 'm' } else { 'M' };
            format!("\x1b[<{code};{col};{row}{end}").into_bytes()
        }
        // The older encodings have no release code of their own: a release
        // is button 3. Each value is offset by 32; the default encoding
        // writes it as one byte, so it cannot pass 223, and UTF-8 as a
        // character.
        legacy => {
            let code = if is_release { 3 + modifiers } else { code };
            let mut bytes = b"\x1b[M".to_vec();
            for value in [code, col, row] {
                let value = value.saturating_add(32);
                if legacy == MouseProtocolEncoding::Utf8 {
                    let c = char::from_u32(u32::from(value)).unwrap_or(' ');
                    bytes.extend(c.to_string().as_bytes());
                } else {
                    bytes.push(u8::try_from(value).unwrap_or(255));
                }
            }
            bytes
        }
    };
    Some(if is_wheel {
        bytes.repeat(WHEEL_LINES)
    } else {
        bytes
    })
}

/// Ctrl+\\, which goes to the next agent's chat. Without the terminal's
/// extended key modes, crossterm reports the byte it sends (0x1c) as Ctrl+4.
pub fn is_next(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('\\' | '4'))
}

/// Ctrl+Q, which leaves the chat. Claude Code binds no Ctrl+Q, and in raw
/// mode it is no flow control, so the chat never needs it; Ctrl+], which
/// Claude Code uses, passes through.
pub fn is_exit(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('q' | 'Q'))
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

    /// Claude Code's own hints, as its plugin gives them.
    fn claude() -> Screen {
        Screen {
            input: "❯".into(),
            rule: "─".into(),
            dialogs: vec!["Do you want to".into(), "❯ 1.".into()],
            away: vec![
                "describe a task for a new session".into(),
                "enter to return".into(),
            ],
        }
    }

    /// The bottom of Claude Code's agent view, which ← in an empty input
    /// box of `claude attach` opens, as it draws it.
    const AGENT_VIEW: &str = "~/treetop\n ✻ dfd   Needs input · tell the agent what you'd like\n\
         ✻ fix   Done · tests green\n\
         ────────────────────────────────────────\n❯ describe a task for a new session\n\
         ────────────────────────────────────────\n  enter to return · space to reply · ctrl+x to delete\n";

    #[test]
    fn the_agent_view_is_away_and_the_chat_is_not() {
        assert!(is_away(AGENT_VIEW, &claude()));
        assert!(!is_away(&idle(""), &claude()));
        let quoted = format!("● press enter to return to it\n{}", idle(""));
        assert!(
            !is_away(&quoted, &claude()),
            "the transcript quoting a marker above the box is still the chat"
        );
    }

    #[test]
    fn left_in_an_empty_input_box_is_kept_from_the_chat() {
        let left = key(KeyCode::Left, KeyModifiers::NONE);
        assert!(leaves(left, &idle(""), &claude()));
        assert!(
            !leaves(left, &idle("commit it"), &claude()),
            "← moves through a draft"
        );
        assert!(!leaves(
            key(KeyCode::Left, KeyModifiers::ALT),
            &idle(""),
            &claude()
        ));
        let no_away = Screen {
            away: Vec::new(),
            ..claude()
        };
        assert!(!leaves(left, &idle(""), &no_away));
    }

    /// The bottom of an idle Claude Code chat, as `claude attach` draws it,
    /// with `input` in its box.
    fn idle(input: &str) -> String {
        format!(
            "❯ dfd\n● \"dfd\" looks like a stray keypress.\n✻ Worked for 4s\n\
             ─────────────────────────────── dfd ─\n❯ {input}\n\
             ──────────────────────────────────────\n  [anthony@ treetop]\n  ⏵⏵ auto mode on"
        )
    }

    #[test]
    fn reads_an_empty_input_box_and_one_holding_a_draft() {
        assert_eq!(input(&idle(""), &claude()), Input::Empty);
        assert_eq!(
            input(&idle("commit it"), &claude()),
            Input::Holding("commit it".into()),
            "a draft left in the box, which a message must not be added to"
        );
    }

    #[test]
    fn a_line_quoted_above_the_box_is_not_the_input() {
        // The transcript's own `❯ dfd` has no rule above it.
        let only_transcript = "❯ dfd\n● answer\n";
        assert_eq!(input(only_transcript, &claude()), Input::Missing);
    }

    #[test]
    fn a_dialog_in_place_of_the_box_is_a_dialog() {
        let dialog = "● Bash(rm -rf node_modules)\n\
             ──────────────────────\n Bash command\n   rm -rf node_modules\n\
             Do you want to proceed?\n ❯ 1. Yes\n   2. No, and tell Claude what to do differently (esc)\n";
        assert_eq!(input(dialog, &claude()), Input::Dialog);
    }

    #[test]
    fn a_long_draft_spans_the_lines_of_the_box() {
        let screen = "──────── x ─\n❯ first line\n  second line\n────────────\n";
        assert_eq!(
            input(screen, &claude()),
            Input::Holding("first line second line".into())
        );
    }

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

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A chat drawn from column 0, row 1 of the screen.
    const BELOW_A_BAR: Rect = Rect::new(0, 1, 80, 24);

    fn sgr(event: MouseEvent, mode: MouseProtocolMode) -> Option<Vec<u8>> {
        encode_mouse(event, BELOW_A_BAR, mode, MouseProtocolEncoding::Sgr, false)
    }

    #[test]
    fn a_popup_shifts_clicks_by_its_corner_and_drops_those_outside_it() {
        let popup = Rect::new(10, 4, 20, 5);
        let click = |column, row| {
            encode_mouse(
                mouse(MouseEventKind::Down(MouseButton::Left), column, row),
                popup,
                MouseProtocolMode::PressRelease,
                MouseProtocolEncoding::Sgr,
                false,
            )
        };
        assert_eq!(click(10, 4).unwrap(), b"\x1b[<0;1;1M");
        assert_eq!(click(29, 8).unwrap(), b"\x1b[<0;20;5M");
        assert_eq!(click(9, 4), None, "left of it");
        assert_eq!(click(30, 8), None, "right of it");
        assert_eq!(click(12, 9), None, "below it");
    }

    #[test]
    fn passes_the_wheel_and_clicks_in_sgr_below_the_bar() {
        let any = MouseProtocolMode::AnyMotion;
        assert_eq!(
            sgr(mouse(MouseEventKind::ScrollUp, 9, 5), any).unwrap(),
            b"\x1b[<64;10;5M".repeat(WHEEL_LINES),
            "a wheel step scrolls WHEEL_LINES lines"
        );
        assert_eq!(
            sgr(mouse(MouseEventKind::ScrollDown, 0, 1), any).unwrap(),
            b"\x1b[<65;1;1M".repeat(WHEEL_LINES)
        );
        assert_eq!(
            sgr(mouse(MouseEventKind::Down(MouseButton::Left), 2, 3), any).unwrap(),
            b"\x1b[<0;3;3M"
        );
        assert_eq!(
            sgr(mouse(MouseEventKind::Up(MouseButton::Left), 2, 3), any).unwrap(),
            b"\x1b[<0;3;3m"
        );
        assert_eq!(
            sgr(mouse(MouseEventKind::ScrollUp, 2, 0), any),
            None,
            "on the bar"
        );
    }

    #[test]
    fn reports_only_what_the_mode_asks_for() {
        let press = MouseProtocolMode::Press;
        let release = MouseProtocolMode::PressRelease;
        let drag = mouse(MouseEventKind::Drag(MouseButton::Left), 1, 2);
        let up = mouse(MouseEventKind::Up(MouseButton::Left), 1, 2);
        let moved = mouse(MouseEventKind::Moved, 1, 2);
        assert_eq!(sgr(up, press), None);
        assert!(sgr(up, release).is_some());
        assert_eq!(sgr(drag, release), None);
        assert_eq!(
            sgr(drag, MouseProtocolMode::ButtonMotion).unwrap(),
            b"\x1b[<32;2;2M"
        );
        assert_eq!(sgr(moved, MouseProtocolMode::ButtonMotion), None);
        assert_eq!(
            sgr(moved, MouseProtocolMode::AnyMotion).unwrap(),
            b"\x1b[<35;2;2M"
        );
        assert!(sgr(mouse(MouseEventKind::ScrollUp, 1, 2), press).is_some());
    }

    #[test]
    fn encodes_modifiers_and_the_legacy_encodings() {
        let shifted = MouseEvent {
            modifiers: KeyModifiers::SHIFT | KeyModifiers::CONTROL,
            ..mouse(MouseEventKind::Down(MouseButton::Right), 4, 6)
        };
        assert_eq!(
            sgr(shifted, MouseProtocolMode::PressRelease).unwrap(),
            b"\x1b[<22;5;6M"
        );
        let legacy = |kind, encoding| {
            encode_mouse(
                mouse(kind, 4, 6),
                BELOW_A_BAR,
                MouseProtocolMode::PressRelease,
                encoding,
                false,
            )
        };
        let default = MouseProtocolEncoding::Default;
        assert_eq!(
            legacy(MouseEventKind::Down(MouseButton::Left), default).unwrap(),
            [0x1b, b'[', b'M', 32, 37, 38]
        );
        assert_eq!(
            legacy(MouseEventKind::Up(MouseButton::Left), default).unwrap(),
            [0x1b, b'[', b'M', 35, 37, 38]
        );
    }

    #[test]
    fn a_chat_without_the_mouse_gets_the_wheel_as_arrows() {
        let none = MouseProtocolMode::None;
        let up = mouse(MouseEventKind::ScrollUp, 1, 4);
        assert_eq!(sgr(up, none).unwrap(), b"\x1b[A\x1b[A\x1b[A");
        assert_eq!(
            encode_mouse(up, BELOW_A_BAR, none, MouseProtocolEncoding::Sgr, true).unwrap(),
            b"\x1bOA\x1bOA\x1bOA"
        );
        assert_eq!(
            sgr(mouse(MouseEventKind::Down(MouseButton::Left), 1, 4), none),
            None
        );
    }

    #[test]
    fn ctrl_backslash_goes_to_the_next_chat_however_it_is_reported() {
        assert!(is_next(key(KeyCode::Char('\\'), KeyModifiers::CONTROL)));
        assert!(is_next(key(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(!is_next(key(KeyCode::Char('\\'), KeyModifiers::NONE)));
    }

    #[test]
    fn ctrl_q_leaves_and_ctrl_right_bracket_goes_to_the_chat() {
        assert!(is_exit(key(KeyCode::Char('q'), KeyModifiers::CONTROL)));
        assert!(!is_exit(key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert!(!is_exit(key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(!is_exit(key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
    }
}
