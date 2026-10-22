//! What the screen shows and what each key does, kept free of terminal I/O so
//! the rules about what may be returned or destroyed are testable.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, UNIX_EPOCH};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::agents::{Agent, Status};
use crate::assistant;
use crate::diff::Layout;
use crate::dispatch::{Edit, Prompt, Purpose, Recipient, Target};
use crate::jobs::{Job, Kind, Launch, Message, Pick, ShipAction, ShipCall};
use crate::keeper::Kept;
use crate::plugin::Plugin;
use crate::pool::{self, Process, Tree, Work};
use crate::reports::{Report, State};
use crate::scratch::Scratch;
use crate::ship::{Answer, Stage};
use crate::sidecar::Sidecar;

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

/// One agent's latest report, with the tree it works in and its agent while
/// it runs.
#[derive(Debug, Clone, Copy)]
pub struct Item<'a> {
    pub report: &'a Report,
    pub tree: Option<&'a Tree>,
    pub agent: Option<&'a Agent>,
    /// The state the list goes by; see `report_state`.
    pub state: State,
}

impl<'a> Item<'a> {
    fn new(report: &'a Report, trees: &'a [Tree]) -> Self {
        Item {
            report,
            tree: crate::reports::tree_of(report, trees),
            agent: trees
                .iter()
                .flat_map(|t| &t.agents)
                .find(|a| a.job.as_deref() == Some(report.job.as_str())),
            state: report_state(report, trees),
        }
    }

    /// Waiting at a prompt in its chat, which only its chat can answer: a
    /// line typed in would land in the dialog, not reach it as a message.
    pub fn is_at_prompt(&self) -> bool {
        self.state != self.report.state
    }

    /// What it has to say, as the list shows it.
    pub fn text(&self) -> &str {
        if self.is_at_prompt() {
            "waiting at a prompt in its chat: Enter opens it"
        } else {
            &self.report.text
        }
    }

    /// Where it sorts in the news: what needs you, then what finished, then
    /// what moves.
    fn rank(&self) -> u8 {
        match self.state {
            State::Blocked => 0,
            State::Done | State::Stopped => 1,
            State::Working => 2,
        }
    }
}

/// A workspace the picker offers: a repository treetop has been opened in,
/// with its held trees from its last listing, None when it has none cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub checkout: PathBuf,
    pub name: String,
    pub held: Option<usize>,
}

/// The workspace picker `w` opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub choices: Vec<Choice>,
    pub selected: usize,
}

/// The ports of a tree serving several, for `o` to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMenu {
    pub tree: String,
    /// Each port and the process listening on it, lowest first.
    pub ports: Vec<(u16, String)>,
    pub selected: usize,
}

/// The scratch chats `C` lists, under a first line that starts a new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchMenu {
    pub chats: Vec<Scratch>,
    /// 0 is the line that starts a new chat; the chats follow.
    pub selected: usize,
    /// `x` was pressed once on the chat under the cursor, and again deletes
    /// it.
    pub is_deleting: bool,
    /// Why the last new chat did not start, shown under the chats.
    pub error: Option<String>,
}

/// What a scratch chat's popup names as its tree, since it has none.
pub const SCRATCH: &str = "scratch";

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

/// A ship run of a tree's branch, waiting on y/n, since it pushes and opens
/// a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shipping {
    pub tree: Tree,
    /// The ship plugin that runs it.
    pub plugin: String,
    /// Where its intent comes from, as the dialog says it.
    pub intent: String,
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
    /// Read the workspaces and open the picker on them.
    Workspaces,
    /// Read the scratch chats and open their list.
    Scratches,
    /// Start a scratch chat and open it once it runs.
    NewScratch,
    /// Delete this scratch chat.
    RemoveScratch(Scratch),
    /// Show the pool of the repository checked out here instead.
    Switch(PathBuf),
    /// Open `http://localhost:<port>`, served from tree `tree`, in the browser.
    Browse {
        port: u16,
        tree: String,
    },
    /// Open `url`, such as a tree's PR, in the browser.
    OpenUrl(String),
    /// Write `record` as the treetop.json of the tree at `path`.
    Record {
        path: PathBuf,
        record: Sidecar,
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
    /// The `?` overlay of every key is open.
    pub is_help: bool,
    /// The Issues screen is shown in place of the trees; the program
    /// around the app owns it and gives it the keys.
    pub is_issues: bool,
    /// What `:` is asked about: what was on screen when it opened.
    ask_context: Option<String>,
    /// The issue to open on the Issues screen as it comes on, by id, taken
    /// once by the program around the app.
    pub open_issue: Option<String>,
    /// The tree an issue is being chosen for on the Issues screen, by path
    /// and name.
    attaching: Option<(PathBuf, String)>,
    /// What the Issues screen should ask for as it comes on: the issue the
    /// named tree works on, taken once by the program around the app.
    pub choose_issue_for: Option<String>,
    /// Agents have been read at least once; until then a tree with none is
    /// unknown, not orphaned.
    pub is_agents_read: bool,
    pub marked: BTreeSet<PathBuf>,
    pub selected: usize,
    pub pending: Option<Pending>,
    pub reviews: VecDeque<Review>,
    pub jobs: HashMap<PathBuf, JobState>,
    /// Every finished job's output, newest last; the detail pane shows the
    /// latest of the tree under the cursor.
    pub log: Vec<LogEntry>,
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
    /// The main checkout of the pool on screen, the workspace.
    pub checkout: PathBuf,
    /// The workspace picker, while it is open.
    pub picker: Option<Picker>,
    /// The ports `o` offers, while its menu is open.
    pub port_menu: Option<PortMenu>,
    /// The scratch chats `C` lists, while the list is open.
    pub scratch_menu: Option<ScratchMenu>,
    /// Where scratch chats run, whose reports are nobody's news.
    pub scratch_root: Option<PathBuf>,
    /// The pool's base branch, which trees are cut from unless one is
    /// chosen.
    pub base: Option<String>,
    /// Every background agent's latest report, for the AGENT column's mark,
    /// asks, the bell and finding a stopped assistant.
    pub reports: Vec<Report>,
    /// Each job's state at the last read, so a change can be announced;
    /// None until the first read, which announces nothing.
    last_states: Option<HashMap<String, State>>,
    /// The compose box being typed in, if it is open.
    pub prompt: Option<Prompt>,
    /// Stand-ins handed out for trees not leased yet.
    new_trees: usize,
    /// Whose branches new agents start on, as `branch::user` gives it.
    pub user: String,
    /// The repository's ship plugins, in its order; with none, the PR
    /// column is left out.
    pub ship_plugins: Vec<Plugin>,
    /// A ship run waiting on y/n in its dialog.
    pub shipping: Option<Shipping>,
    /// Why the ship plugins could not say where the PRs stand, at the last
    /// ask.
    pub ship_error: Option<String>,
    /// Each tree's stage at the last ask, so a change can be announced; None
    /// until the first ask, which announces nothing.
    last_stages: Option<HashMap<PathBuf, Stage>>,
    /// A PR's failure read for its agent, ready to send.
    failure: Option<Outcome>,
    /// The agents whose question or prompt has been on screen, its chat
    /// opened for it or open while it asked, by their chat's command. One
    /// leaves once it stops asking, so its next ask opens again.
    asked: BTreeSet<String>,
    /// Issues a merge has left with nothing blocking them, by id, until
    /// a tree takes one up.
    pub ready: BTreeSet<String>,
    /// Trees whose merge has been followed up, so it happens once.
    landed: BTreeSet<PathBuf>,
    /// Follow-ups of merges, to hand to the worker.
    landings: Vec<Job>,
    /// The Parked group is open, showing the trees set aside.
    pub is_parked_open: bool,
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

/// A tree an agent was just started in, and the agents already there, so
/// the new one is the one whose chat opens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Awaiting {
    path: PathBuf,
    known: Vec<Option<u32>>,
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

/// Why git would refuse the branch the compose box would send: the renamed
/// one, or the one a new agent starts on.
fn branch_problem(prompt: &Prompt) -> Option<String> {
    let name = match &prompt.purpose {
        Purpose::Rename { .. } => prompt.text.trim().to_string(),
        _ => prompt.branch_name()?,
    };
    let problem = crate::branch::problem(&name)?;
    Some(format!("branch {name}: {problem}"))
}

/// The trees, as the Issues screen asks them who is working on an issue.
impl issues::Host for App {
    fn work_on(&self, id: &str) -> Option<issues::Work> {
        let Some(tree) = self.tree_of_ticket(id) else {
            return self.ready.contains(id).then(|| issues::Work {
                tree: String::new(),
                agent: None,
                needs_you: false,
                is_ready: true,
            });
        };
        Some(issues::Work {
            tree: tree.name.clone(),
            agent: tree
                .agents
                .first()
                .map(|a| a.status.map_or("running", Status::label).to_string()),
            needs_you: self.needs_you(tree),
            is_ready: false,
        })
    }
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

    /// The background agents held trees keep awake. The main checkout is
    /// never one, and a tree with an action queued, running or just done
    /// keeps none, so a tree being returned does not wake what its return
    /// just stopped.
    pub fn kept(&self) -> Vec<Kept> {
        self.trees
            .iter()
            .filter(|t| t.is_held() && !t.is_main)
            .filter(|t| {
                self.jobs
                    .get(&t.path)
                    .is_none_or(|state| matches!(state, JobState::Failed(_)))
            })
            .flat_map(|t| &t.agents)
            .filter(|a| a.is_background)
            .filter_map(|a| {
                Some(Kept {
                    plugin: a.plugin.clone()?,
                    id: a.job.clone()?,
                    is_cold: a.status == Some(Status::Cold),
                })
            })
            .collect()
    }

    /// The tree is set aside, under Parked.
    pub fn is_parked(tree: &Tree) -> bool {
        tree.sidecar.as_ref().is_some_and(|s| s.parked)
    }

    /// The bases the held trees were cut from, the pool's first and the
    /// rest by name. With more than one, the list groups its trees by them.
    pub fn bases_in_use(&self) -> Vec<String> {
        let mut bases: Vec<String> = self
            .trees
            .iter()
            .filter(|t| t.is_held() && !t.is_main)
            .filter_map(|t| self.base_of(t))
            .collect();
        bases.sort_by_key(|b| (Some(b) != self.base.as_ref(), b.clone()));
        bases.dedup();
        bases
    }

    /// The base `tree` is listed under among `bases`: the one it was cut
    /// from, or for the main checkout its own branch where that is one of
    /// them, else the pool's. None for a tree held by no one.
    fn group_of(&self, tree: &Tree, bases: &[String]) -> Option<usize> {
        if !tree.is_held() {
            return None;
        }
        let base = if tree.is_main {
            tree.branch
                .clone()
                .filter(|b| bases.contains(b))
                .or_else(|| self.base.clone())
        } else {
            self.base_of(tree)
        };
        base.and_then(|b| bases.iter().position(|x| *x == b))
            .or(Some(0))
    }

    /// The trees the list shows, filtered: those waiting on you first, then
    /// the main checkout, then the rest, each part with the most recently
    /// active agent first, and trees with no activity in pool order last.
    /// Parked trees come last, and only while Parked is open or a filter
    /// looks for them; one that waits on you comes first all the same.
    /// Where the held trees come from more than one base, each base's trees
    /// are listed together, in that order within, the pool's base first,
    /// then the free trees, then Parked.
    pub fn visible(&self) -> Vec<&Tree> {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<&Tree> = self
            .trees
            .iter()
            .filter(|t| {
                needle.is_empty()
                    || [
                        Some(&t.name),
                        t.branch.as_ref(),
                        t.holder.as_ref(),
                        t.sidecar.as_ref().and_then(|s| s.ticket.as_ref()),
                        t.ship.as_ref().map(|s| &s.label),
                    ]
                    .into_iter()
                    .flatten()
                    .chain(t.agents.iter().map(|a| &a.name))
                    .any(|field| field.to_lowercase().contains(&needle))
            })
            .collect();
        visible.retain(|t| {
            self.is_parked_open || !needle.is_empty() || !Self::is_parked(t) || self.needs_you(t)
        });
        let bases = self.bases_in_use();
        let is_grouped = bases.len() > 1;
        visible.sort_by_cached_key(|t| {
            let needs_you = self.needs_you(t);
            let group = match self.group_of(t, &bases) {
                _ if !is_grouped => 0,
                _ if Self::is_parked(t) && !needs_you => bases.len() + 1,
                Some(at) => at,
                None => bases.len(),
            };
            (
                group,
                !needs_you,
                !needs_you && Self::is_parked(t),
                !t.is_main,
                std::cmp::Reverse(self.active_at(t)),
            )
        });
        visible
    }

    /// Where Parked starts among the visible trees, and how many trees it
    /// holds, shown or not; None when no tree is parked.
    pub fn parked_rows(&self, visible: &[&Tree]) -> Option<(usize, usize)> {
        let count = self
            .trees
            .iter()
            .filter(|t| Self::is_parked(t) && !self.needs_you(t))
            .count();
        let start = visible
            .iter()
            .position(|t| Self::is_parked(t) && !self.needs_you(t))
            .unwrap_or(visible.len());
        (count > 0).then_some((start, count))
    }

    /// `z`: sets the tree under the cursor aside under Parked, or brings it
    /// back. Only a leased tree can be parked.
    fn toggle_park(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        if tree.is_main || !tree.is_held() {
            self.message = Some(format!("tree {} is not leased: nothing to park", tree.name));
            return Outcome::Continue;
        }
        let path = tree.path.clone();
        let name = tree.name.clone();
        let mut record = tree
            .sidecar
            .as_deref()
            .cloned()
            .unwrap_or_else(Sidecar::leased_now);
        record.parked = !record.parked;
        self.message = Some(if record.parked {
            format!("parked tree {name}; Z opens Parked")
        } else {
            format!("tree {name} is back from Parked")
        });
        if let Some(tree) = self.trees.iter_mut().find(|t| t.path == path) {
            tree.sidecar = Some(Box::new(record.clone()));
        }
        let last = self.visible().len().saturating_sub(1);
        self.selected = self.selected.min(last);
        Outcome::Record { path, record }
    }

    /// How many of the visible trees at the top wait on you, which the list
    /// sets apart from the rest; none while it is grouped by base, where
    /// they come first in their group instead.
    pub fn waiting_rows(&self, visible: &[&Tree]) -> usize {
        if self.bases_in_use().len() > 1 {
            return 0;
        }
        visible.iter().take_while(|t| self.needs_you(t)).count()
    }

    /// Where each base's trees start among the visible ones, the base, and
    /// how many it lists, while the list is grouped by base, then the free
    /// trees, under None; empty while it is not grouped.
    pub fn base_rows(&self, visible: &[&Tree]) -> Vec<(usize, Option<String>, usize)> {
        let bases = self.bases_in_use();
        if bases.len() < 2 {
            return Vec::new();
        }
        let mut rows: Vec<(usize, Option<String>, usize)> = Vec::new();
        for (at, tree) in visible.iter().enumerate() {
            if Self::is_parked(tree) && !self.needs_you(tree) {
                continue;
            }
            let group = self
                .group_of(tree, &bases)
                .map(|group| bases[group].clone());
            match rows.last_mut() {
                Some((_, base, count)) if *base == group => *count += 1,
                _ => rows.push((at, group, 1)),
            }
        }
        rows
    }

    /// When the tree's agents last worked: their latest report, or what
    /// their source says, in seconds since the epoch.
    pub fn active_at(&self, tree: &Tree) -> Option<u64> {
        let reported = self
            .says(tree)
            .and_then(|item| item.report.updated.duration_since(UNIX_EPOCH).ok())
            .map(|since| since.as_secs());
        tree.agents
            .iter()
            .filter_map(|a| a.active_at)
            .chain(reported)
            .max()
    }

    /// The tree's agents in the order a row and Enter go by: never an
    /// assistant, which is not the tree's; live ones first, waiting on you,
    /// then busy, then idle, the most recently active first; stopped ones
    /// after, since they work nowhere until opened again. The main checkout
    /// keeps none of its stopped ones: they are sessions once run in place,
    /// not its agent.
    pub fn agents_of(tree: &Tree) -> Vec<&Agent> {
        let rank = |agent: &Agent| match agent.status {
            Some(Status::Waiting) => 0,
            Some(Status::Busy) => 1,
            Some(Status::Idle) => 2,
            None => 3,
            Some(Status::Cold) => 4,
        };
        let mut agents: Vec<&Agent> = tree
            .agents
            .iter()
            .filter(|a| !assistant::is_assistant(&a.name))
            .filter(|a| !(tree.is_main && a.status == Some(Status::Cold)))
            .collect();
        agents.sort_by(|a, b| {
            rank(a)
                .cmp(&rank(b))
                .then(b.active_at.cmp(&a.active_at))
                .then(a.name.cmp(&b.name))
        });
        agents
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

    /// Takes the sidecars read on the git clock. A tree absent from the map
    /// has none.
    pub fn apply_sidecars(&mut self, mut sidecars: HashMap<PathBuf, Sidecar>) {
        for tree in &mut self.trees {
            tree.sidecar = sidecars.remove(&tree.path).map(Box::new);
        }
    }

    /// Takes the agents read on the git clock. A tree absent from the map has
    /// no agent working in it.
    pub fn apply_agents(&mut self, agents: &HashMap<PathBuf, Vec<Agent>>) {
        // Until agents are first read the list is unsorted, and the cursor
        // stays at the top rather than following a row down.
        let cursor = self
            .current()
            .filter(|_| self.is_agents_read)
            .map(|t| t.path.clone());
        for tree in &mut self.trees {
            tree.agents = agents.get(&tree.path).cloned().unwrap_or_default();
        }
        self.is_agents_read = true;
        self.reselect(cursor);
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
                tree.sidecar = old.sidecar.clone().filter(|_| tree.is_held());
                tree.work = old.work.filter(|_| tree.is_held());
                tree.ship = old.ship.clone().filter(|_| tree.is_held());
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
        // A merge's follow-up only says what it found: a failure shows in
        // the detail pane, since the work itself is done. Its stand-in goes,
        // or it would count as running forever and hold up a switch.
        if job.kind == Kind::Land {
            self.jobs.remove(&job.key);
            if let (true, Some(pick)) = (is_ok && !job.ready.is_empty(), &job.landing) {
                self.message = Some(format!(
                    "{} merged · {} ready",
                    pick.id,
                    job.ready.join(", ")
                ));
                self.ready.extend(job.ready.iter().cloned());
            }
            self.log.push(LogEntry {
                tree: job.tree.name,
                kind: job.kind,
                is_ok,
                output,
            });
            return;
        }
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
        let is_assistant = job.launch.as_ref().is_some_and(|l| l.is_assistant);
        if !is_ok {
            if job.kind == Kind::Start {
                self.opening = None;
            }
            self.jobs.insert(path, JobState::Failed(job.kind));
            self.message = Some(format!(
                "{} of tree {} failed: {}  (its output is in the pane below)",
                job.kind.noun(),
                job.tree.name,
                last_line(&output)
            ));
        } else if job.kind == Kind::Start && task.is_some() {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            self.message = Some(if is_assistant {
                "the assistant started on your message; c, Enter opens its chat".to_string()
            } else if job.tree.is_main {
                "dispatched to the main checkout; Enter on it opens its chat".to_string()
            } else {
                format!(
                    "dispatched to tree {}; Enter on it opens its chat",
                    job.tree.name
                )
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
            match (self.opening.as_mut(), job.attach.clone()) {
                (None, _) => {
                    self.message = Some(if is_assistant {
                        "the assistant started; c, Enter opens its chat".to_string()
                    } else {
                        format!(
                            "agent started in tree {}; Enter on it opens its chat",
                            job.tree.name
                        )
                    });
                }
                (Some(opening), Some(command)) => {
                    self.started_chat = Some(Outcome::Chat {
                        command,
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
        } else if job.kind == Kind::Ship {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            let plugin = job.shipping.as_ref().map_or("ship", |c| c.plugin.as_str());
            match job.attach.clone() {
                Some(command) => {
                    self.started_chat = Some(Outcome::Chat {
                        command,
                        agent: plugin.to_string(),
                        tree: job.tree.name.clone(),
                    });
                }
                None => {
                    self.message = Some(format!(
                        "{plugin} started a run on tree {}; s on it shows the run",
                        job.tree.name
                    ));
                }
            }
        } else if job.kind == Kind::Fix {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            self.send_failure(&path, &output);
        } else if job.kind == Kind::Preview {
            self.jobs.remove(&path);
            self.reviews.push_back(Review {
                tree: job.tree.clone(),
                preview: output.clone(),
                scroll: 0,
            });
        } else {
            if job.kind == Kind::Return {
                self.returned(&path);
            }
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

    /// A message to `to` as a job for its plugin, when it is not running: its
    /// plugin starts it again with the message, rather than a chat being
    /// opened to type it in. None for an agent that is running.
    pub fn send_cold(&mut self, to: &Recipient, text: &str) -> Option<Job> {
        let (true, Some(plugin), Some(id)) = (to.is_cold, &to.plugin, &to.job) else {
            return None;
        };
        let tree = self
            .trees
            .iter()
            .find(|t| t.name == to.tree)
            .cloned()
            .unwrap_or_else(|| pool::stand_in(PathBuf::from(format!("(job {id})"))));
        self.jobs
            .insert(tree.path.clone(), JobState::Queued(Kind::Send));
        Some(Job {
            message: Some(Message {
                plugin: plugin.clone(),
                id: id.clone(),
                text: text.to_string(),
            }),
            ..Job::new(Kind::Send, tree)
        })
    }

    /// Shows the tree at `path` back in the pool at once, as the next listing
    /// will, rather than leaving it held until that lands.
    fn returned(&mut self, path: &PathBuf) {
        if let Some(tree) = self.trees.iter_mut().find(|t| &t.path == path) {
            tree.status = "available".into();
            tree.holder = None;
            tree.branch = None;
            tree.agents.clear();
            tree.processes.clear();
            tree.work = None;
            tree.sidecar = None;
            tree.ship = None;
        }
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.visible().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// The diff the open view needs read, asked for once until it lands,
    /// and the base to read it against.
    pub fn wanted_diff(&mut self) -> Option<(PathBuf, Option<String>)> {
        let path = self.view.as_ref()?.path.clone();
        if self.diffs.contains_key(&path) || self.diff_requests.contains_key(&path) {
            return None;
        }
        let tree = self.trees.iter().find(|t| t.path == path);
        let work = tree.and_then(|t| t.work);
        let base = tree.and_then(|t| self.base_of(t));
        self.diff_requests.insert(path.clone(), work);
        Some((path, base))
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
        // The popup under its bar, less the file header pinned at its top
        // and the key bar at its bottom.
        let body = crate::popup::body(Rect::new(0, 0, 80, self.screen_height)).height;
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

    /// Opens the agent of the tree under the cursor: the chat of the first
    /// that offers one, in a popup, which starts a cold one again; else the
    /// tree itself, which inside tmux is the pane its agent runs in, or a
    /// window, and outside tmux a shell.
    fn open_agent(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        if let Some((agent, command)) = Self::agents_of(tree)
            .into_iter()
            .find_map(|a| Some((a, a.attach.clone()?)))
        {
            return Outcome::Chat {
                command,
                agent: agent.name.clone(),
                tree: tree.name.clone(),
            };
        }
        Outcome::Enter(tree.clone())
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
                "the agent started in tree {} has not shown up; Enter on it opens it once it does",
                tree.name
            ));
            self.awaiting_chat = None;
            self.opening = None;
        }
        None
    }

    /// The agents asking you something now, a question or a prompt but
    /// not a finished turn, with a chat to answer it in, in list order and
    /// then any a filter hides: each one's tree, name and chat command.
    fn asks(&self) -> Vec<(&Tree, &Agent, Vec<String>)> {
        let shown = self.visible();
        let hidden = self
            .trees
            .iter()
            .filter(|t| !shown.iter().any(|s| s.path == t.path))
            .collect::<Vec<_>>();
        shown
            .into_iter()
            .chain(hidden)
            .flat_map(|tree| tree.agents.iter().map(move |agent| (tree, agent)))
            .filter(|(_, agent)| !assistant::is_assistant(&agent.name))
            // A busy agent asks nothing, whatever its last report says: the
            // report is written as a turn ends, so it lags a turn under way.
            .filter(|(_, agent)| {
                agent.status == Some(Status::Waiting)
                    || agent.status != Some(Status::Busy)
                        && agent.job.as_deref().is_some_and(|job| {
                            self.reports
                                .iter()
                                .any(|r| r.job == job && r.state == State::Blocked)
                        })
            })
            .filter_map(|(tree, agent)| Some((tree, agent, agent.attach.clone()?)))
            .collect()
    }

    /// Nothing else is on screen or being typed: the list alone, where an
    /// ask's chat may open without taking over something under way.
    fn is_free(&self) -> bool {
        self.view.is_none()
            && self.prompt.is_none()
            && self.picker.is_none()
            && self.port_menu.is_none()
            && self.scratch_menu.is_none()
            && self.opening.is_none()
            && self.awaiting_chat.is_none()
            && self.started_chat.is_none()
            && !self.is_help
            && self.reviews.is_empty()
            && self.shipping.is_none()
            && self.pending.is_none()
            && !self.is_filtering
            && !self.is_issues
            && !self.is_quitting
    }

    /// The chat of the next agent asking you something that has not been
    /// on screen yet, once you are free on the list, so each ask comes to
    /// you in turn as you finish with the last. The cursor moves to its
    /// tree, so leaving the chat lands on it.
    pub fn next_ask(&mut self) -> Option<Outcome> {
        let asks: Vec<(PathBuf, String, String, Vec<String>)> = self
            .asks()
            .into_iter()
            .map(|(tree, agent, command)| {
                (
                    tree.path.clone(),
                    tree.name.clone(),
                    agent.name.clone(),
                    command,
                )
            })
            .collect();
        self.asked
            .retain(|key| asks.iter().any(|(.., command)| command.join(" ") == *key));
        if !self.is_free() {
            return None;
        }
        let (path, tree, agent, command) = asks
            .into_iter()
            .find(|(.., command)| !self.asked.contains(&command.join(" ")))?;
        self.asked.insert(command.join(" "));
        if !self.visible().iter().any(|t| t.path == path) {
            self.filter.clear();
        }
        self.reselect(Some(path));
        Some(Outcome::Chat {
            command,
            agent,
            tree,
        })
    }

    /// The chat running `command` is on screen, so whatever its agent asks
    /// now has been seen and does not open again when it closes.
    pub fn saw_chat(&mut self, command: &[String]) {
        if self.asks().iter().any(|(.., c)| c.as_slice() == command) {
            self.asked.insert(command.join(" "));
        }
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
                let agent = Self::agents_of(tree)
                    .into_iter()
                    .find(|a| a.attach.is_some())?;
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
    /// come to need you or finished since the last read, the assistant
    /// included, what needs you first.
    pub fn apply_reports(&mut self, reports: Vec<Report>) -> Vec<String> {
        let cursor = self
            .current()
            .filter(|_| self.last_states.is_some())
            .map(|t| t.path.clone());
        self.reports = reports;
        self.reselect(cursor);
        let items = self.items();
        let states: HashMap<String, State> = items
            .iter()
            .map(|i| (i.report.job.clone(), i.state))
            .collect();
        let news: Vec<String> = match &self.last_states {
            None => Vec::new(),
            Some(last) => items
                .iter()
                .filter(|item| {
                    let was = last.get(&item.report.job).copied();
                    match item.state {
                        State::Blocked => was != Some(State::Blocked),
                        State::Done => was.is_some_and(|w| w != State::Done),
                        State::Working | State::Stopped => false,
                    }
                })
                .map(|item| {
                    format!(
                        "{} {}: {}",
                        item.report.name,
                        item.state.label(),
                        item.text()
                    )
                })
                .collect(),
        };
        self.last_states = Some(states);
        news
    }

    /// Every report, what needs you first, then what finished, then what is
    /// still moving, newest first within each.
    fn items(&self) -> Vec<Item<'_>> {
        let mut items: Vec<Item> = self
            .reports
            .iter()
            .map(|report| Item::new(report, &self.trees))
            .collect();
        items.sort_by(|a, b| {
            a.rank()
                .cmp(&b.rank())
                .then(b.report.updated.cmp(&a.report.updated))
                .then(a.report.job.cmp(&b.report.job))
        });
        items
    }

    /// What the tree's agent last said: the newest report placed in it. The
    /// assistant's is left out, since it is talked to through `n`.
    pub fn says(&self, tree: &Tree) -> Option<Item<'_>> {
        self.reports
            .iter()
            .filter(|r| !assistant::is_assistant(&r.name))
            .filter(|r| {
                crate::reports::tree_of(r, &self.trees).is_some_and(|t| t.path == tree.path)
            })
            .max_by_key(|r| r.updated)
            .map(|r| Item::new(r, &self.trees))
    }

    /// The tree waits on you: its agent asked something or sits at a
    /// prompt, or its PR failed its checks, was asked for changes, or stopped
    /// at a gate.
    pub fn needs_you(&self, tree: &Tree) -> bool {
        tree.agents
            .iter()
            .any(|a| a.status == Some(Status::Waiting) && !assistant::is_assistant(&a.name))
            || self.says(tree).is_some_and(|i| i.state == State::Blocked)
            || tree.ship.as_ref().is_some_and(|s| s.stage.needs_you())
    }

    /// Trees whose PR has merged, for the summary.
    pub fn merged(&self) -> usize {
        self.trees
            .iter()
            .filter(|t| t.ship.as_ref().is_some_and(|s| s.stage == Stage::Merged))
            .count()
    }

    /// Takes where each tree's PR stands. A tree absent from the map has
    /// nothing to say. Returns what to announce: each tree whose PR has come
    /// to need you since the last ask.
    pub fn apply_ship(&mut self, answer: Result<Answer, String>) -> Vec<String> {
        let mut ships = match answer {
            Ok(answer) => {
                self.ship_error = Some(answer.failures.join("; ")).filter(|f| !f.is_empty());
                answer.ships
            }
            Err(why) => {
                self.ship_error = Some(why);
                return Vec::new();
            }
        };
        let cursor = self
            .current()
            .filter(|_| self.last_stages.is_some())
            .map(|t| t.path.clone());
        for tree in &mut self.trees {
            tree.ship = ships.remove(&tree.path).map(Box::new);
        }
        self.reselect(cursor);
        let stages: HashMap<PathBuf, Stage> = self
            .trees
            .iter()
            .filter_map(|t| Some((t.path.clone(), t.ship.as_ref()?.stage)))
            .collect();
        let news = match &self.last_stages {
            None => Vec::new(),
            Some(last) => self
                .trees
                .iter()
                .filter_map(|tree| {
                    let ship = tree.ship.as_ref()?;
                    let is_new = last.get(&tree.path) != Some(&ship.stage);
                    (ship.stage.needs_you() && is_new).then(|| {
                        format!(
                            "tree {}: {} on {}",
                            tree.name,
                            ship.label,
                            tree.branch.as_deref().unwrap_or(&ship.branch)
                        )
                    })
                })
                .collect(),
        };
        self.last_stages = Some(stages);
        self.follow_merges();
        news
    }

    /// Queues the follow-up of each merge seen for the first time, for a
    /// tree that records its ticket. It runs under a stand-in, so the tree
    /// stays free for a return.
    fn follow_merges(&mut self) {
        let merged: Vec<(Tree, String)> = self
            .trees
            .iter()
            .filter(|t| t.ship.as_ref().is_some_and(|s| s.stage == Stage::Merged))
            .filter(|t| !self.landed.contains(&t.path))
            .filter_map(|t| Some((t.clone(), t.sidecar.as_ref()?.ticket.clone()?)))
            .collect();
        for (tree, ticket) in merged {
            self.landed.insert(tree.path.clone());
            self.landings.push(Job {
                key: PathBuf::from(format!("(merge of {})", tree.path.display())),
                landing: Some(Pick {
                    id: ticket,
                    checkout: self.checkout.clone(),
                }),
                ..Job::new(Kind::Land, tree)
            });
        }
    }

    /// The merge follow-ups to hand to the worker, once.
    pub fn ready_landings(&mut self) -> Vec<Job> {
        std::mem::take(&mut self.landings)
    }

    /// Queues reading what failed on the PR of the tree under the cursor,
    /// to send to its agents once read.
    fn read_failure(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        let (name, path) = (tree.name.clone(), tree.path.clone());
        let failing = tree.ship.as_ref().filter(|s| s.stage.needs_you());
        self.message = if failing.is_none() {
            Some(format!("nothing has failed on tree {name}'s PR"))
        } else if !tree
            .agents
            .iter()
            .any(|a| Recipient::of(a, &tree.name).is_some())
        {
            Some(format!("no agent in tree {name} to send the failure to"))
        } else if self.jobs.get(&path).is_some_and(|s| s.is_active()) {
            Some(format!("tree {name} already has an action queued"))
        } else {
            let job = Job::new(Kind::Fix, tree.clone());
            self.jobs.insert(path, JobState::Queued(Kind::Fix));
            return Outcome::Queue(vec![job]);
        };
        Outcome::Continue
    }

    /// Sends `text`, what failed on the PR of the tree at `path`, to the
    /// agents working there, led by a line saying what it is.
    fn send_failure(&mut self, path: &PathBuf, text: &str) {
        let Some(tree) = self.trees.iter().find(|t| &t.path == path) else {
            return;
        };
        let Some(ship) = &tree.ship else { return };
        let to: Vec<Recipient> = tree
            .agents
            .iter()
            .filter_map(|a| Recipient::of(a, &tree.name))
            .collect();
        if to.is_empty() {
            self.message = Some(format!(
                "tree {}'s agent is gone; the failure was not sent",
                tree.name
            ));
            return;
        }
        self.message = Some(format!("sending {}'s failure to its agent...", ship.label));
        self.failure = Some(Outcome::Send {
            to,
            text: crate::ship::message(ship, text),
        });
    }

    /// The failure read for an agent, once, to send.
    pub fn ready_failure(&mut self) -> Option<Outcome> {
        self.failure.take()
    }

    /// The PR column shows: the repository has ship plugins.
    pub fn is_shipping(&self) -> bool {
        !self.ship_plugins.is_empty()
    }

    /// `s`: shows the ship run under way on the tree under the cursor, or
    /// asks to start one.
    fn ship(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        let name = tree.name.clone();
        if tree.is_main || !tree.is_held() || tree.branch.is_none() {
            self.message = Some(format!("tree {name} has no branch of an agent's to ship"));
            return Outcome::Continue;
        }
        if self.jobs.get(&tree.path).is_some_and(|s| s.is_active()) {
            self.message = Some(format!("tree {name} already has an action queued"));
            return Outcome::Continue;
        }
        let under_way = tree
            .ship
            .as_ref()
            .filter(|s| matches!(s.stage, Stage::Running | Stage::Gate))
            .and_then(|s| crate::ship::by_name(&self.ship_plugins, &s.plugin))
            .filter(|p| {
                p.description()
                    .is_ok_and(|d| d.ops.iter().any(|o| o == "attach"))
            });
        if let Some(plugin) = under_way {
            let job = Job {
                shipping: Some(ShipCall {
                    plugin: plugin.name().to_string(),
                    action: ShipAction::Attach,
                }),
                ..Job::new(Kind::Ship, tree.clone())
            };
            self.jobs
                .insert(tree.path.clone(), JobState::Queued(Kind::Ship));
            return Outcome::Queue(vec![job]);
        }
        let Some(plugin) = crate::ship::answering(&self.ship_plugins, "run") else {
            let names: Vec<&str> = self.ship_plugins.iter().map(Plugin::name).collect();
            self.message = Some(format!(
                "no ship plugin here starts a run ({}); name one such as no-mistakes under [repos] ship",
                names.join(", ")
            ));
            return Outcome::Continue;
        };
        let record = tree.sidecar.as_deref();
        let intent = match (
            record.and_then(|r| r.ticket.as_deref()),
            record.and_then(|r| r.task.as_deref()),
        ) {
            (Some(ticket), _) => format!("{ticket}'s title and text"),
            (None, Some(task)) => task.to_string(),
            (None, None) => {
                self.message = Some(format!(
                    "tree {name} has neither a ticket nor a task to give a run as its intent"
                ));
                return Outcome::Continue;
            }
        };
        self.shipping = Some(Shipping {
            tree: tree.clone(),
            plugin: plugin.name().to_string(),
            intent,
        });
        Outcome::Continue
    }

    /// Opens the PR of the tree under the cursor.
    fn open_pr(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        match tree.ship.as_ref().and_then(|s| s.url.clone()) {
            Some(url) => Outcome::OpenUrl(url),
            None => {
                self.message = Some(format!("tree {} has no PR to open", tree.name));
                Outcome::Continue
            }
        }
    }

    /// Trees waiting on you, for the summary.
    pub fn needing_you(&self) -> usize {
        self.trees.iter().filter(|t| self.needs_you(t)).count()
    }

    /// Agents outside the pool waiting on you, which have no row to show it.
    pub fn elsewhere(&self) -> usize {
        self.items()
            .iter()
            .filter(|i| i.tree.is_none() && i.state == State::Blocked)
            .filter(|i| !assistant::is_assistant(&i.report.name))
            .filter(|i| !self.is_scratch(i.report.dir.as_deref()))
            .count()
    }

    /// `dir` is a scratch chat's, which is no tree's and nobody's news.
    fn is_scratch(&self, dir: Option<&std::path::Path>) -> bool {
        dir.zip(self.scratch_root.as_deref())
            .is_some_and(|(dir, root)| dir.starts_with(root))
    }

    /// Opens the scratch chats' list on `chats`, the newest first, under a
    /// line that starts a new one.
    pub fn open_scratch_menu(&mut self, chats: Vec<Scratch>) {
        self.scratch_menu = Some(ScratchMenu {
            chats,
            selected: 0,
            is_deleting: false,
            error: None,
        });
    }

    /// What scratch chat `job` last said, for its line in the list.
    pub fn scratch_says(&self, job: &str) -> Option<String> {
        self.reports
            .iter()
            .find(|r| r.job == job)
            .map(|r| r.text.lines().next().unwrap_or("").to_string())
    }

    /// Keys while the scratch chats' list is open: Enter opens the chat
    /// under the cursor, or starts one on the first line; x twice deletes
    /// one; Esc closes the list.
    fn handle_scratch_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(menu) = self.scratch_menu.as_mut() else {
            return Outcome::Continue;
        };
        let is_deleting = std::mem::take(&mut menu.is_deleting);
        // The first line starts a new chat, so the chats are one down.
        let last = menu.chats.len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => menu.selected = (menu.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Enter if menu.selected == 0 => {
                self.scratch_menu = None;
                return Outcome::NewScratch;
            }
            KeyCode::Enter => {
                let chat = menu.chats[menu.selected - 1].clone();
                self.scratch_menu = None;
                return Outcome::Chat {
                    command: chat.attach(),
                    agent: chat.name,
                    tree: SCRATCH.into(),
                };
            }
            KeyCode::Char('x') if menu.selected > 0 && is_deleting => {
                let chat = menu.chats.remove(menu.selected - 1);
                menu.selected = menu.selected.min(menu.chats.len());
                self.message = Some(format!("deleting {}...", chat.name));
                return Outcome::RemoveScratch(chat);
            }
            KeyCode::Char('x') if menu.selected > 0 => menu.is_deleting = true,
            KeyCode::Esc | KeyCode::Char('q' | 'C') => self.scratch_menu = None,
            _ => {}
        }
        Outcome::Continue
    }

    /// Puts the cursor back on the tree at `path` after the list re-sorts.
    fn reselect(&mut self, path: Option<PathBuf>) {
        if let Some(at) = path.and_then(|p| self.visible().iter().position(|t| t.path == p)) {
            self.selected = at;
        }
    }

    /// What the assistant is doing, for its badge: None while there is
    /// none to show.
    pub fn assistant_state(&self) -> Option<&'static str> {
        let agent = self
            .trees
            .iter()
            .flat_map(|t| &t.agents)
            .find(|a| a.name == assistant::NAME)?;
        Some(match agent.status {
            Some(Status::Busy) => "answering",
            Some(Status::Waiting) => "asks you",
            Some(Status::Idle) => "idle",
            Some(Status::Cold) => "stopped",
            None => "running",
        })
    }

    /// The assistant to talk to: the running one, else the latest stopped
    /// one in this pool, which `claude attach` starts again with its
    /// conversation kept. None when there is neither, and a new one has to be
    /// started. Another repository's assistant is never taken.
    pub fn assistant(&self) -> Option<Recipient> {
        let running = self.trees.iter().find_map(|tree| {
            let agent = tree.agents.iter().find(|a| a.name == assistant::NAME)?;
            Recipient::of(agent, &tree.name)
        });
        running.or_else(|| {
            let report = self
                .reports
                .iter()
                .filter(|r| r.name == assistant::NAME)
                .filter(|r| {
                    r.dir
                        .as_deref()
                        .is_some_and(|dir| pool::holding(&self.trees, dir).is_some())
                })
                .max_by_key(|r| r.updated)?;
            Some(Recipient::of_job(
                &report.name,
                "main".into(),
                &report.job,
                false,
            ))
        })
    }

    /// Opens the compose box for a new agent in a fresh tree, its base line
    /// starting on the pool's base and suggesting the repository's branches.
    fn open_dispatch(&mut self) {
        let mut prompt = Prompt::dispatch(Target::NewTree, self.user.clone());
        prompt.default_base = self.base.clone();
        prompt.bases = crate::branch::bases(&self.checkout);
        self.prompt = Some(prompt);
    }

    /// The branch `tree` was cut from: the one recorded when treetop leased
    /// it from another, else the pool's base.
    pub fn base_of(&self, tree: &Tree) -> Option<String> {
        tree.sidecar
            .as_ref()
            .and_then(|s| s.base.clone())
            .or_else(|| self.base.clone())
    }

    /// Opens the compose box for a new agent in the tree under the cursor:
    /// a free one is leased for it, a held one keeps its branch.
    fn open_start(&mut self) {
        let Some(tree) = self.current().cloned() else {
            return;
        };
        if self.jobs.get(&tree.path).is_some_and(|s| s.is_active()) {
            self.message = Some(format!("tree {} already has an action queued", tree.name));
            return;
        }
        self.prompt = Some(Prompt::start(tree, self.user.clone()));
    }

    /// The agents of `tree` that a message can be typed to.
    fn recipients(tree: &Tree) -> Vec<Recipient> {
        tree.agents
            .iter()
            .filter_map(|a| Recipient::of(a, &tree.name))
            .collect()
    }

    /// Opens the box for one line to the assistant, about `context`, what
    /// was under the cursor.
    pub fn open_ask(&mut self, context: Option<String>) {
        self.ask_context = context;
        self.prompt = Some(Prompt::dispatch(Target::Ask, self.user.clone()));
    }

    /// Sends `text` to the agents its leading `@` words name, else to the
    /// assistant, with what was on screen when the box opened.
    fn ask(&mut self, text: String) -> Outcome {
        let context = self.ask_context.take();
        if text.is_empty() {
            return Outcome::Continue;
        }
        let (targets, message) = Self::split_box(&text);
        if targets.is_empty() {
            let mut message = text;
            if let Some(context) = context {
                message.push_str(&format!("\n\n(On screen: {context}.)"));
            }
            return self.converse(message);
        }
        let trees: Result<Vec<Tree>, String> = targets.iter().map(|w| self.address(w)).collect();
        let why = match trees {
            Err(why) => why,
            Ok(_) if message.is_empty() => {
                format!("write the message after {}", targets.join(" "))
            }
            Ok(trees) => {
                let to: Vec<Recipient> = trees.iter().flat_map(Self::recipients).collect();
                if to.is_empty() {
                    "no agent there with a chat to type into".to_string()
                } else {
                    let names: Vec<String> = trees.iter().map(|t| t.name.clone()).collect();
                    self.message = Some(format!("sending to tree {}...", names.join(", ")));
                    return Outcome::Send {
                        to,
                        text: message.to_string(),
                    };
                }
            }
        };
        // The box opens again on what was typed, so it is fixed rather than
        // typed again.
        self.message = Some(why);
        self.ask_context = context;
        self.prompt = Some(Prompt {
            text,
            ..Prompt::dispatch(Target::Ask, self.user.clone())
        });
        Outcome::Continue
    }

    /// The tree an `@` in the box names: its number, `main`, or the start of
    /// an agent's name, which must be one tree's; else why not.
    pub fn address(&self, word: &str) -> Result<Tree, String> {
        let token = word.trim_start_matches('@');
        if token.is_empty() {
            return Err("@ takes a tree number or an agent's name".into());
        }
        if let Some(tree) = self
            .trees
            .iter()
            .find(|t| t.name == token || (token == "main" && t.is_main))
        {
            return Ok(tree.clone());
        }
        // `@fix-log` names "fix the login redirect": each part of the word
        // starts a word of the name, in order.
        let pieces: Vec<String> = token
            .split(|c: char| !c.is_alphanumeric())
            .filter(|p| !p.is_empty())
            .map(str::to_lowercase)
            .collect();
        let names = |name: &str| {
            let words: Vec<String> = name
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_lowercase)
                .collect();
            let mut at = 0;
            pieces.iter().all(|piece| {
                match words[at..]
                    .iter()
                    .position(|w| w.starts_with(piece.as_str()))
                {
                    Some(found) => {
                        at += found + 1;
                        true
                    }
                    None => false,
                }
            })
        };
        let matching: Vec<&Tree> = self
            .trees
            .iter()
            .filter(|t| {
                t.agents
                    .iter()
                    .any(|a| !assistant::is_assistant(&a.name) && names(&a.name))
            })
            .collect();
        match matching.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(format!("no tree or agent {word}")),
            many => Err(format!(
                "{word} is in trees {}: use the number",
                many.iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }

    /// The box's leading `@` words and the message after them.
    fn split_box(text: &str) -> (Vec<&str>, &str) {
        let mut rest = text.trim_start();
        let mut targets = Vec::new();
        while let Some(word) = rest
            .split_whitespace()
            .next()
            .filter(|w| w.starts_with('@'))
        {
            targets.push(word);
            rest = rest[rest.find(word).unwrap_or(0) + word.len()..].trim_start();
        }
        (targets, rest)
    }

    /// What is under the cursor, for the assistant: the tree and its agent.
    fn cursor_context(&self) -> Option<String> {
        self.current().map(|tree| {
            let agent = Self::agents_of(tree)
                .first()
                .map(|a| format!(", agent {}", a.name));
            format!("tree {}{}", tree.name, agent.unwrap_or_default())
        })
    }

    /// Opens the compose box to the assistant.
    fn open_assistant(&mut self) {
        self.prompt = Some(Prompt::dispatch(Target::Assistant, self.user.clone()));
    }

    /// Opens the compose box on the name of the branch of the tree under the
    /// cursor, to rename it. A tree nobody holds has no branch to keep, and
    /// the main checkout's is yours.
    fn open_rename(&mut self) {
        let Some(tree) = self.current() else { return };
        self.message = if tree.is_main {
            Some("the main checkout's branch is yours to rename".into())
        } else if !tree.is_held() {
            Some(format!(
                "tree {} is available: no branch to rename",
                tree.name
            ))
        } else if self.jobs.get(&tree.path).is_some_and(|s| s.is_active()) {
            Some(format!("tree {} already has an action queued", tree.name))
        } else {
            self.prompt = Some(Prompt::rename(tree.clone()));
            None
        };
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
            .filter_map(|(tree, agent)| Recipient::of(agent, &tree.name))
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
            // A branch git would refuse keeps the box open on it, so it is
            // fixed rather than typed again.
            Edit::Submit if let Some(why) = branch_problem(prompt) => {
                self.message = Some(why);
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
        let branch = prompt.branch_name();
        let base = prompt.base_name();
        match prompt.purpose {
            Purpose::Rename { tree } => {
                if tree.branch.as_deref() == Some(text.as_str()) {
                    return Outcome::Continue;
                }
                self.jobs
                    .insert(tree.path.clone(), JobState::Queued(Kind::Rename));
                Outcome::Queue(vec![Job {
                    rename: Some(text),
                    ..Job::new(Kind::Rename, *tree)
                }])
            }
            Purpose::Message { to } => {
                if text.is_empty() {
                    self.message = Some("nothing to send".into());
                    return Outcome::Continue;
                }
                self.message = Some(format!("sending to {} agent(s)...", to.len()));
                Outcome::Send { to, text }
            }
            Purpose::Start { tree } => {
                let launch = Launch {
                    branch,
                    ..Launch::agent(text, false)
                };
                self.message = Some(format!("starting an agent in tree {}...", tree.name));
                self.start(*tree, launch)
            }
            Purpose::Dispatch { target } => match target {
                Target::Assistant => self.converse(text),
                Target::Ask => self.ask(text),
                Target::NewTree => self.dispatch(text, branch, base),
            },
        }
    }

    /// Talks to the assistant the way `m` talks to an agent: `text` goes
    /// to it behind the screen and treetop stays on the list. With no text,
    /// its chat opens instead, which is where its answers are read. With no
    /// assistant, one starts in the main checkout with `text` as its
    /// first prompt.
    fn converse(&mut self, text: String) -> Outcome {
        if let Some(to) = self.assistant() {
            if text.trim().is_empty() {
                return Outcome::Chat {
                    command: to.command,
                    agent: to.agent,
                    tree: to.tree,
                };
            }
            self.message = Some("sending to the assistant...".into());
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
                name: Some(assistant::NAME.into()),
                branch: None,
                is_new_tree: false,
                base: None,
                is_assistant: true,
                ticket: None,
            },
        )
    }

    /// Queues a Start for `task` in a new tree, on `branch`, cut from `base`
    /// or else the pool's. An empty task
    /// starts the agent idle and opens its chat, as `n` always has.
    fn dispatch(&mut self, task: String, branch: Option<String>, base: Option<String>) -> Outcome {
        let launch = Launch {
            branch,
            base,
            ..Launch::agent(task, true)
        };
        self.new_trees += 1;
        let tree = pool::stand_in(PathBuf::from(format!("(new tree {})", self.new_trees)));
        self.message = Some("leasing a new tree for the agent...".into());
        self.start(tree, launch)
    }

    /// Queues a Start of `launch` in `tree`, unless the tree is busy with
    /// another action.
    fn start(&mut self, tree: Tree, launch: Launch) -> Outcome {
        if self.jobs.get(&tree.path).is_some_and(|s| s.is_active()) {
            self.message = Some(format!("tree {} already has an action queued", tree.name));
            return Outcome::Continue;
        }
        // A pick's task comes from its ticket when the job runs, so it is
        // never an idle start.
        if launch.task.trim().is_empty() && launch.ticket.is_none() {
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

    /// Opens the picker on `choices`, the cursor on the most recent workspace
    /// other than this one, so `w` then Enter goes back and forth between two.
    pub fn open_picker(&mut self, choices: Vec<Choice>) {
        if choices.iter().all(|c| c.checkout == self.checkout) {
            self.message = Some(
                "no other workspace yet: run treetop once in another pooled repository".into(),
            );
            return;
        }
        let selected = choices
            .iter()
            .position(|c| c.checkout != self.checkout)
            .unwrap_or(0);
        self.picker = Some(Picker { choices, selected });
    }

    fn handle_picker_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(picker) = self.picker.as_mut() else {
            return Outcome::Continue;
        };
        let last = picker.choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => picker.selected = (picker.selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Enter => {
                let chosen = picker
                    .choices
                    .get(picker.selected)
                    .map(|c| c.checkout.clone());
                self.picker = None;
                match chosen {
                    Some(path) if path == self.checkout => {}
                    Some(_) if self.active_jobs() > 0 => {
                        self.message = Some(format!(
                            "{} action(s) still running here; switch once they finish",
                            self.active_jobs()
                        ));
                    }
                    Some(path) => return Outcome::Switch(path),
                    None => {}
                }
            }
            KeyCode::Esc | KeyCode::Char('q' | 'w') => self.picker = None,
            _ => {}
        }
        Outcome::Continue
    }

    /// Shows the issue the tree under the cursor works on, on the Issues
    /// screen.
    fn show_issue(&mut self) {
        let Some(tree) = self.current() else {
            return;
        };
        match tree.sidecar.as_ref().and_then(|s| s.ticket.clone()) {
            Some(ticket) => {
                self.open_issue = Some(ticket);
                self.is_issues = true;
            }
            None => self.choose_issue(),
        }
    }

    /// Lets the issue the tree under the cursor works on be chosen on the
    /// Issues screen, for a tree started without one or on another.
    fn choose_issue(&mut self) {
        let Some(tree) = self.current() else {
            return;
        };
        if !tree.is_held() || tree.is_main {
            self.message = Some(format!(
                "tree {} is not leased; only a leased tree works on an issue",
                tree.name
            ));
            return;
        }
        let (path, name) = (tree.path.clone(), tree.name.clone());
        self.choose_issue_for = Some(name.clone());
        self.attaching = Some((path, name));
        self.is_issues = true;
    }

    /// Records that the tree an issue was being chosen for works on issue
    /// `id`, at once on screen; returns the tree and its record for the
    /// program to write.
    pub fn attach(&mut self, id: String) -> Option<(PathBuf, Sidecar)> {
        let (path, name) = self.attaching.take()?;
        self.is_issues = false;
        // A tree returned while its issue was chosen is free, or someone
        // else's by now, and an issue on it would mislead.
        let Some(tree) = self
            .trees
            .iter_mut()
            .find(|t| t.path == path && t.is_held() && !t.is_main)
        else {
            self.message = Some(format!(
                "tree {name} has been returned; {id} was not recorded on it"
            ));
            return None;
        };
        let mut record = tree
            .sidecar
            .as_deref()
            .cloned()
            .unwrap_or_else(Sidecar::leased_now);
        record.ticket = Some(id.clone());
        tree.sidecar = Some(Box::new(record.clone()));
        self.message = Some(format!("tree {name} works on {id}"));
        Some((path, record))
    }

    /// The Issues screen was left without an issue chosen.
    pub fn stop_attaching(&mut self) {
        self.attaching = None;
    }

    /// The tree a ticket has been picked up in, by its sidecar.
    fn tree_of_ticket(&self, id: &str) -> Option<&Tree> {
        self.trees.iter().find(|t| {
            t.sidecar
                .as_ref()
                .and_then(|s| s.ticket.as_deref())
                .is_some_and(|ticket| ticket.eq_ignore_ascii_case(id))
        })
    }

    /// Picks up issue `id`, from the Issues screen: a new agent on it in a
    /// fresh tree, back on the trees. An issue already in a tree is not
    /// picked up twice; the cursor goes to its tree instead.
    pub fn pick(&mut self, id: String) -> Outcome {
        self.is_issues = false;
        self.attaching = None;
        if let Some(path) = self.tree_of_ticket(&id).map(|t| t.path.clone()) {
            self.filter.clear();
            self.reselect(Some(path));
            self.message = Some(format!(
                "{id} is already in this tree; Enter opens its agent"
            ));
            return Outcome::Continue;
        }
        self.new_trees += 1;
        let tree = pool::stand_in(PathBuf::from(format!("(new tree {})", self.new_trees)));
        self.message = Some(format!("picking up {id} in a new tree..."));
        self.start(tree, Launch::pick(id, self.checkout.clone()))
    }

    /// Opens the port the tree under the cursor serves, or, where it
    /// serves several, such as a frontend and a backend, a menu of them.
    fn browse(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        let tree_name = tree.name.clone();
        let ports = tree.served();
        match ports.as_slice() {
            [] => {
                self.message = Some(format!("tree {tree_name} serves no port"));
                Outcome::Continue
            }
            [(port, _)] => Outcome::Browse {
                port: *port,
                tree: tree_name,
            },
            _ => {
                self.port_menu = Some(PortMenu {
                    tree: tree_name,
                    ports,
                    selected: 0,
                });
                Outcome::Continue
            }
        }
    }

    /// Keys while the port menu is open: a number or Enter opens a port,
    /// and Esc closes it.
    fn handle_port_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(menu) = self.port_menu.as_mut() else {
            return Outcome::Continue;
        };
        let last = menu.ports.len().saturating_sub(1);
        let chosen = match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                menu.selected = (menu.selected + 1).min(last);
                return Outcome::Continue;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                menu.selected = menu.selected.saturating_sub(1);
                return Outcome::Continue;
            }
            KeyCode::Enter => menu.selected,
            KeyCode::Char(c @ '1'..='9') => {
                let at = usize::from(c as u8 - b'1');
                if at > last {
                    return Outcome::Continue;
                }
                at
            }
            KeyCode::Esc | KeyCode::Char('q' | 'o') => {
                self.port_menu = None;
                return Outcome::Continue;
            }
            _ => return Outcome::Continue,
        };
        match self.port_menu.take() {
            Some(menu) => Outcome::Browse {
                port: menu.ports[chosen].0,
                tree: menu.tree,
            },
            None => Outcome::Continue,
        }
    }

    /// Quits, or waits for running actions to finish first.
    pub fn quit(&mut self) -> Outcome {
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
        if self.picker.is_some() {
            return self.handle_picker_key(key);
        }
        if self.port_menu.is_some() {
            return self.handle_port_key(key);
        }
        if self.scratch_menu.is_some() {
            return self.handle_scratch_key(key);
        }
        if let Some(opening) = self.opening.clone() {
            let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            if is_ctrl && key.code == KeyCode::Char('c') {
                return Outcome::Quit;
            }
            let is_back = key.code == KeyCode::Esc || crate::chat::is_exit(key);
            if is_back {
                self.message = Some(format!(
                    "{} keeps starting; Enter on its tree opens its chat once it is up",
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
                _ => Outcome::Continue,
            };
        }
        if self.is_help {
            self.is_help = false;
            return Outcome::Continue;
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
        if let Some(shipping) = self.shipping.take() {
            if !matches!(key.code, KeyCode::Char('y' | 'Y')) {
                return Outcome::Continue;
            }
            let path = shipping.tree.path.clone();
            self.jobs.insert(path, JobState::Queued(Kind::Ship));
            return Outcome::Queue(vec![Job {
                shipping: Some(ShipCall {
                    plugin: shipping.plugin,
                    action: ShipAction::Run {
                        checkout: self.checkout.clone(),
                    },
                }),
                ..Job::new(Kind::Ship, shipping.tree)
            }]);
        }
        if let Some(pending) = self.pending.take() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.marked.clear();
                    let (held, free): (Vec<Tree>, Vec<Tree>) =
                        pending.trees.into_iter().partition(Tree::is_held);
                    if let Some(tree) = free.first() {
                        self.message = Some(format!(
                            "tree {} is already available; D in the dialog destroys it",
                            tree.name
                        ));
                    }
                    self.queue(Kind::Return, held)
                }
                KeyCode::Char('D') => {
                    self.marked.clear();
                    self.queue(Kind::Preview, pending.trees)
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
        if key.code == KeyCode::Char('w') {
            return Outcome::Workspaces;
        }
        if key.code == KeyCode::Char('C') {
            return Outcome::Scratches;
        }
        match key.code {
            KeyCode::Enter => return self.open_agent(),
            // The tree itself, even where Enter would open its agent's chat.
            KeyCode::Char('t') => {
                if let Some(tree) = self.current() {
                    return Outcome::Enter(tree.clone());
                }
            }
            KeyCode::Tab => self.is_issues = true,
            KeyCode::Char(':') => {
                let context = self.cursor_context();
                self.open_ask(context);
            }
            KeyCode::Char('i') => self.show_issue(),
            KeyCode::Char('I') => self.choose_issue(),
            KeyCode::Char('m') => self.open_message(),
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('?') => self.is_help = true,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc => self.marked.clear(),
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
            KeyCode::Char('o') => return self.browse(),
            KeyCode::Char('O') => return self.open_pr(),
            KeyCode::Char('f') => return self.read_failure(),
            KeyCode::Char('s') => return self.ship(),
            KeyCode::Char('z') => return self.toggle_park(),
            KeyCode::Char('Z') => {
                self.is_parked_open = !self.is_parked_open;
                let last = self.visible().len().saturating_sub(1);
                self.selected = self.selected.min(last);
            }
            KeyCode::Char('c') => self.open_assistant(),
            KeyCode::Char('n') => self.open_dispatch(),
            KeyCode::Char('N') => self.open_start(),
            KeyCode::Char('d') => self.open_diff(),
            KeyCode::Char('b') => self.open_rename(),
            KeyCode::Char('r') => {
                // Destroy is asked inside the return dialog, so it takes
                // every tree a destroy could, available ones too.
                let trees = self.targets(Kind::Destroy);
                if !trees.is_empty() {
                    self.pending = Some(Pending { trees });
                }
            }
            _ => {}
        }
        Outcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::pool::parse;
    use crate::ship::Ship;
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
        assert_eq!(
            app.pending.as_ref().unwrap().trees.len(),
            3,
            "7 can be destroyed"
        );
        let jobs = queued(press(&mut app, KeyCode::Char('y')));
        assert_eq!(names(&jobs), [(Kind::Return, "4"), (Kind::Return, "8")]);
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .starts_with("tree 7 is already available")
        );
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
    fn d_in_the_return_dialog_previews_a_destroy_and_only_y_on_it_destroys() {
        let mut app = app();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('r'));
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
        press(&mut app, KeyCode::Char('r'));
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
    fn a_returned_tree_turns_available_without_waiting_for_a_listing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('r'));
        let job = queued(press(&mut app, KeyCode::Char('y'))).remove(0);
        app.job_finished(job, true, String::new());
        let tree = app.trees.iter().find(|t| t.name == "4").unwrap();
        assert_eq!(tree.status, "available");
        assert_eq!((tree.holder.as_ref(), tree.branch.as_ref()), (None, None));
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
        press(&mut app, KeyCode::Char('r'));
        assert!(app.reviews.is_empty() && app.pending.is_none());
        assert_eq!(
            app.message.as_deref(),
            Some("tree 4 already has an action queued")
        );
    }

    #[test]
    fn a_held_tree_keeps_its_background_agents_until_it_is_being_returned() {
        let mut app = app();
        let background = |name: &str, job: &str, status: Status| Agent {
            name: name.into(),
            pid: None,
            status: Some(status),
            source: crate::agents::Source::Plugin,
            is_background: true,
            attach: None,
            job: Some(job.into()),
            plugin: Some("claude".into()),
            active_at: None,
        };
        let interactive = Agent {
            is_background: false,
            job: None,
            ..background("claude", "", Status::Idle)
        };
        app.trees
            .insert(0, pool::main_tree(std::path::Path::new("/p/main")));
        app.apply_agents(&HashMap::from([
            (
                PathBuf::from("/p/main"),
                vec![background("assistant", "0000main", Status::Cold)],
            ),
            (
                PathBuf::from("/p/4/app"),
                vec![background("fix", "00000004", Status::Cold), interactive],
            ),
            (
                PathBuf::from("/p/8/app"),
                vec![background("docs", "00000008", Status::Idle)],
            ),
        ]));
        let kept = |app: &App| -> Vec<(String, bool)> {
            app.kept().into_iter().map(|k| (k.id, k.is_cold)).collect()
        };
        assert_eq!(
            kept(&app),
            [("00000004".into(), true), ("00000008".into(), false)]
        );

        app.selected = app.visible().iter().position(|t| t.name == "4").unwrap();
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(
            kept(&app),
            [("00000008".into(), false)],
            "a tree being returned does not wake what its return stops"
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
        assert_eq!(press(&mut app, KeyCode::Char('r')), Outcome::Continue);
        assert!(app.pending.is_none());
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
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "8"),
            "with no chat to open, Enter goes to the tree"
        );
    }

    #[test]
    fn i_shows_the_issue_a_tree_works_on() {
        let mut app = app();
        dispatched(&mut app, "/p/8/app", "j1");
        let mut sidecar = *app.trees[2].sidecar.clone().unwrap();
        sidecar.ticket = Some("END-799".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/8/app"), sidecar)]));
        app.selected = app.visible().iter().position(|t| t.name == "8").unwrap();
        press(&mut app, KeyCode::Char('i'));
        assert!(app.is_issues);
        assert_eq!(app.open_issue.as_deref(), Some("END-799"));
        app.is_issues = false;
        app.open_issue = None;
        let unleased = app.visible().iter().position(|t| !t.is_held()).unwrap();
        app.selected = unleased;
        press(&mut app, KeyCode::Char('i'));
        assert!(!app.is_issues, "an unleased tree works on nothing");
        assert!(app.message.as_deref().unwrap().contains("is not leased"));
    }

    #[test]
    fn an_issue_chosen_for_a_tree_is_recorded_in_its_sidecar() {
        let mut app = app();
        dispatched(&mut app, "/p/8/app", "j1");
        app.selected = app.visible().iter().position(|t| t.name == "8").unwrap();
        press(&mut app, KeyCode::Char('i'));
        assert!(app.is_issues, "a tree on no issue chooses one");
        assert_eq!(app.choose_issue_for.as_deref(), Some("8"));
        let (path, record) = app.attach("END-42".into()).unwrap();
        assert_eq!(path, PathBuf::from("/p/8/app"));
        assert_eq!(record.ticket.as_deref(), Some("END-42"));
        assert!(!app.is_issues, "back on the trees");
        let tree = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert_eq!(
            tree.sidecar.as_ref().unwrap().ticket.as_deref(),
            Some("END-42"),
            "shown at once"
        );
        assert_eq!(app.message.as_deref(), Some("tree 8 works on END-42"));
        assert_eq!(app.attach("END-43".into()), None, "chosen once");
        press(&mut app, KeyCode::Char('I'));
        assert_eq!(
            app.choose_issue_for.as_deref(),
            Some("8"),
            "I chooses again"
        );
    }

    /// `i` on a tree with no issue, then picking another issue up instead
    /// of choosing one, then returning the tree: the next issue chosen
    /// went to that returned tree, now free.
    #[test]
    fn an_issue_is_never_chosen_for_a_tree_left_behind() {
        let mut app = app();
        app.checkout = PathBuf::from("/p/main");
        dispatched(&mut app, "/p/8/app", "j1");
        app.selected = app.visible().iter().position(|t| t.name == "8").unwrap();
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.choose_issue_for.as_deref(), Some("8"));
        assert!(matches!(app.pick("END-9".into()), Outcome::Queue(_)));
        app.returned(&PathBuf::from("/p/8/app"));
        assert_eq!(
            app.attach("END-7".into()),
            None,
            "picking up ended the choice"
        );
        let tree = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert!(tree.sidecar.is_none(), "the free tree carries no issue");
    }

    #[test]
    fn an_issue_chosen_for_a_tree_returned_meanwhile_goes_nowhere() {
        let mut app = app();
        app.selected = app.visible().iter().position(|t| t.name == "8").unwrap();
        press(&mut app, KeyCode::Char('I'));
        app.returned(&PathBuf::from("/p/8/app"));
        assert_eq!(app.attach("END-7".into()), None);
        assert_eq!(
            app.message.as_deref(),
            Some("tree 8 has been returned; END-7 was not recorded on it")
        );
    }

    #[test]
    fn tab_shows_the_issues_screen() {
        let mut app = app();
        press(&mut app, KeyCode::Tab);
        assert!(app.is_issues);
    }

    #[test]
    fn the_issues_screen_learns_which_tree_works_on_an_issue() {
        use issues::Host;
        let mut app = app();
        dispatched(&mut app, "/p/8/app", "j1");
        let mut sidecar = *app.trees[2].sidecar.clone().unwrap();
        sidecar.ticket = Some("END-799".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/8/app"), sidecar)]));
        let waiting = Agent {
            status: Some(Status::Waiting),
            ..attachable("fix")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/8/app"), vec![waiting])]));
        assert_eq!(
            app.work_on("end-799"),
            Some(issues::Work {
                tree: "8".into(),
                agent: Some("waiting".into()),
                needs_you: true,
                is_ready: false,
            })
        );
        assert_eq!(app.work_on("END-1"), None);
    }

    #[test]
    fn picking_an_issue_starts_it_in_a_new_tree_once() {
        let mut app = app();
        app.checkout = PathBuf::from("/repo");
        app.is_issues = true;
        let job = queued(app.pick("END-812".into())).remove(0);
        assert!(!app.is_issues, "back on the trees");
        let launch = job.launch.unwrap();
        assert!(launch.is_new_tree);
        assert_eq!(
            launch.ticket,
            Some(crate::jobs::Pick {
                id: "END-812".into(),
                checkout: PathBuf::from("/repo"),
            })
        );
        dispatched(&mut app, "/p/8/app", "j1");
        let mut sidecar = *app.trees[2].sidecar.clone().unwrap();
        sidecar.ticket = Some("END-799".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/8/app"), sidecar)]));
        assert_eq!(app.pick("end-799".into()), Outcome::Continue);
        assert_eq!(
            app.current().unwrap().name,
            "8",
            "the cursor went to its tree"
        );
        assert!(
            app.message
                .unwrap()
                .starts_with("end-799 is already in this tree")
        );
    }

    #[test]
    fn question_mark_shows_every_key_and_the_next_key_only_closes_it() {
        let mut app = app();
        press(&mut app, KeyCode::Char('?'));
        assert!(app.is_help);
        assert_eq!(press(&mut app, KeyCode::Char('r')), Outcome::Continue);
        assert!(!app.is_help && app.pending.is_none());
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
            ..Work::default()
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
                        ..Work::default()
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
        assert_eq!(app.wanted_diff(), Some((PathBuf::from("/p/4/app"), None)));
        assert_eq!(app.wanted_diff(), None, "in flight");
        app.diff_loaded(PathBuf::from("/p/4/app"), two_files());
        assert_eq!(app.wanted_diff(), None, "loaded");
        press(&mut app, KeyCode::Esc);
        assert!(app.view.is_none());
        with_work(&mut app, 2);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            app.wanted_diff(),
            Some((PathBuf::from("/p/4/app"), None)),
            "stale on reopen"
        );
    }

    #[test]
    fn a_tree_cut_from_another_base_is_diffed_against_it() {
        let mut app = app();
        app.base = Some("development".into());
        let mut record = Sidecar::leased_now();
        record.base = Some("main".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/8/app"), record)]));
        let base = |app: &App, name: &str| {
            let tree = app.trees.iter().find(|t| t.name == name).unwrap();
            app.base_of(tree)
        };
        assert_eq!(base(&app, "8").as_deref(), Some("main"));
        assert_eq!(
            base(&app, "4").as_deref(),
            Some("development"),
            "the pool's"
        );
        app.reselect(Some(PathBuf::from("/p/8/app")));
        with_work(&mut app, 1);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            app.wanted_diff(),
            Some((PathBuf::from("/p/8/app"), Some("main".into())))
        );
    }

    #[test]
    fn the_list_groups_trees_by_base_once_more_than_one_is_in_use() {
        let mut app = app();
        app.base = Some("development".into());
        let names =
            |app: &App| -> Vec<String> { app.visible().iter().map(|t| t.name.clone()).collect() };
        assert!(
            app.base_rows(&app.visible()).is_empty(),
            "one base, no headings"
        );
        let mut record = Sidecar::leased_now();
        record.base = Some("main".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/4/app"), record)]));
        let waiting = Agent {
            status: Some(Status::Waiting),
            ..attachable("asks")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![waiting])]));
        assert_eq!(
            app.bases_in_use(),
            ["development", "main"],
            "the pool's first"
        );
        assert_eq!(
            names(&app),
            ["8", "4", "7"],
            "tree 4 waits on you, but first in its own group, not the list"
        );
        assert_eq!(
            app.base_rows(&app.visible()),
            [
                (0, Some("development".into()), 1),
                (1, Some("main".into()), 1),
                (2, None, 1)
            ]
        );
        assert_eq!(app.waiting_rows(&app.visible()), 0, "no gap while grouped");
    }

    fn scratch(job: &str, name: &str) -> Scratch {
        Scratch {
            job: job.into(),
            name: name.into(),
            created: 0,
            record: PathBuf::from(format!("/state/scratch/{name}.json")),
        }
    }

    #[test]
    fn c_lists_scratch_chats_to_start_open_or_delete_twice_confirmed() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Char('C')), Outcome::Scratches);
        app.open_scratch_menu(vec![
            scratch("bbbb", "scratch 2"),
            scratch("aaaa", "scratch 1"),
        ]);
        assert_eq!(app.next_ask(), None, "the list keeps chats shut");
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::NewScratch);
        assert!(app.scratch_menu.is_none());
        app.open_scratch_menu(vec![
            scratch("bbbb", "scratch 2"),
            scratch("aaaa", "scratch 1"),
        ]);
        press(&mut app, KeyCode::Char('x'));
        assert!(
            !app.scratch_menu.as_ref().unwrap().is_deleting,
            "not on the new line"
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('x'));
        assert!(app.scratch_menu.as_ref().unwrap().is_deleting);
        press(&mut app, KeyCode::Char('k'));
        assert!(
            !app.scratch_menu.as_ref().unwrap().is_deleting,
            "any other key keeps it"
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            Outcome::RemoveScratch(scratch("aaaa", "scratch 1"))
        );
        assert_eq!(app.scratch_menu.as_ref().unwrap().chats.len(), 1);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Outcome::Chat {
                command: ["claude", "attach", "bbbb"].map(String::from).into(),
                agent: "scratch 2".into(),
                tree: SCRATCH.into(),
            }
        );
    }

    #[test]
    fn a_scratch_chat_asking_something_is_nobodys_news() {
        let mut app = app();
        app.scratch_root = Some(PathBuf::from("/state/scratch"));
        app.apply_reports(vec![
            report("s1", State::Blocked, "/state/scratch", 5),
            report("o1", State::Blocked, "/elsewhere/app", 5),
        ]);
        assert_eq!(app.elsewhere(), 1);
    }

    #[test]
    fn a_fresh_tree_is_leased_from_the_base_typed() {
        let mut app = app();
        app.base = Some("development".into());
        press(&mut app, KeyCode::Char('n'));
        let prompt = app.prompt.as_mut().unwrap();
        assert_eq!(prompt.default_base.as_deref(), Some("development"));
        prompt.bases = vec!["development".into(), "main".into()];
        type_in(&mut app, "fix the login");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        type_in(&mut app, "ma");
        press(&mut app, KeyCode::Tab);
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(job.launch.unwrap().base.as_deref(), Some("main"));
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
            3,
            "a page is the popup's inside less its bar, the pinned header and the key bar"
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
            pid: Some(1),
            status: None,
            source: crate::agents::Source::Process,
            is_background: false,
            attach: None,
            job: None,
            plugin: None,
            active_at: None,
        }
    }

    #[test]
    fn a_tree_leads_with_its_most_pressing_live_agent_and_never_an_assistant() {
        let with = |name: &str, status, active_at| Agent {
            status: Some(status),
            active_at,
            ..attachable(name)
        };
        let tree = Tree {
            is_main: false,
            agents: vec![
                with("old session", Status::Cold, Some(90)),
                with(assistant::NAME, Status::Waiting, Some(99)),
                with("idle", Status::Idle, Some(50)),
                with("working", Status::Busy, Some(10)),
                with("newer idle", Status::Idle, Some(60)),
            ],
            ..pool::main_tree(std::path::Path::new("/p/main"))
        };
        let names: Vec<&str> = App::agents_of(&tree)
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["working", "newer idle", "idle", "old session"]);
    }

    #[test]
    fn the_assistants_badge_says_what_it_is_doing() {
        let mut app = app_with_main();
        assert_eq!(app.assistant_state(), None, "none started yet");
        let main = app.trees.iter().find(|t| t.is_main).unwrap().path.clone();
        let mut assistant = attachable(assistant::NAME);
        for (status, said) in [
            (Status::Busy, "answering"),
            (Status::Waiting, "asks you"),
            (Status::Idle, "idle"),
            (Status::Cold, "stopped"),
        ] {
            assistant.status = Some(status);
            app.apply_agents(&HashMap::from([(main.clone(), vec![assistant.clone()])]));
            assert_eq!(app.assistant_state(), Some(said));
        }
    }

    #[test]
    fn enter_on_the_main_checkout_never_opens_the_assistant() {
        let mut app = app_with_main();
        let main = app.trees.iter().find(|t| t.is_main).unwrap().path.clone();
        let mut assistant = attachable(assistant::NAME);
        assistant.status = Some(Status::Idle);
        let mut own = attachable("fix the export");
        own.status = Some(Status::Idle);
        let mut old = attachable("old session");
        old.status = Some(Status::Cold);
        app.apply_agents(&HashMap::from([(
            main.clone(),
            vec![assistant.clone(), old.clone(), own],
        )]));
        app.reselect(Some(main.clone()));
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Outcome::Chat { agent, .. } if agent == "fix the export"
        ));
        app.apply_agents(&HashMap::from([(main, vec![assistant, old])]));
        let main = app.trees.iter().find(|t| t.is_main).unwrap();
        assert!(
            App::agents_of(main).is_empty(),
            "nothing live, nothing shown"
        );
    }

    #[test]
    fn enter_opens_an_attachable_chat_else_goes_to_the_tree() {
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
            press(&mut app, KeyCode::Enter),
            Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "e6b79280".into()],
                agent: "fix-login".into(),
                tree: "4".into()
            }
        );
        press(&mut app, KeyCode::Char('j'));
        assert!(matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "7"));
        press(&mut app, KeyCode::Char('j'));
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "8"),
            "an agent with no chat is in a pane, which going to the tree finds"
        );
        press(&mut app, KeyCode::Home);
        assert!(
            matches!(press(&mut app, KeyCode::Char('t')), Outcome::Enter(t) if t.name == "4"),
            "t goes to the tree, though its agent has a chat"
        );
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
    fn o_opens_the_one_port_or_says_there_is_none() {
        let mut app = app();
        let process = Process {
            pid: 1,
            name: "node".into(),
            ports: vec![5223],
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
    fn o_offers_each_port_of_a_tree_serving_a_frontend_and_a_backend() {
        let mut app = app();
        let process = |pid, name: &str, ports| Process {
            pid,
            name: name.into(),
            ports,
        };
        app.apply_processes(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![
                process(1, "uvicorn", vec![8000]),
                process(2, "node", vec![5173]),
            ],
        )]));
        let browse = |port| Outcome::Browse {
            port,
            tree: "4".into(),
        };
        assert_eq!(press(&mut app, KeyCode::Char('o')), Outcome::Continue);
        let menu = app.port_menu.as_ref().unwrap();
        assert_eq!(
            menu.ports,
            [(5173, "node".to_string()), (8000, "uvicorn".to_string())]
        );
        assert_eq!(app.next_ask(), None, "the menu keeps chats shut");
        assert_eq!(press(&mut app, KeyCode::Char('2')), browse(8000));
        assert!(app.port_menu.is_none());
        press(&mut app, KeyCode::Char('o'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(press(&mut app, KeyCode::Enter), browse(8000));
        press(&mut app, KeyCode::Char('o'));
        assert_eq!(press(&mut app, KeyCode::Char('9')), Outcome::Continue);
        press(&mut app, KeyCode::Esc);
        assert!(app.port_menu.is_none(), "Esc closes it");
    }

    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn n_with_no_task_starts_an_idle_agent_and_opens_its_chat_once_it_shows() {
        let mut app = app();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![agent("old")],
        )]));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.prompt.as_ref().unwrap().target(), Some(Target::NewTree));
        let mut job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(job.kind, Kind::Start);
        assert!(job.launch.as_ref().unwrap().is_new_tree);
        assert_eq!(job.launch.as_ref().unwrap().task, "");
        assert!(app.opening.is_some(), "its popup stands in");
        // The pool handed it tree 4, where an old agent already was.
        job.tree.path = PathBuf::from("/p/4/app");
        job.tree.name = "4".into();
        app.job_finished(job, true, "printed no id".into());
        assert_eq!(app.ready_chat(), None, "not shown up yet");
        let started = Agent {
            pid: Some(99),
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

    /// What a start's job carries once its agent's plugin says how to open it.
    fn started(mut job: Job) -> Job {
        job.attach = Some(vec!["claude".into(), "attach".into(), "cbcfd8c8".into()]);
        job
    }

    #[test]
    fn a_started_chat_opens_from_the_command_its_start_gave_without_waiting_to_see_it() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('c'));
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(app.opening.as_ref().unwrap().agent, assistant::NAME);
        app.job_finished(started(job), true, String::new());
        assert_eq!(
            app.ready_chat(),
            Some(Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "cbcfd8c8".into()],
                agent: assistant::NAME.into(),
                tree: "main".into()
            })
        );
        assert!(app.opening.is_none() && app.ready_chat().is_none());
    }

    #[test]
    fn esc_on_a_chat_on_its_way_stops_waiting_and_a_failed_start_ends_it() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('c'));
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(app.opening.is_some());
        assert_eq!(
            press(&mut app, KeyCode::Char('q')),
            Outcome::Continue,
            "keys wait"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.opening.is_none());
        app.job_finished(started(job), true, String::new());
        assert_eq!(app.ready_chat(), None, "dismissed, it opens only with c");
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("c, Enter opens its chat")
        );
        press(&mut app, KeyCode::Char('c'));
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
    fn a_new_tree_dispatch_runs_under_a_stand_in_until_its_tree_is_leased() {
        let mut app = app();
        press(&mut app, KeyCode::Char('n'));
        type_in(&mut app, "fix login");
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
    }

    fn report(job: &str, state: State, dir: &str, age: u64) -> Report {
        Report {
            job: job.into(),
            name: format!("job {job}"),
            state,
            text: format!("{job} says"),
            dir: Some(PathBuf::from(dir)),
            updated: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 - age),
            steps: Vec::new(),
        }
    }

    /// Records `job` as the job treetop started in the tree at `path`.
    fn dispatched(app: &mut App, path: &str, job: &str) {
        let mut sidecars = HashMap::new();
        for tree in app.trees.iter().filter(|t| t.sidecar.is_some()) {
            sidecars.insert(tree.path.clone(), *tree.sidecar.clone().unwrap());
        }
        sidecars.insert(
            PathBuf::from(path),
            Sidecar {
                job: Some(job.into()),
                ..Sidecar::leased_now()
            },
        );
        app.apply_sidecars(sidecars);
    }

    fn rows(app: &App) -> Vec<String> {
        app.visible().iter().map(|t| t.name.clone()).collect()
    }

    #[test]
    fn a_report_is_never_placed_by_its_directory_alone() {
        let mut app = app();
        app.apply_reports(vec![report("gone", State::Blocked, "/p/4/app", 1)]);
        let four = app.trees.iter().find(|t| t.name == "4").unwrap();
        assert!(
            app.says(four).is_none(),
            "tree 4 was returned and handed out again since"
        );
        assert_eq!(app.elsewhere(), 1, "it waits on you outside the pool");
    }

    #[test]
    fn a_tree_says_what_its_agent_last_reported_and_needing_you_sorts_it_first() {
        let mut app = app();
        dispatched(&mut app, "/p/8/app", "ask");
        app.apply_reports(vec![
            report("older", State::Done, "/p/8/app", 90),
            report("ask", State::Blocked, "/p/8/app/ui", 40),
            report("away", State::Done, "/home/me/app", 5),
        ]);
        let eight = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert_eq!(app.says(eight).unwrap().report.job, "ask");
        assert_eq!(rows(&app), ["8", "4", "7"], "waiting on you, so first");
        assert_eq!(app.needing_you(), 1);
        assert_eq!(app.elsewhere(), 0, "a finished job outside needs nothing");
    }

    #[test]
    fn the_cursor_stays_on_its_tree_when_an_agent_starts_waiting() {
        let mut app = app();
        app.apply_agents(&HashMap::new());
        press(&mut app, KeyCode::End);
        assert_eq!(app.current().unwrap().name, "8");
        let waiting = Agent {
            status: Some(Status::Waiting),
            job: Some("ask".into()),
            ..attachable("ask")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/8/app"), vec![waiting])]));
        assert_eq!(rows(&app)[0], "8", "it moved to the top");
        assert_eq!(app.current().unwrap().name, "8", "the cursor went with it");
    }

    #[test]
    fn the_assistant_says_nothing_on_its_row_but_still_rings_the_bell() {
        let mut app = app_with_main();
        let assistant = |state| Report {
            name: assistant::NAME.into(),
            ..report("c0", state, "/repo", 1)
        };
        app.apply_reports(vec![assistant(State::Working)]);
        let news = app.apply_reports(vec![assistant(State::Blocked)]);
        assert_eq!(news, [format!("{} needs you: c0 says", assistant::NAME)]);
        let main = app.trees.iter().find(|t| t.is_main).unwrap();
        assert!(app.says(main).is_none());
        assert_eq!(app.elsewhere(), 0);
    }

    fn choice(path: &str, held: Option<usize>) -> Choice {
        Choice {
            checkout: PathBuf::from(path),
            name: crate::workspaces::name(std::path::Path::new(path)),
            held,
        }
    }

    #[test]
    fn w_opens_the_picker_on_the_last_other_workspace_and_enter_switches() {
        let mut app = app_with_main();
        app.checkout = PathBuf::from("/repo");
        assert_eq!(press(&mut app, KeyCode::Char('w')), Outcome::Workspaces);
        app.open_picker(vec![
            choice("/repo", Some(2)),
            choice("/home/me/harness", Some(1)),
            choice("/home/me/lander", None),
        ]);
        assert_eq!(app.picker.as_ref().unwrap().selected, 1, "the other one");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.picker.as_ref().unwrap().selected, 2, "stops at the end");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Outcome::Switch(PathBuf::from("/home/me/lander"))
        );
        assert!(app.picker.is_none());
    }

    #[test]
    fn the_picker_closes_on_esc_stays_put_on_this_workspace_and_says_when_alone() {
        let mut app = app_with_main();
        app.checkout = PathBuf::from("/repo");
        app.open_picker(vec![choice("/repo", Some(2))]);
        assert!(app.picker.is_none());
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("no other workspace")
        );
        app.open_picker(vec![
            choice("/repo", None),
            choice("/home/me/harness", None),
        ]);
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Outcome::Continue,
            "already here"
        );
        app.open_picker(vec![
            choice("/repo", None),
            choice("/home/me/harness", None),
        ]);
        assert_eq!(press(&mut app, KeyCode::Esc), Outcome::Continue);
        assert!(
            app.picker.is_none() && !app.is_quitting,
            "Esc closes, never quits"
        );
    }

    #[test]
    fn a_switch_waits_for_running_actions() {
        let mut app = app_with_main();
        app.checkout = PathBuf::from("/repo");
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('y'));
        app.open_picker(vec![
            choice("/repo", None),
            choice("/home/me/harness", None),
        ]);
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .contains("still running here")
        );
    }

    #[test]
    fn another_repositorys_stopped_assistant_is_never_taken() {
        let mut app = app_with_main();
        app.apply_reports(vec![Report {
            name: assistant::NAME.into(),
            ..report("elsewhere", State::Stopped, "/home/me/harness", 1)
        }]);
        assert_eq!(app.assistant(), None);
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
            ["job cde62519 needs you: waiting at a prompt in its chat: Enter opens it"]
        );
        assert_eq!(app.needing_you(), 1);
        press(&mut app, KeyCode::Home);
        assert_eq!(app.current().unwrap().name, "8", "it sorts first");
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Outcome::Chat { tree, .. } if tree == "8"
        ));
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
    fn colon_types_to_the_assistant_about_the_tree_under_the_cursor() {
        let mut app = app_with_main();
        let at = app.visible().iter().position(|t| t.name == "4").unwrap();
        app.selected = at;
        press(&mut app, KeyCode::Char(':'));
        assert_eq!(app.prompt.as_ref().unwrap().target(), Some(Target::Ask));
        type_in(&mut app, "what is 4 waiting on?");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        let task = job.launch.unwrap().task;
        assert_eq!(task, "what is 4 waiting on?\n\n(On screen: tree 4.)");
        assert!(app.prompt.is_none());
    }

    #[test]
    fn an_at_in_the_box_sends_straight_to_that_agent() {
        let mut app = app();
        let fix = Agent {
            name: "fix the login redirect".into(),
            ..attachable("fix")
        };
        let seed = Agent {
            name: "seed insights".into(),
            ..attachable("seed")
        };
        app.apply_agents(&HashMap::from([
            (PathBuf::from("/p/8/app"), vec![fix]),
            (PathBuf::from("/p/4/app"), vec![seed]),
        ]));
        press(&mut app, KeyCode::Char(':'));
        type_in(&mut app, "@fix-log @4 rebase first");
        let Outcome::Send { to, text } = press(&mut app, KeyCode::Enter) else {
            panic!("sent to both agents, not the assistant");
        };
        let trees: Vec<&str> = to.iter().map(|r| r.tree.as_str()).collect();
        assert_eq!((trees, text.as_str()), (vec!["8", "4"], "rebase first"));
        press(&mut app, KeyCode::Char(':'));
        type_in(&mut app, "@nobody hi");
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert_eq!(
            app.prompt.as_ref().map(|p| p.text.as_str()),
            Some("@nobody hi"),
            "the box opens again on it, to fix"
        );
        assert_eq!(app.message.as_deref(), Some("no tree or agent @nobody"));
    }

    #[test]
    fn capital_n_starts_an_agent_in_the_tree_under_the_cursor() {
        let mut app = app();
        let at = |app: &App, name: &str| app.visible().iter().position(|t| t.name == name).unwrap();
        app.selected = at(&app, "7");
        press(&mut app, KeyCode::Char('N'));
        assert!(
            app.prompt.as_ref().unwrap().names_branch(),
            "a free tree is leased on a branch named for the task"
        );
        type_in(&mut app, "fix login");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!((job.kind, job.tree.name.as_str()), (Kind::Start, "7"));
        let launch = job.launch.unwrap();
        assert_eq!(launch.task, "fix login");
        assert!(!launch.is_new_tree, "this tree, not a fresh one");
        assert!(launch.branch.is_some());
        app.selected = at(&app, "4");
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "add tests");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(job.tree.name, "4");
        assert_eq!(
            job.launch.unwrap().branch,
            None,
            "a held tree keeps its branch"
        );
        app.selected = at(&app, "4");
        press(&mut app, KeyCode::Char('N'));
        assert!(app.prompt.is_none(), "not while its start is queued");
        assert_eq!(
            app.message.as_deref(),
            Some("tree 4 already has an action queued")
        );
    }

    #[test]
    fn an_old_assistant_is_not_the_assistant_so_a_new_one_starts() {
        let mut app = app_with_main();
        let old = Agent {
            name: "treetop-coordinator".into(),
            ..attachable("0897cd3f")
        };
        let main = app.trees.iter().find(|t| t.is_main).unwrap().path.clone();
        app.apply_agents(&HashMap::from([(main, vec![old])]));
        assert_eq!(app.assistant(), None, "it answers in chat and acts itself");
        press(&mut app, KeyCode::Char(':'));
        type_in(&mut app, "what needs me?");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        let launch = job.launch.unwrap();
        assert!(launch.is_assistant);
        assert_eq!(launch.name.as_deref(), Some(assistant::NAME));
    }

    #[test]
    fn c_talks_to_the_assistant_and_starts_it_in_the_main_checkout() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char('c'));
        assert_eq!(
            app.prompt.as_ref().unwrap().target(),
            Some(Target::Assistant)
        );
        type_in(&mut app, "what needs me?");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(job.tree.is_main);
        let launch = job.launch.clone().unwrap();
        assert!(launch.is_assistant);
        assert_eq!(launch.task, "what needs me?");
        assert_eq!(launch.name.as_deref(), Some(assistant::NAME));
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
    fn a_running_assistant_is_talked_to_rather_than_started_again() {
        let mut app = app_with_main();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/repo"),
            vec![attachable(assistant::NAME)],
        )]));
        press(&mut app, KeyCode::Char('c'));
        type_in(&mut app, "status?");
        match press(&mut app, KeyCode::Enter) {
            Outcome::Send { to, text } => {
                assert_eq!(text, "status?");
                assert_eq!(to[0].command, ["claude", "attach", assistant::NAME]);
                assert_eq!(to[0].tree, "main");
            }
            other => panic!("expected the message sent as m sends, got {other:?}"),
        }
        assert!(app.opening.is_none(), "no chat opens");
        press(&mut app, KeyCode::Char('c'));
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::Chat { agent, .. } if agent == assistant::NAME),
            "nothing typed opens its chat"
        );
    }

    #[test]
    fn a_stopped_assistant_is_attached_again_by_its_latest_job() {
        let mut app = app_with_main();
        let stopped = |job: &str, age| Report {
            name: assistant::NAME.into(),
            ..report(job, State::Stopped, "/repo", age)
        };
        app.apply_reports(vec![stopped("old", 50), stopped("latest", 5)]);
        press(&mut app, KeyCode::Char('c'));
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

    fn chat_of(outcome: Option<Outcome>) -> Option<String> {
        match outcome {
            Some(Outcome::Chat { agent, .. }) => Some(agent),
            _ => None,
        }
    }

    #[test]
    fn each_ask_opens_its_chat_in_turn_once_you_are_free_on_the_list() {
        let mut app = app();
        let asking = Agent {
            job: Some("j4".into()),
            ..attachable("asks")
        };
        let at_prompt = Agent {
            status: Some(Status::Waiting),
            ..attachable("prompt")
        };
        let finished = Agent {
            job: Some("j8".into()),
            ..attachable("finished")
        };
        app.apply_agents(&HashMap::from([
            (PathBuf::from("/p/4/app"), vec![asking]),
            (PathBuf::from("/p/8/app"), vec![at_prompt, finished]),
        ]));
        app.apply_reports(vec![
            report("j4", State::Blocked, "/p/4/app", 5),
            report("j8", State::Done, "/p/8/app", 1),
        ]);
        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.next_ask(), None, "not while a message is typed");
        press(&mut app, KeyCode::Esc);
        let first = chat_of(app.next_ask()).unwrap();
        assert_eq!(
            app.current().unwrap().name,
            if first == "asks" { "4" } else { "8" }
        );
        let second = chat_of(app.next_ask()).unwrap();
        let mut opened = [first, second];
        opened.sort();
        assert_eq!(opened, ["asks", "prompt"], "a finished turn never opens");
        assert_eq!(app.next_ask(), None, "each ask opens once");
    }

    #[test]
    fn an_ask_a_filter_hides_still_opens_and_the_filter_clears() {
        let mut app = app();
        let asking = Agent {
            status: Some(Status::Waiting),
            ..attachable("asks")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/8/app"), vec![asking])]));
        app.filter = "end-1".into();
        assert_eq!(chat_of(app.next_ask()).as_deref(), Some("asks"));
        assert!(app.filter.is_empty());
        assert_eq!(app.current().unwrap().name, "8");
    }

    #[test]
    fn an_ask_seen_in_its_open_chat_stays_shut_and_a_new_one_opens() {
        let mut app = app();
        let asking = Agent {
            job: Some("j4".into()),
            ..attachable("asks")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![asking])]));
        let command: Vec<String> = ["claude", "attach", "asks"].map(String::from).into();
        app.apply_reports(vec![report("j4", State::Blocked, "/p/4/app", 5)]);
        app.saw_chat(&command);
        assert_eq!(app.next_ask(), None, "answered in the chat it asked in");
        app.apply_reports(vec![report("j4", State::Working, "/p/4/app", 4)]);
        assert_eq!(app.next_ask(), None);
        app.apply_reports(vec![report("j4", State::Blocked, "/p/4/app", 3)]);
        assert_eq!(
            chat_of(app.next_ask()).as_deref(),
            Some("asks"),
            "asks again"
        );
    }

    #[test]
    fn a_busy_agent_never_opens_on_a_question_its_last_report_asked() {
        let mut app = app();
        let busy = Agent {
            job: Some("j4".into()),
            status: Some(Status::Busy),
            ..attachable("busy")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![busy])]));
        app.apply_reports(vec![report("j4", State::Blocked, "/p/4/app", 5)]);
        assert_eq!(app.next_ask(), None);
    }

    #[test]
    fn a_message_to_a_cold_agent_goes_through_its_plugin_not_its_chat() {
        let mut app = app();
        let cold = Agent {
            status: Some(Status::Cold),
            pid: None,
            job: Some("42494f41".into()),
            plugin: Some("claude".into()),
            ..attachable("idle one")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![cold])]));
        press(&mut app, KeyCode::Char('m'));
        type_in(&mut app, "carry on");
        let Outcome::Send { to, text } = press(&mut app, KeyCode::Enter) else {
            panic!("expected a send");
        };
        assert!(to[0].is_cold);
        let job = app.send_cold(&to[0], &text).unwrap();
        assert_eq!(job.kind, Kind::Send);
        assert_eq!(job.tree.name, "4");
        assert_eq!(
            job.message,
            Some(Message {
                plugin: "claude".into(),
                id: "42494f41".into(),
                text: "carry on".into(),
            })
        );
        assert_eq!(
            app.jobs[&PathBuf::from("/p/4/app")],
            JobState::Queued(Kind::Send)
        );
        let live = Recipient {
            is_cold: false,
            ..to[0].clone()
        };
        assert_eq!(
            app.send_cold(&live, &text),
            None,
            "a running one is typed to"
        );
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

    fn ship(branch: &str, stage: Stage, label: &str) -> Ship {
        Ship {
            branch: branch.into(),
            stage,
            label: label.into(),
            url: Some("https://github.com/o/r/pull/12".into()),
            head: Some("abc".into()),
            detail: Vec::new(),
            plugin: "github".into(),
        }
    }

    fn ships(entries: &[(&str, Ship)]) -> Result<Answer, String> {
        Ok(Answer {
            ships: entries
                .iter()
                .map(|(path, ship)| (PathBuf::from(path), ship.clone()))
                .collect(),
            failures: Vec::new(),
        })
    }

    #[test]
    fn a_failing_pr_needs_you_sorts_first_and_rings_once() {
        let mut app = app();
        let review = ship("feature/end-2-b", Stage::Review, "#12 review");
        let failing = ship("feature/end-2-b", Stage::CiFailed, "#12 ✗ ci");
        assert!(app.apply_ship(ships(&[("/p/8/app", review)])).is_empty());
        assert_eq!(app.needing_you(), 0);
        let news = app.apply_ship(ships(&[("/p/8/app", failing.clone())]));
        assert_eq!(news, ["tree 8: #12 ✗ ci on feature/end-2-b"]);
        assert_eq!(app.needing_you(), 1);
        assert_eq!(app.visible()[0].name, "8", "it sorts first");
        assert!(
            app.apply_ship(ships(&[("/p/8/app", failing)])).is_empty(),
            "the same stage rings once"
        );
    }

    #[test]
    fn the_first_ask_announces_nothing_and_an_error_keeps_what_was_known() {
        let mut app = app();
        let changes = ship("feature/end-2-b", Stage::Changes, "#12 changes");
        assert!(app.apply_ship(ships(&[("/p/8/app", changes)])).is_empty());
        assert_eq!(app.needing_you(), 1);
        assert!(app.apply_ship(Err("gh is not installed".into())).is_empty());
        assert_eq!(app.ship_error.as_deref(), Some("gh is not installed"));
        assert_eq!(app.needing_you(), 1);
        // One plugin that cannot say is said, though another could.
        let mut answer = ships(&[]).unwrap();
        answer.failures = vec!["github: auth: run gh auth login".into()];
        app.apply_ship(Ok(answer));
        assert_eq!(
            app.ship_error.as_deref(),
            Some("github: auth: run gh auth login")
        );
        app.apply_ship(ships(&[]));
        assert_eq!(app.ship_error, None);
    }

    #[test]
    fn merged_trees_are_counted_filtered_and_their_pr_opens() {
        let mut app = app();
        let merged = ship("feature/end-1-a", Stage::Merged, "#12 merged");
        app.apply_ship(ships(&[("/p/4/app", merged)]));
        assert_eq!(app.merged(), 1);
        app.filter = "merged".into();
        let visible: Vec<&str> = app.visible().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(visible, ["4"]);
        assert_eq!(
            press(&mut app, KeyCode::Char('O')),
            Outcome::OpenUrl("https://github.com/o/r/pull/12".into())
        );
        app.filter.clear();
        press(&mut app, KeyCode::End);
        assert_eq!(press(&mut app, KeyCode::Char('O')), Outcome::Continue);
        assert_eq!(app.message.as_deref(), Some("tree 8 has no PR to open"));
    }

    #[test]
    fn a_listing_keeps_a_held_trees_pr_and_a_return_drops_it() {
        let mut app = app();
        let merged = ship("feature/end-1-a", Stage::Merged, "#12 merged");
        app.apply_ship(ships(&[("/p/4/app", merged)]));
        app.set_trees(
            app.trees
                .clone()
                .into_iter()
                .map(|mut t| {
                    t.ship = None;
                    t
                })
                .collect(),
        );
        assert!(app.trees[0].ship.is_some());
        app.returned(&PathBuf::from("/p/4/app"));
        assert!(app.trees[0].ship.is_none());
    }

    #[test]
    fn f_reads_a_failure_and_sends_it_to_the_trees_agent() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Char('f')), Outcome::Continue);
        assert_eq!(
            app.message.as_deref(),
            Some("nothing has failed on tree 4's PR")
        );
        let failing = ship("feature/end-1-a", Stage::CiFailed, "#12 ✗ ci");
        app.apply_ship(ships(&[("/p/4/app", failing)]));
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(
            app.message.as_deref(),
            Some("no agent in tree 4 to send the failure to")
        );
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![attachable("0908e7f3")],
        )]));
        let job = queued(press(&mut app, KeyCode::Char('f'))).remove(0);
        assert_eq!((job.kind, job.tree.name.as_str()), (Kind::Fix, "4"));
        assert_eq!(
            press(&mut app, KeyCode::Char('f')),
            Outcome::Continue,
            "one read at a time"
        );
        app.job_finished(job, true, "Failing checks: test".into());
        let Some(Outcome::Send { to, text }) = app.ready_failure() else {
            panic!("expected a send");
        };
        assert_eq!(to.len(), 1);
        assert_eq!(
            text,
            "CI failed on #12. Fix it and push.\n\nFailing checks: test"
        );
        assert!(app.ready_failure().is_none(), "sent once");
    }

    #[test]
    fn a_merge_is_followed_up_once_and_names_what_it_leaves_ready() {
        let mut app = app();
        app.checkout = PathBuf::from("/p/main");
        let mut record = Sidecar::leased_now();
        record.ticket = Some("END-1".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/4/app"), record)]));
        let merged = ship("feature/end-1-a", Stage::Merged, "#12 merged");
        app.apply_ship(ships(&[("/p/8/app", merged.clone())]));
        assert!(app.ready_landings().is_empty(), "tree 8 records no ticket");
        app.apply_ship(ships(&[("/p/4/app", merged.clone())]));
        let mut landings = app.ready_landings();
        assert_eq!(landings.len(), 1);
        let mut job = landings.remove(0);
        assert_eq!(job.kind, Kind::Land);
        assert_eq!(
            job.landing,
            Some(Pick {
                id: "END-1".into(),
                checkout: PathBuf::from("/p/main"),
            })
        );
        assert_ne!(job.key, job.tree.path, "the tree stays free for a return");
        app.apply_ship(ships(&[("/p/4/app", merged)]));
        assert!(app.ready_landings().is_empty(), "once");
        job.ready = vec!["END-2".into()];
        app.job_finished(job, true, "END-1 merged; ready now: END-2".into());
        assert_eq!(app.message.as_deref(), Some("END-1 merged · END-2 ready"));
        assert!(app.jobs.is_empty());
        use issues::Host;
        assert_eq!(
            app.work_on("END-2"),
            Some(issues::Work {
                tree: String::new(),
                agent: None,
                needs_you: false,
                is_ready: true,
            })
        );
    }

    #[test]
    fn a_finished_merge_follow_up_never_holds_up_a_switch() {
        let mut app = app();
        app.checkout = PathBuf::from("/p/main");
        let mut record = Sidecar::leased_now();
        record.ticket = Some("END-1".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/4/app"), record)]));
        let merged = ship("feature/end-1-a", Stage::Merged, "#12 merged");
        app.apply_ship(ships(&[("/p/4/app", merged)]));
        let job = app.ready_landings().remove(0);
        app.job_started(&job);
        app.job_finished(job, true, "END-1 merged".into());
        assert_eq!(app.active_jobs(), 0);
        app.open_picker(vec![
            choice("/p/main", None),
            choice("/home/me/lander", None),
        ]);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Outcome::Switch(PathBuf::from("/home/me/lander"))
        );
    }

    #[test]
    fn s_asks_before_a_ship_run_and_shows_one_under_way() {
        let mut app = app();
        app.checkout = PathBuf::from("/p/main");
        app.ship_plugins = vec![Plugin::Builtin("github")];
        press(&mut app, KeyCode::Char('s'));
        assert!(app.shipping.is_none());
        assert!(
            app.message
                .as_deref()
                .unwrap()
                .starts_with("no ship plugin here starts a run (github)")
        );
        app.ship_plugins = vec![Plugin::Builtin("no-mistakes"), Plugin::Builtin("github")];
        press(&mut app, KeyCode::Char('s'));
        assert_eq!(
            app.message.as_deref(),
            Some("tree 4 has neither a ticket nor a task to give a run as its intent")
        );
        let mut record = Sidecar::leased_now();
        record.task = Some("fix the login redirect".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/4/app"), record)]));
        press(&mut app, KeyCode::Char('s'));
        let shipping = app.shipping.clone().unwrap();
        assert_eq!(
            (shipping.plugin.as_str(), shipping.intent.as_str()),
            ("no-mistakes", "fix the login redirect")
        );
        assert_eq!(press(&mut app, KeyCode::Char('n')), Outcome::Continue);
        assert!(app.shipping.is_none() && app.jobs.is_empty(), "n cancels");
        press(&mut app, KeyCode::Char('s'));
        let job = queued(press(&mut app, KeyCode::Char('y'))).remove(0);
        assert_eq!(job.kind, Kind::Ship);
        assert_eq!(
            job.shipping.unwrap().action,
            ShipAction::Run {
                checkout: PathBuf::from("/p/main")
            }
        );

        let mut app = app_on_a_run();
        let job = queued(press(&mut app, KeyCode::Char('s'))).remove(0);
        assert_eq!(job.shipping.as_ref().unwrap().action, ShipAction::Attach);
        let mut job = job;
        job.attach = Some(vec![
            "no-mistakes".into(),
            "attach".into(),
            "--run".into(),
            "r1".into(),
        ]);
        app.job_finished(job, true, String::new());
        assert!(matches!(
            app.ready_chat(),
            Some(Outcome::Chat { agent, tree, .. }) if agent == "no-mistakes" && tree == "4"
        ));
    }

    /// Tree 4 with a no-mistakes run waiting at a gate.
    fn app_on_a_run() -> App {
        let mut app = app();
        app.ship_plugins = vec![Plugin::Builtin("no-mistakes"), Plugin::Builtin("github")];
        let mut gate = ship("feature/end-1-a", Stage::Gate, "nm review ⏸");
        gate.plugin = "no-mistakes".into();
        app.apply_ship(ships(&[("/p/4/app", gate)]));
        app
    }

    #[test]
    fn trees_sort_waiting_first_then_main_then_by_their_agents_last_work() {
        let names =
            |app: &App| -> Vec<String> { app.visible().iter().map(|t| t.name.clone()).collect() };
        let mut app = app_with_main();
        assert_eq!(names(&app), ["main", "4", "7", "8"], "pool order to begin");
        dispatched(&mut app, "/p/4/app", "old");
        dispatched(&mut app, "/p/8/app", "new");
        app.apply_reports(vec![
            report("old", State::Working, "/p/4/app", 500),
            report("new", State::Working, "/p/8/app", 10),
        ]);
        assert_eq!(names(&app), ["main", "8", "4", "7"]);
        assert_eq!(app.waiting_rows(&app.visible()), 0);
        let quiet = Agent {
            active_at: Some(1_000),
            ..agent("quiet")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![quiet])]));
        assert_eq!(
            names(&app),
            ["main", "4", "8", "7"],
            "an agent's own time counts"
        );
        app.apply_reports(vec![
            report("old", State::Working, "/p/4/app", 500),
            report("new", State::Blocked, "/p/8/app", 10),
        ]);
        assert_eq!(names(&app), ["8", "main", "4", "7"]);
        assert_eq!(app.waiting_rows(&app.visible()), 1);
    }

    #[test]
    fn z_parks_a_tree_out_of_sight_and_z_again_brings_it_back() {
        let names =
            |app: &App| -> Vec<String> { app.visible().iter().map(|t| t.name.clone()).collect() };
        let mut app = app();
        assert_eq!(names(&app), ["4", "7", "8"]);
        let Outcome::Record { path, record } = press(&mut app, KeyCode::Char('z')) else {
            panic!("z records the park");
        };
        assert_eq!(path, PathBuf::from("/p/4/app"));
        assert!(record.parked);
        assert_eq!(
            app.message.as_deref(),
            Some("parked tree 4; Z opens Parked")
        );
        assert_eq!(names(&app), ["7", "8"], "folded away");
        assert_eq!(app.parked_rows(&app.visible()), Some((2, 1)));
        app.filter = "end-1".into();
        assert_eq!(names(&app), ["4"], "a filter still finds it");
        app.filter.clear();
        press(&mut app, KeyCode::Char('Z'));
        assert_eq!(names(&app), ["7", "8", "4"], "Parked opens below the rest");
        press(&mut app, KeyCode::End);
        let Outcome::Record { record, .. } = press(&mut app, KeyCode::Char('z')) else {
            panic!("z records it back");
        };
        assert!(!record.parked);
        assert_eq!(app.parked_rows(&app.visible()), None);
        press(&mut app, KeyCode::Home);
        assert_eq!(press(&mut app, KeyCode::Char('j')), Outcome::Continue);
        assert!(
            matches!(press(&mut app, KeyCode::Char('z')), Outcome::Continue),
            "an available tree has nothing to park"
        );
    }

    #[test]
    fn a_parked_tree_that_needs_you_comes_to_the_top() {
        let mut app = app();
        press(&mut app, KeyCode::Char('z'));
        let mut sidecars = HashMap::new();
        sidecars.insert(PathBuf::from("/p/4/app"), {
            let mut record = Sidecar::leased_now();
            record.parked = true;
            record.job = Some("ask".into());
            record
        });
        app.apply_sidecars(sidecars);
        app.apply_reports(vec![report("ask", State::Blocked, "/p/4/app", 1)]);
        assert_eq!(app.visible()[0].name, "4");
        assert_eq!(
            app.parked_rows(&app.visible()),
            None,
            "it is not counted as set aside"
        );
    }
}
