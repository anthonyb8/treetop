//! Answering a prompt an agent waits on, from the phone's chat: a
//! permission (`Allow` or `Deny`), or a question, answered in words. It
//! attaches to the agent's session only for the answer, as `treetop send`
//! does to type a message, and only presses a key once the screen shows a
//! prompt has the keys, so a key never lands in the input box instead.
//!
//! A question is answered by putting its prompt away with Esc and sending
//! the answer as a message, rather than by picking its option on the
//! screen, so the answer does not depend on how Claude Code numbers and
//! lays out its options.

use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::chat::{self, Chat, Input};
use crate::plugin::Screen;

/// The size of the terminal an answer is pressed in, unseen: as `treetop
/// send`'s.
const SIZE: (u16, u16) = (40, 120);

/// How long the session may take to show its screen once attached.
const OPENING: Duration = Duration::from_secs(10);

/// How long the prompt may take to go once its key is pressed.
const TAKING: Duration = Duration::from_secs(5);

const TICK: Duration = Duration::from_millis(30);

/// What to do with the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Its first choice, the one already picked: Enter.
    Allow,
    /// Put it away: Esc, which a permission prompt takes as No.
    Deny,
}

/// Presses `press` on the prompt the agent at `command` waits on, its
/// screen read with `hints`; an error says why nothing was pressed or why
/// the prompt did not take it.
pub fn press(command: &[String], hints: &Screen, press: Press) -> Result<(), String> {
    let mut chat = Chat::open(command, SIZE.0, SIZE.1, String::new(), String::new())
        .map_err(|err| format!("could not reach the agent: {err:#}"))?;
    let shows = |chat: &mut Chat| {
        chat.pump();
        chat::input(&chat.screen().contents(), hints)
    };
    let opened = Instant::now();
    loop {
        let input = shows(&mut chat);
        if chat.is_ended() {
            return Err("the agent's session closed".into());
        }
        if input == Input::Dialog && chat.is_ready() {
            break;
        }
        if matches!(input, Input::Empty | Input::Holding(_)) && chat.is_ready() {
            return Err("nothing is waiting on an answer: the agent's input box is open".into());
        }
        if opened.elapsed() >= OPENING {
            return Err("the agent's screen showed no prompt to answer".into());
        }
        thread::sleep(TICK);
    }
    let key = match press {
        Press::Allow => KeyCode::Enter,
        Press::Deny => KeyCode::Esc,
    };
    chat.send_key(KeyEvent::new(key, KeyModifiers::NONE));
    let pressed = Instant::now();
    while pressed.elapsed() < TAKING {
        thread::sleep(TICK);
        if shows(&mut chat) != Input::Dialog || chat.is_ended() {
            return Ok(());
        }
    }
    Err("the prompt is still there after the answer".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// What Claude Code's plugin says its screen looks like.
    fn hints() -> Screen {
        Screen {
            input: "❯".into(),
            rule: "─".into(),
            dialogs: vec!["Do you want to".into(), "❯ 1.".into()],
            away: Vec::new(),
        }
    }

    /// A stand-in session: it shows a permission prompt, or with `ready` an
    /// empty input box, and writes the first key it gets to `out`, then
    /// shows the input box.
    fn session(out: &Path, ready: bool) -> Vec<String> {
        let first = if ready {
            "box"
        } else {
            "printf 'Bash command\\n  npm test\\nDo you want to proceed?\\n❯ 1. Yes\\n  2. No\\n'"
        };
        let script = format!(
            "box() {{ printf '\\033[2J\\033[H────\\n❯ \\n────\\n'; }}; \
             stty raw -echo; {first}; \
             key=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' '); \
             printf '%s' \"$key\" > {out}; box; sleep 30",
            out = out.display()
        );
        vec!["bash".into(), "-c".into(), script]
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("treetop-answer-{name}-{}", std::process::id()))
    }

    #[test]
    fn allow_and_deny_press_enter_and_esc_on_a_prompt() {
        for (answer, key) in [(Press::Allow, "0d"), (Press::Deny, "1b")] {
            let out = scratch(key);
            let _ = std::fs::remove_file(&out);
            press(&session(&out, false), &hints(), answer).unwrap();
            assert_eq!(std::fs::read_to_string(&out).unwrap(), key, "{answer:?}");
            let _ = std::fs::remove_file(&out);
        }
    }

    #[test]
    fn with_no_prompt_on_screen_nothing_is_pressed() {
        let out = scratch("none");
        let _ = std::fs::remove_file(&out);
        let why = press(&session(&out, true), &hints(), Press::Allow).unwrap_err();
        assert!(why.contains("nothing is waiting"), "{why}");
        thread::sleep(Duration::from_millis(300));
        assert!(!out.exists(), "a key was pressed into the input box");
    }
}
