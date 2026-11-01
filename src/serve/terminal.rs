//! The agent's own screen on the phone, for what a chat line cannot answer:
//! a permission prompt, a question with options, a menu. It is the same
//! `claude attach` the terminal's chat popup runs, in a pseudo-terminal the
//! server owns (`crate::chat::Chat`), sized for a phone. Its screen goes to
//! the phone as rows of coloured text whenever it changes, at most ten
//! times a second, and the keys the phone sends are written to it.
//!
//! One terminal per agent, shared by every view of it, so two screens on
//! the phone see the same thing. One nobody has looked at for IDLE is
//! ended; ending it leaves the agent running, as leaving its chat in the
//! terminal does.

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};
use vt100::Color;

use crate::chat::Chat;

/// The size a terminal opens at when the phone names none: about what an
/// iPhone shows across, in a monospace font that stays readable.
pub const COLS: u16 = 44;
pub const ROWS: u16 = 32;

/// How often the terminal is read, which is also the most often its screen
/// is sent: ten times a second.
const TICK: Duration = Duration::from_millis(100);

/// How long a terminal nobody is watching is kept, so coming back to it is
/// instant, before it is ended.
const IDLE: Duration = Duration::from_secs(5 * 60);

/// How often a quiet stream says it is still there.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// One agent's terminal, and what its views wait on.
pub struct Terminal {
    state: Mutex<Shared>,
    changed: Condvar,
}

struct Shared {
    chat: Chat,
    /// Bumped each time the screen is worth sending again.
    version: u64,
    /// Views open now, and when the last one closed.
    viewers: usize,
    left_at: Instant,
}

/// A terminal's name: the tree's checkout and the command that opens its
/// agent's chat.
pub type Key = (PathBuf, Vec<String>);

/// Every open terminal, by its key.
#[derive(Default)]
pub struct Terminals {
    open: Mutex<HashMap<Key, Arc<Terminal>>>,
}

impl Terminals {
    /// The terminal running `command`, opened at `rows` by `cols` if it is
    /// not open yet, else resized to them.
    pub fn get(
        &self,
        key: Key,
        rows: u16,
        cols: u16,
        agent: &str,
        tree: &str,
    ) -> Result<Arc<Terminal>> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = open.get(&key) {
            found.lock().chat.resize(rows, cols);
            return Ok(Arc::clone(found));
        }
        let chat = Chat::open(&key.1, rows, cols, agent.into(), tree.into())?;
        let terminal = Arc::new(Terminal {
            state: Mutex::new(Shared {
                chat,
                version: 0,
                viewers: 0,
                left_at: Instant::now(),
            }),
            changed: Condvar::new(),
        });
        open.insert(key, Arc::clone(&terminal));
        thread::spawn({
            let terminal = Arc::downgrade(&terminal);
            move || Terminal::run(&terminal)
        });
        Ok(terminal)
    }

    /// Ends the terminals nobody has watched for IDLE, and those whose
    /// program has exited.
    pub fn sweep(&self) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        open.retain(|_, terminal| {
            let shared = terminal.lock();
            !shared.chat.is_ended() && (shared.viewers > 0 || shared.left_at.elapsed() < IDLE)
        });
    }

    /// The terminal already open for `key`, to write keys to.
    pub fn find(&self, key: &Key) -> Option<Arc<Terminal>> {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        open.get(key).cloned()
    }
}

impl Terminal {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Reads the terminal every TICK until its program exits or the
    /// terminal is let go of, waking its views when the screen is worth
    /// sending again. It holds the terminal only while reading it, so
    /// letting go of it ends its program.
    fn run(terminal: &Weak<Terminal>) {
        loop {
            thread::sleep(TICK);
            let Some(terminal) = terminal.upgrade() else {
                return;
            };
            let mut shared = terminal.lock();
            let is_stale = shared.chat.pump();
            let is_ended = shared.chat.is_ended();
            if is_stale || is_ended {
                shared.version += 1;
                terminal.changed.notify_all();
            }
            if is_ended {
                return;
            }
        }
    }

    /// Writes the keys `names` names, in order, and `text` as a paste.
    pub fn write(&self, names: &[String], text: Option<&str>) -> Result<()> {
        let keys: Vec<KeyEvent> = names
            .iter()
            .map(|name| key(name).ok_or_else(|| anyhow::anyhow!("no key called {name}")))
            .collect::<Result<_>>()?;
        let mut shared = self.lock();
        if shared.chat.is_ended() {
            bail!("the agent's terminal has closed");
        }
        if let Some(text) = text.filter(|t| !t.is_empty()) {
            shared.chat.paste(text);
        }
        for key in keys {
            shared.chat.send_key(key);
        }
        Ok(())
    }

    /// The screen as server-sent events: one `screen` event now, then one
    /// each time it changes, until the phone goes away or the terminal
    /// closes, which an `ended` event says.
    pub fn stream(self: Arc<Self>) -> impl Read + Send + 'static {
        let (tx, rx) = sync_channel::<Vec<u8>>(4);
        thread::spawn(move || {
            self.lock().viewers += 1;
            self.watch(&tx);
            let mut shared = self.lock();
            shared.viewers -= 1;
            shared.left_at = Instant::now();
        });
        super::chat::Events::new(rx)
    }

    fn watch(&self, tx: &SyncSender<Vec<u8>>) {
        let mut sent = None;
        let mut said_at = Instant::now();
        loop {
            let (frame, version, is_ended) = {
                let mut shared = self.lock();
                if sent == Some(shared.version) && !shared.chat.is_ended() {
                    shared = self
                        .changed
                        .wait_timeout(shared, HEARTBEAT)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                let changed = sent != Some(shared.version);
                let frame = changed.then(|| screen_json(shared.chat.screen()));
                (frame, shared.version, shared.chat.is_ended())
            };
            if let Some(frame) = frame {
                if tx.send(super::chat::event("screen", &frame)).is_err() {
                    return;
                }
                sent = Some(version);
                said_at = Instant::now();
            } else if said_at.elapsed() >= HEARTBEAT {
                if tx.send(b": still here\n\n".to_vec()).is_err() {
                    return;
                }
                said_at = Instant::now();
            }
            if is_ended {
                let _ = tx.send(super::chat::event("ended", &json!({})));
                return;
            }
        }
    }
}

/// The key a name from the phone's key row stands for.
pub fn key(name: &str) -> Option<KeyEvent> {
    let plain = |code| Some(KeyEvent::new(code, KeyModifiers::NONE));
    match name {
        "Enter" => plain(KeyCode::Enter),
        "Esc" => plain(KeyCode::Esc),
        "Tab" => plain(KeyCode::Tab),
        "Up" => plain(KeyCode::Up),
        "Down" => plain(KeyCode::Down),
        _ => {
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if !c.is_control() => plain(KeyCode::Char(c)),
                _ => None,
            }
        }
    }
}

/// The screen as rows of runs: each run text with one look, its colours as
/// CSS, so the phone draws it as the terminal shows it.
pub fn screen_json(screen: &vt100::Screen) -> Value {
    let (rows, cols) = screen.size();
    let (cursor_row, cursor_col) = screen.cursor_position();
    let mut lines = Vec::with_capacity(rows.into());
    for row in 0..rows {
        let mut runs: Vec<Value> = Vec::new();
        let mut text = String::new();
        let mut look: Option<Look> = None;
        for col in 0..cols {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let this = Look::of(cell);
            if look.as_ref() != Some(&this) {
                if let Some(done) = look.take() {
                    runs.push(done.run(&text));
                }
                text.clear();
                look = Some(this);
            }
            let contents = cell.contents();
            text.push_str(if contents.is_empty() { " " } else { contents });
        }
        if let Some(done) = look {
            let trimmed = text.trim_end();
            if !(trimmed.is_empty() && done.bg.is_none()) {
                runs.push(done.run(if done.bg.is_none() { trimmed } else { &text }));
            }
        }
        lines.push(Value::Array(runs));
    }
    json!({
        "rows": rows,
        "cols": cols,
        "lines": lines,
        "cursor": (!screen.hide_cursor()).then(|| json!([cursor_row, cursor_col])),
    })
}

/// How a run of cells looks.
#[derive(PartialEq)]
struct Look {
    fg: Option<String>,
    bg: Option<String>,
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
}

impl Look {
    fn of(cell: &vt100::Cell) -> Look {
        let (mut fg, mut bg) = (css(cell.fgcolor()), css(cell.bgcolor()));
        if cell.inverse() {
            (fg, bg) = (
                bg.or_else(|| Some(BACKGROUND.into())),
                fg.or_else(|| Some(FOREGROUND.into())),
            );
        }
        Look {
            fg,
            bg,
            bold: cell.bold(),
            dim: cell.dim(),
            italic: cell.italic(),
            underline: cell.underline(),
        }
    }

    fn run(&self, text: &str) -> Value {
        let mut run = json!({ "t": text });
        if let Some(fg) = &self.fg {
            run["fg"] = json!(fg);
        }
        if let Some(bg) = &self.bg {
            run["bg"] = json!(bg);
        }
        for (key, on) in [
            ("b", self.bold),
            ("d", self.dim),
            ("i", self.italic),
            ("u", self.underline),
        ] {
            if on {
                run[key] = json!(true);
            }
        }
        run
    }
}

/// The app's own text and page colours, which an inverted default cell
/// swaps.
const FOREGROUND: &str = "#d4be98";
const BACKGROUND: &str = "#1d2021";

/// The 16 ANSI colours in gruvbox-material, as treetop's own screen draws
/// in a 16-colour terminal.
const ANSI: [&str; 16] = [
    "#32302f", "#ea6962", "#a9b665", "#d8a657", "#7daea3", "#d3869b", "#89b482", "#d4be98",
    "#5a524c", "#ea6962", "#a9b665", "#d8a657", "#7daea3", "#d3869b", "#89b482", "#ddc7a1",
];

/// A terminal colour as CSS; None for the default, which the page draws.
fn css(color: Color) -> Option<String> {
    match color {
        Color::Default => None,
        Color::Idx(i @ 0..16) => Some(ANSI[usize::from(i)].into()),
        Color::Idx(i @ 16..232) => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Some(format!(
                "#{:02x}{:02x}{:02x}",
                level(i / 36),
                level(i / 6 % 6),
                level(i % 6)
            ))
        }
        Color::Idx(i) => {
            let grey = 8 + (i - 232) * 10;
            Some(format!("#{grey:02x}{grey:02x}{grey:02x}"))
        }
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::pool::tests::{declared, keys};

    #[test]
    fn the_key_row_s_names_are_the_keys_a_terminal_sends() {
        // Every name web/src/screens/Terminal.svelte's key row posts.
        let cases = [
            ("1", Some(KeyCode::Char('1'))),
            ("4", Some(KeyCode::Char('4'))),
            ("Up", Some(KeyCode::Up)),
            ("Down", Some(KeyCode::Down)),
            ("Tab", Some(KeyCode::Tab)),
            ("Esc", Some(KeyCode::Esc)),
            ("Enter", Some(KeyCode::Enter)),
            ("Enter2", None),
            ("\u{1b}", None),
        ];
        for (name, want) in cases {
            let got = key(name).map(|k| (k.code, k.modifiers));
            assert_eq!(got, want.map(|code| (code, KeyModifiers::NONE)), "{name:?}");
        }
    }

    #[test]
    fn the_screen_is_sent_as_runs_of_one_look() {
        let mut parser = vt100::Parser::new(3, 20, 0);
        // A bold blue "❯ 1. Clerk", then a plain " 2." and an inverted "ok".
        parser.process("\x1b[1;34m❯ 1. Clerk\x1b[0m 2.\r\n\x1b[7mok\x1b[0m".as_bytes());
        let screen = screen_json(parser.screen());
        assert_eq!(keys(&screen), declared("TerminalScreen"));
        let runs = screen["lines"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|l| l.as_array().unwrap());
        for run in runs {
            assert!(keys(run).is_subset(&declared("Run")), "{run}");
        }
        assert_eq!(
            screen["lines"][0],
            json!([{"t": "❯ 1. Clerk", "fg": ANSI[4], "b": true}, {"t": " 2."}])
        );
        assert_eq!(
            screen["lines"][1],
            json!([{"t": "ok", "fg": BACKGROUND, "bg": FOREGROUND}])
        );
        assert_eq!(screen["lines"][2], json!([]));
        assert_eq!(
            (screen["rows"].clone(), screen["cols"].clone()),
            (json!(3), json!(20))
        );
    }

    #[test]
    fn keys_pressed_on_the_phone_answer_a_prompt_on_the_agent_s_screen() {
        let terminals = Terminals::default();
        let command: Vec<String> = [
            "sh",
            "-c",
            "printf 'Pick one: 1 or 2\\n'; read answer; printf 'picked %s\\n' \"$answer\"; sleep 30",
        ]
        .map(String::from)
        .to_vec();
        let key = (PathBuf::from("/nowhere"), command);
        let terminal = terminals.get(key.clone(), 6, 30, "agent", "17").unwrap();
        let mut stream = Arc::clone(&terminal).stream();
        let shown = |stream: &mut dyn Read, want: &str| -> bool {
            let started = Instant::now();
            let mut seen = String::new();
            let mut buf = [0; 8192];
            while started.elapsed() < Duration::from_secs(10) {
                let n = stream.read(&mut buf).unwrap();
                seen.push_str(&String::from_utf8_lossy(&buf[..n]));
                if seen.contains(want) {
                    return true;
                }
            }
            false
        };
        assert!(shown(&mut stream, "Pick one: 1 or 2"));
        let found = terminals.find(&key).unwrap();
        found.write(&["2".into(), "Enter".into()], None).unwrap();
        assert!(shown(&mut stream, "picked 2"));
        assert!(found.write(&["Nope".into()], None).is_err());
    }

    #[test]
    fn a_terminal_let_go_of_ends_its_program() {
        let terminals = Terminals::default();
        let marker = format!("treetop-terminal-test-{}", std::process::id());
        // The marker is the shell's $0, which any sh shows in its command
        // line; `; true` keeps the shell from exec-ing into sleep.
        let command: Vec<String> = ["sh", "-c", "sleep 60; true", &marker]
            .map(String::from)
            .to_vec();
        let key = (PathBuf::from("/nowhere"), command);
        let terminal = terminals.get(key.clone(), 6, 30, "agent", "17").unwrap();
        let is_running = || {
            std::process::Command::new("pgrep")
                .args(["-f", &marker])
                .output()
                .is_ok_and(|out| out.status.success())
        };
        let started = Instant::now();
        while !is_running() && started.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(is_running());
        drop(terminal);
        // Left a moment ago, it is kept so coming back is instant.
        terminals.sweep();
        assert!(terminals.find(&key).is_some());
        // Watched, it is kept however long ago a view last closed.
        terminals.find(&key).unwrap().lock().left_at = Instant::now() - IDLE;
        terminals.find(&key).unwrap().lock().viewers = 1;
        terminals.sweep();
        assert!(terminals.find(&key).is_some());
        // Nobody watches it, and it has been idle long enough to go.
        terminals.find(&key).unwrap().lock().viewers = 0;
        terminals.sweep();
        assert!(terminals.find(&key).is_none());
        let gone_by = Instant::now() + Duration::from_secs(5);
        while is_running() && Instant::now() < gone_by {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!is_running());
    }
}
