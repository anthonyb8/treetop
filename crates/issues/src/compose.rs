//! A box to write a comment in: Markdown, as long as it needs, line breaks
//! with Alt+Enter, sent with Enter.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose {
    /// The issue it comments on, such as `END-123`.
    pub issue: String,
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
    pub fn new(issue: &str, text: String) -> Self {
        Compose {
            issue: issue.to_string(),
            text,
        }
    }

    /// A paste is kept whole, line breaks and all.
    pub fn paste(&mut self, text: &str) {
        self.text
            .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
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
                self.text.push('\n');
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
        let mut compose = Compose::new("END-1", String::new());
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
}
