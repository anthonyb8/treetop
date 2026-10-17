//! The Issues screen's state and keys, kept free of terminal I/O: the
//! program it runs in feeds it keys, polls it for what the network brought,
//! and draws it.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use linear::{Detail, Issue, Team};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::backend::{Backend, READ_ONLY};
use crate::compose::{Compose, Edit};
use crate::picker::{self, Picked, Picker};
use crate::views::{self, Group, View};
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
    /// The views on keys 1 to 9: the five built in, then the config's.
    pub(crate) views: Vec<View>,
    /// The view on screen, None once the filter line has changed it.
    pub(crate) view: Option<usize>,
    pub(crate) group: Group,
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
    /// Sources whose issues cannot be changed from here.
    read_only: HashSet<usize>,
    /// The picker open over the list, if one is.
    pub(crate) picker: Option<Picker>,
    /// The comment being written, if one is.
    pub(crate) compose: Option<Compose>,
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
    /// list already on its way, with `extra` views after the built-in ones.
    pub fn new(backends: Vec<Arc<dyn Backend>>, source: usize, extra: Vec<View>) -> Self {
        let worker = Worker::spawn(backends.clone());
        let mut views = views::builtin();
        views.extend(extra);
        views.truncate(9);
        let mut screen = Screen {
            source: source.min(backends.len().saturating_sub(1)),
            backends,
            worker,
            issues: Vec::new(),
            selected: 0,
            filters: views[0].filters.clone(),
            group: views[0].group,
            view: Some(0),
            views,
            editing: None,
            focus: Focus::List,
            scroll: 0,
            details: HashMap::new(),
            asked: None,
            teams: HashMap::new(),
            read_only: HashSet::new(),
            picker: None,
            compose: None,
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
                        self.arrange();
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
                    if source == self.source {
                        let cursor = self.current().map(|i| i.id.clone());
                        self.arrange();
                        self.selected = cursor
                            .and_then(|id| self.issues.iter().position(|i| i.id == id))
                            .unwrap_or(0);
                    }
                }
                Ok(None) => {
                    self.read_only.insert(source);
                }
                Err(err) if source == self.source => {
                    self.message = Some(format!("{}: {err}", self.source_name()));
                }
                Err(_) => {}
            },
            Reply::Updated {
                source,
                before,
                result,
            } => {
                if source != self.source {
                    return;
                }
                let (issue, said) = match result {
                    Ok(issue) => (*issue, format!("{} updated", before.id)),
                    Err(err) => {
                        let said = format!("{}: {err}; kept as it was", before.id);
                        (*before, said)
                    }
                };
                self.message = Some(said);
                self.replace(issue);
            }
            Reply::Commented {
                source,
                id,
                body,
                result,
            } => match result {
                Ok(comment) => {
                    if let Some(detail) =
                        self.details.get_mut(&id).filter(|_| source == self.source)
                    {
                        detail.comments.push(comment);
                    }
                    self.message = Some(format!("commented on {id}"));
                }
                // The text is never lost: the box opens again with it.
                Err(err) => {
                    self.message = Some(format!("{id}: {err}; the comment is still in the box"));
                    if source == self.source && self.compose.is_none() {
                        self.compose = Some(Compose::new(&id, body));
                    }
                }
            },
        }
    }

    /// Pastes into the comment being written, if one is.
    pub fn paste(&mut self, text: &str) {
        if let Some(compose) = &mut self.compose {
            compose.paste(text);
        }
    }

    /// Opens the comment box on the issue under the cursor.
    fn open_compose(&mut self) {
        let Some(issue) = self.current() else {
            return;
        };
        if self.read_only.contains(&self.source) {
            self.message = Some(READ_ONLY.into());
            return;
        }
        self.compose = Some(Compose::new(&issue.id, String::new()));
    }

    /// Posts the comment in the box on its issue.
    fn post(&mut self, compose: Compose) {
        let Some(issue) = self.issues.iter().find(|i| i.id == compose.issue).cloned() else {
            self.message = Some(format!("{} is no longer listed", compose.issue));
            return;
        };
        self.message = Some(format!("commenting on {}...", issue.id));
        self.worker.send(Request::Comment {
            source: self.source,
            issue: Box::new(issue),
            body: compose.text.trim_end().to_string(),
        });
    }

    /// Puts `issue` in place of the one with its id, regrouped, the cursor
    /// staying on the issue it was on.
    fn replace(&mut self, issue: Issue) {
        let cursor = self.current().map(|i| i.id.clone());
        if let Some(slot) = self.issues.iter_mut().find(|i| i.id == issue.id) {
            *slot = issue;
        }
        self.arrange();
        self.selected = cursor
            .and_then(|id| self.issues.iter().position(|i| i.id == id))
            .unwrap_or(self.selected);
    }

    /// Opens a picker of `kind` on the issue under the cursor, once its team
    /// is known.
    fn open_picker(&mut self, kind: picker::Kind) {
        let Some(issue) = self.current() else {
            return;
        };
        if self.read_only.contains(&self.source) {
            self.message = Some(READ_ONLY.into());
            return;
        }
        let Some(team) = self.teams.get(&self.source) else {
            self.message = Some(format!(
                "reading {}'s states, labels and members; try again in a moment",
                self.source_name()
            ));
            self.worker.send(Request::Team {
                source: self.source,
            });
            return;
        };
        self.picker = Some(Picker::new(kind, issue, team));
    }

    /// Makes `change` on screen at once, and sends it.
    fn change(&mut self, change: linear::Change, after: Issue) {
        let Some(before) = self.current().cloned() else {
            return;
        };
        self.message = Some(format!("updating {}...", before.id));
        self.replace(after);
        self.worker.send(Request::Update {
            source: self.source,
            before: Box::new(before),
            change: Box::new(change),
        });
    }

    /// Orders the list into its groups, keeping each group in Linear's
    /// order.
    fn arrange(&mut self) {
        let team = self.teams.get(&self.source).cloned();
        let group = self.group;
        self.issues
            .sort_by_cached_key(|issue| views::place(issue, group, team.as_deref()));
    }

    /// The heading the issue at `at` starts, when it is the first of its
    /// group.
    pub(crate) fn heading(&self, at: usize) -> Option<String> {
        if self.group == Group::None {
            return None;
        }
        let team = self.teams.get(&self.source).map(Arc::as_ref);
        let place = |i: &Issue| views::place(i, self.group, team);
        let this = place(self.issues.get(at)?);
        let is_first = at == 0 || self.issues.get(at - 1).map(place) != Some(this.clone());
        is_first.then_some(this.1)
    }

    /// Shows view `at`, listing it afresh.
    fn show_view(&mut self, at: usize) {
        let Some(view) = self.views.get(at) else {
            self.message = Some(format!(
                "no view {}; add views under [views] in the config",
                at + 1
            ));
            return;
        };
        self.filters = view.filters.clone();
        self.group = view.group;
        self.view = Some(at);
        self.selected = 0;
        self.scroll = 0;
        self.load();
    }

    /// The name of the view on screen, `custom` once the filter line has
    /// changed it.
    pub(crate) fn view_name(&self) -> &str {
        self.view
            .and_then(|at| self.views.get(at))
            .map_or("custom", |v| v.name.as_str())
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
        if let Some(compose) = &mut self.compose {
            match compose.handle_key(key) {
                Edit::Continue => {}
                Edit::Cancel => self.compose = None,
                Edit::Send => {
                    if let Some(compose) = self.compose.take() {
                        self.post(compose);
                    }
                }
            }
            return Action::None;
        }
        if let Some(picker) = &mut self.picker {
            let Some(issue) = self.issues.get(self.selected) else {
                self.picker = None;
                return Action::None;
            };
            match picker.handle_key(key, issue) {
                Picked::Continue => {}
                Picked::Cancel => self.picker = None,
                Picked::Apply(change, after) => {
                    self.picker = None;
                    self.change(*change, *after);
                }
            }
            return Action::None;
        }
        if let Some(text) = &mut self.editing {
            match key.code {
                KeyCode::Enter => {
                    self.filters = parse(text);
                    self.view = None;
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
            KeyCode::Right if self.current().is_some() => {
                self.focus = Focus::Detail;
            }
            KeyCode::Char('/') => {
                let mut text = line(&self.filters);
                if !text.is_empty() {
                    text.push(' ');
                }
                self.editing = Some(text);
            }
            KeyCode::Char(c @ '1'..='9') => {
                self.show_view(usize::from(c as u8 - b'1'));
            }
            KeyCode::Char('m') => {
                self.view = None;
                self.filters.mine = !self.filters.mine;
                self.selected = 0;
                self.load();
            }
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('s') => self.open_picker(picker::Kind::Status),
            KeyCode::Char('l') => self.open_picker(picker::Kind::Labels),
            KeyCode::Char('a') => self.open_picker(picker::Kind::Assignee),
            KeyCode::Char('c') => self.open_compose(),
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
        pub(crate) team: Option<Arc<Team>>,
        /// Why an update is refused, when it is.
        pub(crate) refuse: Option<String>,
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

        fn team(&self) -> Result<Option<Arc<Team>>, String> {
            Ok(self.team.clone())
        }

        fn comment(&self, _issue: &Issue, body: &str) -> Result<linear::Comment, String> {
            match &self.refuse {
                Some(why) => Err(why.clone()),
                None => Ok(linear::Comment {
                    author: "Anthony".into(),
                    body: body.to_string(),
                    created_at: "2026-10-17T11:00:00Z".into(),
                }),
            }
        }

        fn update(&self, issue: &Issue, change: &linear::Change) -> Result<Issue, String> {
            if let Some(why) = &self.refuse {
                return Err(why.clone());
            }
            let state = self.team.as_ref().and_then(|team| {
                team.states
                    .iter()
                    .find(|s| Some(&s.id) == change.state_id.as_ref())
            });
            Ok(Issue {
                state: state.map_or(issue.state.clone(), |s| s.name.clone()),
                state_id: change.state_id.clone().or(issue.state_id.clone()),
                updated_at: "2026-10-17T11:00:00Z".into(),
                ..issue.clone()
            })
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
        let mut screen = Screen::new(vec![fake.clone()], 0, Vec::new());
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
    fn number_keys_show_views_and_status_groups_follow_the_workflow() {
        let todo = Issue {
            state: "Todo".into(),
            state_id: Some("todo".into()),
            ..issue("END-1")
        };
        let doing = Issue {
            state: "In Progress".into(),
            state_id: Some("doing".into()),
            ..issue("END-2")
        };
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![doing, todo],
            team: Some(Arc::new(Team {
                id: "t".into(),
                key: "END".into(),
                name: "Endo".into(),
                states: ["todo", "doing"]
                    .iter()
                    .map(|id| linear::State {
                        id: (*id).into(),
                        name: (*id).into(),
                        kind: "unstarted".into(),
                        position: 0.0,
                    })
                    .collect(),
                labels: Vec::new(),
                members: Vec::new(),
                projects: Vec::new(),
                cycles: Vec::new(),
                me: None,
            })),
            ..Fake::default()
        });
        let mine = View::parse("bugs", "mine label:Bug");
        let mut screen = Screen::new(vec![fake.clone()], 0, vec![mine]);
        settle(&mut screen);
        assert_eq!(screen.view_name(), "mine");
        screen.handle_key(key(KeyCode::Char('4')));
        settle(&mut screen);
        assert_eq!(screen.view_name(), "by status");
        let order: Vec<&str> = screen.issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(order, ["END-1", "END-2"], "Todo comes before In Progress");
        assert_eq!(screen.heading(0).as_deref(), Some("Todo"));
        assert_eq!(screen.heading(1).as_deref(), Some("In Progress"));
        screen.handle_key(key(KeyCode::Char('6')));
        settle(&mut screen);
        assert_eq!(screen.view_name(), "bugs");
        assert_eq!(screen.heading(0), None, "no grouping");
        screen.handle_key(key(KeyCode::Char('m')));
        assert_eq!(screen.view_name(), "custom");
        screen.handle_key(key(KeyCode::Char('9')));
        assert!(screen.message.as_deref().unwrap().starts_with("no view 9"));
    }

    fn team_of(states: &[&str]) -> Arc<Team> {
        Arc::new(Team {
            id: "t".into(),
            key: "END".into(),
            name: "Endo".into(),
            states: states
                .iter()
                .map(|id| linear::State {
                    id: (*id).into(),
                    name: (*id).into(),
                    kind: "unstarted".into(),
                    position: 0.0,
                })
                .collect(),
            labels: Vec::new(),
            members: Vec::new(),
            projects: Vec::new(),
            cycles: Vec::new(),
            me: None,
        })
    }

    fn picked_status(refuse: Option<&str>) -> Screen {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![Issue {
                state: "todo".into(),
                state_id: Some("todo".into()),
                ..issue("END-1")
            }],
            team: Some(team_of(&["todo", "doing"])),
            refuse: refuse.map(str::to_string),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('s')));
        assert!(screen.picker.is_some());
        screen.handle_key(key(KeyCode::Down));
        screen.handle_key(key(KeyCode::Enter));
        assert!(screen.picker.is_none());
        assert_eq!(screen.issues[0].state, "doing", "shown at once");
        screen
    }

    fn settle_update(screen: &mut Screen) {
        for _ in 0..200 {
            screen.poll();
            if screen
                .message
                .as_deref()
                .is_some_and(|m| !m.starts_with("updating"))
            {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// What `screen` draws on a terminal of 80 by 24, row by row.
    fn drawn(screen: &Screen) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| screen.draw(frame, frame.area(), &crate::Palette::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..24)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn the_status_picker_marks_the_issues_status_and_filters_as_you_type() {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![Issue {
                state: "todo".into(),
                state_id: Some("todo".into()),
                ..issue("END-1")
            }],
            team: Some(team_of(&["todo", "doing", "done"])),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('s')));
        let rows = drawn(&screen);
        let text = rows.join("\n");
        assert!(text.contains(" Status of END-1 "), "{text}");
        assert!(rows.iter().any(|r| r.contains("● todo")), "{text}");
        assert!(rows.iter().any(|r| r.contains("  doing")), "{text}");
        screen.handle_key(key(KeyCode::Char('d')));
        screen.handle_key(key(KeyCode::Char('o')));
        screen.handle_key(key(KeyCode::Char('n')));
        let text = drawn(&screen).join("\n");
        assert!(text.contains("› don_"), "{text}");
        assert!(!text.contains("doing"), "{text}");
    }

    #[test]
    fn a_status_picked_shows_at_once_and_stays_when_the_source_takes_it() {
        let mut screen = picked_status(None);
        settle_update(&mut screen);
        assert_eq!(screen.message.as_deref(), Some("END-1 updated"));
        assert_eq!(screen.issues[0].state, "doing");
        assert_eq!(screen.issues[0].updated_at, "2026-10-17T11:00:00Z");
    }

    #[test]
    fn a_status_the_source_refuses_goes_back_and_says_why() {
        let mut screen = picked_status(Some("Linear: forbidden"));
        settle_update(&mut screen);
        assert_eq!(screen.issues[0].state, "todo");
        assert_eq!(
            screen.message.as_deref(),
            Some("END-1: Linear: forbidden; kept as it was")
        );
    }

    fn commented(refuse: Option<&str>) -> Screen {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            team: Some(team_of(&["todo"])),
            refuse: refuse.map(str::to_string),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('c')));
        assert!(screen.compose.is_some());
        screen.paste("ship it");
        screen.handle_key(key(KeyCode::Enter));
        assert!(screen.compose.is_none(), "sent");
        for _ in 0..200 {
            screen.poll();
            if screen
                .message
                .as_deref()
                .is_some_and(|m| !m.starts_with("commenting"))
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        screen
    }

    #[test]
    fn a_comment_posted_joins_the_issues_comments() {
        let screen = commented(None);
        assert_eq!(screen.message.as_deref(), Some("commented on END-1"));
        let comments = &screen.detail().unwrap().comments;
        assert_eq!(comments.last().unwrap().body, "ship it");
    }

    #[test]
    fn a_comment_refused_comes_back_in_the_box() {
        let screen = commented(Some("Linear: offline"));
        assert_eq!(screen.compose.as_ref().unwrap().text, "ship it");
        assert!(
            screen
                .message
                .as_deref()
                .unwrap()
                .ends_with("the comment is still in the box")
        );
    }

    #[test]
    fn a_source_that_cannot_change_issues_says_so() {
        let (mut screen, _) = screen();
        screen.handle_key(key(KeyCode::Char('s')));
        assert!(screen.picker.is_none());
        assert_eq!(screen.message.as_deref(), Some(READ_ONLY));
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
        let mut screen = Screen::new(vec![work, personal], 0, Vec::new());
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('w')));
        settle(&mut screen);
        assert_eq!(screen.source_name(), "personal");
        assert_eq!(screen.current().unwrap().id, "ANT-7");
    }
}
