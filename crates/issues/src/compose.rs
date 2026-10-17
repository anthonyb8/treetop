//! A box to write in: a comment, as long as it needs, with line breaks on
//! Alt+Enter, or a title on one line. Enter sends either.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the box is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// A comment on the issue with this id, such as `END-123`.
    Comment(String),
    /// The title of a new issue.
    NewTitle,
    /// A new title for the issue with this id.
    Title(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose {
    pub purpose: Purpose,
    pub text: String,
}

/// What a key did to the box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Continue,
    Cancel,
    Send,
}

impl Compose {
    pub fn new(purpose: Purpose, text: String) -> Self {
        Compose { purpose, text }
    }

    /// A title is one line; a comment takes as many as it needs.
    pub fn is_one_line(&self) -> bool {
        !matches!(self.purpose, Purpose::Comment(_))
    }

    /// The box's title.
    pub fn title(&self) -> String {
        match &self.purpose {
            Purpose::Comment(id) => format!(" Comment on {id} "),
            Purpose::NewTitle => " New issue: its title ".into(),
            Purpose::Title(id) => format!(" Title of {id} "),
        }
    }

    /// A paste is kept whole, line breaks and all, except in a title, which
    /// takes it as one line.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.is_one_line() {
            self.text.push_str(&text.replace('\n', " "));
        } else {
            self.text.push_str(&text);
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('c') if ctrl => return Edit::Cancel,
            // A terminal reports Shift+Enter as Enter unless asked for
            // extended keys, so Alt+Enter is the break that always arrives.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                if !self.is_one_line() {
                    self.text.push('\n');
                }
            }
            KeyCode::Enter if self.text.trim().is_empty() => {}
            KeyCode::Enter => return Edit::Send,
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
            _ => {}
        }
        Edit::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_lines_and_sends_only_something() {
        let mut compose = Compose::new(Purpose::Comment("END-1".into()), String::new());
        assert_eq!(
            compose.handle_key(KeyEvent::from(KeyCode::Enter)),
            Edit::Continue,
            "nothing to send"
        );
        for c in "looks good".chars() {
            compose.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        compose.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        compose.paste("ship it\r\n");
        assert_eq!(compose.text, "looks good\nship it\n");
        compose.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(compose.text, "looks good\nship ");
        assert_eq!(
            compose.handle_key(KeyEvent::from(KeyCode::Enter)),
            Edit::Send
        );
        assert_eq!(
            compose.handle_key(KeyEvent::from(KeyCode::Esc)),
            Edit::Cancel
        );
    }

    #[test]
    fn a_title_stays_on_one_line() {
        let mut title = Compose::new(Purpose::NewTitle, "Fix".into());
        title.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        title.paste(" the\nlogin");
        assert_eq!(title.text, "Fix the login");
    }
}
