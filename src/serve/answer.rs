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
//!
//! Claude Code writes a question to the transcript only once it has been
//! answered, so while the agent waits the phone reads it off the screen
//! here instead: the question and its options' labels, for its buttons.

use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::{Value, json};

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
    /// Put away whatever prompt there is, before words go in as a message:
    /// Esc on a prompt, and nothing when the input box is already open, as
    /// it is once the agent has finished its turn.
    Clear,
}

/// What the agent's screen shows once it has settled.
enum Shown {
    /// A prompt has the keys.
    Prompt,
    /// The input box is open.
    Box,
}

/// Attaches to the agent at `command` and waits until its screen shows a
/// prompt or its input box.
fn open(command: &[String], hints: &Screen) -> Result<(Chat, Shown), String> {
    let mut chat = Chat::open(command, SIZE.0, SIZE.1, String::new(), String::new())
        .map_err(|err| format!("could not reach the agent: {err:#}"))?;
    let opened = Instant::now();
    loop {
        chat.pump();
        let input = chat::input(&chat.screen().contents(), hints);
        if chat.is_ended() {
            return Err("the agent's session closed".into());
        }
        if input == Input::Dialog && chat.is_ready() {
            return Ok((chat, Shown::Prompt));
        }
        if matches!(input, Input::Empty | Input::Holding(_)) && chat.is_ready() {
            return Ok((chat, Shown::Box));
        }
        if opened.elapsed() >= OPENING {
            return Err("the agent's screen showed no prompt to answer".into());
        }
        thread::sleep(TICK);
    }
}

const NOTHING_WAITING: &str = "nothing is waiting on an answer: the agent's input box is open";

/// Presses `press` on the prompt the agent at `command` waits on, its
/// screen read with `hints`; an error says why nothing was pressed or why
/// the prompt did not take it.
pub fn press(command: &[String], hints: &Screen, press: Press) -> Result<(), String> {
    let (mut chat, shown) = open(command, hints)?;
    if let Shown::Box = shown {
        return match press {
            Press::Clear => Ok(()),
            Press::Allow | Press::Deny => Err(NOTHING_WAITING.into()),
        };
    }
    let key = match press {
        Press::Allow => KeyCode::Enter,
        Press::Deny | Press::Clear => KeyCode::Esc,
    };
    chat.send_key(KeyEvent::new(key, KeyModifiers::NONE));
    let pressed = Instant::now();
    while pressed.elapsed() < TAKING {
        thread::sleep(TICK);
        chat.pump();
        if chat::input(&chat.screen().contents(), hints) != Input::Dialog || chat.is_ended() {
            return Ok(());
        }
    }
    Err("the prompt is still there after the answer".into())
}

/// The question the agent at `command` waits on, read off its screen, in
/// the shape the chat gives a question from the transcript.
pub fn read(command: &[String], hints: &Screen) -> Result<Value, String> {
    let (chat, shown) = open(command, hints)?;
    match shown {
        Shown::Box => Err(NOTHING_WAITING.into()),
        Shown::Prompt => asked(&chat.screen().contents())
            .ok_or_else(|| "the agent's screen shows no question treetop can read".into()),
    }
}

/// Claude Code's own last entries in a question's list: one to type an
/// answer, and one, under a rule, to talk it over. Neither is an option.
const OWN: &str = "Type something.";

/// A question as Claude Code lays it out at the bottom of its screen:
///
/// ```text
/// ←  ☐ Auth  ✔ Submit  →
/// Which auth provider should the login use?
/// ❯ 1. Clerk
///      Keep the current SDK.
///   2. Supabase
///      Swap the session check.
///   3. Type something.
/// ────────
///   4. Chat about this
/// ```
///
/// The tab row is there when the agent asks more than one thing, and then
/// only the open tab's options are on screen, so a prompt of several
/// questions is not read: the phone answers it in words. A layout this
/// does not recognise gives None.
fn asked(text: &str) -> Option<Value> {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let numbered = |line: &str| -> Option<(u32, String)> {
        let line = line.trim_start();
        let line = line.strip_prefix('❯').unwrap_or(line).trim_start();
        let (number, rest) = line.split_once(". ")?;
        Some((number.parse().ok()?, rest.trim().to_string()))
    };
    // The dialog is at the bottom, so its first option is the last "1.".
    let first = (0..lines.len())
        .rev()
        .find(|&at| numbered(lines[at]).is_some_and(|(n, _)| n == 1))?;
    let is_rule = |line: &str| !line.trim().is_empty() && line.trim().chars().all(|c| c == '─');
    let is_tabs = |line: &str| line.trim_start().starts_with('←') && line.trim_end().ends_with('→');

    let mut options: Vec<(String, Vec<String>)> = Vec::new();
    for line in &lines[first..] {
        if is_rule(line) {
            break;
        }
        match numbered(line) {
            Some((n, label)) if n as usize == options.len() + 1 => {
                if label == OWN {
                    break;
                }
                options.push((label, Vec::new()));
            }
            _ if line.trim().is_empty() => {}
            _ => options.last_mut()?.1.push(line.trim().to_string()),
        }
    }
    if options.is_empty() {
        return None;
    }

    let mut question = Vec::new();
    let mut header = String::new();
    for line in lines[..first].iter().rev() {
        if is_tabs(line) {
            let tabs: Vec<&str> = line
                .trim()
                .trim_start_matches('←')
                .trim_end_matches('→')
                .split(['☐', '☒', '✔'])
                .map(str::trim)
                .filter(|tab| !tab.is_empty() && *tab != "Submit")
                .collect();
            match tabs.as_slice() {
                [only] => header = (*only).to_string(),
                _ => return None,
            }
            break;
        }
        if line.trim().is_empty() || is_rule(line) {
            break;
        }
        question.push(line.trim());
    }
    question.reverse();
    if question.is_empty() {
        return None;
    }

    // A question that takes several shows a box before each label.
    let boxes = ["[ ] ", "[✔] ", "[x] ", "[X] "];
    let multi = options
        .iter()
        .all(|(label, _)| boxes.iter().any(|b| label.starts_with(b)));
    let options: Vec<Value> = options
        .into_iter()
        .map(|(label, about)| {
            let label = if multi {
                label[label.find("] ").map_or(0, |at| at + 2)..].to_string()
            } else {
                label
            };
            json!({ "label": label, "description": about.join(" ") })
        })
        .collect();
    Some(json!([{
        "question": question.join(" "),
        "header": header,
        "multi": multi,
        "options": options,
    }]))
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

    /// A stand-in session showing `screen`, as Claude Code draws a question
    /// at the bottom of its conversation, then waiting.
    fn showing(screen: &str) -> Vec<String> {
        let path = scratch(&format!("screen-{}", screen.len()));
        std::fs::write(&path, screen).unwrap();
        let script = format!("stty raw -echo; cat {}; sleep 30", path.display());
        vec!["bash".into(), "-c".into(), script]
    }

    /// The top of the screen: the conversation, with a numbered list of its
    /// own that is not the question's.
    const ABOVE: &str = "● Two ways to go:\r\n  1. Keep the guard\r\n  2. Move it\r\n\r\n";

    /// Claude Code 2.1.317's question, as captured from a session.
    const QUESTION: &str = "────────────────\r\n\
        ←  ☐ Auth  ✔ Submit  →\r\n\
        Which auth provider should the login use?\r\n\
        ❯ 1. Clerk\r\n     Keep the current SDK.\r\n\
          2. Supabase\r\n     Swap the session check.\r\n\
          3. Type something.\r\n\
        ────────────────\r\n\
          4. Chat about this\r\n\
        Enter to select · Tab/Arrow keys to navigate · Esc to cancel\r\n";

    #[test]
    fn a_question_is_read_off_the_screen_with_its_options() {
        let ask = read(&showing(&format!("{ABOVE}{QUESTION}")), &hints()).unwrap();
        assert_eq!(
            ask,
            json!([{
                "question": "Which auth provider should the login use?",
                "header": "Auth",
                "multi": false,
                "options": [
                    { "label": "Clerk", "description": "Keep the current SDK." },
                    { "label": "Supabase", "description": "Swap the session check." },
                ],
            }])
        );
    }

    #[test]
    fn a_prompt_of_several_questions_is_left_to_words() {
        let several = QUESTION.replace("☐ Auth", "☐ Auth  ☐ Methods");
        let why = read(&showing(&format!("{ABOVE}{several}")), &hints()).unwrap_err();
        assert!(why.contains("no question treetop can read"), "{why}");
    }

    #[test]
    fn words_with_no_prompt_open_go_in_without_a_key() {
        let out = scratch("clear");
        let _ = std::fs::remove_file(&out);
        press(&session(&out, true), &hints(), Press::Clear).unwrap();
        thread::sleep(Duration::from_millis(300));
        assert!(!out.exists(), "a key was pressed into the input box");
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
