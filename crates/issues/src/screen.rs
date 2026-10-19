//! The Issues screen's state and keys, kept free of terminal I/O: the
//! program it runs in feeds it keys, polls it for what the network brought,
//! and draws it.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use linear::{Detail, Issue, Team};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui_image::picker::Picker as ImagePicker;
use ratatui_image::picker::cap_parser::QueryStdioOptions;

use crate::Filters;
use crate::backend::{Backend, READ_ONLY};
use crate::compose::{Compose, Edit};
use crate::form::{Filled, Form, TextBox};
use crate::panel::{self, ROWS};
use crate::picker::{self, Picked, Picker};
use crate::picture::Picture;
use crate::targets::{self, Target};
use crate::views::{self, Group, View};
use crate::worker::{Reply, Request, Worker};

/// How long typing in the search rests before Linear is asked too.
const SEARCH_REST: Duration = Duration::from_millis(350);

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
    /// This is the issue asked for by `choose_issue`.
    Attach(Box<Issue>),
    /// Open this URL in the browser.
    Open(String),
    /// Ask the program's assistant something, about what this says is on
    /// screen.
    Ask(String),
}

/// What fills the screen: the list, with the issue under the cursor
/// below it, or one issue, whole, to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Issue,
}

pub struct Screen {
    backends: Vec<Arc<dyn Backend>>,
    worker: Worker,
    /// The source on screen, an index into `backends`.
    pub(crate) source: usize,
    /// The issues as the source listed them.
    all: Vec<Issue>,
    /// The issues on screen: the listed ones the search keeps, grouped.
    pub(crate) issues: Vec<Issue>,
    pub(crate) selected: usize,
    pub(crate) filters: Filters,
    /// The views on keys 1 to 9: the five built in, then the config's.
    pub(crate) views: Vec<View>,
    /// The view on screen, None once the filter line has changed it.
    pub(crate) view: Option<usize>,
    pub(crate) group: Group,
    /// The groups folded to their heading, by heading, as Linear folds them.
    pub(crate) folded: BTreeSet<String>,
    /// Closed status groups opened by hand, which stay open; the rest fold
    /// as they appear.
    pub(crate) opened: BTreeSet<String>,
    /// The search, while it is being typed, and when it was last typed in.
    pub(crate) search: Option<String>,
    search_at: Option<Instant>,
    /// The filter panel's row the cursor is on, while it is open.
    pub(crate) panel: Option<usize>,
    pub(crate) focus: Focus,
    /// Rows the open issue is scrolled down.
    pub(crate) scroll: u16,
    /// The rows the open issue's text had on screen, as it was last drawn.
    pub(crate) view_height: Cell<u16>,
    /// The furthest the open issue scrolls, as it was last drawn.
    pub(crate) max_scroll: Cell<u16>,
    /// The preview below the list scrolled down this many rows, for the
    /// issue of this id; any other issue's preview starts at its top.
    pub(crate) preview: (String, u16),
    /// The furthest the preview scrolls, and how tall it is, as last drawn.
    pub(crate) preview_max: Cell<u16>,
    pub(crate) preview_height: Cell<u16>,
    /// Where the preview was last drawn, so the wheel over it scrolls it.
    pub(crate) preview_area: Cell<Rect>,
    /// The issues opened from the open one's links, by id, the one on
    /// screen last; Esc goes back along them.
    pub(crate) trail: Vec<String>,
    /// The link or image of the open issue Tab is on, an index into
    /// `targets`.
    pub(crate) target: Option<usize>,
    /// The image filling the screen, an index into `targets`.
    pub(crate) viewer: Option<usize>,
    /// Where each target was drawn last, as (target, first row, rows) in
    /// the open issue's text, so Tab scrolls to it.
    pub(crate) placed: RefCell<Vec<(usize, u16, u16)>>,
    /// Where on screen each target was drawn last, for a click.
    pub(crate) hits: RefCell<Vec<(Rect, usize)>>,
    /// Each image read, by URL.
    pub(crate) pictures: HashMap<String, Picture>,
    /// How images draw in this terminal.
    pub(crate) images: ImagePicker,
    /// Whether the terminal has been asked how it draws images; images are
    /// fetched only once it has, so each is made for the terminal.
    pub(crate) has_asked_terminal: bool,
    /// Each issue's detail as last read, by id.
    pub(crate) details: HashMap<String, Detail>,
    /// The issue whose detail was last asked for, by id and the time it last
    /// changed, so each version of it is asked for once.
    asked: Option<(String, String)>,
    pub(crate) teams: HashMap<usize, Arc<Team>>,
    /// Sources whose issues cannot be changed from here.
    read_only: HashSet<usize>,
    /// The picker open over the list, if one is.
    pub(crate) picker: Option<Picker>,
    /// The comment being written, if one is.
    pub(crate) compose: Option<Compose>,
    /// What an issue is being chosen for, said in the bar, while one is:
    /// Enter on the list chooses it.
    pub(crate) choosing: Option<String>,
    /// The repository has no issue source of its own, and none has been
    /// asked to show yet.
    pub(crate) is_without_source: bool,
    /// The open issue was opened from the program's own screen, which Esc
    /// goes back to.
    pub(crate) is_from_outside: bool,
    /// The issue being written, new or edited, if one is.
    pub(crate) form: Option<Form>,
    /// A form Linear refused while another was open, which `n` opens again.
    retry: Option<Form>,
    /// Text waiting for the program to open in the user's editor, taken
    /// once; what comes back is the form's description.
    editor_text: Option<String>,
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
        let mut screen = Screen::idle(backends, source, extra);
        screen.load();
        screen
    }

    /// The screen for a repository with no issue source of its own: it
    /// lists nothing, says `why`, and `w` shows the sources there are.
    pub fn without_source(backends: Vec<Arc<dyn Backend>>, why: String, extra: Vec<View>) -> Self {
        let mut screen = Screen::idle(backends, 0, extra);
        screen.is_without_source = true;
        let more = if screen.backends.is_empty() {
            String::new()
        } else {
            format!("; w shows {} anyway", screen.source_name())
        };
        screen.error = Some(format!("{why}{more}"));
        screen
    }

    /// The screen, nothing asked for yet.
    fn idle(backends: Vec<Arc<dyn Backend>>, source: usize, extra: Vec<View>) -> Self {
        let worker = Worker::spawn(backends.clone());
        let mut views = views::builtin();
        views.extend(extra);
        views.truncate(9);
        Screen {
            source: source.min(backends.len().saturating_sub(1)),
            backends,
            worker,
            all: Vec::new(),
            issues: Vec::new(),
            selected: 0,
            filters: views[0].filters.clone(),
            group: views[0].group,
            view: Some(0),
            views,
            search: None,
            search_at: None,
            panel: None,
            focus: Focus::List,
            scroll: 0,
            max_scroll: Cell::new(0),
            preview: (String::new(), 0),
            preview_max: Cell::new(0),
            preview_height: Cell::new(0),
            preview_area: Cell::new(Rect::default()),
            view_height: Cell::new(0),
            trail: Vec::new(),
            target: None,
            viewer: None,
            placed: RefCell::new(Vec::new()),
            hits: RefCell::new(Vec::new()),
            pictures: HashMap::new(),
            images: ImagePicker::halfblocks(),
            has_asked_terminal: false,
            details: HashMap::new(),
            asked: None,
            teams: HashMap::new(),
            read_only: HashSet::new(),
            picker: None,
            compose: None,
            form: None,
            is_from_outside: false,
            is_without_source: false,
            choosing: None,
            folded: BTreeSet::new(),
            opened: BTreeSet::new(),
            retry: None,
            editor_text: None,
            is_loading: false,
            error: None,
            message: None,
            is_help: false,
            seq: 0,
            loaded_at: None,
        }
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

    /// Whether the screen needs to know how the terminal draws images: an
    /// open issue has some, and it has not asked yet.
    pub fn wants_terminal_asked(&self) -> bool {
        !self.has_asked_terminal
            && self.focus == Focus::Issue
            && self
                .targets()
                .iter()
                .any(|t| matches!(t, Target::Image { .. }))
    }

    /// Asks the terminal how it draws images: in its own graphics where it
    /// has them, through tmux too, else in characters, which any terminal
    /// shows. Call it on the terminal's raw screen, between reading keys; a
    /// terminal that does not answer holds the screen up to half a second,
    /// once.
    pub fn ask_terminal_for_images(&mut self) {
        let options = QueryStdioOptions {
            timeout: Duration::from_millis(500),
            ..QueryStdioOptions::default()
        };
        self.images = ImagePicker::from_query_stdio_with_options(options)
            .unwrap_or_else(|_| ImagePicker::halfblocks());
        self.has_asked_terminal = true;
        self.pictures.clear();
    }

    /// Whether the screen wants the mouse: while an issue is open, to click
    /// its links and scroll it with the wheel.
    pub fn wants_mouse(&self) -> bool {
        self.form.is_none()
            && self.picker.is_none()
            && self.compose.is_none()
            && (self.focus == Focus::Issue
                || (self.panel.is_none() && self.search.is_none() && !self.is_help))
    }

    /// The id of the issue on screen: the last one opened from a link, else
    /// the one under the cursor.
    pub(crate) fn viewed_id(&self) -> Option<&str> {
        match self.trail.last() {
            Some(id) => Some(id),
            None => self.issues.get(self.selected).map(|i| i.id.as_str()),
        }
    }

    /// The issue on screen: under the cursor, or opened from a link, which
    /// may not be on the list.
    pub fn current(&self) -> Option<&Issue> {
        let Some(id) = self.trail.last() else {
            // A folded group shows only its heading: no issue is under the
            // cursor, so nothing is changed or picked up unseen.
            if self.is_folded(self.selected) {
                return None;
            }
            return self.issues.get(self.selected);
        };
        self.issues
            .iter()
            .find(|i| i.id == *id)
            .or_else(|| self.details.get(id).map(|d| &d.issue))
    }

    /// The detail of the issue on screen, if it has been read since the
    /// issue last changed.
    pub(crate) fn detail(&self) -> Option<&Detail> {
        let detail = self.details.get(self.viewed_id()?)?;
        // Times are RFC 3339 in one format, so they compare as text.
        let is_fresh = self
            .current()
            .is_none_or(|issue| detail.issue.updated_at >= issue.updated_at);
        is_fresh.then_some(detail)
    }

    /// What the open issue leads to: its links, mentions and images.
    pub(crate) fn targets(&self) -> Vec<Target> {
        let mut targets = self.detail().map(targets::targets).unwrap_or_default();
        // A mention names an id alone; the list, or a read of it, knows
        // its title and status.
        for target in &mut targets {
            if let Target::Issue(link) = target
                && link.title.is_empty()
                && let Some(known) = self
                    .issues
                    .iter()
                    .find(|i| i.id == link.id)
                    .or_else(|| self.details.get(&link.id).map(|d| &d.issue))
            {
                link.title = known.title.clone();
                link.state = known.state.clone();
            }
        }
        targets
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
        // An issue opened from a link is known by its id alone until read.
        let viewed = self.viewed_id().map(|id| {
            let updated = self.current().map(|i| i.updated_at.clone());
            (id.to_string(), updated.unwrap_or_default())
        });
        if let Some(viewed) = viewed
            && self.detail().is_none()
            && self.asked.as_ref() != Some(&viewed)
        {
            self.asked = Some(viewed.clone());
            self.worker.send(Request::Detail {
                source: self.source,
                id: viewed.0,
            });
        }
        if self.focus == Focus::Issue && self.has_asked_terminal {
            for target in self.targets() {
                if let Target::Image { url, .. } = target
                    && !self.pictures.contains_key(&url)
                {
                    self.pictures.insert(url.clone(), Picture::Loading);
                    self.worker.send(Request::Image {
                        source: self.source,
                        url,
                    });
                    changed = true;
                }
            }
        }
        // Once typing rests, the search goes to the source too, for issues
        // beyond the ones listed.
        if self.search_at.is_some_and(|at| at.elapsed() >= SEARCH_REST) {
            self.search_at = None;
            self.send_search();
            changed = true;
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
                        self.all = issues;
                        self.narrow();
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
                form,
                result,
            } => {
                if source != self.source {
                    return;
                }
                let (issue, said) = match result {
                    Ok(issue) => (*issue, format!("{} updated", before.id)),
                    Err(err) => {
                        let said = match form {
                            Some(form) => self.reopen(*form, &err),
                            None => format!("{}: {err}; kept as it was", before.id),
                        };
                        (*before, said)
                    }
                };
                self.message = Some(said);
                self.replace(issue);
            }
            Reply::Created {
                source,
                form,
                result,
            } => match result {
                Ok(issue) if source == self.source => {
                    self.message = Some(format!("created {}", issue.id));
                    let id = issue.id.clone();
                    self.all.insert(0, *issue);
                    self.narrow();
                    self.selected = self.issues.iter().position(|i| i.id == id).unwrap_or(0);
                    self.scroll = 0;
                }
                Ok(issue) => self.message = Some(format!("created {}", issue.id)),
                Err(err) => self.message = Some(self.reopen(*form, &err)),
            },
            Reply::Image { url, result } => {
                let picture = match result {
                    Ok(image) => Picture::new(*image, &self.images),
                    Err(err) => Picture::Failed(err),
                };
                self.pictures.insert(url, picture);
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

    /// Pastes into the comment or the issue being written, if one is.
    pub fn paste(&mut self, text: &str) {
        if let Some(compose) = &mut self.compose {
            compose.paste(text);
        } else if let Some(form) = &mut self.form {
            form.paste(text);
        }
    }

    /// Opens `form` again, which Linear refused for `err`, so nothing
    /// written is lost; returns what to say.
    fn reopen(&mut self, form: Form, err: &str) -> String {
        let what = form
            .id()
            .map_or("the new issue".to_string(), str::to_string);
        if self.form.is_none() {
            self.form = Some(form);
            format!("{what}: {err}; it is open again to retry")
        } else {
            self.retry = Some(form);
            format!("{what}: {err}; n opens it again")
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

    /// Posts `body` as a comment on the issue with id `id`.
    fn post(&mut self, id: &str, body: &str) {
        let known = self.issues.iter().find(|i| i.id == id).cloned();
        let Some(issue) = known.or_else(|| self.details.get(id).map(|d| d.issue.clone())) else {
            self.message = Some(format!("{id} is no longer listed"));
            return;
        };
        self.message = Some(format!("commenting on {}...", issue.id));
        self.worker.send(Request::Comment {
            source: self.source,
            issue: Box::new(issue),
            body: body.trim_end().to_string(),
        });
    }

    /// The team of the source on screen, if its issues can be changed and it
    /// has been read; else says why not, and asks for it.
    fn team_to_change(&mut self) -> Option<Arc<Team>> {
        if self.read_only.contains(&self.source) {
            self.message = Some(READ_ONLY.into());
            return None;
        }
        let team = self.teams.get(&self.source).cloned();
        if team.is_none() {
            self.message = Some(format!(
                "reading {}'s states, labels and members; try again in a moment",
                self.source_name()
            ));
            self.worker.send(Request::Team {
                source: self.source,
            });
        }
        team
    }

    /// Opens the form on a new issue, or on the one Linear last refused.
    fn open_new(&mut self) {
        if self.team_to_change().is_none() {
            return;
        }
        let form = self
            .retry
            .take()
            .unwrap_or_else(|| Form::new(self.new_issue()));
        self.form = Some(form);
    }

    /// Opens the form on the issue under the cursor, once its description
    /// has been read.
    fn open_edit(&mut self) {
        let Some(issue) = self.current() else {
            return;
        };
        let id = issue.id.clone();
        if self.team_to_change().is_none() {
            return;
        }
        let Some(detail) = self.detail() else {
            self.message = Some(format!("reading {id}; try e again in a moment"));
            return;
        };
        let description = detail.issue.description.clone().unwrap_or_default();
        let issue = self.current().cloned().unwrap_or_default();
        self.form = Some(Form::edit(&issue, &description));
    }

    /// Text for the user's editor, taken once: the program opens it, then
    /// hands back what was written to `edited`.
    pub fn take_editor(&mut self) -> Option<String> {
        self.editor_text.take()
    }

    /// What the user's editor gave back for the form's description, or why
    /// it gave nothing.
    pub fn edited(&mut self, result: Result<String, String>) {
        let Some(form) = &mut self.form else {
            return;
        };
        match result {
            Ok(text) => form.description = TextBox::new(text.trim_end(), true),
            Err(err) => self.message = Some(format!("the editor did not run: {err}")),
        }
    }

    /// Sends what the form holds: a new issue, or the changes to the one
    /// it edits.
    fn save(&mut self, mut form: Form) {
        let Some(id) = form.id().map(str::to_string) else {
            if form.title.is_empty() {
                self.message = Some("a new issue needs a title".into());
                form.field = crate::form::Field::Title;
                self.form = Some(form);
                return;
            }
            self.create(form);
            return;
        };
        let Some((change, after)) = form.change() else {
            self.message = Some(format!("{id} unchanged"));
            return;
        };
        let Some(at) = self.issues.iter().position(|i| i.id == id) else {
            self.message = Some(format!("{id} is no longer listed"));
            return;
        };
        self.selected = at;
        self.change(change, after, Some(form));
    }

    /// Sends the new issue `form` describes.
    fn create(&mut self, form: Form) {
        let Some(team) = self.teams.get(&self.source) else {
            return;
        };
        let new = form.new_issue(&team.id);
        self.message = Some(format!("creating {}...", new.title));
        self.worker.send(Request::Create {
            source: self.source,
            new: Box::new(new),
            form: Box::new(form),
        });
    }

    /// Puts `issue` in place of the one with its id, regrouped, the cursor
    /// staying on the issue it was on.
    fn replace(&mut self, issue: Issue) {
        let cursor = self.issues.get(self.selected).map(|i| i.id.clone());
        if let Some(slot) = self.all.iter_mut().find(|i| i.id == issue.id) {
            *slot = issue.clone();
        }
        if let Some(slot) = self.issues.iter_mut().find(|i| i.id == issue.id) {
            *slot = issue;
        } else if let Some(detail) = self.details.get_mut(&issue.id) {
            // An issue opened from a link lives in its detail alone, which
            // keeps its text: a change returns the fields without it.
            let description = detail.issue.description.take();
            detail.issue = Issue {
                description,
                ..issue
            };
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

    /// Posts the comment the box holds, now that Enter sent it.
    fn sent(&mut self, compose: Compose) {
        self.post(&compose.issue, &compose.text);
    }

    /// A new issue's fields as the form starts them: the team's first open
    /// state, and assigned to you.
    fn new_issue(&self) -> Issue {
        let team = self.teams.get(&self.source);
        let state = team.and_then(|t| {
            t.states
                .iter()
                .find(|s| s.kind == "unstarted")
                .or_else(|| t.states.first())
        });
        let me = team.and_then(|t| t.me.clone());
        Issue {
            id: "new issue".into(),
            state: state.map(|s| s.name.clone()).unwrap_or_default(),
            state_id: state.map(|s| s.id.clone()),
            assignee: me.as_ref().map(|m| m.name.clone()),
            assignee_id: me.map(|m| m.id),
            ..Issue::default()
        }
    }

    /// Makes `change` on screen at once, and sends it, with the `form` it
    /// was written in, if one, to open again should it be refused.
    fn change(&mut self, change: linear::Change, after: Issue, form: Option<Form>) {
        let Some(before) = self.current().cloned() else {
            return;
        };
        self.message = Some(format!("updating {}...", before.id));
        self.replace(after);
        self.worker.send(Request::Update {
            source: self.source,
            before: Box::new(before),
            change: Box::new(change),
            form: form.map(Box::new),
        });
    }

    /// The search on screen: as it is being typed, else as it was kept.
    pub(crate) fn search_text(&self) -> Option<&str> {
        self.search
            .as_deref()
            .or(self.filters.query.as_deref())
            .filter(|s| !s.trim().is_empty())
    }

    /// Shows the listed issues the search keeps, grouped, the cursor
    /// staying on the issue it was on.
    fn narrow(&mut self) {
        let cursor = self.issues.get(self.selected).map(|i| i.id.clone());
        let words: Vec<String> = self
            .search_text()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.issues = self
            .all
            .iter()
            .filter(|issue| {
                let text = format!(
                    "{} {} {} {}",
                    issue.id,
                    issue.title,
                    issue.labels.join(" "),
                    issue.assignee.as_deref().unwrap_or_default()
                )
                .to_lowercase();
                words.iter().all(|word| text.contains(word))
            })
            .cloned()
            .collect();
        self.arrange();
        self.selected = cursor
            .and_then(|id| self.issues.iter().position(|i| i.id == id))
            .unwrap_or(0);
    }

    /// Asks the source for the search as typed, when it is not what the
    /// list was read with.
    fn send_search(&mut self) {
        let typed = self
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(typed) = self.search.is_some().then_some(typed)
            && typed != self.filters.query
        {
            self.filters.query = typed;
            self.load();
        }
    }

    /// Keys while the search is being typed: it narrows the list as it
    /// goes; Enter keeps it, Esc clears it.
    fn on_search(&mut self, key: KeyEvent) {
        let Some(text) = &mut self.search else {
            return;
        };
        match key.code {
            KeyCode::Enter => {
                self.send_search();
                self.search = None;
                self.search_at = None;
                return;
            }
            KeyCode::Esc => {
                self.search = None;
                self.search_at = None;
                if self.filters.query.take().is_some() {
                    self.load();
                }
                self.narrow();
                return;
            }
            KeyCode::Down => return self.move_by(1),
            KeyCode::Up => return self.move_by(-1),
            KeyCode::Backspace => {
                text.pop();
            }
            KeyCode::Char(c) => text.push(c),
            _ => return,
        }
        self.search_at = Some(Instant::now());
        self.narrow();
    }

    /// Keys in the filter panel: move between its rows, Enter changes one,
    /// `x` clears it and `c` clears them all.
    fn on_panel(&mut self, key: KeyEvent, at: usize) {
        let row = ROWS[at];
        match key.code {
            KeyCode::Esc | KeyCode::Char('f') | KeyCode::Char('q') => self.panel = None,
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.panel = Some((at + 1).min(ROWS.len() - 1));
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.panel = Some(at.saturating_sub(1));
            }
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right | KeyCode::Char('l') => {
                let team = self.teams.get(&self.source).map(Arc::as_ref);
                let choices = panel::choices(row, team);
                let current = panel::current(row, &self.filters, self.group);
                self.picker = Some(Picker::of(row.label(), choices, current));
            }
            KeyCode::Char('x') | KeyCode::Backspace | KeyCode::Delete => self.set_filter(row, None),
            KeyCode::Char('c') => {
                for row in ROWS {
                    panel::set(row, None, &mut self.filters, &mut self.group);
                }
                self.filters.query = None;
                self.view = None;
                self.reload_list();
            }
            _ => {}
        }
    }

    /// Sets filter `row` to the choice with id `id`, and lists again.
    fn set_filter(&mut self, row: panel::Row, id: Option<String>) {
        panel::set(row, id, &mut self.filters, &mut self.group);
        self.view = None;
        self.reload_list();
    }

    /// Lists afresh under the filters on screen, the cursor at the top.
    fn reload_list(&mut self) {
        self.selected = 0;
        self.arrange();
        self.load();
    }

    /// Orders the list into its groups, keeping each group in Linear's
    /// order.
    fn arrange(&mut self) {
        let team = self.teams.get(&self.source).cloned();
        let group = self.group;
        self.issues
            .sort_by_cached_key(|issue| views::place(issue, group, team.as_deref()));
        // Done, cancelled and duplicate issues are rarely wanted, so their
        // groups come folded, as Linear folds them, until opened by hand.
        if let (Group::Status, Some(team)) = (group, team) {
            for state in team.states.iter().filter(|s| views::is_closed(s)) {
                if !self.opened.contains(&state.name) {
                    self.folded.insert(state.name.clone());
                }
            }
        }
        while self.selected > 0 && !self.is_stop(self.selected) {
            self.selected -= 1;
        }
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

    /// The heading of the group issue `at` is in, in a grouped list.
    pub(crate) fn group_of(&self, at: usize) -> Option<String> {
        if self.group == Group::None {
            return None;
        }
        let team = self.teams.get(&self.source).map(Arc::as_ref);
        Some(views::place(self.issues.get(at)?, self.group, team).1)
    }

    /// Issue `at` is in a folded group, which shows only its heading.
    pub(crate) fn is_folded(&self, at: usize) -> bool {
        self.group_of(at)
            .is_some_and(|group| self.folded.contains(&group))
    }

    /// The cursor can rest on issue `at`: it shows, or it is the first of a
    /// folded group and stands for its heading.
    fn is_stop(&self, at: usize) -> bool {
        !self.is_folded(at) || self.heading(at).is_some()
    }

    /// Folds the group under the cursor to its heading, or unfolds it.
    fn toggle_fold(&mut self) {
        let Some(group) = self.group_of(self.selected) else {
            self.message = Some("z folds a group: 4 groups the list by status".into());
            return;
        };
        if self.folded.remove(&group) {
            self.opened.insert(group);
        } else {
            while self.selected > 0 && self.group_of(self.selected - 1).as_ref() == Some(&group) {
                self.selected -= 1;
            }
            self.opened.remove(&group);
            self.folded.insert(group);
        }
        self.scroll = 0;
    }

    /// The rows the preview of the issue under the cursor is scrolled down.
    pub(crate) fn preview_scroll(&self, id: &str) -> u16 {
        if self.preview.0 == id {
            self.preview.1.min(self.preview_max.get())
        } else {
            0
        }
    }

    /// Scrolls the preview below the list `delta` rows, within what it
    /// holds, so an issue is read without opening it.
    fn scroll_preview(&mut self, delta: i32) {
        let Some(id) = self.current().map(|i| i.id.clone()) else {
            return;
        };
        let now = i32::from(self.preview_scroll(&id));
        let max = i32::from(self.preview_max.get());
        let to = u16::try_from((now + delta).clamp(0, max)).unwrap_or(0);
        self.preview = (id, to);
    }

    /// Folds every group, or unfolds them all when every one is folded.
    fn toggle_all_folds(&mut self) {
        let groups: BTreeSet<String> = (0..self.issues.len())
            .filter_map(|at| self.group_of(at))
            .collect();
        if groups.is_empty() {
            self.message = Some("Z folds the groups: 4 groups the list by status".into());
            return;
        }
        if groups.is_subset(&self.folded) {
            self.folded.retain(|g| !groups.contains(g));
        } else {
            self.folded.extend(groups);
            while self.selected > 0 && !self.is_stop(self.selected) {
                self.selected -= 1;
            }
        }
        self.scroll = 0;
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

    /// Moves the cursor `delta` rows, a folded group counting as one row: a
    /// step goes to the next row there is, a jump as far as it can.
    fn move_by(&mut self, delta: isize) {
        let last = self.issues.len().saturating_sub(1);
        let mut selected = self.selected.saturating_add_signed(delta).min(last);
        if delta.unsigned_abs() == 1 {
            while selected != self.selected && !self.is_stop(selected) {
                match selected.checked_add_signed(delta).filter(|n| *n <= last) {
                    Some(next) => selected = next,
                    None => selected = self.selected,
                }
            }
        } else {
            let back = if delta > 0 { -1 } else { 1 };
            while !self.is_stop(selected) && selected != self.selected {
                selected = selected.saturating_add_signed(back);
            }
        }
        if selected != self.selected {
            self.selected = selected;
            self.scroll = 0;
        }
    }

    /// Shows the next source, listing it afresh.
    fn next_source(&mut self) {
        if self.is_without_source && !self.backends.is_empty() {
            self.is_without_source = false;
            self.error = None;
            self.load();
            return;
        }
        if self.backends.len() < 2 {
            self.message = Some("one issue source; add another [issues.<name>] to switch".into());
            return;
        }
        self.source = (self.source + 1) % self.backends.len();
        self.all.clear();
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
                        self.sent(compose);
                    }
                }
            }
            return Action::None;
        }
        if let Some(picker) = &mut self.picker {
            let placeholder = Issue::default();
            let issue = match &self.form {
                _ if self.panel.is_some() => &placeholder,
                Some(form) => &form.issue,
                None => match self.issues.get(self.selected) {
                    Some(issue) => issue,
                    None => {
                        self.picker = None;
                        return Action::None;
                    }
                },
            };
            match picker.handle_key(key, issue) {
                Picked::Continue => {}
                Picked::Cancel => self.picker = None,
                Picked::Apply(_, _) if self.panel.is_some() => {
                    let id = picker.chosen().and_then(|c| c.id.clone());
                    self.picker = None;
                    let row = ROWS[self.panel.unwrap_or(0)];
                    self.set_filter(row, id);
                }
                Picked::Apply(change, after) => {
                    self.picker = None;
                    // In the form a pick waits for the rest; on the list it
                    // goes at once.
                    match &mut self.form {
                        Some(form) => form.issue = *after,
                        None => self.change(*change, *after, None),
                    }
                }
            }
            return Action::None;
        }
        if let Some(form) = &mut self.form {
            self.message = None;
            match form.handle_key(key) {
                Filled::Continue if form.is_leaving => {
                    self.message = Some("Esc again drops what you wrote; Ctrl+S saves it".into());
                }
                Filled::Continue => {}
                Filled::Cancel => self.form = None,
                Filled::Save => {
                    if let Some(form) = self.form.take() {
                        self.save(form);
                    }
                }
                Filled::Pick(kind, query) => {
                    if let Some(team) = self.teams.get(&self.source) {
                        let mut picker = Picker::new(kind, &form.issue, team);
                        picker.query = query;
                        if !picker.query.is_empty() {
                            picker.cursor = 0;
                        }
                        self.picker = Some(picker);
                    }
                }
                Filled::Editor => self.editor_text = Some(form.description.text()),
            }
            return Action::None;
        }
        // Ctrl keys belong to the boxes and pickers above; past them only
        // Ctrl+C means anything, and it quits.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Action::Quit,
                _ => Action::None,
            };
        }
        self.message = None;
        if self.search.is_some() {
            self.on_search(key);
            return Action::None;
        }
        if let Some(at) = self.panel {
            self.on_panel(key, at);
            return Action::None;
        }
        if let Some(at) = self.viewer {
            return self.on_viewer(key, at);
        }
        if self.focus == Focus::Issue {
            match key.code {
                KeyCode::Tab => return self.select(1),
                KeyCode::BackTab => return self.select(-1),
                KeyCode::Enter => {
                    if let Some(at) = self.target {
                        self.open_target(at);
                    }
                    return Action::None;
                }
                _ => {}
            }
            let max = self.max_scroll.get();
            let by = |scroll: u16, delta: i32| {
                u16::try_from((i32::from(scroll) + delta).clamp(0, i32::from(max))).unwrap_or(0)
            };
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => self.scroll = by(self.scroll, 1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = by(self.scroll, -1),
                KeyCode::PageDown | KeyCode::Char(' ') => self.scroll = by(self.scroll, 20),
                KeyCode::PageUp => self.scroll = by(self.scroll, -20),
                KeyCode::Home | KeyCode::Char('g') => self.scroll = 0,
                KeyCode::End | KeyCode::Char('G') => self.scroll = max,
                KeyCode::Left | KeyCode::Char('h') | KeyCode::Esc => return self.back(),
                _ => return self.on_issue(key),
            }
            return Action::None;
        }
        if self.choosing.is_some() {
            match key.code {
                KeyCode::Enter => {
                    if let Some(issue) = self.current().cloned() {
                        self.choosing = None;
                        return Action::Attach(Box::new(issue));
                    }
                    if self.is_folded(self.selected) {
                        self.toggle_fold();
                    }
                    return Action::None;
                }
                KeyCode::Esc | KeyCode::Tab => {
                    self.choosing = None;
                    return Action::Back;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab | KeyCode::Esc => return Action::Back,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX),
            KeyCode::Enter | KeyCode::Right if self.is_folded(self.selected) => {
                self.toggle_fold();
            }
            KeyCode::Char('J') => self.scroll_preview(1),
            KeyCode::Char('K') => self.scroll_preview(-1),
            KeyCode::PageDown => {
                let page = i32::from(self.preview_height.get().saturating_sub(1).max(1));
                self.scroll_preview(page);
            }
            KeyCode::PageUp => {
                let page = i32::from(self.preview_height.get().saturating_sub(1).max(1));
                self.scroll_preview(-page);
            }
            KeyCode::Char('z') => self.toggle_fold(),
            KeyCode::Char('Z') => self.toggle_all_folds(),
            KeyCode::Enter | KeyCode::Right if self.current().is_some() => {
                self.focus = Focus::Issue;
                self.is_from_outside = false;
                self.scroll = 0;
                self.target = None;
            }
            KeyCode::Char('/') => {
                self.search = Some(self.filters.query.clone().unwrap_or_default());
            }
            KeyCode::Char('f') => self.panel = Some(0),
            KeyCode::Char(c @ '1'..='9') => {
                self.show_view(usize::from(c as u8 - b'1'));
            }
            KeyCode::Char('m') => {
                self.view = None;
                self.filters.mine = !self.filters.mine;
                self.filters.assignee = None;
                self.reload_list();
            }
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('n') => self.open_new(),
            KeyCode::Char('w') => self.next_source(),
            _ => return self.on_issue(key),
        }
        Action::None
    }

    /// Moves Tab's mark to the next target, `delta` away, round from the
    /// last to the first, scrolling it into view.
    fn select(&mut self, delta: isize) -> Action {
        let count = self.targets().len();
        if count == 0 {
            self.message = Some("this issue has no links or images".into());
            return Action::None;
        }
        let at = match self.target {
            None if delta < 0 => count - 1,
            None => 0,
            Some(at) => (at as isize + delta).rem_euclid(count as isize) as usize,
        };
        self.target = Some(at);
        let placed = self
            .placed
            .borrow()
            .iter()
            .find(|(t, ..)| *t == at)
            .copied();
        if let Some((_, row, rows)) = placed {
            let height = self.view_height.get();
            if row < self.scroll {
                self.scroll = row;
            } else if row + rows > self.scroll + height {
                self.scroll = (row + rows).saturating_sub(height).min(row);
            }
            self.scroll = self.scroll.min(self.max_scroll.get());
        }
        Action::None
    }

    /// Opens target `at` of the open issue: an issue in its place, Esc
    /// coming back, or an image over the whole screen.
    fn open_target(&mut self, at: usize) {
        match self.targets().into_iter().nth(at) {
            Some(Target::Issue(link)) => {
                if self.viewed_id() != Some(link.id.as_str()) {
                    self.trail.push(link.id);
                }
                self.scroll = 0;
                self.target = None;
            }
            Some(Target::Image { .. }) => self.viewer = Some(at),
            None => {}
        }
    }

    /// Asks for an issue to be chosen from the list for `what`, such as
    /// "the issue tree 4 works on": Enter answers with it, Esc without.
    pub fn choose_issue(&mut self, what: String) {
        self.choosing = Some(what);
        self.focus = Focus::List;
        self.trail.clear();
        self.viewer = None;
        self.panel = None;
    }

    /// Opens issue `id` on the whole screen, as asked from the program's
    /// own screen; Esc goes back there.
    pub fn show_issue(&mut self, id: &str) {
        self.focus = Focus::Issue;
        self.trail = vec![id.to_string()];
        self.is_from_outside = true;
        self.scroll = 0;
        self.target = None;
        self.viewer = None;
        self.panel = None;
        self.search = None;
    }

    /// Back from the open issue: to the one it was opened from, else the
    /// list, or the program's own screen when it was opened from there.
    fn back(&mut self) -> Action {
        let from = self.trail.pop();
        self.scroll = 0;
        self.target = None;
        if self.is_from_outside && self.trail.is_empty() {
            self.is_from_outside = false;
            self.focus = Focus::List;
            return Action::Back;
        }
        if from.is_none() {
            self.focus = Focus::List;
        }
        Action::None
    }

    /// The keys of an image over the whole screen.
    fn on_viewer(&mut self, key: KeyEvent, at: usize) -> Action {
        let images: Vec<(usize, String)> = self
            .targets()
            .into_iter()
            .enumerate()
            .filter_map(|(i, t)| match t {
                Target::Image { url, .. } => Some((i, url)),
                Target::Issue(_) => None,
            })
            .collect();
        let here = images.iter().position(|(i, _)| *i == at).unwrap_or(0);
        let step = |delta: isize| {
            let to = (here as isize + delta).rem_euclid(images.len().max(1) as isize);
            images.get(to as usize).map(|(i, _)| *i)
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter | KeyCode::Backspace => {
                self.viewer = None;
                self.target = Some(at);
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab | KeyCode::Char(' ') => {
                self.viewer = step(1);
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => self.viewer = step(-1),
            KeyCode::Char('o') => {
                if let Some((_, url)) = images.get(here) {
                    return Action::Open(url.clone());
                }
            }
            _ => {}
        }
        Action::None
    }

    /// A click opens the link or image under it; the wheel scrolls.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Action {
        if !self.wants_mouse() {
            return Action::None;
        }
        // On the list the wheel scrolls the preview when over it, and moves
        // through the issues anywhere else.
        if self.focus == Focus::List {
            let is_preview = self
                .preview_area
                .get()
                .contains(Position::new(mouse.column, mouse.row));
            match (mouse.kind, is_preview) {
                (MouseEventKind::ScrollDown, true) => self.scroll_preview(3),
                (MouseEventKind::ScrollUp, true) => self.scroll_preview(-3),
                (MouseEventKind::ScrollDown, false) => self.move_by(1),
                (MouseEventKind::ScrollUp, false) => self.move_by(-1),
                _ => {}
            }
            return Action::None;
        }
        if self.viewer.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.viewer = None;
            }
            return Action::None;
        }
        let max = self.max_scroll.get();
        match mouse.kind {
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_add(3).min(max),
            MouseEventKind::ScrollUp => self.scroll = self.scroll.saturating_sub(3),
            MouseEventKind::Down(MouseButton::Left) => {
                let at = Position::new(mouse.column, mouse.row);
                let hit = self
                    .hits
                    .borrow()
                    .iter()
                    .find(|(area, _)| area.contains(at))
                    .map(|(_, target)| *target);
                if let Some(target) = hit {
                    self.target = Some(target);
                    self.open_target(target);
                }
            }
            _ => {}
        }
        Action::None
    }

    /// The keys that act on the issue under the cursor, the same from the
    /// list and from inside the issue.
    fn on_issue(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char(':') => {
                let context = match self.current() {
                    Some(issue) => format!(
                        "the Issues screen, on {} \"{}\" ({})",
                        issue.id, issue.title, issue.state
                    ),
                    None => "the Issues screen".to_string(),
                };
                return Action::Ask(context);
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.is_help = true,
            KeyCode::Char('s') => self.open_picker(picker::Kind::Status),
            KeyCode::Char('l') => self.open_picker(picker::Kind::Labels),
            KeyCode::Char('a') => self.open_picker(picker::Kind::Assignee),
            KeyCode::Char('c') => self.open_compose(),
            KeyCode::Char('e') => self.open_edit(),
            KeyCode::Char('o') => {
                if let Some(url) = self.current().and_then(|i| i.url.clone()) {
                    return Action::Open(url);
                }
            }
            KeyCode::Char('p') => {
                if let Some(issue) = self.current().cloned() {
                    // Picking up leaves the screen, so a tree's choice ends
                    // unanswered rather than answering the next Enter.
                    self.choosing = None;
                    return Action::Pick(Box::new(issue));
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
        /// Issues as their updates left them, by id.
        pub(crate) changed: Mutex<HashMap<String, Issue>>,
        /// Descriptions other than `<id> text`, by id.
        pub(crate) descriptions: HashMap<String, String>,
        /// Each issue's links, by id.
        pub(crate) links: HashMap<String, Vec<linear::Link>>,
        /// Files `fetch` returns, by URL.
        pub(crate) files: HashMap<String, Vec<u8>>,
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
            let asked = crate::line(&Filters {
                mine: false,
                ..filters.clone()
            });
            if !asked.is_empty() {
                self.asked.lock().unwrap().push(format!("filters {asked}"));
            }
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
            let updated = Issue {
                state: state.map_or(issue.state.clone(), |s| s.name.clone()),
                state_id: change.state_id.clone().or(issue.state_id.clone()),
                title: change.title.clone().unwrap_or(issue.title.clone()),
                updated_at: "2026-10-17T11:00:00Z".into(),
                ..issue.clone()
            };
            self.changed
                .lock()
                .unwrap()
                .insert(updated.id.clone(), updated.clone());
            Ok(updated)
        }

        fn create(&self, new: &linear::NewIssue) -> Result<Issue, String> {
            if let Some(why) = &self.refuse {
                return Err(why.clone());
            }
            self.asked.lock().unwrap().push(format!(
                "create {} in {:?} priority {:?}",
                new.title, new.state_id, new.priority
            ));
            Ok(Issue {
                description: new.description.clone(),
                state_id: new.state_id.clone(),
                ..issue("END-900")
            })
        }

        fn detail(&self, id: &str) -> Result<Detail, String> {
            self.asked.lock().unwrap().push(format!("detail {id}"));
            let changed = self.changed.lock().unwrap().get(id).cloned();
            // An issue off the list, as a link leads to, is read too.
            let issue = changed
                .or_else(|| self.issues.iter().find(|i| i.id == id).cloned())
                .unwrap_or_else(|| issue(id));
            Ok(Detail {
                issue: Issue {
                    description: Some(
                        self.descriptions
                            .get(id)
                            .cloned()
                            .unwrap_or_else(|| format!("{id} text")),
                    ),
                    ..issue
                },
                comments: Vec::new(),
                links: self.links.get(id).cloned().unwrap_or_default(),
            })
        }

        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            self.asked.lock().unwrap().push(format!("fetch {url}"));
            self.files
                .get(url)
                .cloned()
                .ok_or_else(|| format!("{url}: 404 Not Found"))
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
            matches!(screen.handle_key(key(KeyCode::Char('p'))), Action::Pick(i) if i.id == "END-1")
        );
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('o'))),
            Action::Open("https://linear.app/x/issue/END-1".into())
        );
        assert_eq!(screen.handle_key(key(KeyCode::Tab)), Action::Back);
        assert_eq!(screen.handle_key(key(KeyCode::Char('q'))), Action::Quit);
    }

    #[test]
    fn enter_opens_the_issue_to_scroll_through_until_esc() {
        let (mut screen, _) = screen();
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.focus, Focus::Issue);
        screen.max_scroll.set(3);
        for _ in 0..5 {
            screen.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(
            (screen.scroll, screen.selected),
            (3, 0),
            "it scrolls to the end, the cursor stays"
        );
        screen.handle_key(key(KeyCode::Char('g')));
        assert_eq!(screen.scroll, 0);
        assert!(
            matches!(screen.handle_key(key(KeyCode::Char('p'))), Action::Pick(i) if i.id == "END-1"),
            "and picks it up from inside"
        );
        screen.handle_key(key(KeyCode::Esc));
        assert_eq!(screen.focus, Focus::List);
        screen.handle_key(key(KeyCode::Char('j')));
        assert_eq!((screen.selected, screen.scroll), (1, 0));
    }

    #[test]
    fn an_open_issue_fills_the_screen_and_says_how_far_down_it_is() {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            descriptions: [(
                "END-1".to_string(),
                (1..=60).map(|n| format!("line {n}\n\n")).collect(),
            )]
            .into(),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        wait_for(&mut screen, |s| s.detail().is_some());
        let below = drawn(&screen).join("\n");
        assert!(
            below.contains("J K or PgDn PgUp scroll it, Enter reads it whole"),
            "{below}"
        );
        screen.handle_key(key(KeyCode::Enter));
        let open = drawn(&screen).join("\n");
        assert!(!open.contains("STATUS"), "the list is gone: {open}");
        assert!(open.contains("Esc  back"), "{open}");
        assert!(open.contains(" 0% "), "{open}");
        screen.handle_key(key(KeyCode::Char('G')));
        let end = drawn(&screen).join("\n");
        assert!(end.contains("line 60"), "{end}");
        assert!(end.contains(" 100% "), "{end}");
    }

    #[test]
    fn a_repository_without_a_source_lists_nothing_until_w() {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            ..Fake::default()
        });
        let mut screen = Screen::without_source(
            vec![fake.clone()],
            "/home/me/blog has no issue source".into(),
            Vec::new(),
        );
        settle(&mut screen);
        assert!(
            fake.asked.lock().unwrap().is_empty(),
            "nothing is asked of work"
        );
        assert!(screen.issues.is_empty());
        let drawn = drawn(&screen).join("\n");
        assert!(drawn.contains(" no issue source for this repository"), "{drawn}");
        assert!(
            drawn.contains("/home/me/blog has no issue source; w shows work anyway"),
            "{drawn}"
        );
        screen.handle_key(key(KeyCode::Char('w')));
        settle(&mut screen);
        assert_eq!(screen.issues.len(), 1);
    }

    #[test]
    fn an_issue_chosen_for_a_tree_comes_back_with_enter() {
        let (mut screen, _) = screen();
        screen.choose_issue("the issue tree 4 works on".into());
        let bar = drawn(&screen)[0].clone();
        assert!(
            bar.contains("Choose the issue tree 4 works on  ·  Enter chooses it, Esc cancels"),
            "{bar}"
        );
        screen.handle_key(key(KeyCode::Char('/')));
        typed(&mut screen, "end-2");
        screen.handle_key(key(KeyCode::Enter));
        assert!(
            matches!(screen.handle_key(key(KeyCode::Enter)), Action::Attach(i) if i.id == "END-2"),
            "search, then Enter chooses"
        );
        assert!(screen.choosing.is_none());
        assert_eq!(
            screen.handle_key(key(KeyCode::Enter)),
            Action::None,
            "Enter reads again once chosen"
        );
        screen.handle_key(key(KeyCode::Esc));
        screen.choose_issue("the issue tree 4 works on".into());
        assert_eq!(screen.handle_key(key(KeyCode::Esc)), Action::Back);
        assert!(screen.choosing.is_none());
    }

    /// Choosing an issue for tree 4, then picking another up instead, used
    /// to leave the choice open, so the next Enter on any issue chose it.
    #[test]
    fn picking_an_issue_up_ends_choosing_one_for_a_tree() {
        let (mut screen, _) = screen();
        screen.choose_issue("the issue tree 4 works on".into());
        assert!(
            matches!(screen.handle_key(key(KeyCode::Char('p'))), Action::Pick(i) if i.id == "END-1")
        );
        assert!(screen.choosing.is_none());
        screen.open();
        assert_eq!(
            screen.handle_key(key(KeyCode::Enter)),
            Action::None,
            "Enter reads the issue rather than choosing it"
        );
    }

    #[test]
    fn colon_asks_the_assistant_about_the_issue_under_the_cursor() {
        let (mut screen, _) = screen();
        assert_eq!(
            screen.handle_key(key(KeyCode::Char(':'))),
            Action::Ask("the Issues screen, on END-1 \"END-1 title\" ()".into())
        );
    }

    #[test]
    fn m_lists_mine_or_everyones() {
        let (mut screen, fake) = screen();
        screen.handle_key(key(KeyCode::Char('m')));
        settle(&mut screen);
        let lists: Vec<String> = fake
            .asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.starts_with("list"))
            .cloned()
            .collect();
        assert_eq!(lists, ["list mine=true", "list mine=false"]);
    }

    fn filters_asked(fake: &Fake) -> Vec<String> {
        fake.asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| a.starts_with("filters"))
            .cloned()
            .collect()
    }

    #[test]
    fn search_narrows_the_list_as_you_type_then_asks_the_source() {
        let (mut screen, fake) = screen();
        screen.handle_key(key(KeyCode::Char('/')));
        typed(&mut screen, "end-2");
        let ids: Vec<&str> = screen.issues.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["END-2"], "at once, from the issues listed");
        assert!(drawn(&screen)[0].contains("Search › end-2"));
        assert!(
            filters_asked(&fake).is_empty(),
            "the source waits for typing to rest"
        );
        wait_for(&mut screen, |_| !filters_asked(&fake).is_empty());
        assert_eq!(filters_asked(&fake), ["filters end-2"]);
        settle(&mut screen);
        assert_eq!(
            screen.issues.len(),
            1,
            "what the source sends is narrowed too"
        );
        screen.handle_key(key(KeyCode::Enter));
        assert!(screen.search.is_none());
        let bar = drawn(&screen)[0].clone();
        assert!(bar.contains("“end-2”"), "{bar}");
        screen.handle_key(key(KeyCode::Char('/')));
        screen.handle_key(key(KeyCode::Esc));
        settle(&mut screen);
        assert_eq!(screen.issues.len(), 2, "Esc clears it");
        assert_eq!(screen.filters.query, None);
    }

    #[test]
    fn the_filter_panel_picks_each_filter_from_the_teams_own() {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            team: Some(team_of(&["Todo", "In Review"])),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake.clone()], 0, Vec::new());
        settle(&mut screen);
        let bar = drawn(&screen)[0].clone();
        assert!(bar.contains("my issues  ·  open"), "{bar}");
        assert!(bar.contains("f filters  / search"), "{bar}");
        screen.handle_key(key(KeyCode::Char('f')));
        let panel = drawn(&screen).join("\n");
        assert!(panel.contains(" Filters "), "{panel}");
        assert!(panel.contains("› Assignee  me"), "{panel}");
        screen.handle_key(key(KeyCode::Down));
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.picker.as_ref().unwrap().issue, "Status");
        typed(&mut screen, "review");
        screen.handle_key(key(KeyCode::Enter));
        assert!(
            screen.picker.is_none() && screen.panel.is_some(),
            "back in the panel"
        );
        settle(&mut screen);
        assert_eq!(filters_asked(&fake), [r#"filters state:"In Review""#]);
        screen.handle_key(key(KeyCode::Up));
        screen.handle_key(key(KeyCode::Char('x')));
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Esc));
        assert!(screen.panel.is_none());
        let bar = drawn(&screen)[0].clone();
        assert!(bar.contains("everyone's issues  ·  In Review"), "{bar}");
        screen.handle_key(key(KeyCode::Char('f')));
        screen.handle_key(key(KeyCode::Char('c')));
        settle(&mut screen);
        assert_eq!(screen.filters, Filters::default(), "c clears them all");
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
        assert_eq!(order, ["END-2", "END-1"], "closest to done first");
        assert_eq!(screen.heading(0).as_deref(), Some("In Progress"));
        assert_eq!(screen.heading(1).as_deref(), Some("Todo"));
        let header = |screen: &Screen| drawn(screen)[1].clone();
        assert!(!header(&screen).contains("STATUS"), "the headings say it");
        screen.handle_key(key(KeyCode::Char('6')));
        settle(&mut screen);
        assert_eq!(screen.view_name(), "bugs");
        assert!(header(&screen).contains("STATUS"), "{}", header(&screen));
        assert_eq!(screen.heading(0), None, "no grouping");
        screen.handle_key(key(KeyCode::Char('m')));
        assert_eq!(screen.view_name(), "custom");
        screen.handle_key(key(KeyCode::Char('9')));
        assert!(screen.message.as_deref().unwrap().starts_with("no view 9"));
    }

    #[test]
    fn a_status_group_folds_to_its_heading_and_the_cursor_steps_over_it() {
        let in_state = |id: &str, state: &str| Issue {
            state: state.into(),
            state_id: Some(state.into()),
            ..issue(id)
        };
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![
                in_state("END-1", "doing"),
                in_state("END-2", "doing"),
                in_state("END-3", "todo"),
            ],
            team: Some(team_of(&["todo", "doing"])),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        assert_eq!(
            screen.heading(0).as_deref(),
            Some("doing"),
            "mine groups by status"
        );
        screen.handle_key(key(KeyCode::Char('g')));
        screen.handle_key(key(KeyCode::Char('j')));
        assert_eq!(screen.current().unwrap().id, "END-2");
        screen.handle_key(key(KeyCode::Char('z')));
        assert_eq!(screen.current(), None, "a folded group is its heading");
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('p'))),
            Action::None,
            "nothing hidden is picked up"
        );
        let drawn = drawn(&screen).join("\n");
        assert!(drawn.contains("▸"), "{drawn}");
        assert!(!drawn.contains("END-1"), "{drawn}");
        screen.handle_key(key(KeyCode::Char('j')));
        assert_eq!(
            screen.current().unwrap().id,
            "END-3",
            "one step past the fold"
        );
        screen.handle_key(key(KeyCode::Char('k')));
        assert_eq!(screen.current(), None, "back onto the folded heading");
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.current().unwrap().id, "END-1", "Enter unfolds it");
        screen.handle_key(key(KeyCode::Char('Z')));
        assert_eq!(screen.folded.len(), 2, "Z folds every group");
        screen.handle_key(key(KeyCode::Char('G')));
        assert_eq!(screen.selected, 2, "the last heading");
        screen.handle_key(key(KeyCode::Char('Z')));
        assert!(screen.folded.is_empty(), "and unfolds them all");
    }

    #[test]
    fn the_preview_scrolls_within_its_issue_and_starts_at_the_top_of_the_next() {
        let (mut screen, _) = screen();
        drawn(&screen);
        let first = screen.current().unwrap().id.clone();
        screen.preview_max.set(5);
        screen.preview_height.set(4);
        screen.handle_key(key(KeyCode::Char('J')));
        screen.handle_key(key(KeyCode::Char('J')));
        assert_eq!(screen.preview_scroll(&first), 2);
        screen.handle_key(key(KeyCode::PageDown));
        assert_eq!(screen.preview_scroll(&first), 5, "no further than it holds");
        screen.handle_key(key(KeyCode::Char('K')));
        assert_eq!(screen.preview_scroll(&first), 4);
        screen.handle_key(key(KeyCode::PageUp));
        assert_eq!(screen.preview_scroll(&first), 1);
        screen.handle_key(key(KeyCode::Char('J')));
        screen.handle_key(key(KeyCode::Char('j')));
        let second = screen.current().unwrap().id.clone();
        assert_ne!(first, second);
        assert_eq!(
            screen.preview_scroll(&second),
            0,
            "the next issue starts at its top"
        );
        assert_eq!(
            screen.current().unwrap().id,
            second,
            "J and K scroll; j and k still move"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_preview_over_it_and_moves_the_list_elsewhere() {
        let (mut screen, _) = screen();
        drawn(&screen);
        assert!(screen.wants_mouse(), "the list takes the wheel");
        let first = screen.current().unwrap().id.clone();
        screen.preview_max.set(10);
        let preview = screen.preview_area.get();
        let wheel = |kind, row| MouseEvent {
            kind,
            column: preview.x + 2,
            row,
            modifiers: KeyModifiers::NONE,
        };
        screen.handle_mouse(wheel(MouseEventKind::ScrollDown, preview.y + 2));
        assert_eq!(screen.preview_scroll(&first), 3);
        screen.handle_mouse(wheel(MouseEventKind::ScrollUp, preview.y + 2));
        assert_eq!(screen.preview_scroll(&first), 0);
        screen.handle_mouse(wheel(MouseEventKind::ScrollDown, 3));
        assert_ne!(
            screen.current().unwrap().id,
            first,
            "over the list it moves"
        );
    }

    #[test]
    fn statuses_stack_closest_to_done_first_and_closed_ones_come_folded() {
        let in_state = |id: &str, state: &str| Issue {
            state: state.into(),
            state_id: Some(state.into()),
            ..issue(id)
        };
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![
                in_state("END-1", "backlog"),
                in_state("END-2", "done"),
                in_state("END-3", "todo"),
                in_state("END-4", "staging"),
                in_state("END-5", "duplicate"),
                in_state("END-6", "review"),
            ],
            team: Some(team_of(&[
                "backlog",
                "todo",
                "review",
                "staging",
                "done",
                "canceled",
                "duplicate",
            ])),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        let headings: Vec<String> = (0..screen.issues.len())
            .filter_map(|at| screen.heading(at))
            .collect();
        assert_eq!(
            headings,
            ["staging", "review", "todo", "backlog", "done", "duplicate"]
        );
        assert_eq!(
            screen.folded,
            ["canceled", "done", "duplicate"].map(String::from).into(),
            "closed groups come folded"
        );
        screen.handle_key(key(KeyCode::Char('G')));
        assert_eq!(
            screen.current(),
            None,
            "the cursor rests on the folded heading"
        );
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.current().unwrap().id, "END-5", "opened by hand");
        screen.handle_key(key(KeyCode::Char('r')));
        settle(&mut screen);
        assert!(
            !screen.folded.contains("duplicate"),
            "a group opened by hand stays open when the list is read again"
        );
        assert!(screen.folded.contains("done"));
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
                    kind: match *id {
                        "done" => "completed",
                        "canceled" => "canceled",
                        "duplicate" => "duplicate",
                        _ => "unstarted",
                    }
                    .into(),
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
        drawn_with(screen, &crate::NoTrees, 80)
    }

    /// What `screen` draws `width` columns wide, with `host` saying who
    /// works on what.
    fn drawn_with(screen: &Screen, host: &dyn crate::Host, width: u16) -> Vec<String> {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal
            .draw(|frame| screen.draw(frame, frame.area(), &crate::Palette::default(), host))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..24)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// Tree 4 works on END-2, its agent waiting on you.
    struct TreeFour;

    impl crate::Host for TreeFour {
        fn work_on(&self, id: &str) -> Option<crate::Work> {
            (id == "END-2").then(|| crate::Work {
                tree: "4".into(),
                agent: Some("waiting".into()),
                needs_you: true,
                is_ready: false,
            })
        }
    }

    #[test]
    fn the_work_column_says_which_tree_and_agent_work_on_an_issue() {
        let (screen, _) = screen();
        let rows = drawn_with(&screen, &TreeFour, 140);
        let row = |id: &str| rows.iter().find(|r| r.starts_with(id)).unwrap().clone();
        assert!(
            rows[1].contains("WORK"),
            "the header, under the bar: {}",
            rows[1]
        );
        assert!(
            row("END-2").trim_end().ends_with("4 · waiting"),
            "{}",
            row("END-2")
        );
        assert!(!row("END-1").contains('·'), "{}", row("END-1"));
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

    fn wait_for(screen: &mut Screen, done: impl Fn(&Screen) -> bool) {
        for _ in 0..200 {
            screen.poll();
            if done(screen) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn typed(screen: &mut Screen, text: &str) {
        for c in text.chars() {
            screen.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn creating(refuse: Option<&str>) -> (Screen, Arc<Fake>) {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            team: Some(team_of(&["todo", "doing"])),
            refuse: refuse.map(str::to_string),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake.clone()], 0, Vec::new());
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Char('n')));
        typed(&mut screen, "Fix login");
        screen.handle_key(key(KeyCode::Tab));
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.picker.as_ref().unwrap().kind.noun(), "Status");
        screen.handle_key(key(KeyCode::Down));
        screen.handle_key(key(KeyCode::Enter));
        screen.handle_key(key(KeyCode::Tab));
        typed(&mut screen, "hi");
        assert_eq!(
            screen.picker.as_ref().unwrap().query,
            "hi",
            "typing on a field opens its picker with what was typed"
        );
        screen.handle_key(key(KeyCode::Enter));
        for _ in 0..4 {
            screen.handle_key(key(KeyCode::Tab));
        }
        typed(&mut screen, "Keep return_to.");
        screen.handle_key(ctrl('s'));
        wait_for(&mut screen, |s| {
            s.message
                .as_deref()
                .is_some_and(|m| !m.starts_with("creating"))
        });
        (screen, fake)
    }

    #[test]
    fn n_writes_a_new_issue_in_one_form_and_creates_it() {
        let (screen, fake) = creating(None);
        assert_eq!(screen.message.as_deref(), Some("created END-900"));
        assert!(screen.form.is_none());
        assert_eq!(
            screen.current().unwrap().id,
            "END-900",
            "first, under the cursor"
        );
        assert_eq!(
            screen.current().unwrap().description.as_deref(),
            Some("Keep return_to.")
        );
        assert!(
            fake.asked
                .lock()
                .unwrap()
                .contains(&"create Fix login in Some(\"doing\") priority Some(2)".to_string()),
            "{:?}",
            fake.asked.lock().unwrap()
        );
    }

    #[test]
    fn a_new_issue_refused_opens_again_as_it_was_written() {
        let (screen, _) = creating(Some("Linear: title too long"));
        assert!(
            screen
                .message
                .as_deref()
                .unwrap()
                .ends_with("it is open again to retry")
        );
        let form = screen.form.as_ref().unwrap();
        assert_eq!(form.title.text(), "Fix login");
        assert_eq!(form.description.text(), "Keep return_to.");
        assert_eq!(form.issue.state, "doing");
    }

    #[test]
    fn a_new_issue_needs_a_title() {
        let (mut screen, fake) = screen_with_team();
        screen.handle_key(key(KeyCode::Char('n')));
        screen.handle_key(key(KeyCode::BackTab));
        screen.handle_key(ctrl('s'));
        assert_eq!(screen.message.as_deref(), Some("a new issue needs a title"));
        assert_eq!(
            screen.form.as_ref().unwrap().field,
            crate::form::Field::Title
        );
        assert!(
            !fake
                .asked
                .lock()
                .unwrap()
                .iter()
                .any(|a| a.starts_with("create"))
        );
    }

    #[test]
    fn esc_twice_drops_a_form_written_in() {
        let (mut screen, _) = screen_with_team();
        screen.handle_key(key(KeyCode::Char('n')));
        typed(&mut screen, "half a thought");
        screen.handle_key(key(KeyCode::Esc));
        assert!(screen.form.is_some());
        assert!(screen.message.as_deref().unwrap().starts_with("Esc again"));
        screen.handle_key(key(KeyCode::Esc));
        assert!(screen.form.is_none());
    }

    #[test]
    fn e_edits_the_issue_in_the_same_form() {
        let (mut screen, _) = screen_with_team();
        wait_for(&mut screen, |s| s.detail().is_some());
        screen.handle_key(key(KeyCode::Char('e')));
        let form = screen.form.as_ref().unwrap();
        assert_eq!(form.title.text(), "END-1 title");
        assert_eq!(form.description.text(), "END-1 text");
        screen.handle_key(ctrl('u'));
        typed(&mut screen, "Fix the redirect");
        screen.handle_key(ctrl('s'));
        assert!(screen.form.is_none());
        assert_eq!(screen.issues[0].title, "Fix the redirect", "shown at once");
        wait_for(&mut screen, |s| {
            s.message.as_deref() == Some("END-1 updated")
        });
        assert_eq!(screen.issues[0].title, "Fix the redirect");
        // The issue changed, so its text is read again before the next edit.
        wait_for(&mut screen, |s| s.detail().is_some());
        screen.handle_key(key(KeyCode::Char('e')));
        screen.handle_key(ctrl('s'));
        assert_eq!(screen.message.as_deref(), Some("END-1 unchanged"));
    }

    #[test]
    fn ctrl_e_writes_the_description_in_the_users_editor() {
        let (mut screen, _) = screen_with_team();
        wait_for(&mut screen, |s| s.detail().is_some());
        screen.handle_key(key(KeyCode::Char('e')));
        screen.handle_key(ctrl('e'));
        assert_eq!(screen.take_editor(), None, "only from the description");
        screen.handle_key(key(KeyCode::BackTab));
        screen.handle_key(ctrl('e'));
        assert_eq!(screen.take_editor().as_deref(), Some("END-1 text"));
        screen.edited(Ok("Longer text.\n\n- a list\n".into()));
        assert_eq!(
            screen.form.as_ref().unwrap().description.text(),
            "Longer text.\n\n- a list"
        );
    }

    #[test]
    fn an_edit_refused_opens_again_with_what_was_written() {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            team: Some(team_of(&["todo"])),
            refuse: Some("Linear: offline".into()),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake], 0, Vec::new());
        settle(&mut screen);
        wait_for(&mut screen, |s| s.detail().is_some());
        screen.handle_key(key(KeyCode::Char('e')));
        typed(&mut screen, " now");
        screen.handle_key(ctrl('s'));
        wait_for(&mut screen, |s| s.form.is_some());
        assert_eq!(screen.issues[0].title, "END-1 title", "the row goes back");
        assert_eq!(
            screen.form.as_ref().unwrap().title.text(),
            "END-1 title now"
        );
        assert!(
            screen
                .message
                .as_deref()
                .unwrap()
                .contains("Linear: offline")
        );
    }

    #[test]
    fn the_form_fills_the_screen_and_marks_the_field_its_cursor_is_in() {
        let (mut screen, _) = screen_with_team();
        screen.handle_key(key(KeyCode::Char('n')));
        typed(&mut screen, "Fix login");
        screen.handle_key(key(KeyCode::Tab));
        let drawn = drawn(&screen).join("\n");
        assert!(drawn.contains(" New issue in END "), "{drawn}");
        assert!(!drawn.contains("STATUS"), "the list is gone: {drawn}");
        assert!(drawn.contains("Fix login"), "{drawn}");
        assert!(drawn.contains("› Status"), "{drawn}");
        assert!(drawn.contains("Enter changes it"), "{drawn}");
        assert!(drawn.contains("Ctrl+S  save"), "{drawn}");
    }

    fn reading(fake: Fake) -> Screen {
        let mut screen = Screen::new(vec![Arc::new(fake)], 0, Vec::new());
        // As a terminal that answers in no graphics of its own would.
        screen.has_asked_terminal = true;
        settle(&mut screen);
        screen.handle_key(key(KeyCode::Enter));
        wait_for(&mut screen, |s| s.detail().is_some());
        screen
    }

    fn blocked_by(id: &str) -> linear::Link {
        linear::Link {
            kind: "blocked by".into(),
            id: id.into(),
            title: "Fix the callback".into(),
            state: "Todo".into(),
            is_done: false,
        }
    }

    #[test]
    fn tab_and_enter_follow_a_link_and_esc_comes_back() {
        let mut screen = reading(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            links: [("END-1".to_string(), vec![blocked_by("END-7")])].into(),
            ..Fake::default()
        });
        let drawn_open = drawn(&screen).join("\n");
        assert!(drawn_open.contains("Linked issues"), "{drawn_open}");
        assert!(
            drawn_open.contains("blocked by    END-7     Fix the callback  Todo"),
            "{drawn_open}"
        );
        screen.handle_key(key(KeyCode::Tab));
        assert_eq!(screen.target, Some(0));
        assert!(drawn(&screen).join("\n").contains("› blocked by"));
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.trail, ["END-7"]);
        wait_for(&mut screen, |s| s.current().is_some());
        assert_eq!(
            screen.current().unwrap().id,
            "END-7",
            "off the list, read on its own"
        );
        let followed = drawn(&screen).join("\n");
        assert!(followed.contains("END-1 › END-7"), "{followed}");
        assert!(followed.contains("END-7 title"), "{followed}");
        screen.handle_key(key(KeyCode::Esc));
        assert_eq!(
            (screen.focus, screen.current().unwrap().id.as_str()),
            (Focus::Issue, "END-1")
        );
        screen.handle_key(key(KeyCode::Esc));
        assert_eq!(screen.focus, Focus::List);
    }

    #[test]
    fn an_issue_shown_from_the_trees_goes_back_there() {
        let (mut screen, _) = screen();
        screen.show_issue("END-40");
        wait_for(&mut screen, |s| s.current().is_some());
        assert_eq!(
            screen.current().unwrap().id,
            "END-40",
            "off the list, read on its own"
        );
        let top = drawn(&screen)[0].clone();
        assert!(top.contains(" END-40 ") && !top.contains("END-1"), "{top}");
        assert_eq!(screen.handle_key(key(KeyCode::Esc)), Action::Back);
        assert_eq!(screen.focus, Focus::List);
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(
            screen.handle_key(key(KeyCode::Esc)),
            Action::None,
            "an issue opened from the list goes back to the list"
        );
    }

    #[test]
    fn an_issue_mentioned_in_the_text_is_a_link_too() {
        let screen = reading(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            descriptions: [("END-1".to_string(), "Same cause as END-9.".to_string())].into(),
            ..Fake::default()
        });
        let drawn = drawn(&screen).join("\n");
        assert!(drawn.contains("mentioned     END-9"), "{drawn}");
    }

    #[test]
    fn a_click_opens_the_link_under_it() {
        let mut screen = reading(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            links: [("END-1".to_string(), vec![blocked_by("END-7")])].into(),
            ..Fake::default()
        });
        assert!(screen.wants_mouse());
        let drawn = drawn(&screen);
        let row = drawn.iter().position(|l| l.contains("END-7")).unwrap();
        screen.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 20,
            row: u16::try_from(row).unwrap(),
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(screen.trail, ["END-7"]);
    }

    fn png() -> Vec<u8> {
        let image = image::RgbImage::from_pixel(300, 60, image::Rgb([200, 40, 40]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    const FLOW: &str = "https://uploads.linear.app/flow.png";

    /// Whether the picture's red shows anywhere on screen.
    fn shows_red(screen: &Screen) -> bool {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        use ratatui::style::Color;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                screen.draw(
                    frame,
                    frame.area(),
                    &crate::Palette::default(),
                    &crate::NoTrees,
                )
            })
            .unwrap();
        let red = Color::Rgb(200, 40, 40);
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.bg == red || cell.fg == red)
    }

    #[test]
    fn an_image_shows_in_the_issue_and_on_the_whole_screen() {
        let mut screen = reading(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            descriptions: [(
                "END-1".to_string(),
                format!("Look:\n\n![flow]({FLOW})\n\nThe end."),
            )]
            .into(),
            files: [(FLOW.to_string(), png())].into(),
            ..Fake::default()
        });
        wait_for(&mut screen, |s| {
            matches!(s.pictures.get(FLOW), Some(Picture::Ready { .. }))
        });
        let open = drawn(&screen).join("\n");
        assert!(open.contains("▣ flow"), "{open}");
        assert!(shows_red(&screen), "the picture is drawn: {open}");
        assert!(open.contains("The end."), "{open}");
        screen.handle_key(key(KeyCode::Tab));
        assert!(
            drawn(&screen)
                .join("\n")
                .contains("Enter shows it on the whole screen")
        );
        screen.handle_key(key(KeyCode::Enter));
        assert_eq!(screen.viewer, Some(0));
        let whole = drawn(&screen).join("\n");
        assert!(whole.contains("flow · 1 of 1"), "{whole}");
        assert!(!whole.contains("The end."), "the image alone: {whole}");
        assert!(shows_red(&screen));
        assert!(whole.contains("other images"), "{whole}");
        assert_eq!(
            screen.handle_key(key(KeyCode::Char('o'))),
            Action::Open(FLOW.into())
        );
        screen.handle_key(key(KeyCode::Esc));
        assert_eq!((screen.viewer, screen.focus), (None, Focus::Issue));
    }

    #[test]
    fn an_image_that_cannot_be_read_says_why() {
        let mut screen = reading(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            descriptions: [("END-1".to_string(), format!("![flow]({FLOW})"))].into(),
            ..Fake::default()
        });
        wait_for(&mut screen, |s| {
            matches!(s.pictures.get(FLOW), Some(Picture::Failed(_)))
        });
        let drawn = drawn(&screen).join("\n");
        assert!(drawn.contains("could not show it"), "{drawn}");
    }

    #[test]
    fn a_long_issue_scrolls_an_image_a_row_at_a_time() {
        let text = format!(
            "{}\n\n![flow]({FLOW})\n\n{}",
            "above\n\n".repeat(12),
            "below\n\n".repeat(12)
        );
        let mut screen = reading(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            descriptions: [("END-1".to_string(), text)].into(),
            files: [(FLOW.to_string(), png())].into(),
            ..Fake::default()
        });
        wait_for(&mut screen, |s| {
            matches!(s.pictures.get(FLOW), Some(Picture::Ready { .. }))
        });
        // Every scroll position draws, the picture partly off either edge.
        for _ in 0..80 {
            drawn(&screen);
            screen.handle_key(key(KeyCode::Char('j')));
        }
        let end = drawn(&screen).join("\n");
        assert!(end.contains("below") && end.contains("100%"), "{end}");
    }

    fn screen_with_team() -> (Screen, Arc<Fake>) {
        let fake = Arc::new(Fake {
            name: "work".into(),
            issues: vec![issue("END-1")],
            team: Some(team_of(&["todo"])),
            ..Fake::default()
        });
        let mut screen = Screen::new(vec![fake.clone()], 0, Vec::new());
        settle(&mut screen);
        (screen, fake)
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
