//! What the screen shows and what each key does, kept free of terminal I/O so
//! the rules about what may be returned or destroyed are testable.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::agents::{Agent, Status};
use crate::coordinator;
use crate::diff::Layout;
use crate::dispatch::{Edit, Prompt, Purpose, Recipient, Target};
use crate::jobs::{Job, Kind, Launch};
use crate::pool::{self, Process, Tree, Work};
use crate::reports::{Report, State};

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

/// The state a report goes by: its own, except that an agent sitting at a
/// question dialog or permission prompt needs you, though its report still
/// says working.
pub fn report_state(report: &Report, trees: &[Tree]) -> State {
    let is_waiting = trees.iter().flat_map(|t| &t.agents).any(|a| {
        a.job.as_deref() == Some(report.job.as_str()) && a.status == Some(Status::Waiting)
    });
    if report.state == State::Working && is_waiting {
        State::Blocked
    } else {
        report.state
    }
}

/// What the pane under the table shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Pane {
    #[default]
    Info,
    Log,
}

/// The diff of one tree in its popup, and where it is scrolled to.
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
    /// Open `http://localhost:<port>`, served from tree `tree`, in the browser.
    Browse {
        port: u16,
        tree: String,
    },
    /// Open `command`, agent `agent`'s chat in tree `tree`, in a popup over
    /// treetop until Ctrl+Q or until it exits.
    Chat {
        command: Vec<String>,
        agent: String,
        tree: String,
    },
    /// Type `text` into each agent's chat and submit it.
    Send {
        to: Vec<Recipient>,
        text: String,
    },
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
    /// Only held trees with no agent are shown, for reviewing what was left
    /// behind.
    pub is_orphans_only: bool,
    /// Agents have been read at least once; until then a tree with none is
    /// unknown, not orphaned.
    pub is_agents_read: bool,
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
    /// The terminal's height, for paging the diff popup.
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
    /// treetop runs inside tmux, so an agent in a pane can be switched to.
    pub is_in_tmux: bool,
    /// An agent `n` started, whose chat opens once it shows up.
    awaiting_chat: Option<Awaiting>,
    /// The chat of an agent just started, ready to open: `claude --bg`
    /// prints the id `claude attach` takes, so there is no waiting for the
    /// agent to be seen in its tree.
    started_chat: Option<Outcome>,
    /// A chat on its way: its popup stands in from the moment the box is
    /// sent until the chat itself opens in the same frame.
    pub opening: Option<Opening>,
    /// Every background agent's latest report, for the bell and for finding
    /// a stopped coordinator.
    pub reports: Vec<Report>,
    /// Each job's state at the last read, so a change can be announced;
    /// None until the first read, which announces nothing.
    last_states: Option<HashMap<String, State>>,
    /// The compose box being typed in, if it is open.
    pub prompt: Option<Prompt>,
    /// Stand-ins handed out for trees not leased yet.
    new_trees: usize,
}

/// How long a started agent may take to show up before treetop stops
/// waiting to open its chat.
const AWAIT_CHAT: Duration = Duration::from_secs(30);

/// The chat a popup is standing in for while its agent starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opening {
    pub agent: String,
    /// The tree's name, `new` until a new tree is leased.
    pub tree: String,
}

/// The id `claude --bg` prints for `claude attach`, from the line it
/// suggests running: `claude attach cbcfd8c8    open in this terminal`.
fn attach_id(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let id = line
            .split("claude attach ")
            .nth(1)?
            .split_whitespace()
            .next()?;
        id.chars().all(|c| c.is_ascii_alphanumeric()).then_some(id)
    })
}

/// A tree an agent was just started in, and the agents already there, so
/// the new one is the one whose chat opens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Awaiting {
    path: PathBuf,
    known: Vec<u32>,
    since: Instant,
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
            .filter(|t| !self.is_orphans_only || t.is_orphan())
            .filter(|t| {
                needle.is_empty()
                    || [Some(&t.name), t.branch.as_ref(), t.holder.as_ref()]
                        .into_iter()
                        .flatten()
                        .chain(t.agents.iter().map(|a| &a.name))
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

    /// Takes the agents read on the git clock. A tree absent from the map has
    /// no agent working in it.
    pub fn apply_agents(&mut self, agents: &HashMap<PathBuf, Vec<Agent>>) {
        for tree in &mut self.trees {
            tree.agents = agents.get(&tree.path).cloned().unwrap_or_default();
        }
        self.is_agents_read = true;
    }

    /// Keeps the cursor on the same tree, keeps each surviving tree's probed
    /// processes, agents and git state until the next probe replaces them, and drops
    /// marks and settled job states on trees that have left the pool.
    fn replace_trees(&mut self, mut trees: Vec<Tree>) {
        let cursor = self.current().map(|t| t.path.clone());
        for tree in &mut trees {
            if let Some(old) = self.trees.iter().find(|old| old.path == tree.path) {
                tree.processes = old.processes.clone();
                tree.agents = old.agents.clone();
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
            .insert(job.key.clone(), JobState::Running(job.kind));
    }

    /// Records a finished job. A successful preview becomes a review; a failure
    /// stays on its row and its last line goes to the status line.
    pub fn job_finished(&mut self, job: Job, is_ok: bool, output: String) {
        // A dispatch to a new tree ran under a stand-in until it had one.
        let path = job.tree.path.clone();
        if job.key != path {
            self.jobs.remove(&job.key);
        }
        let task = job
            .launch
            .as_ref()
            .map(|l| l.task.trim())
            .filter(|t| !t.is_empty());
        let is_coordinator = job.launch.as_ref().is_some_and(|l| l.is_coordinator);
        if !is_ok {
            if job.kind == Kind::Start {
                self.opening = None;
            }
            self.jobs.insert(path, JobState::Failed(job.kind));
            self.message = Some(format!(
                "{} of tree {} failed: {}  (L shows the log)",
                job.kind.noun(),
                job.tree.name,
                last_line(&output)
            ));
        } else if job.kind == Kind::Start && task.is_some() {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            self.message = Some(if is_coordinator {
                "the coordinator started on your message; n, Enter opens its chat".to_string()
            } else if job.tree.is_main {
                "dispatched to the main checkout; c opens its chat".to_string()
            } else {
                format!("dispatched to tree {}; c opens its chat", job.tree.name)
            });
        } else if job.kind == Kind::Start {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            // Its chat opens only while its popup still stands in for it; Esc
            // on the popup means it is opened later, with `c`.
            let known = self
                .trees
                .iter()
                .find(|t| t.path == path)
                .map(|t| t.agents.iter().map(|a| a.pid).collect())
                .unwrap_or_default();
            match (self.opening.as_mut(), attach_id(&output)) {
                (None, _) => {
                    self.message = Some(if is_coordinator {
                        "the coordinator started; c on main opens its chat".to_string()
                    } else {
                        format!("agent started in tree {}; c opens its chat", job.tree.name)
                    });
                }
                (Some(opening), Some(id)) => {
                    self.started_chat = Some(Outcome::Chat {
                        command: vec!["claude".into(), "attach".into(), id.to_string()],
                        agent: opening.agent.clone(),
                        tree: job.tree.name.clone(),
                    });
                }
                (Some(opening), None) => {
                    opening.tree = job.tree.name.clone();
                    self.awaiting_chat = Some(Awaiting {
                        path,
                        known,
                        since: Instant::now(),
                    });
                }
            }
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
        // The popup's height, less the file header pinned at its top and the
        // key bar at its bottom.
        let body = crate::popup::inner(Rect::new(0, 0, 80, self.screen_height)).height;
        let page = isize::try_from(body.saturating_sub(2)).map_or(1, |p| p.max(1));
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
                } else if t.is_main {
                    Some("the main checkout is never returned or destroyed".to_string())
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
                Job::new(kind, tree)
            })
            .collect();
        Outcome::Queue(jobs)
    }

    /// Opens the chat of the first agent in the tree under the cursor that
    /// offers one. An agent in a tmux pane cannot be attached from here, so
    /// inside tmux `c` switches to its pane instead, as Enter does.
    fn chat(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        if let Some((agent, command)) = tree
            .agents
            .iter()
            .find_map(|a| Some((a, a.attach.clone()?)))
        {
            return Outcome::Chat {
                command,
                agent: agent.name.clone(),
                tree: tree.name.clone(),
            };
        }
        if self.is_in_tmux && tree.agents.iter().any(|a| !a.is_background) {
            return Outcome::Enter(tree.clone());
        }
        self.message = Some(match tree.agents.first() {
            None => format!("tree {} has no agent to open", tree.name),
            Some(agent) => format!("{} has no chat to open in place", agent.name),
        });
        Outcome::Continue
    }

    /// The chat of an agent `n` started: at once when its start printed the
    /// id to attach to, else once it has shown up in its tree with a chat to
    /// open. Gives up after AWAIT_CHAT, saying so.
    pub fn ready_chat(&mut self) -> Option<Outcome> {
        if let Some(chat) = self.started_chat.take() {
            self.opening = None;
            self.awaiting_chat = None;
            return Some(chat);
        }
        let awaiting = self.awaiting_chat.as_ref()?;
        let tree = self.trees.iter().find(|t| t.path == awaiting.path)?;
        let started = tree
            .agents
            .iter()
            .filter(|a| !awaiting.known.contains(&a.pid))
            .find_map(|a| Some((a.name.clone(), a.attach.clone()?)));
        if let Some((agent, command)) = started {
            let tree = tree.name.clone();
            self.awaiting_chat = None;
            self.opening = None;
            return Some(Outcome::Chat {
                command,
                agent,
                tree,
            });
        }
        if awaiting.since.elapsed() >= AWAIT_CHAT {
            self.message = Some(format!(
                "the agent started in tree {} has not shown up; c opens it once it does",
                tree.name
            ));
            self.awaiting_chat = None;
            self.opening = None;
        }
        None
    }

    /// The chat of the next tree down the list, as filtered, that has one,
    /// after tree `from`, wrapping past the end. The cursor moves to that
    /// tree, so leaving the chat lands on it. None when no other tree has a
    /// chat to open.
    pub fn next_chat(&mut self, from: &str) -> Option<Outcome> {
        let visible = self.visible();
        let start = visible.iter().position(|t| t.name == from)?;
        let (index, agent, command, tree) = (1..visible.len())
            .map(|step| (start + step) % visible.len())
            .find_map(|index| {
                let tree = visible[index];
                let agent = tree.agents.iter().find(|a| a.attach.is_some())?;
                Some((
                    index,
                    agent.name.clone(),
                    agent.attach.clone()?,
                    tree.name.clone(),
                ))
            })?;
        self.selected = index;
        Some(Outcome::Chat {
            command,
            agent,
            tree,
        })
    }

    /// Takes the latest reports. Returns what to announce: each job that has
    /// come to need you or finished since the last read, what needs you
    /// first.
    pub fn apply_reports(&mut self, reports: Vec<Report>) -> Vec<String> {
        self.reports = reports;
        let mut reports: Vec<(&Report, State)> = self
            .reports
            .iter()
            .map(|r| (r, report_state(r, &self.trees)))
            .collect();
        reports.sort_by_key(|(r, state)| (*state != State::Blocked, std::cmp::Reverse(r.updated)));
        let news = match &self.last_states {
            None => Vec::new(),
            Some(last) => reports
                .iter()
                .filter(|(report, state)| {
                    let was = last.get(&report.job).copied();
                    match state {
                        State::Blocked => was != Some(State::Blocked),
                        State::Done => was.is_some_and(|w| w != State::Done),
                        State::Working | State::Stopped => false,
                    }
                })
                .map(|(report, state)| {
                    let text = if *state != report.state {
                        "waiting at a prompt in its chat"
                    } else {
                        &report.text
                    };
                    format!("{} {}: {text}", report.name, state.label())
                })
                .collect(),
        };
        self.last_states = Some(
            reports
                .iter()
                .map(|(r, state)| (r.job.clone(), *state))
                .collect(),
        );
        news
    }

    /// Reports waiting on you, for the summary.
    pub fn needing_you(&self) -> usize {
        self.reports
            .iter()
            .filter(|r| report_state(r, &self.trees) == State::Blocked)
            .count()
    }

    /// The coordinator to talk to: the running one, else the latest stopped
    /// one, which `claude attach` starts again with its conversation kept.
    /// None when there is neither, and a new one has to be started.
    pub fn coordinator(&self) -> Option<Recipient> {
        let running = self.trees.iter().find_map(|tree| {
            let agent = tree.agents.iter().find(|a| a.name == coordinator::NAME)?;
            Some(Recipient {
                agent: agent.name.clone(),
                tree: tree.name.clone(),
                command: agent.attach.clone()?,
            })
        });
        running.or_else(|| {
            let report = self
                .reports
                .iter()
                .filter(|r| r.name == coordinator::NAME)
                .max_by_key(|r| r.updated)?;
            Some(Recipient {
                agent: report.name.clone(),
                tree: "main".into(),
                command: vec!["claude".into(), "attach".into(), report.job.clone()],
            })
        })
    }

    /// Opens the compose box. The coordinator comes first, since it is who
    /// you talk to about the agents; then a new tree, since most new work
    /// wants its own; then the tree under the cursor; then in place.
    fn open_dispatch(&mut self) {
        let mut targets = vec![Target::Coordinator, Target::NewTree];
        if let Some(tree) = self.current().filter(|t| !t.is_main) {
            targets.push(Target::Tree(tree.clone()));
        }
        if let Some(main) = self.trees.iter().find(|t| t.is_main) {
            targets.push(Target::InPlace(main.clone()));
        }
        self.prompt = Some(Prompt::dispatch(targets));
    }

    /// Opens the compose box to write to the agents in the marked trees, else in the
    /// tree under the cursor. Agents with no chat to type into are left out,
    /// and the status line says how many.
    fn open_message(&mut self) {
        let trees: Vec<&Tree> = if self.marked.is_empty() {
            self.current().into_iter().collect()
        } else {
            self.trees
                .iter()
                .filter(|t| self.marked.contains(&t.path))
                .collect()
        };
        let agents: Vec<(&Tree, &Agent)> = trees
            .iter()
            .flat_map(|t| t.agents.iter().map(move |a| (*t, a)))
            .collect();
        let to: Vec<Recipient> = agents
            .iter()
            .filter_map(|(tree, agent)| {
                Some(Recipient {
                    agent: agent.name.clone(),
                    tree: tree.name.clone(),
                    command: agent.attach.clone()?,
                })
            })
            .collect();
        let skipped = agents.len() - to.len();
        if to.is_empty() {
            self.message = Some(match skipped {
                0 => "no agent there to write to".to_string(),
                n => format!("{n} agent(s) there, none with a chat to type into"),
            });
            return;
        }
        if skipped > 0 {
            self.message = Some(format!("{skipped} agent(s) left out: no chat to type into"));
        }
        self.prompt = Some(Prompt::message(to));
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(prompt) = &mut self.prompt {
            prompt.paste(text);
        }
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(prompt) = &mut self.prompt else {
            return Outcome::Continue;
        };
        match prompt.edit(key) {
            Edit::Continue => Outcome::Continue,
            Edit::Cancel => {
                self.prompt = None;
                Outcome::Continue
            }
            Edit::Submit => match self.prompt.take() {
                Some(prompt) => self.submit(prompt),
                None => Outcome::Continue,
            },
        }
    }

    fn submit(&mut self, prompt: Prompt) -> Outcome {
        let text = prompt.text.trim().to_string();
        match prompt.purpose {
            Purpose::Message { to } => {
                if text.is_empty() {
                    self.message = Some("nothing to send".into());
                    return Outcome::Continue;
                }
                self.message = Some(format!("sending to {} agent(s)...", to.len()));
                Outcome::Send { to, text }
            }
            Purpose::Dispatch { mut targets, at } => {
                if at >= targets.len() {
                    return Outcome::Continue;
                }
                match targets.swap_remove(at) {
                    Target::Coordinator => self.converse(text),
                    target => self.dispatch(target, text),
                }
            }
        }
    }

    /// Talks to the coordinator the way `m` talks to an agent: `text` goes
    /// to it behind the screen and treetop stays on the list. With no text,
    /// its chat opens instead, which is where its answers are read. With no
    /// coordinator, one starts in the main checkout with `text` as its
    /// first prompt.
    fn converse(&mut self, text: String) -> Outcome {
        if let Some(to) = self.coordinator() {
            if text.trim().is_empty() {
                return Outcome::Chat {
                    command: to.command,
                    agent: to.agent,
                    tree: to.tree,
                };
            }
            self.message = Some("sending to the coordinator...".into());
            return Outcome::Send { to: vec![to], text };
        }
        let Some(main) = self.trees.iter().find(|t| t.is_main).cloned() else {
            self.message = Some("the main checkout is not listed yet".into());
            return Outcome::Continue;
        };
        self.start(
            main,
            Launch {
                task: text,
                name: Some(coordinator::NAME.into()),
                is_new_tree: false,
                is_coordinator: true,
            },
        )
    }

    /// Queues a Start for `task` at `target`. An empty task starts the agent
    /// idle and opens its chat, as `n` always has.
    fn dispatch(&mut self, target: Target, task: String) -> Outcome {
        let launch = Launch::agent(task, target == Target::NewTree);
        let tree = match target {
            Target::Coordinator => return self.converse(launch.task),
            Target::Tree(tree) | Target::InPlace(tree) => tree,
            Target::NewTree => {
                self.new_trees += 1;
                pool::stand_in(PathBuf::from(format!("(new tree {})", self.new_trees)))
            }
        };
        if launch.is_new_tree {
            self.message = Some("leasing a new tree for the agent...".into());
        }
        self.start(tree, launch)
    }

    /// Queues a Start of `launch` in `tree`, unless the tree is busy with
    /// another action.
    fn start(&mut self, tree: Tree, launch: Launch) -> Outcome {
        if self.jobs.get(&tree.path).is_some_and(|s| s.is_active()) {
            self.message = Some(format!("tree {} already has an action queued", tree.name));
            return Outcome::Continue;
        }
        if launch.task.trim().is_empty() {
            self.opening = Some(Opening {
                agent: launch.name.clone().unwrap_or_else(|| "new agent".into()),
                tree: tree.name.clone(),
            });
        }
        let job = Job {
            launch: Some(launch),
            ..Job::new(Kind::Start, tree)
        };
        self.jobs
            .insert(job.key.clone(), JobState::Queued(Kind::Start));
        Outcome::Queue(vec![job])
    }

    /// Opens the lowest port the tree under the cursor serves.
    fn browse(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        match tree.ports().first() {
            Some(&port) => Outcome::Browse {
                port,
                tree: tree.name.clone(),
            },
            None => {
                self.message = Some(format!("tree {} serves no port", tree.name));
                Outcome::Continue
            }
        }
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
        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }
        if let Some(opening) = self.opening.clone() {
            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if is_ctrl && key.code == KeyCode::Char('c') {
                return Outcome::Quit;
            }
            let is_back = key.code == KeyCode::Esc || crate::chat::is_exit(key);
            if is_back {
                self.message = Some(format!(
                    "{} keeps starting; c opens its chat once it is up",
                    opening.agent
                ));
                self.opening = None;
                self.awaiting_chat = None;
                self.started_chat = None;
            }
            return Outcome::Continue;
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
            KeyCode::Char('m') => self.open_message(),
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
            KeyCode::Char('a') => {
                self.is_orphans_only = !self.is_orphans_only;
                self.selected = 0;
            }
            KeyCode::Char('o') => return self.browse(),
            KeyCode::Char('c') => return self.chat(),
            KeyCode::Char('n') => self.open_dispatch(),
            KeyCode::Char('L') => {
                self.pane = if self.pane == Pane::Log {
                    Pane::Info
                } else {
                    Pane::Log
                };
                self.log_scroll = 0;
            }
            KeyCode::Char('d') => self.open_diff(),
            KeyCode::PageUp if self.pane == Pane::Log => self.log_scroll += 10,
            KeyCode::PageDown if self.pane == Pane::Log => {
                self.log_scroll = self.log_scroll.saturating_sub(10);
            }
            KeyCode::Char('t') => {
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
    use std::time::SystemTime;

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
        assert!(matches!(press(&mut app, KeyCode::Char('t')), Outcome::Enter(t) if t.name == "8"));
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
            ports: vec![5223],
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
    fn d_opens_the_diff_of_a_held_tree_and_reads_it_once() {
        let mut app = app();
        with_work(&mut app, 1);
        assert_eq!(app.wanted_diff(), None, "no view, nothing to read");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.view.as_ref().unwrap().path, PathBuf::from("/p/4/app"));
        assert_eq!(app.wanted_diff(), Some(PathBuf::from("/p/4/app")));
        assert_eq!(app.wanted_diff(), None, "in flight");
        app.diff_loaded(PathBuf::from("/p/4/app"), two_files());
        assert_eq!(app.wanted_diff(), None, "loaded");
        press(&mut app, KeyCode::Esc);
        assert!(app.view.is_none());
        with_work(&mut app, 2);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            app.wanted_diff(),
            Some(PathBuf::from("/p/4/app")),
            "stale on reopen"
        );
    }

    #[test]
    fn d_on_an_available_tree_says_there_is_nothing_to_diff() {
        let mut app = app();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('d'));
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
        press(&mut app, KeyCode::Char('d'));
        app.diff_loaded(PathBuf::from("/p/4/app"), two_files());
        let scroll = |app: &App| app.view.as_ref().unwrap().scroll;
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(scroll(&app), 8);
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(scroll(&app), 8, "no file after the last");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(scroll(&app), 0);
        press(&mut app, KeyCode::Char(' '));
        assert_eq!(
            scroll(&app),
            4,
            "a page is the popup's inside less the pinned header and the key bar"
        );
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(scroll(&app), 14);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(scroll(&app), 14, "never past the last row");
        press(&mut app, KeyCode::Char('q'));
        assert!(app.view.is_none(), "q closes the view, not treetop");
    }

    fn agent(name: &str) -> Agent {
        Agent {
            name: name.into(),
            pid: 1,
            status: None,
            source: crate::agents::Source::Process,
            is_background: false,
            attach: None,
            job: None,
        }
    }

    #[test]
    fn c_opens_an_attachable_chat_else_the_pane_inside_tmux_else_says_why() {
        let mut app = app();
        let attachable = Agent {
            is_background: true,
            attach: Some(vec!["claude".into(), "attach".into(), "e6b79280".into()]),
            ..agent("fix-login")
        };
        app.apply_agents(&HashMap::from([
            (PathBuf::from("/p/4/app"), vec![agent("codex"), attachable]),
            (PathBuf::from("/p/8/app"), vec![agent("codex")]),
        ]));
        assert_eq!(
            press(&mut app, KeyCode::Char('c')),
            Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "e6b79280".into()],
                agent: "fix-login".into(),
                tree: "4".into()
            }
        );
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Char('c')), Outcome::Continue);
        assert_eq!(app.message.as_deref(), Some("tree 7 has no agent to open"));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Char('c')), Outcome::Continue);
        assert_eq!(
            app.message.as_deref(),
            Some("codex has no chat to open in place")
        );
        app.is_in_tmux = true;
        assert!(matches!(press(&mut app, KeyCode::Char('c')), Outcome::Enter(t) if t.name == "8"));
    }

    #[test]
    fn filter_matches_agent_names_and_agents_survive_a_listing() {
        let mut app = app();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![agent("codex")],
        )]));
        app.set_trees(app.trees.clone());
        press(&mut app, KeyCode::Char('/'));
        for c in "codex".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let visible: Vec<&str> = app.visible().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(visible, ["4"]);
    }

    #[test]
    fn a_shows_only_held_trees_with_no_agent() {
        let mut app = app();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![agent("codex")],
        )]));
        press(&mut app, KeyCode::Char('a'));
        let visible: Vec<&str> = app.visible().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(visible, ["8"]);
        press(&mut app, KeyCode::Char('a'));
        assert_eq!(app.visible().len(), 3);
    }

    #[test]
    fn o_opens_the_lowest_port_or_says_there_is_none() {
        let mut app = app();
        let process = Process {
            pid: 1,
            name: "node".into(),
            ports: vec![5223, 9229],
        };
        app.apply_processes(&HashMap::from([(PathBuf::from("/p/4/app"), vec![process])]));
        assert_eq!(
            press(&mut app, KeyCode::Char('o')),
            Outcome::Browse {
                port: 5223,
                tree: "4".into()
            }
        );
        press(&mut app, KeyCode::Down);
        assert_eq!(press(&mut app, KeyCode::Char('o')), Outcome::Continue);
        assert_eq!(app.message.as_deref(), Some("tree 7 serves no port"));
    }

    #[test]
    fn enter_does_nothing_and_t_goes_to_the_tree() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert!(matches!(press(&mut app, KeyCode::Char('t')), Outcome::Enter(t) if t.name == "4"));
    }

    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn n_to_the_tree_under_the_cursor_with_no_task_starts_it_idle_and_opens_its_chat() {
        let mut app = app();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![agent("old")],
        )]));
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Tab);
        assert_eq!(
            app.prompt.as_ref().unwrap().target().unwrap().label(),
            "tree 4"
        );
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!((job.kind, job.tree.name.as_str()), (Kind::Start, "4"));
        assert_eq!(job.launch.as_ref().unwrap().task, "");
        assert_eq!(
            app.opening.as_ref().unwrap().tree,
            "4",
            "its popup stands in"
        );
        app.job_finished(job, true, "printed no id".into());
        assert_eq!(app.ready_chat(), None, "not shown up yet");
        let started = Agent {
            pid: 99,
            is_background: true,
            attach: Some(vec!["claude".into(), "attach".into(), "1234abcd".into()]),
            ..agent("new-session")
        };
        let old_attachable = Agent {
            attach: Some(vec!["claude".into(), "attach".into(), "old".into()]),
            ..agent("old")
        };
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![old_attachable, started],
        )]));
        assert_eq!(
            app.ready_chat(),
            Some(Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "1234abcd".into()],
                agent: "new-session".into(),
                tree: "4".into()
            })
        );
        assert_eq!(app.ready_chat(), None, "opened once");
        assert!(app.opening.is_none());
    }

    /// What `claude --bg` prints.
    const BACKGROUNDED: &str = "backgrounded · cbcfd8c8 · treetop-coordinator (working)
  claude agents             list sessions
  claude attach cbcfd8c8    open in this terminal
  claude stop cbcfd8c8      stop this session
";

    #[test]
    fn a_started_chat_opens_from_the_id_claude_prints_without_waiting_to_see_it() {
        assert_eq!(attach_id(BACKGROUNDED), Some("cbcfd8c8"));
        assert_eq!(attach_id("started"), None);
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('n'));
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(app.opening.as_ref().unwrap().agent, coordinator::NAME);
        app.job_finished(job, true, BACKGROUNDED.into());
        assert_eq!(
            app.ready_chat(),
            Some(Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "cbcfd8c8".into()],
                agent: coordinator::NAME.into(),
                tree: "main".into()
            })
        );
        assert!(app.opening.is_none() && app.ready_chat().is_none());
    }

    #[test]
    fn esc_on_a_chat_on_its_way_stops_waiting_and_a_failed_start_ends_it() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('n'));
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(app.opening.is_some());
        assert_eq!(
            press(&mut app, KeyCode::Char('q')),
            Outcome::Continue,
            "keys wait"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.opening.is_none());
        app.job_finished(job, true, BACKGROUNDED.into());
        assert_eq!(app.ready_chat(), None, "dismissed, it opens only with c");
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("c on main opens its chat")
        );
        press(&mut app, KeyCode::Char('n'));
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        app.job_finished(job, false, "claude: not found".into());
        assert!(app.opening.is_none());
    }

    /// The fixture's pool with the main checkout above it, as listings come.
    fn app_with_main() -> App {
        let mut app = app();
        let mut trees = app.trees.clone();
        trees.insert(0, crate::pool::main_tree(std::path::Path::new("/repo")));
        app.set_trees(trees);
        app
    }

    #[test]
    fn a_task_dispatched_in_place_starts_in_the_main_checkout_and_opens_no_chat() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('n'));
        type_in(&mut app, "say hi");
        press(&mut app, KeyCode::BackTab);
        assert_eq!(
            app.prompt.as_ref().unwrap().target().unwrap().label(),
            "in place"
        );
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(job.tree.is_main);
        let launch = job.launch.clone().unwrap();
        assert_eq!(
            (launch.task.as_str(), launch.is_new_tree),
            ("say hi", false)
        );
        assert_eq!(launch.name.as_deref(), Some("say hi"));
        app.job_finished(job, true, String::new());
        assert_eq!(app.ready_chat(), None);
        assert!(app.message.unwrap().contains("the main checkout"));
    }

    #[test]
    fn a_new_tree_dispatch_runs_under_a_stand_in_until_its_tree_is_leased() {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        type_in(&mut app, "fix login");
        press(&mut app, KeyCode::Tab);
        let mut job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(job.launch.as_ref().unwrap().is_new_tree);
        assert_eq!(job.key, PathBuf::from("(new tree 1)"));
        assert_eq!(app.active_jobs(), 1);
        job.tree.path = PathBuf::from("/p/12/app");
        job.tree.name = "12".into();
        app.job_finished(job, true, String::new());
        assert_eq!(app.active_jobs(), 0, "the stand-in is cleared");
        assert!(app.message.unwrap().contains("tree 12"));
    }

    #[test]
    fn esc_closes_the_bar_and_queues_nothing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        type_in(&mut app, "q");
        assert_eq!(press(&mut app, KeyCode::Esc), Outcome::Continue);
        assert!(app.prompt.is_none() && app.active_jobs() == 0);
        assert!(!app.is_quitting, "q went into the bar, not to quit");
    }

    #[test]
    fn m_writes_to_the_marked_trees_agents_and_leaves_out_those_with_no_chat() {
        let mut app = app();
        app.apply_agents(&HashMap::from([
            (
                PathBuf::from("/p/4/app"),
                vec![attachable("a"), agent("pane-only")],
            ),
            (PathBuf::from("/p/8/app"), vec![attachable("b")]),
        ]));
        for _ in 0..3 {
            press(&mut app, KeyCode::Char(' '));
        }
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(
            app.message.as_deref(),
            Some("1 agent(s) left out: no chat to type into")
        );
        type_in(&mut app, "rebase on development");
        match press(&mut app, KeyCode::Enter) {
            Outcome::Send { to, text } => {
                assert_eq!(text, "rebase on development");
                let names: Vec<(&str, &str)> = to
                    .iter()
                    .map(|r| (r.agent.as_str(), r.tree.as_str()))
                    .collect();
                assert_eq!(names, [("a", "4"), ("b", "8")]);
            }
            other => panic!("expected a send, got {other:?}"),
        }
    }

    #[test]
    fn the_main_checkout_is_never_returned_or_destroyed() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Home);
        assert_eq!(app.current().unwrap().name, "main");
        press(&mut app, KeyCode::Char('r'));
        assert!(app.pending.is_none());
        assert_eq!(
            app.message.as_deref(),
            Some("the main checkout is never returned or destroyed")
        );
        press(&mut app, KeyCode::Char('a'));
        assert!(
            app.visible().iter().all(|t| !t.is_main),
            "an agentless main is no orphan"
        );
    }

    fn report(job: &str, state: State, dir: &str, age: u64) -> Report {
        Report {
            job: job.into(),
            name: format!("job {job}"),
            state,
            text: format!("{job} says"),
            dir: Some(PathBuf::from(dir)),
            updated: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 - age),
        }
    }

    #[test]
    fn an_agent_waiting_at_a_prompt_needs_you_though_its_report_says_working() {
        let mut app = app();
        let report = report("cde62519", State::Working, "/p/8/app", 1);
        app.apply_reports(vec![report.clone()]);
        let at_prompt = Agent {
            status: Some(Status::Waiting),
            job: Some("cde62519".into()),
            ..attachable("cde62519")
        };
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/8/app"),
            vec![at_prompt],
        )]));
        let news = app.apply_reports(vec![report]);
        assert_eq!(
            news,
            ["job cde62519 needs you: waiting at a prompt in its chat"]
        );
        assert_eq!(app.needing_you(), 1);
    }

    #[test]
    fn news_puts_what_needs_you_first() {
        let mut app = app();
        app.apply_reports(vec![
            report("done", State::Working, "/p/4/app", 1),
            report("ask", State::Working, "/p/8/app", 2),
        ]);
        let news = app.apply_reports(vec![
            report("done", State::Done, "/p/4/app", 1),
            report("ask", State::Blocked, "/p/8/app", 2),
        ]);
        assert_eq!(
            news,
            ["job ask needs you: ask says", "job done done: done says"]
        );
    }

    #[test]
    fn n_talks_to_the_coordinator_first_and_starts_it_in_the_main_checkout() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(
            app.prompt.as_ref().unwrap().target(),
            Some(&Target::Coordinator)
        );
        type_in(&mut app, "what needs me?");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(job.tree.is_main);
        let launch = job.launch.clone().unwrap();
        assert!(launch.is_coordinator);
        assert_eq!(launch.task, "what needs me?");
        assert_eq!(launch.name.as_deref(), Some(coordinator::NAME));
        assert!(app.opening.is_none(), "given a message, nothing opens");
        app.job_finished(
            job,
            true,
            "  claude attach c00rd1na    open in this terminal".into(),
        );
        assert_eq!(
            app.ready_chat(),
            None,
            "treetop stays on the list, as m does"
        );
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("started on your message")
        );
    }

    #[test]
    fn a_running_coordinator_is_talked_to_rather_than_started_again() {
        let mut app = app_with_main();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/repo"),
            vec![attachable(coordinator::NAME)],
        )]));
        press(&mut app, KeyCode::Char('n'));
        type_in(&mut app, "status?");
        match press(&mut app, KeyCode::Enter) {
            Outcome::Send { to, text } => {
                assert_eq!(text, "status?");
                assert_eq!(to[0].command, ["claude", "attach", coordinator::NAME]);
                assert_eq!(to[0].tree, "main");
            }
            other => panic!("expected the message sent as m sends, got {other:?}"),
        }
        assert!(app.opening.is_none(), "no chat opens");
        press(&mut app, KeyCode::Char('n'));
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::Chat { agent, .. } if agent == coordinator::NAME),
            "nothing typed opens its chat"
        );
    }

    #[test]
    fn a_stopped_coordinator_is_attached_again_by_its_latest_job() {
        let mut app = app_with_main();
        let stopped = |job: &str, age| Report {
            name: coordinator::NAME.into(),
            ..report(job, State::Stopped, "/repo", age)
        };
        app.apply_reports(vec![stopped("old", 50), stopped("latest", 5)]);
        press(&mut app, KeyCode::Char('n'));
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Outcome::Chat { command, .. } if command == ["claude", "attach", "latest"]
        ));
    }

    #[test]
    fn news_is_announced_once_and_never_on_the_first_read() {
        let mut app = app();
        assert!(
            app.apply_reports(vec![report("a", State::Blocked, "/p/4/app", 1)])
                .is_empty()
        );
        let reports = vec![
            report("a", State::Blocked, "/p/4/app", 1),
            report("b", State::Working, "/p/8/app", 1),
        ];
        assert!(
            app.apply_reports(reports).is_empty(),
            "still blocked is no news"
        );
        let news = app.apply_reports(vec![
            report("a", State::Working, "/p/4/app", 1),
            report("b", State::Done, "/p/8/app", 1),
        ]);
        assert_eq!(news, ["job b done: b says"]);
        let news = app.apply_reports(vec![report("a", State::Blocked, "/p/4/app", 1)]);
        assert_eq!(news, ["job a needs you: a says"]);
    }

    fn attachable(name: &str) -> Agent {
        Agent {
            is_background: true,
            attach: Some(vec!["claude".into(), "attach".into(), name.into()]),
            ..agent(name)
        }
    }

    #[test]
    fn next_chat_goes_down_the_list_skipping_trees_without_one_and_wraps() {
        let mut app = app();
        app.apply_agents(&HashMap::from([
            (PathBuf::from("/p/4/app"), vec![attachable("a")]),
            (PathBuf::from("/p/7/app"), vec![agent("pane-only")]),
            (PathBuf::from("/p/8/app"), vec![attachable("b")]),
        ]));
        let chat_in = |outcome: Option<Outcome>| match outcome {
            Some(Outcome::Chat { agent, tree, .. }) => (agent, tree),
            other => panic!("expected a chat, got {other:?}"),
        };
        assert_eq!(chat_in(app.next_chat("4")), ("b".into(), "8".into()));
        assert_eq!(app.current().unwrap().name, "8", "the cursor follows");
        assert_eq!(
            chat_in(app.next_chat("8")),
            ("a".into(), "4".into()),
            "wraps"
        );
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![attachable("a")],
        )]));
        assert_eq!(app.next_chat("4"), None, "no other tree has a chat");
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
