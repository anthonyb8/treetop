//! What the screen shows and what each key does, kept free of terminal I/O so
//! the rules about what may be returned or destroyed are testable.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::diff::Layout;
use crate::jobs::{Job, Kind};
use crate::pool::{Process, Tree, Work};

/// Where a tree's latest job stands, shown in its STATUS cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued(Kind),
    Running(Kind),
    /// Shown until a listing that started after it lands, which carries the
    /// result: a returned tree turns available, a destroyed one disappears.
    Done(Kind, Instant),
    Failed(Kind),
}

impl JobState {
    pub fn is_active(self) -> bool {
        matches!(self, JobState::Queued(_) | JobState::Running(_))
    }
}

/// What the pane under the table shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Info,
    Log,
}

/// The full-screen diff of one tree, and where it is scrolled to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    pub path: PathBuf,
    /// The first row on screen.
    pub scroll: usize,
    /// Columns scrolled sideways, for lines wider than their half.
    pub hscroll: u16,
}

/// A tree's diff laid out for the view, or why it could not be read, with the
/// git counts it was asked for at. Reopening the view after the counts move on
/// reads it again; while open it holds still, so nothing jumps under a read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedDiff {
    pub layout: Result<Layout, String>,
    work: Option<Work>,
}

/// A return waiting on y/n in the confirm dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub trees: Vec<Tree>,
}

/// A tree's destroy preview, waiting on y/n.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub tree: Tree,
    pub preview: String,
    pub scroll: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub tree: String,
    pub kind: Kind,
    pub is_ok: bool,
    pub output: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    /// Open this tree: a tmux window inside tmux, else a shell that comes
    /// back to treetop when it exits.
    Enter(Tree),
    /// Hand these to the worker, in order.
    Queue(Vec<Job>),
    /// List the pool again now rather than at the next interval.
    Refresh,
}

#[derive(Default)]
pub struct App {
    pub trees: Vec<Tree>,
    pub error: Option<String>,
    /// A first listing has arrived; until then an empty table means nothing.
    pub is_loaded: bool,
    /// The trees on screen come from the cache, not from treehouse yet.
    pub is_cached: bool,
    /// A `treehouse status` is running.
    pub is_listing: bool,
    listing_started_at: Option<Instant>,
    /// When the last live listing landed, for the summary line.
    pub listed_at: Option<Instant>,
    pub filter: String,
    pub is_filtering: bool,
    pub marked: BTreeSet<PathBuf>,
    pub selected: usize,
    pub pending: Option<Pending>,
    pub reviews: VecDeque<Review>,
    pub jobs: HashMap<PathBuf, JobState>,
    pub log: Vec<LogEntry>,
    pub pane: Pane,
    /// Lines scrolled up from the newest end of the log.
    pub log_scroll: usize,
    pub view: Option<DiffView>,
    /// The terminal's height, for paging the diff view.
    pub screen_height: u16,
    pub diffs: HashMap<PathBuf, LoadedDiff>,
    /// Diffs asked for and not yet loaded, with the counts they were asked at.
    diff_requests: HashMap<PathBuf, Option<Work>>,
    /// `q` was pressed while jobs were running; quit once they finish.
    pub is_quitting: bool,
    pub message: Option<String>,
    /// The tree treetop was started in. Returning or destroying it would kill
    /// the shell standing in it, so it is never a target.
    pub here: Option<PathBuf>,
}

/// The last line worth showing from a command's output, for the status line.
fn last_line(output: &str) -> &str {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
}

impl App {
    pub fn new(here: Option<PathBuf>) -> Self {
        App {
            here,
            ..App::default()
        }
    }

    pub fn is_here(&self, tree: &Tree) -> bool {
        self.here.as_deref().is_some_and(|h| tree.contains(h))
    }

    /// Jobs still queued or running.
    pub fn active_jobs(&self) -> usize {
        self.jobs.values().filter(|s| s.is_active()).count()
    }

    pub fn visible(&self) -> Vec<&Tree> {
        let needle = self.filter.to_lowercase();
        self.trees
            .iter()
            .filter(|t| {
                needle.is_empty()
                    || [Some(&t.name), t.branch.as_ref(), t.holder.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|field| field.to_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn current(&self) -> Option<&Tree> {
        self.visible().get(self.selected).copied()
    }

    pub fn start_listing(&mut self) {
        self.is_listing = true;
        self.listing_started_at = Some(Instant::now());
    }

    /// Swaps in a live listing from treehouse. Finished jobs stop showing once
    /// a listing that started after them lands, since it carries their result.
    pub fn set_trees(&mut self, trees: Vec<Tree>) {
        self.replace_trees(trees);
        self.is_cached = false;
        self.is_listing = false;
        self.listed_at = Some(Instant::now());
        let started = self.listing_started_at;
        self.jobs.retain(
            |_, state| !matches!(state, JobState::Done(_, at) if started.is_some_and(|s| *at <= s)),
        );
    }

    /// Shows the cached listing until a live one lands.
    pub fn set_cached(&mut self, trees: Vec<Tree>) {
        self.replace_trees(trees);
        self.is_cached = true;
    }

    /// Takes the processes read from /proc, which are fresher than any
    /// listing. A tree absent from the map has none running.
    pub fn apply_processes(&mut self, processes: &HashMap<PathBuf, Vec<Process>>) {
        for tree in &mut self.trees {
            tree.processes = processes.get(&tree.path).cloned().unwrap_or_default();
        }
    }

    /// Takes the git state of the held trees in the map, leaving the rest as
    /// they were.
    pub fn apply_work(&mut self, work: &HashMap<PathBuf, Option<Work>>) {
        for tree in self.trees.iter_mut().filter(|t| t.is_held()) {
            if let Some(entry) = work.get(&tree.path) {
                tree.work = *entry;
            }
        }
    }

    /// Keeps the cursor on the same tree, keeps each surviving tree's probed
    /// processes and git state until the next probe replaces them, and drops
    /// marks and settled job states on trees that have left the pool.
    fn replace_trees(&mut self, mut trees: Vec<Tree>) {
        let cursor = self.current().map(|t| t.path.clone());
        for tree in &mut trees {
            if let Some(old) = self.trees.iter().find(|old| old.path == tree.path) {
                tree.processes = old.processes.clone();
                tree.work = old.work.filter(|_| tree.is_held());
            }
        }
        self.trees = trees;
        self.error = None;
        self.is_loaded = true;
        let paths: BTreeSet<PathBuf> = self.trees.iter().map(|t| t.path.clone()).collect();
        self.marked.retain(|p| paths.contains(p));
        self.jobs
            .retain(|path, state| state.is_active() || paths.contains(path));
        let visible = self.visible();
        self.selected = cursor
            .and_then(|c| visible.iter().position(|t| t.path == c))
            .unwrap_or(self.selected)
            .min(visible.len().saturating_sub(1));
    }

    pub fn job_started(&mut self, job: &Job) {
        self.jobs
            .insert(job.tree.path.clone(), JobState::Running(job.kind));
    }

    /// Records a finished job. A successful preview becomes a review; a failure
    /// stays on its row and its last line goes to the status line.
    pub fn job_finished(&mut self, job: Job, is_ok: bool, output: String) {
        let path = job.tree.path.clone();
        if !is_ok {
            self.jobs.insert(path, JobState::Failed(job.kind));
            self.message = Some(format!(
                "{} of tree {} failed: {}  (L shows the log)",
                job.kind.noun(),
                job.tree.name,
                last_line(&output)
            ));
        } else if job.kind == Kind::Preview {
            self.jobs.remove(&path);
            self.reviews.push_back(Review {
                tree: job.tree.clone(),
                preview: output.clone(),
                scroll: 0,
            });
        } else {
            self.jobs
                .insert(path, JobState::Done(job.kind, Instant::now()));
        }
        self.log.push(LogEntry {
            tree: job.tree.name,
            kind: job.kind,
            is_ok,
            output,
        });
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.visible().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// The diff the open view needs read, asked for once until it lands.
    pub fn wanted_diff(&mut self) -> Option<PathBuf> {
        let path = self.view.as_ref()?.path.clone();
        if self.diffs.contains_key(&path) || self.diff_requests.contains_key(&path) {
            return None;
        }
        let work = self
            .trees
            .iter()
            .find(|t| t.path == path)
            .and_then(|t| t.work);
        self.diff_requests.insert(path.clone(), work);
        Some(path)
    }

    pub fn diff_loaded(&mut self, path: PathBuf, layout: Result<Layout, String>) {
        let work = self.diff_requests.remove(&path).flatten();
        self.diffs.insert(path, LoadedDiff { layout, work });
    }

    /// Opens the diff of the tree under the cursor, dropping a cached one its
    /// counts have moved on from.
    fn open_diff(&mut self) {
        let Some(tree) = self.current() else { return };
        if !tree.is_held() {
            self.message = Some(format!("tree {} is available: nothing to diff", tree.name));
            return;
        }
        let (path, work) = (tree.path.clone(), tree.work);
        if self.diffs.get(&path).is_some_and(|d| d.work != work) {
            self.diffs.remove(&path);
        }
        self.view = Some(DiffView {
            path,
            scroll: 0,
            hscroll: 0,
        });
    }

    fn handle_view_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(path) = self.view.as_ref().map(|v| v.path.clone()) else {
            return Outcome::Continue;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Outcome::Quit,
                KeyCode::Char('r') => {
                    self.diffs.remove(&path);
                }
                _ => {}
            }
            return Outcome::Continue;
        }
        if matches!(key.code, KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q')) {
            self.view = None;
            return Outcome::Continue;
        }
        let (len, starts) = match self.diffs.get(&path).map(|d| &d.layout) {
            Some(Ok(layout)) => (layout.rows.len(), layout.file_starts.clone()),
            _ => (0, Vec::new()),
        };
        let page = isize::try_from(self.screen_height.saturating_sub(4)).map_or(1, |p| p.max(1));
        let Some(view) = self.view.as_mut() else {
            return Outcome::Continue;
        };
        let last = len.saturating_sub(1);
        let by = |delta: isize| view.scroll.saturating_add_signed(delta).min(last);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => view.scroll = by(1),
            KeyCode::Up | KeyCode::Char('k') => view.scroll = by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => view.scroll = by(page),
            KeyCode::PageUp | KeyCode::Char('b') => view.scroll = by(-page),
            KeyCode::Char('d') => view.scroll = by(page / 2),
            KeyCode::Char('u') => view.scroll = by(-page / 2),
            KeyCode::Home | KeyCode::Char('g') => view.scroll = 0,
            KeyCode::End | KeyCode::Char('G') => view.scroll = last,
            KeyCode::Char('n') => {
                if let Some(&start) = starts.iter().find(|&&s| s > view.scroll) {
                    view.scroll = start;
                }
            }
            KeyCode::Char('p') => {
                if let Some(&start) = starts.iter().rev().find(|&&s| s < view.scroll) {
                    view.scroll = start;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => view.hscroll = view.hscroll.saturating_add(8),
            KeyCode::Left | KeyCode::Char('h') => view.hscroll = view.hscroll.saturating_sub(8),
            _ => {}
        }
        Outcome::Continue
    }

    /// Marked trees, else the one under the cursor, minus any the job cannot
    /// take. The reason for the first exclusion goes to the status line.
    fn targets(&mut self, kind: Kind) -> Vec<Tree> {
        let chosen: Vec<Tree> = if self.marked.is_empty() {
            self.current().cloned().into_iter().collect()
        } else {
            self.trees
                .iter()
                .filter(|t| self.marked.contains(&t.path))
                .cloned()
                .collect()
        };
        let mut skipped = None;
        let targets = chosen
            .into_iter()
            .filter(|t| {
                let reason = if self.is_here(t) {
                    Some(format!(
                        "tree {} is where you started treetop; cd out first",
                        t.name
                    ))
                } else if self.jobs.get(&t.path).is_some_and(|s| s.is_active()) {
                    Some(format!("tree {} already has an action queued", t.name))
                } else if kind == Kind::Return && !t.is_held() {
                    Some(format!("tree {} is already available", t.name))
                } else {
                    None
                };
                if reason.is_some() && skipped.is_none() {
                    skipped = reason.clone();
                }
                reason.is_none()
            })
            .collect();
        self.message = skipped;
        targets
    }

    fn queue(&mut self, kind: Kind, trees: Vec<Tree>) -> Outcome {
        let jobs = trees
            .into_iter()
            .map(|tree| {
                self.jobs.insert(tree.path.clone(), JobState::Queued(kind));
                Job { kind, tree }
            })
            .collect();
        Outcome::Queue(jobs)
    }

    fn quit(&mut self) -> Outcome {
        match self.active_jobs() {
            0 => Outcome::Quit,
            n => {
                self.is_quitting = true;
                self.message = Some(format!(
                    "waiting for {n} action(s) to finish; Ctrl-C quits now"
                ));
                Outcome::Continue
            }
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if self.view.is_some() {
            return self.handle_view_key(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Outcome::Quit,
                KeyCode::Char('r') => {
                    self.diffs.clear();
                    Outcome::Refresh
                }
                _ => Outcome::Continue,
            };
        }
        if let Some(review) = self.reviews.front_mut() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(review) = self.reviews.pop_front() {
                        return self.queue(Kind::Destroy, vec![review.tree]);
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    if let Some(review) = self.reviews.pop_front() {
                        self.message = Some(format!("kept tree {}", review.tree.name));
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    review.scroll = review.scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => review.scroll = review.scroll.saturating_sub(1),
                KeyCode::PageDown => review.scroll = review.scroll.saturating_add(10),
                KeyCode::PageUp => review.scroll = review.scroll.saturating_sub(10),
                _ => {}
            }
            return Outcome::Continue;
        }
        if let Some(pending) = self.pending.take() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.marked.clear();
                    self.queue(Kind::Return, pending.trees)
                }
                _ => Outcome::Continue,
            };
        }
        if self.is_filtering {
            match key.code {
                KeyCode::Enter => self.is_filtering = false,
                KeyCode::Esc => {
                    self.is_filtering = false;
                    self.filter.clear();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.selected = 0;
            return Outcome::Continue;
        }
        if !self.is_quitting {
            self.message = None;
        }
        match key.code {
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc => return self.quit(),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Home | KeyCode::Char('g') => self.move_by(isize::MIN),
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX),
            KeyCode::Char('/') => self.is_filtering = true,
            KeyCode::Char(' ') => {
                if let Some(path) = self.current().map(|t| t.path.clone()) {
                    if !self.marked.remove(&path) {
                        self.marked.insert(path);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Char('u') => self.marked.clear(),
            KeyCode::Char('L') => {
                self.pane = if self.pane == Pane::Log {
                    Pane::Info
                } else {
                    Pane::Log
                };
                self.log_scroll = 0;
            }
            KeyCode::Tab => self.open_diff(),
            KeyCode::PageUp if self.pane == Pane::Log => self.log_scroll += 10,
            KeyCode::PageDown if self.pane == Pane::Log => {
                self.log_scroll = self.log_scroll.saturating_sub(10);
            }
            KeyCode::Enter => {
                if let Some(tree) = self.current() {
                    return Outcome::Enter(tree.clone());
                }
            }
            KeyCode::Char('r') => {
                let trees = self.targets(Kind::Return);
                if !trees.is_empty() {
                    self.pending = Some(Pending { trees });
                }
            }
            KeyCode::Char('D') => {
                let trees = self.targets(Kind::Destroy);
                if !trees.is_empty() {
                    self.marked.clear();
                    return self.queue(Kind::Preview, trees);
                }
            }
            _ => {}
        }
        Outcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;
    use ratatui::crossterm::event::KeyEvent;

    fn app() -> App {
        let trees = parse(
            r#"[
          {"name":"4","status":"leased","branch":"feature/end-1-a","lease_holder":"anthony","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"7","status":"available","branch":"","lease_holder":"","path":"/p/7/app","leased_at":null,"processes":[]},
          {"name":"8","status":"leased","branch":"feature/end-2-b","lease_holder":"claude:x","path":"/p/8/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap();
        let mut app = App::new(None);
        app.set_trees(trees);
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Outcome {
        app.handle_key(KeyEvent::from(code))
    }

    fn names(jobs: &[Job]) -> Vec<(Kind, &str)> {
        jobs.iter()
            .map(|j| (j.kind, j.tree.name.as_str()))
            .collect()
    }

    fn queued(outcome: Outcome) -> Vec<Job> {
        match outcome {
            Outcome::Queue(jobs) => jobs,
            other => panic!("expected jobs, got {other:?}"),
        }
    }

    #[test]
    fn return_confirms_marked_trees_then_queues_them() {
        let mut app = app();
        for _ in 0..3 {
            press(&mut app, KeyCode::Char(' '));
        }
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.message.as_deref(), Some("tree 7 is already available"));
        let jobs = queued(press(&mut app, KeyCode::Char('y')));
        assert_eq!(names(&jobs), [(Kind::Return, "4"), (Kind::Return, "8")]);
        assert_eq!(app.active_jobs(), 2);
        assert!(app.marked.is_empty());
    }

    #[test]
    fn any_key_but_y_cancels_a_return() {
        let mut app = app();
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), Outcome::Continue);
        assert!(app.pending.is_none());
        assert_eq!(app.active_jobs(), 0);
    }

    #[test]
    fn destroy_previews_first_and_only_y_on_the_review_destroys() {
        let mut app = app();
        press(&mut app, KeyCode::Down);
        let jobs = queued(press(&mut app, KeyCode::Char('D')));
        assert_eq!(names(&jobs), [(Kind::Preview, "7")]);
        let preview = jobs.into_iter().next().unwrap();
        app.job_finished(preview, true, "would remove /p/7/app".into());
        assert_eq!(
            app.reviews.front().unwrap().preview,
            "would remove /p/7/app"
        );
        let jobs = queued(press(&mut app, KeyCode::Char('y')));
        assert_eq!(names(&jobs), [(Kind::Destroy, "7")]);
        assert!(app.reviews.is_empty());
    }

    #[test]
    fn n_on_a_review_keeps_the_tree() {
        let mut app = app();
        let preview = queued(press(&mut app, KeyCode::Char('D'))).remove(0);
        app.job_finished(preview, true, "preview".into());
        assert_eq!(press(&mut app, KeyCode::Char('n')), Outcome::Continue);
        assert_eq!(app.message.as_deref(), Some("kept tree 4"));
        assert_eq!(app.active_jobs(), 0);
    }

    #[test]
    fn a_failed_job_marks_its_row_and_says_why() {
        let mut app = app();
        press(&mut app, KeyCode::Char('r'));
        let job = queued(press(&mut app, KeyCode::Char('y'))).remove(0);
        app.job_started(&job);
        app.job_finished(job, false, "working\nError: worktree is locked\n".into());
        assert_eq!(
            app.jobs[&PathBuf::from("/p/4/app")],
            JobState::Failed(Kind::Return)
        );
        assert!(
            app.message
                .unwrap()
                .contains("return of tree 4 failed: Error: worktree is locked")
        );
        assert!(!app.log[0].is_ok);
    }

    #[test]
    fn done_shows_until_a_listing_started_after_it_lands() {
        let mut app = app();
        press(&mut app, KeyCode::Char('r'));
        let job = queued(press(&mut app, KeyCode::Char('y'))).remove(0);
        app.start_listing();
        app.job_finished(job, true, String::new());
        let path = PathBuf::from("/p/4/app");
        app.set_trees(app.trees.clone());
        assert!(matches!(
            app.jobs.get(&path),
            Some(JobState::Done(Kind::Return, _))
        ));
        app.start_listing();
        app.set_trees(app.trees.clone());
        assert!(!app.jobs.contains_key(&path));
    }

    #[test]
    fn a_tree_with_a_queued_job_is_not_targeted_again() {
        let mut app = app();
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('y'));
        press(&mut app, KeyCode::Char('D'));
        assert!(app.reviews.is_empty() && app.pending.is_none());
        assert_eq!(
            app.message.as_deref(),
            Some("tree 4 already has an action queued")
        );
    }

    #[test]
    fn quitting_waits_for_running_jobs() {
        let mut app = app();
        press(&mut app, KeyCode::Char('r'));
        let job = queued(press(&mut app, KeyCode::Char('y'))).remove(0);
        assert_eq!(press(&mut app, KeyCode::Char('q')), Outcome::Continue);
        assert!(app.is_quitting);
        app.job_finished(job, true, String::new());
        assert_eq!(app.active_jobs(), 0);
        assert_eq!(press(&mut app, KeyCode::Char('q')), Outcome::Quit);
    }

    #[test]
    fn never_targets_the_tree_treetop_started_in() {
        let mut app = app();
        app.here = Some(PathBuf::from("/p/4/app/ui"));
        assert_eq!(press(&mut app, KeyCode::Char('D')), Outcome::Continue);
        assert!(app.message.unwrap().contains("cd out first"));
    }

    #[test]
    fn filter_matches_branch_and_holder() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for c in "claude".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        let visible: Vec<&str> = app.visible().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(visible, ["8"]);
        assert!(matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "8"));
    }

    #[test]
    fn ctrl_r_refreshes_and_never_returns() {
        let mut app = app();
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_key(ctrl_r), Outcome::Refresh);
        assert!(app.pending.is_none());
    }

    #[test]
    fn a_new_listing_keeps_probed_state_until_the_next_probe() {
        let mut app = app();
        let path = PathBuf::from("/p/8/app");
        let node = Process {
            pid: 1,
            name: "node".into(),
        };
        app.apply_processes(&HashMap::from([(path.clone(), vec![node])]));
        let work = Work {
            changed: 2,
            unpushed: 1,
        };
        app.apply_work(&HashMap::from([(path, Some(work))]));
        let listing = app
            .trees
            .iter()
            .cloned()
            .map(|t| Tree {
                processes: vec![],
                work: None,
                ..t
            })
            .collect();
        app.set_trees(listing);
        let tree = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert_eq!(
            (tree.processes.len(), tree.work.map(|w| w.changed)),
            (1, Some(2))
        );
        assert!(!app.is_cached && app.listed_at.is_some());
    }

    fn with_work(app: &mut App, changed: usize) {
        let work: HashMap<PathBuf, Option<Work>> = app
            .trees
            .iter()
            .filter(|t| t.is_held())
            .map(|t| {
                (
                    t.path.clone(),
                    Some(Work {
                        changed,
                        unpushed: 0,
                    }),
                )
            })
            .collect();
        app.apply_work(&work);
    }

    /// Two files of 7 rows each, one spacer apart: files start at rows 0 and 8.
    fn two_files() -> Result<Layout, String> {
        let file =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n a\n-b\n+c\n d\n e\n f\n";
        let files = [crate::diff::parse(file), crate::diff::parse(file)].concat();
        Ok(crate::diff::layout(crate::diff::BranchDiff {
            base: "origin/main".into(),
            files,
        }))
    }

    #[test]
    fn tab_opens_the_diff_of_a_held_tree_and_reads_it_once() {
        let mut app = app();
        with_work(&mut app, 1);
        assert_eq!(app.wanted_diff(), None, "no view, nothing to read");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.view.as_ref().unwrap().path, PathBuf::from("/p/4/app"));
        assert_eq!(app.wanted_diff(), Some(PathBuf::from("/p/4/app")));
        assert_eq!(app.wanted_diff(), None, "in flight");
        app.diff_loaded(PathBuf::from("/p/4/app"), two_files());
        assert_eq!(app.wanted_diff(), None, "loaded");
        press(&mut app, KeyCode::Esc);
        assert!(app.view.is_none());
        with_work(&mut app, 2);
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.wanted_diff(),
            Some(PathBuf::from("/p/4/app")),
            "stale on reopen"
        );
    }

    #[test]
    fn tab_on_an_available_tree_says_there_is_nothing_to_diff() {
        let mut app = app();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Tab);
        assert!(app.view.is_none());
        assert_eq!(
            app.message.as_deref(),
            Some("tree 7 is available: nothing to diff")
        );
    }

    #[test]
    fn the_view_scrolls_jumps_between_files_and_stays_in_bounds() {
        let mut app = app();
        app.screen_height = 10;
        press(&mut app, KeyCode::Tab);
        app.diff_loaded(PathBuf::from("/p/4/app"), two_files());
        let scroll = |app: &App| app.view.as_ref().unwrap().scroll;
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(scroll(&app), 8);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(scroll(&app), 8, "no file after the last");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(scroll(&app), 0);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(scroll(&app), 6, "a page is the height less the bars");
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(scroll(&app), 14);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(scroll(&app), 14, "never past the last row");
        press(&mut app, KeyCode::Char('q'));
        assert!(app.view.is_none(), "q closes the view, not treetop");
    }

    #[test]
    fn refresh_keeps_the_cursor_on_the_same_tree() {
        let mut app = app();
        press(&mut app, KeyCode::End);
        let mut trees = app.trees.clone();
        trees.remove(1);
        app.set_trees(trees);
        assert_eq!(app.current().unwrap().name, "8");
    }
}
