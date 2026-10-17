//! The form an issue is written in, new or edited: its title, its status,
//! priority, labels, assignee and project, and its description, each a field
//! Tab moves between, saved together with Ctrl+S.

use std::cell::Cell;
use std::collections::BTreeSet;

use linear::{Change, Issue, NewIssue};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::picker::Kind;

/// Text being written, one line or many, with a cursor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextBox {
    lines: Vec<String>,
    /// The cursor's line, and its place in it in chars.
    row: usize,
    col: usize,
    is_multiline: bool,
}

/// The rows a line wraps to `width` columns wide, as ranges of its chars.
/// A row breaks after its last space, which it keeps, so every char is on
/// exactly one row; a word longer than a row is cut.
pub(crate) fn wrap(line: &str, width: usize) -> Vec<(usize, usize)> {
    let chars: Vec<char> = line.chars().collect();
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut start = 0;
    while chars.len() - start > width {
        let window = &chars[start..start + width];
        let end = window
            .iter()
            .rposition(|c| *c == ' ')
            .map_or(start + width, |at| start + at + 1);
        rows.push((start, end));
        start = end;
    }
    rows.push((start, chars.len()));
    rows
}

impl TextBox {
    /// A box holding `text`, the cursor at its end.
    pub fn new(text: &str, is_multiline: bool) -> Self {
        let mut lines: Vec<String> = if is_multiline {
            text.split('\n').map(str::to_string).collect()
        } else {
            vec![text.replace('\n', " ")]
        };
        if lines.is_empty() {
            lines.push(String::new());
        }
        let row = lines.len() - 1;
        let col = lines[row].chars().count();
        TextBox {
            lines,
            row,
            col,
            is_multiline,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn is_empty(&self) -> bool {
        self.lines.iter().all(|l| l.trim().is_empty())
    }

    /// The byte where char `col` of line `row` starts.
    fn byte(&self, row: usize, col: usize) -> usize {
        let line = &self.lines[row];
        line.char_indices()
            .nth(col)
            .map_or(line.len(), |(at, _)| at)
    }

    fn len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    pub fn insert(&mut self, c: char) {
        let at = self.byte(self.row, self.col);
        self.lines[self.row].insert(at, c);
        self.col += 1;
    }

    /// Puts `text` in at the cursor, its line breaks kept in a box of many
    /// lines and made spaces in one of one.
    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in text.chars() {
            match c {
                '\n' if self.is_multiline => self.newline(),
                '\n' | '\t' => self.insert(' '),
                c => self.insert(c),
            }
        }
    }

    fn newline(&mut self) {
        let at = self.byte(self.row, self.col);
        let rest = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        if self.col > 0 {
            self.col -= 1;
            let at = self.byte(self.row, self.col);
            self.lines[self.row].remove(at);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.len(self.row);
            self.lines[self.row].push_str(&line);
        }
    }

    fn delete(&mut self) {
        if self.col < self.len(self.row) {
            let at = self.byte(self.row, self.col);
            self.lines[self.row].remove(at);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].push_str(&next);
        }
    }

    /// Deletes back to the start of the word before the cursor.
    fn delete_word(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut to = self.col;
        while to > 0 && chars[to - 1] == ' ' {
            to -= 1;
        }
        while to > 0 && chars[to - 1] != ' ' {
            to -= 1;
        }
        if to == self.col {
            self.backspace();
            return;
        }
        let (from, end) = (self.byte(self.row, to), self.byte(self.row, self.col));
        self.lines[self.row].replace_range(from..end, "");
        self.col = to;
    }

    fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.len(self.row);
        }
    }

    fn right(&mut self) {
        if self.col < self.len(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// The rows the text takes `width` columns wide, as (line, start, end)
    /// in chars.
    pub(crate) fn rows(&self, width: usize) -> Vec<(usize, usize, usize)> {
        self.lines
            .iter()
            .enumerate()
            .flat_map(|(at, line)| {
                wrap(line, width)
                    .into_iter()
                    .map(move |(start, end)| (at, start, end))
            })
            .collect()
    }

    /// Where the cursor shows `width` columns wide: its row among `rows`,
    /// and its column.
    pub(crate) fn cursor(&self, width: usize) -> (usize, usize) {
        let rows = self.rows(width);
        let at = rows
            .iter()
            .rposition(|(line, start, _)| *line == self.row && *start <= self.col)
            .unwrap_or(0);
        (at, self.col - rows[at].1)
    }

    /// Moves the cursor a row up or down, as the text wraps `width` wide;
    /// false when there is no row there.
    fn vertical(&mut self, delta: isize, width: usize) -> bool {
        let rows = self.rows(width);
        let (at, col) = self.cursor(width);
        let Some(to) = at.checked_add_signed(delta).filter(|to| *to < rows.len()) else {
            return false;
        };
        let (line, start, end) = rows[to];
        // A row that wraps ends on the space it broke at; the cursor stays
        // before it, so it stays on that row.
        let is_last = rows.get(to + 1).is_none_or(|r| r.0 != line);
        let last = if is_last {
            end
        } else {
            end.saturating_sub(1).max(start)
        };
        self.row = line;
        self.col = (start + col).min(last);
        true
    }

    /// Takes a key that edits the text; false for one it leaves alone.
    /// `width` is how wide the box wraps, which Up and Down move by.
    pub fn handle_key(&mut self, key: KeyEvent, width: usize) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Backspace if ctrl || alt => self.delete_word(),
            KeyCode::Char('u') if ctrl => {
                let at = self.byte(self.row, self.col);
                self.lines[self.row].replace_range(..at, "");
                self.col = 0;
            }
            KeyCode::Char('a') if ctrl => self.col = 0,
            KeyCode::Char(c) if !ctrl => self.insert(c),
            KeyCode::Enter if self.is_multiline => self.newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.len(self.row),
            KeyCode::Up if self.is_multiline => return self.vertical(-1, width),
            KeyCode::Down if self.is_multiline => return self.vertical(1, width),
            _ => return false,
        }
        true
    }
}

/// A field of the form, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Status,
    Priority,
    Labels,
    Assignee,
    Project,
    Description,
}

pub const FIELDS: [Field; 7] = [
    Field::Title,
    Field::Status,
    Field::Priority,
    Field::Labels,
    Field::Assignee,
    Field::Project,
    Field::Description,
];

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Status => "Status",
            Field::Priority => "Priority",
            Field::Labels => "Labels",
            Field::Assignee => "Assignee",
            Field::Project => "Project",
            Field::Description => "Description",
        }
    }

    /// The picker the field is set from, for those set from one.
    pub fn kind(self) -> Option<Kind> {
        match self {
            Field::Status => Some(Kind::Status),
            Field::Priority => Some(Kind::Priority),
            Field::Labels => Some(Kind::Labels),
            Field::Assignee => Some(Kind::Assignee),
            Field::Project => Some(Kind::Project),
            Field::Title | Field::Description => None,
        }
    }
}

/// What a key asks of the screen the form is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filled {
    Continue,
    /// Close it; nothing is sent.
    Cancel,
    Save,
    /// Open the picker of this kind, its query started with what was typed.
    Pick(Kind, String),
    /// Write the description in the user's editor.
    Editor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    /// The issue it edits, as it was when the form opened, and its
    /// description; None for a new issue.
    pub before: Option<(Issue, String)>,
    pub title: TextBox,
    pub description: TextBox,
    /// The fields the pickers set, held on an issue they read and write.
    pub issue: Issue,
    pub field: Field,
    /// Esc was pressed once on changes, which a second Esc drops.
    pub is_leaving: bool,
    /// How wide the description wraps, as last drawn.
    pub(crate) width: Cell<u16>,
    /// The first row of the description shown, as last drawn.
    pub(crate) top: Cell<usize>,
}

impl Form {
    /// A new issue, starting from the fields `issue` holds.
    pub fn new(issue: Issue) -> Self {
        Form {
            before: None,
            title: TextBox::new("", false),
            description: TextBox::new("", true),
            issue,
            field: Field::Title,
            is_leaving: false,
            width: Cell::new(60),
            top: Cell::new(0),
        }
    }

    /// `issue`, with its description, to edit.
    pub fn edit(issue: &Issue, description: &str) -> Self {
        Form {
            before: Some((issue.clone(), description.to_string())),
            title: TextBox::new(&issue.title, false),
            description: TextBox::new(description, true),
            ..Form::new(issue.clone())
        }
    }

    /// The issue it edits, by id, or None for a new one.
    pub fn id(&self) -> Option<&str> {
        self.before.as_ref().map(|(issue, _)| issue.id.as_str())
    }

    pub fn paste(&mut self, text: &str) {
        match self.field {
            Field::Title => self.title.paste(text),
            Field::Description => self.description.paste(text),
            _ => {}
        }
    }

    /// Whether anything was written or picked since it opened.
    pub fn is_changed(&self) -> bool {
        match &self.before {
            Some(_) => self.change().is_some(),
            None => {
                !self.title.is_empty()
                    || !self.description.is_empty()
                    || self.issue != Form::new(self.issue.clone()).issue
            }
        }
    }

    /// Moves to the field `delta` away, stopping at the first and last.
    fn step(&mut self, delta: isize) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0);
        let to = at.saturating_add_signed(delta).min(FIELDS.len() - 1);
        self.field = FIELDS[to];
    }

    /// Moves to the field `delta` away, round from the last to the first.
    fn cycle(&mut self, delta: isize) {
        let at = FIELDS.iter().position(|f| *f == self.field).unwrap_or(0);
        let count = FIELDS.len() as isize;
        let to = (at as isize + delta).rem_euclid(count);
        self.field = FIELDS[usize::try_from(to).unwrap_or(0)];
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Filled {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let is_leave = key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c'));
        if !is_leave {
            self.is_leaving = false;
        }
        match key.code {
            _ if is_leave => {
                if self.is_changed() && !self.is_leaving {
                    self.is_leaving = true;
                    return Filled::Continue;
                }
                return Filled::Cancel;
            }
            KeyCode::Char('s') if ctrl => return Filled::Save,
            KeyCode::Char('e') if ctrl && self.field == Field::Description => {
                return Filled::Editor;
            }
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            _ => {}
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            return Filled::Continue;
        }
        let width = usize::from(self.width.get());
        match self.field {
            Field::Title => match key.code {
                KeyCode::Enter | KeyCode::Down => self.step(1),
                _ => {
                    self.title.handle_key(key, width);
                }
            },
            Field::Description => {
                if !self.description.handle_key(key, width) && key.code == KeyCode::Up {
                    self.step(-1);
                }
            }
            field => {
                let kind = field.kind().unwrap_or(Kind::Status);
                match key.code {
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        return Filled::Pick(kind, String::new());
                    }
                    KeyCode::Char(c) if !ctrl => return Filled::Pick(kind, c.to_string()),
                    KeyCode::Down => self.step(1),
                    KeyCode::Up => self.step(-1),
                    _ => {}
                }
            }
        }
        Filled::Continue
    }

    /// What saving it changes on the issue it edits, and the issue as it
    /// will look; None when nothing changed. An emptied title keeps the
    /// old one.
    pub fn change(&self) -> Option<(Change, Issue)> {
        let (before, old) = self.before.as_ref()?;
        let title = self.title.text().trim().to_string();
        let title = if title.is_empty() {
            before.title.clone()
        } else {
            title
        };
        let description = self.description.text();
        let labels = |issue: &Issue| issue.label_ids.iter().cloned().collect::<BTreeSet<_>>();
        let after = &self.issue;
        let change = Change {
            title: Some(title.clone()).filter(|t| *t != before.title),
            description: Some(description.trim_end().to_string())
                .filter(|d| d.trim() != old.trim()),
            state_id: after
                .state_id
                .clone()
                .filter(|_| after.state_id != before.state_id),
            assignee_id: (after.assignee_id != before.assignee_id)
                .then(|| after.assignee_id.clone()),
            label_ids: (labels(after) != labels(before)).then(|| after.label_ids.clone()),
            project_id: (after.project_id != before.project_id).then(|| after.project_id.clone()),
            priority: (after.priority != before.priority).then_some(after.priority),
        };
        if change.is_empty() {
            return None;
        }
        Some((
            change,
            Issue {
                title,
                ..after.clone()
            },
        ))
    }

    /// The new issue it describes, in team `team_id`.
    pub fn new_issue(&self, team_id: &str) -> NewIssue {
        let description = self.description.text();
        NewIssue {
            team_id: team_id.to_string(),
            title: self.title.text().trim().to_string(),
            description: Some(description.trim_end().to_string()).filter(|d| !d.trim().is_empty()),
            state_id: self.issue.state_id.clone(),
            assignee_id: self.issue.assignee_id.clone(),
            label_ids: self.issue.label_ids.clone(),
            project_id: self.issue.project_id.clone(),
            priority: Some(self.issue.priority).filter(|p| *p > 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn typed(text: &mut TextBox, keys: &str) {
        for c in keys.chars() {
            text.handle_key(key(KeyCode::Char(c)), 20);
        }
    }

    #[test]
    fn a_line_wraps_after_its_last_space_and_cuts_a_long_word() {
        assert_eq!(wrap("", 10), [(0, 0)]);
        assert_eq!(wrap("keep the return_to", 10), [(0, 9), (9, 18)]);
        assert_eq!(wrap("abcdefghijkl", 5), [(0, 5), (5, 10), (10, 12)]);
    }

    #[test]
    fn the_text_box_edits_at_its_cursor() {
        let mut text = TextBox::new("fix login", true);
        text.handle_key(key(KeyCode::Home), 20);
        typed(&mut text, "please ");
        text.handle_key(key(KeyCode::End), 20);
        text.handle_key(key(KeyCode::Enter), 20);
        typed(&mut text, "after SSO");
        assert_eq!(text.text(), "please fix login\nafter SSO");
        text.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL), 20);
        assert_eq!(text.text(), "please fix login\nafter ");
        for _ in 0..7 {
            text.handle_key(key(KeyCode::Backspace), 20);
        }
        assert_eq!(text.text(), "please fix login", "back over the line break");
        text.paste("\r\n- one\r\n- two");
        assert_eq!(text.lines(), ["please fix login", "- one", "- two"]);
        let mut title = TextBox::new("", false);
        title.paste("a\nb");
        assert_eq!(title.text(), "a b", "one line stays one line");
        assert!(!title.handle_key(key(KeyCode::Enter), 20));
    }

    #[test]
    fn up_and_down_move_by_the_rows_the_text_wraps_to() {
        // "aaaa bbbb cccc" wraps 10 wide to "aaaa bbbb " and "cccc".
        let mut text = TextBox::new("aaaa bbbb cccc\nend", true);
        assert_eq!(text.cursor(10), (2, 3));
        assert!(text.handle_key(key(KeyCode::Up), 10));
        assert_eq!(text.cursor(10), (1, 3), "the wrapped row above");
        assert!(text.handle_key(key(KeyCode::Up), 10));
        assert_eq!((text.row, text.col), (0, 3));
        assert!(!text.handle_key(key(KeyCode::Up), 10), "nothing above");
        text.handle_key(key(KeyCode::End), 10);
        assert_eq!(text.cursor(10), (1, 4), "the line's end is on its last row");
        assert!(text.handle_key(key(KeyCode::Down), 10));
        assert_eq!(text.cursor(10), (2, 3), "kept within a shorter row");
    }

    fn issue() -> Issue {
        Issue {
            id: "END-1".into(),
            title: "Fix login".into(),
            state: "Todo".into(),
            state_id: Some("s1".into()),
            label_ids: vec!["l1".into(), "l2".into()],
            ..Issue::default()
        }
    }

    #[test]
    fn tab_walks_the_fields_and_a_picker_field_opens_its_picker() {
        let mut form = Form::new(Issue::default());
        assert_eq!(form.field, Field::Title);
        for c in "Fix login".chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        form.handle_key(key(KeyCode::Tab));
        assert_eq!(form.field, Field::Status);
        assert_eq!(
            form.handle_key(key(KeyCode::Enter)),
            Filled::Pick(Kind::Status, String::new())
        );
        assert_eq!(
            form.handle_key(key(KeyCode::Char('d'))),
            Filled::Pick(Kind::Status, "d".into()),
            "typing starts the picker's query"
        );
        form.handle_key(key(KeyCode::BackTab));
        form.handle_key(key(KeyCode::Enter));
        assert_eq!(form.field, Field::Status, "Enter leaves the title");
        form.handle_key(key(KeyCode::BackTab));
        form.handle_key(key(KeyCode::BackTab));
        assert_eq!(form.field, Field::Description, "Tab goes round");
        form.handle_key(key(KeyCode::Char('x')));
        form.handle_key(key(KeyCode::Enter));
        form.handle_key(key(KeyCode::Char('y')));
        assert_eq!(form.description.text(), "x\ny", "Enter is a new line here");
        assert_eq!(
            form.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Filled::Save
        );
        let new = form.new_issue("t");
        assert_eq!(
            (new.title.as_str(), new.description.as_deref()),
            ("Fix login", Some("x\ny"))
        );
    }

    #[test]
    fn esc_on_changes_asks_twice() {
        let mut form = Form::edit(&issue(), "text");
        assert_eq!(
            form.handle_key(key(KeyCode::Esc)),
            Filled::Cancel,
            "nothing to lose"
        );
        form.handle_key(key(KeyCode::Char('!')));
        assert_eq!(form.handle_key(key(KeyCode::Esc)), Filled::Continue);
        assert!(form.is_leaving);
        assert_eq!(form.handle_key(key(KeyCode::Esc)), Filled::Cancel);
    }

    #[test]
    fn an_edit_sends_only_what_changed() {
        let mut form = Form::edit(&issue(), "text\n");
        assert_eq!(form.change(), None);
        form.issue.label_ids = vec!["l2".into(), "l1".into()];
        assert_eq!(form.change(), None, "the same labels in another order");
        form.title = TextBox::new("  ", false);
        assert_eq!(form.change(), None, "an emptied title keeps the old one");
        form.title = TextBox::new("Fix the redirect", false);
        form.issue.priority = 2;
        let (change, after) = form.change().unwrap();
        assert_eq!(
            change,
            Change {
                title: Some("Fix the redirect".into()),
                priority: Some(2),
                ..Change::default()
            }
        );
        assert_eq!(after.title, "Fix the redirect");
    }
}
