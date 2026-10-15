//! The compose box where you write to agents: a word with the coordinator, a
//! task for a new agent in a fresh tree, the tree under the cursor or the main
//! checkout, or a line for agents already working. Kept free of terminal I/O,
//! like `app`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::pool::Tree;

/// Where the text goes: to the coordinator, or to a new agent and where it
/// runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The one agent that oversees the rest, started in the main checkout on
    /// first use; see `coordinator`.
    Coordinator,
    /// A tree the pool hands out when the job runs, so two quick dispatches
    /// never share one.
    NewTree,
    /// A tree from the pool, leased first if nobody holds it.
    Tree(Tree),
    /// The main checkout, for a quick job that needs no tree of its own.
    InPlace(Tree),
}

impl Target {
    pub fn label(&self) -> String {
        match self {
            Target::Coordinator => "coordinator".into(),
            Target::NewTree => "new tree".into(),
            Target::Tree(tree) => format!("tree {}", tree.name),
            Target::InPlace(_) => "in place".into(),
        }
    }
}

/// An agent a line is typed into, through the chat command it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub agent: String,
    pub tree: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// Send the text to `targets[at]`; Tab moves `at`.
    Dispatch { targets: Vec<Target>, at: usize },
    /// Type the text into each of these agents.
    Message { to: Vec<Recipient> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub purpose: Purpose,
    pub text: String,
}

/// What a key did to the box.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    Continue,
    Submit,
    Cancel,
}

impl Prompt {
    pub fn dispatch(targets: Vec<Target>) -> Self {
        Prompt {
            purpose: Purpose::Dispatch { targets, at: 0 },
            text: String::new(),
        }
    }

    pub fn message(to: Vec<Recipient>) -> Self {
        Prompt {
            purpose: Purpose::Message { to },
            text: String::new(),
        }
    }

    pub fn target(&self) -> Option<&Target> {
        match &self.purpose {
            Purpose::Dispatch { targets, at } => targets.get(*at),
            Purpose::Message { .. } => None,
        }
    }

    /// Pasted text is kept whole, line breaks and all: the agent reads it as
    /// one message.
    pub fn paste(&mut self, text: &str) {
        self.text
            .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    pub fn edit(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('c') if ctrl => return Edit::Cancel,
            // A terminal reports Shift+Enter as Enter unless it is asked for
            // extended keys, so Alt+Enter is the break that always arrives.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                self.text.push('\n');
            }
            KeyCode::Enter => return Edit::Submit,
            KeyCode::Char('u') if ctrl => self.text.clear(),
            KeyCode::Char('w') if ctrl => {
                let kept = self.text.trim_end().len();
                let cut = self.text[..kept]
                    .rfind(char::is_whitespace)
                    .map_or(0, |at| at + 1);
                self.text.truncate(cut);
            }
            KeyCode::Char(c) if !ctrl => self.text.push(c),
            KeyCode::Backspace => {
                self.text.pop();
            }
            KeyCode::Tab | KeyCode::BackTab => {
                if let Purpose::Dispatch { targets, at } = &mut self.purpose {
                    let step = if key.code == KeyCode::Tab {
                        1
                    } else {
                        targets.len().saturating_sub(1)
                    };
                    *at = (*at + step) % targets.len().max(1);
                }
            }
            _ => {}
        }
        Edit::Continue
    }
}

/// The session name for a task: its first few words, so `claude agents` and
/// treetop's AGENT column say what it is doing. A leading issue id such as
/// `END-123` stays at the front, as session names put it.
pub fn session_name(task: &str) -> Option<String> {
    const WORDS: usize = 6;
    const LONGEST: usize = 48;
    let words: Vec<String> = task
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .take(WORDS)
        .collect();
    let mut name = String::new();
    for word in words {
        if name.len() + word.len() + 1 > LONGEST {
            break;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(&word);
    }
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_in(prompt: &mut Prompt, text: &str) {
        for c in text.chars() {
            prompt.edit(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn tab_cycles_the_targets_both_ways() {
        let main = crate::pool::main_tree(std::path::Path::new("/repo"));
        let mut prompt = Prompt::dispatch(vec![
            Target::Coordinator,
            Target::NewTree,
            Target::InPlace(main),
        ]);
        assert_eq!(prompt.target(), Some(&Target::Coordinator));
        prompt.edit(KeyEvent::from(KeyCode::Tab));
        assert_eq!(prompt.target(), Some(&Target::NewTree));
        prompt.edit(KeyEvent::from(KeyCode::BackTab));
        prompt.edit(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(prompt.target().unwrap().label(), "in place", "wraps");
    }

    #[test]
    fn edits_like_a_shell_line() {
        let mut prompt = Prompt::message(Vec::new());
        type_in(&mut prompt, "fix the  login");
        prompt.edit(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(prompt.text, "fix the  ");
        prompt.edit(KeyEvent::from(KeyCode::Backspace));
        type_in(&mut prompt, "x");
        assert_eq!(prompt.text, "fix the x");
        prompt.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(prompt.text, "");
        prompt.edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(prompt.text, "\n", "Alt+Enter breaks the line");
        assert_eq!(prompt.edit(KeyEvent::from(KeyCode::Enter)), Edit::Submit);
        assert_eq!(prompt.edit(KeyEvent::from(KeyCode::Esc)), Edit::Cancel);
    }

    #[test]
    fn names_a_session_from_the_task() {
        assert_eq!(
            session_name("END-123: fix the login redirect, then add a test for it").as_deref(),
            Some("END-123 fix the login redirect then")
        );
        assert_eq!(session_name("  ...  "), None);
    }
}
