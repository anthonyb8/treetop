//! The Issues screen's state and keys, kept free of terminal I/O: the
//! program it runs in feeds it keys, polls it for what the network brought,
//! and draws it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use linear::{Detail, Issue, Team};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::backend::Backend;
use crate::worker::{Reply, Request, Worker};
use crate::{Filters, line, parse};

/// How old a list may get on screen before it is read again.
const STALE: Duration = Duration::from_secs(60);

/// What a key asks the program around the screen to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Go back to the program's own screen.
    Back,
    Quit,
    /// Put an agent on this issue.
    Pick(Box<Issue>),
    /// Open this URL in the browser.
    Open(String),
}

/// Which half of the screen the keys move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Detail,
}

pub struct Screen {
    backends: Vec<Arc<dyn Backend>>,
    worker: Worker,
    /// The source on screen, an index into `backends`.
    pub(crate) source: usize,
    pub(crate) issues: Vec<Issue>,
    pub(crate) selected: usize,
    pub(crate) filters: Filters,
    /// The filter line, while it is being edited.
    pub(crate) editing: Option<String>,
    pub(crate) focus: Focus,
    /// Rows the detail pane is scrolled down.
    pub(crate) scroll: u16,
    /// Each issue's detail as last read, by id.
    pub(crate) details: HashMap<String, Detail>,
    /// The issue whose detail was last asked for, so it is asked once.
    asked: Option<String>,
    pub(crate) teams: HashMap<usize, Arc<Team>>,
    pub(crate) is_loading: bool,
    pub(crate) error: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) is_help: bool,
    /// The sequence of the list asked for last; an older reply is stale.
    seq: u64,
    loaded_at: Option<Instant>,
}

impl Screen {
    /// The screen over `backends`, showing the one at `source` first, its
    /// list already on its way.
    pub fn new(backends: Vec<Arc<dyn Backend>>, source: usize) -> Self {
        let worker = Worker::spawn(backends.clone());
        let mut screen = Screen {
            source: source.min(backends.len().saturating_sub(1)),
            backends,
            worker,
            issues: Vec::new(),
            selected: 0,
            filters: Filters {
                mine: true,
                ..Filters::default()
            },
            editing: None,
            focus: Focus::List,
            scroll: 0,
            details: HashMap::new(),
            asked: None,
            teams: HashMap::new(),
            is_loading: false,
            error: None,
            message: None,
            is_help: false,
            seq: 0,
            loaded_at: None,
        };
        screen.load();
        screen
    }

    /// The source's name, as the config gives it.
    pub fn source_name(&self) -> &str {
        self.backends
            .get(self.source)
            .map_or("no source", |b| b.name())
    }

    pub(crate) fn source_count(&self) -> usize {
        self.backends.len()
    }

    /// Shows `message` in the bar until the next key.
    pub fn say(&mut self, message: String) {
        self.message = Some(message);
    }

    pub fn current(&self) -> Option<&Issue> {
        self.issues.get(self.selected)
    }

    /// The detail of the issue under the cursor, if it has been read since
    /// the issue last changed.
    pub(crate) fn detail(&self) -> Option<&Detail> {
        let issue = self.current()?;
        self.details
            .get(&issue.id)
            .filter(|d| d.issue.updated_at == issue.updated_at || issue.updated_at.is_empty())
    }

    /// Asks for the list again, and the team when it is not known yet.
    fn load(&mut self) {
        if self.backends.is_empty() {
            self.error =
                Some("no issue source: add an [issues.<name>] table to config.toml".into());
            return;
        }
        self.seq += 1;
        self.is_loading = true;
        self.worker.send(Request::List {
            source: self.source,
            seq: self.seq,
            filters: self.filters.clone(),
        });
        if !self.teams.contains_key(&self.source) {
            self.worker.send(Request::Team {
                source: self.source,
            });
        }
    }

    /// Called as the screen comes on: lists again when the list on screen is
    /// old.
    pub fn open(&mut self) {
        if self.loaded_at.is_some_and(|at| at.elapsed() >= STALE) {
            self.load();
        }
    }

    /// Takes what the network brought, asks for the detail under the cursor
    /// once it rests there, and lists again when the list grows old. Returns
    /// whether anything on screen changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(reply) = self.worker.replies.try_recv() {
            changed = true;
            self.take(reply);
        }
        if let Some(issue) = self.current()
            && self.detail().is_none()
            && self.asked.as_deref() != Some(issue.id.as_str())
        {
            let id = issue.id.clone();
            self.asked = Some(id.clone());
            self.worker.send(Request::Detail {
                source: self.source,
                id,
            });
        }
        if !self.is_loading && self.loaded_at.is_some_and(|at| at.elapsed() >= STALE) {
            self.load();
            changed = true;
        }
        changed
    }

    fn take(&mut self, reply: Reply) {
        match reply {
            Reply::List {
                source,
                seq,
                result,
            } => {
                if source != self.source || seq != self.seq {
                    return;
                }
                self.is_loading = false;
                self.loaded_at = Some(Instant::now());
                match result {
                    Ok(issues) => {
                        let cursor = self.current().map(|i| i.id.clone());
                        self.issues = issues;
                        self.selected = cursor
                            .and_then(|id| self.issues.iter().position(|i| i.id == id))
                            .unwrap_or(0);
                        self.error = None;
                    }
                    Err(err) => self.error = Some(format!("{}: {err}", self.source_name())),
                }
            }
            Reply::Detail { source, id, result } => {
                if source != self.source {
                    return;
                }
                match result {
                    Ok(detail) => {
                        self.details.insert(id, *detail);
                    }
                    Err(err) => self.message = Some(format!("{id}: {err}")),
                }
            }
            Reply::Team { source, result } => match result {
                Ok(Some(team)) => {
                    self.teams.insert(source, team);
                }
                Ok(None) => {}
                Err(err) if source == self.source => {
                    self.message = Some(format!("{}: {err}", self.source_name()));
                }
                Err(_) => {}
            },
        }
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.issues.len().saturating_sub(1);
        let selected = self.selected.saturating_add_signed(delta).min(last);
        if selected != self.selected {
            self.selected = selected;
            self.scroll = 0;
        }
    }

    /// Shows the next source, listing it afresh.
    fn next_source(&mut self) {
        if self.backends.len() < 2 {
            self.message = Some("one issue source; add another [issues.<name>] to switch".into());
            return;
        }
        self.source = (self.source + 1) % self.backends.len();
        self.issues.clear();
        self.details.clear();
        self.asked = None;
        self.selected = 0;
        self.scroll = 0;
        self.load();
    }

    fn reload(&mut self) {
        if let Some(backend) = self.backends.get(self.source) {
            backend.refresh();
        }
        self.teams.remove(&self.source);
        self.details.clear();
        self.asked = None;
        self.load();
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                _ => Action::None,
            };
        }
        if self.is_help {
            self.is_help = false;
            return Action::None;
        }
        if let Some(text) = &mut self.editing {
            match key.code {
                KeyCode::Enter => {
                    self.filters = parse(text);
                    self.editing = None;
                    self.selected = 0;
                    self.load();
                }
                KeyCode::Esc => self.editing = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            }
            return Action::None;
        }
        self.message = None;
        if self.focus == Focus::Detail {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    self.scroll = self.scroll.saturating_add(10)
                }
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
                KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => self.focus = Focus::List,
                KeyCode::Char('q') => return Action::Quit,
                KeyCode::Char('?') => self.is_help = true,
                _ => {}
            }
            return Action::None;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Esc => return Action::Back,
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.is_help = true,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX),
            KeyCode::Right | KeyCode::Char('l') if self.current().is_some() => {
                self.focus = Focus::Detail;
            }
            KeyCode::Char('/') => {
                let mut text = line(&self.filters);
                if !text.is_empty() {
                    text.push(' ');
                }
                self.editing = Some(text);
            }
            KeyCode::Char('m') => {
                self.filters.mine = !self.filters.mine;
                self.selected = 0;
                self.load();
            }
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('w') => self.next_source(),
            KeyCode::Char('o') => {
                if let Some(url) = self.current().and_then(|i| i.url.clone()) {
                    return Action::Open(url);
                }
            }
            KeyCode::Enter => {
                if let Some(issue) = self.current() {
                    return Action::Pick(Box::new(issue.clone()));
                }
            }
            _ => {}
        }
        Action::None
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::thread;

    use super::*;

    /// A source that answers from memory, and remembers what it was asked.
    #[derive(Default)]
    pub(crate) struct Fake {
        pub(crate) name: String,
        pub(crate) issues: Vec<Issue>,
        pub(crate) asked: Mutex<Vec<String>>,
    }

    impl Backend for Fake {
        fn name(&self) -> &str {
            &self.name
        }

        fn list(&self, filters: &Filters) -> Result<Vec<Issue>, String> {
            self.asked
                .lock()
                .unwrap()
                .push(format!("list mine={}", filters.mine));
            Ok(self.issues.clone())
        }

        fn detail(&self, id: &str) -> Result<Detail, String> {
            self.asked.lock().unwrap().push(format!("detail {id}"));
            let issue = self
                .issues
                .iter()
                .find(|i| i.id == id)
                .cloned()
                .unwrap_or_default();
            Ok(Detail {
                issue: Issue {
                    description: Some(format!("{id} text")),
                    ..issue
                },
                comments: Vec::new(),
            })
        }
    }

    pub(crate) fn issue(id: &str) -> Issue {
        Issue {
            id: id.into(),
            title: format!("{id} title"),
            updated_at: "2026-10-17T10:00:00Z".into(),
            url: Some(format!("https://linear.app/x/issue/{id}")),
            ..Issue::default()
        }
    }

    fn settle(screen: &mut Screen) {
        for _ in 0..200 {
            screen.poll();
            if !screen.is_loading && screen.detail().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn screen() -> (Screen, Arc<Fake>) {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1"), issue("END-2")],
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake.clone()], 0);
        settle(&mut screen);
        (screen, fake)
    }

    #[test]
    fn lists_your_issues_and_reads_the_one_under_the_cursor() {
        let (mut screen, fake) = screen();
        assert_eq!(screen.issues.len(), 2);
        assert_eq!(
            screen.detail().unwrap().issue.description.as_deref(),
            Some("END-1 text")
        );
        screen.handle_key(key(KeyCode::Char('j')));
        settle(&mut screen);
        assert_eq!(screen.detail().unwrap().issue.id, "END-2");
        screen.handle_key(key(KeyCode::Char('k')));
        screen.poll();
        let asked = fake.asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            ["list mine=true", "detail END-1", "detail END-2"],
            "a detail is read once and kept"
        );
    }

    #[test]
    fn keys_ask_the_program_to_pick_open_go_back_and_quit() {
        let (mut screen, _) = screen();
        assert!(
            matches!(screen.handle_key(key(KeyCode::Enter)), Action::Pick(i) if i.id == "END-1")
        );
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('o'))),
            Action::Open("https://linear.app/x/issue/END-1".into())
        );
        assert_eq!(screen.handle_key(key(KeyCode::Tab)), Action::Back);
        assert_eq!(screen.handle_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn the_detail_takes_the_keys_until_left() {
        let (mut screen, _) = screen();
        screen.handle_key(key(KeyCode::Right));
        assert_eq!(screen.focus, Focus::Detail);
        screen.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            (screen.scroll, screen.selected),
            (1, 0),
            "it scrolls, the cursor stays"
        );
        screen.handle_key(key(KeyCode::Left));
        assert_eq!(screen.focus, Focus::List);
        screen.handle_key(key(KeyCode::Char('j')));
        assert_eq!((screen.selected, screen.scroll), (1, 0));
    }

    #[test]
    fn the_filter_line_and_m_list_again() {
        let (mut screen, fake) = screen();
        screen.handle_key(key(KeyCode::Char('m')));
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('/')));
        assert_eq!(screen.editing.as_deref(), Some(""));
        for c in "mine bug".chars() {
            screen.handle_key(key(KeyCode::Char(c)));
        }
        screen.handle_key(key(KeyCode::Enter));
        settle(&mut screen);
        assert_eq!(screen.filters.query.as_deref(), Some("bug"));
        let lists: Vec<String> = fake
            .asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.starts_with("list"))
            .cloned()
            .collect();
        assert_eq!(
            lists,
            ["list mine=true", "list mine=false", "list mine=true"]
        );
    }

    #[test]
    fn w_switches_between_sources() {
        let work = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            ..Fake::default()
        });
        let personal = Arc::new(Fake {
            name: "personal".into(),
            issues: vec![issue("ANT-7")],
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![work, personal], 0);
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('w')));
        settle(&mut screen);
        assert_eq!(screen.source_name(), "personal");
        assert_eq!(screen.current().unwrap().id, "ANT-7");
    }
}
