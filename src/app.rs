//! What the screen shows and what each key does, kept free of terminal I/O so
//! the rules about what may be returned or destroyed are testable.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agents::{Agent, Status};
use crate::assistant;
use crate::diff::Layout;
use crate::dispatch::{Edit, Prompt, Purpose, Recipient, Target};
use crate::handoff;
use crate::jobs::{Job, Kind, Launch, Message, Pick, Replaced, ShipAction, ShipCall};
use crate::keeper::Kept;
use crate::phone::{Code, Shared};
use crate::plugin::{self, Plugin, Usage};
use crate::pool::{self, Process, Tree, Work};
use crate::pr_view::{self, PrPopup, PrRead, Tab};
use crate::reports::{Report, State};
use crate::scratch::Scratch;
use crate::ship::{self, Answer, PrView, Stage};
use crate::sidecar::{Shelf, Sidecar};

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
            State::Done | State::Stopped | State::Failed => 1,
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

/// The command palette Ctrl+P opens: what has been typed to narrow it,
/// and the command picked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Palette {
    pub query: String,
    pub selected: usize,
}

/// A command the palette offers: what it says it does, the key that does
/// the same, and what running it presses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub label: String,
    pub key: String,
    run: Run,
}

/// What running a command does: presses its key on the table, or puts the
/// cursor on a tree and opens it, as Enter there does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Run {
    Key(KeyEvent),
    Open(String),
}

/// The commands on the tree under the cursor, `{}` its name, each with the
/// key that does it.
const TREE_COMMANDS: [(&str, char); 20] = [
    ("Return tree {} to the pool", 'r'),
    ("New agent in tree {}", 'n'),
    ("Write to tree {}'s agent", 'm'),
    ("Show the diff of tree {}", 'd'),
    ("Show the PR of tree {}", 'v'),
    ("Open tree {}'s PR on GitHub", 'O'),
    ("Send tree {}'s failure to its agent", 'f'),
    ("Start a ship run of tree {}", 's'),
    ("Resume tree {}'s agent", 'R'),
    ("Hand tree {}'s work to a new agent", 'h'),
    ("Go to tree {} itself", 't'),
    ("Rename the branch of tree {}", 'b'),
    ("Park tree {}", 'z'),
    ("Put tree {} on the Backburner", 'x'),
    ("Mark tree {}", ' '),
    ("Open tree {}'s port in the browser", 'o'),
    ("Share tree {}'s port with the phone", 'p'),
    ("Put tree {}'s port on the internet", 'P'),
    ("Open tree {}'s issue", 'i'),
    ("Choose the issue tree {} works on", 'I'),
];

/// The commands on the pool, each with the key that does it, `^` for one
/// with Ctrl.
const POOL_COMMANDS: [(&str, &str); 13] = [
    ("New agent in a fresh tree", "N"),
    ("Ask the assistant", ":"),
    ("Open the assistant's chat", "a"),
    ("Issues", "Tab"),
    ("Switch workspace", "w"),
    ("Scratch chats", "C"),
    ("Filter the trees", "/"),
    ("Open the inbox", "e"),
    ("Hide or show the inbox", "E"),
    ("Two windows, or one", "^\\"),
    ("Zoom the table or the windows", "^F"),
    ("Every key", "?"),
    ("Quit", "q"),
];

/// The key event a command's key stands for.
fn key_event(key: &str) -> KeyEvent {
    match key {
        "Tab" => KeyEvent::from(KeyCode::Tab),
        _ => match key.strip_prefix('^').and_then(|rest| rest.chars().next()) {
            Some(c) => KeyEvent::new(KeyCode::Char(c.to_ascii_lowercase()), KeyModifiers::CONTROL),
            None => KeyEvent::from(KeyCode::Char(key.chars().next().unwrap_or(' '))),
        },
    }
}

/// How well `label` answers `query`: every word typed is in it, scored by
/// where the first one falls, at its start, at a word's start, or within.
/// None where a word is missing.
fn rank(label: &str, query: &str) -> Option<u8> {
    let label = label.to_lowercase();
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    let first = words.first()?;
    if !words.iter().all(|word| label.contains(word.as_str())) {
        return None;
    }
    Some(if label.starts_with(first.as_str()) {
        0
    } else if label
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| word.starts_with(first.as_str()))
    {
        1
    } else {
        2
    })
}

/// The ports of a tree serving several, for `o` or `p` to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortMenu {
    pub tree: String,
    /// Each port and the process listening on it, lowest first.
    pub ports: Vec<(u16, String)>,
    pub selected: usize,
    /// Where the port chosen goes.
    pub reach: Reach,
}

/// Where a tree's port goes once chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The browser, as `o` opens it.
    Browser,
    /// The phone, over the tailnet, as `p` shares it.
    Phone,
    /// The internet, for anyone with the link, as `P` publishes it.
    Public,
    /// Neither: the code of a share already up, as `Q` shows it.
    Code,
}

/// A tree's port on its way to the phone, as `p` shares it: its address as
/// a QR code once the share command prints one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    pub tree: String,
    pub port: u16,
    /// None while the share command runs; then the address and its code,
    /// or why there is none.
    pub shared: Option<Result<(String, Code), String>>,
    /// On the internet, as `P` publishes it, not only the tailnet.
    pub is_public: bool,
}

/// A group of the list, under a heading of its own.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    /// The trees cut from this base; the main checkout heads its branch's.
    Base(String),
    /// The trees worked on, but not now.
    Backburner,
    /// The trees nobody holds.
    Free,
    /// The trees set aside, not worked on at all.
    Parked,
}

/// A row of the list: a group's heading, or a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListRow<'a> {
    Heading {
        group: Group,
        /// The trees in it, shown or folded.
        count: usize,
        /// How many of them wait on you, which a folded heading says.
        waiting: usize,
        is_folded: bool,
    },
    Tree(&'a Tree),
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
    /// The keel runs that hold any of them, by tree, which the dialog warns
    /// about.
    pub runs: HashMap<PathBuf, crate::keel::Run>,
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

/// What the window under the table shows of its tree, in the order its
/// tab bar has them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WindowTab {
    /// Its agent's chat, live.
    #[default]
    Chat,
    /// Its diff against its base, as `d` shows it.
    Diff,
    /// Its PR: checks, review threads and CodeRabbit, as `v` shows it.
    Pr,
    /// The steps of the keel run that holds it.
    Run,
    /// A shell in it, live.
    Shell,
    /// Where it is, its agents, ports and processes, and what its agent
    /// last said: the detail the pane under the list used to hold.
    Info,
}

/// What Ctrl+F zooms: the table over the whole screen, as treetop was
/// before the windows, or the windows, the table folded to their trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    Table,
    Windows,
}

/// What a shell in the window under the table names as its agent.
pub const SHELL: &str = "shell";

/// The second window of a double: the tree it keeps showing, and its tab,
/// while the cursor drives the other. Kept per workspace, so a restart
/// opens the double as it was left.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OtherWindow {
    pub path: PathBuf,
    pub tab: WindowTab,
}

/// A double as it is kept between runs: the second window, the window the
/// cursor drove and the side it was on.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct KeptLayout {
    other: Option<OtherWindow>,
    cursor: Option<OtherWindow>,
    side: usize,
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
    /// Show the pool of the repository checked out at `checkout`, and open
    /// what its inbox entry of `kind` for the tree at `tree` opens once it
    /// is read: an inbox entry of another workspace.
    SwitchTo {
        checkout: PathBuf,
        tree: PathBuf,
        kind: crate::inbox::Kind,
    },
    /// Start the newer treetop installed over this one, on this workspace.
    Restart,
    /// Install the newer release, then restart on it.
    Update,
    /// Open `http://localhost:<port>`, served from tree `tree`, in the browser.
    Browse {
        port: u16,
        tree: String,
    },
    /// Open `url`, such as a tree's PR, in the browser.
    OpenUrl(String),
    /// Share `port` with the phone, or with `is_public` the internet, for
    /// the code `phone` shows.
    Share {
        port: u16,
        is_public: bool,
    },
    /// Take `port` off the phone and the internet both, as one unshare
    /// command does.
    Unshare(u16),
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
    /// Scroll the window's chat by this many lines, down when positive.
    ScrollChat(isize),
    /// Type `text` into each agent's chat and submit it.
    Send {
        to: Vec<Recipient>,
        text: String,
    },
    /// Open the compose box's text in the editor, and put back what is
    /// saved.
    Editor,
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
    /// The held trees' git counts have been read at least once. They land
    /// after the agents, since git takes seconds across a pool, and the
    /// status snapshot waits for them rather than print them unknown.
    pub is_work_read: bool,
    pub marked: BTreeSet<PathBuf>,
    pub selected: usize,
    pub pending: Option<Pending>,
    pub reviews: VecDeque<Review>,
    pub jobs: HashMap<PathBuf, JobState>,
    /// Every finished job's output, newest last; the detail pane shows the
    /// latest of the tree under the cursor.
    pub log: Vec<LogEntry>,
    pub view: Option<DiffView>,
    pub diffs: HashMap<PathBuf, LoadedDiff>,
    /// Diffs asked for and not yet loaded, with the counts they were asked at.
    diff_requests: HashMap<PathBuf, Option<Work>>,
    /// A tree's PR in its popup over the pool.
    pub pr_popup: Option<PrPopup>,
    /// Each tree's PR as the popup last read it.
    pub prs: HashMap<PathBuf, PrRead>,
    /// The tree whose PR the popup wants read, until the main loop takes it.
    pr_wanted: Option<PathBuf>,
    /// `q` was pressed while jobs were running; quit once they finish.
    pub is_quitting: bool,
    pub message: Option<String>,
    /// The tree treetop was started in. Returning or destroying it would kill
    /// the shell standing in it, so it is never a target.
    pub here: Option<PathBuf>,
    /// treetop runs inside tmux, so an agent in a pane can be switched to.
    pub is_in_tmux: bool,
    /// Every port this machine shares, by port: how far it reaches and the
    /// address it is reached at, as the PORTS column marks them and `Q`
    /// draws them. A share outlives treetop, so these are read back, never
    /// remembered.
    pub shares: HashMap<u16, Shared>,
    /// An agent `n` started, whose chat opens once it shows up.
    awaiting_chat: Option<Awaiting>,
    /// The chat of an agent just started, ready to open: `claude --bg`
    /// prints the id `claude attach` takes, so there is no waiting for the
    /// agent to be seen in its tree.
    started_chat: Option<Outcome>,
    /// A chat on its way: its popup, or the window under the table for a
    /// tree's agent, stands in from the moment the box is sent until the
    /// chat itself opens in the same place.
    pub opening: Option<Opening>,
    /// The steps each start has finished so far, by its row, as the worker
    /// says them.
    pub steps: HashMap<PathBuf, Vec<&'static str>>,
    /// Since when a started agent's chat has waited for its new tree to be
    /// listed.
    started_at: Option<Instant>,
    /// The main checkout of the pool on screen, the workspace.
    pub checkout: PathBuf,
    /// The workspace picker, while it is open.
    pub picker: Option<Picker>,
    /// The ports `o` offers, while its menu is open.
    pub port_menu: Option<PortMenu>,
    /// The port `p` shares with the phone, and its code, while shown.
    pub phone: Option<Phone>,
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
    /// How much of the plan the agents run on is used, as the last agent
    /// plugin that could say reported it.
    pub usage: Option<Usage>,
    /// keel's runs, by the tree each works in; empty without keel.
    pub runs: HashMap<PathBuf, crate::keel::Run>,
    /// How many commits the main checkout sits behind its upstream, left
    /// there because it has work of its own or a busy agent.
    pub behind: usize,
    /// The newer treetop installed over the running one since it started,
    /// which `U` restarts on.
    pub replaced: Option<PathBuf>,
    /// A newer release than the one running, which `U` installs.
    pub newer: Option<String>,
    /// `U` is installing the newer release.
    pub is_updating: bool,
    /// Each tree's stage at the last ask, so a change can be announced; None
    /// until the first ask, which announces nothing.
    last_stages: Option<HashMap<PathBuf, Stage>>,
    /// A PR's failure read for its agent, ready to send.
    failure: Option<Outcome>,
    /// The agents whose question or prompt has been on screen, its chat
    /// opened for it or open while it asked, by their chat's command. One
    /// leaves once it stops asking, so its next ask opens again.
    asked: BTreeSet<String>,
    /// Where `asked` is kept for this workspace, so a switch back or a new
    /// run does not open an ask already seen. None keeps it in memory only.
    asked_file: Option<PathBuf>,
    /// Issues a merge has left with nothing blocking them, by id, until
    /// a tree takes one up.
    pub ready: BTreeSet<String>,
    /// Trees whose merge has been followed up, so it happens once.
    landed: BTreeSet<PathBuf>,
    /// Follow-ups of merges, to hand to the worker.
    landings: Vec<Job>,
    /// The Parked group is open, showing the trees set aside.
    pub is_parked_open: bool,
    /// The groups folded to their heading, besides Parked.
    pub folded: BTreeSet<Group>,
    /// The cursor has been put on a tree once, so a listing that moves it
    /// keeps it near where it was rather than on the first tree.
    is_cursor_placed: bool,
    /// An agent writing the handoff for the one that takes over from it.
    handing: Option<Handing>,
    /// The other workspaces' pools as their caches list them, where the
    /// inbox places their agents.
    pub pools: Vec<crate::inbox::Pool>,
    /// The other workspaces' trees with their agents and PRs, as the last
    /// read of each found them, by its checkout.
    pub away_live: HashMap<PathBuf, Vec<Tree>>,
    /// The inbox entries opened and dismissed, and whether it is hidden.
    pub inbox: crate::inbox::Marks,
    /// Where `inbox` is kept. None keeps it in memory only.
    inbox_file: Option<PathBuf>,
    /// The inbox has the keys, with its cursor on this entry.
    pub rail: Option<usize>,
    /// The tab the window under the table shows.
    pub tab: WindowTab,
    /// The window under the table has the keys, in Normal: treetop's keys
    /// step its tabs and scroll it, and Ctrl+Q gives them back to the table.
    pub is_window_focused: bool,
    /// The window with the keys is in Insert: its chat or shell takes every
    /// key but Ctrl+Q, which drops back to Normal.
    pub is_inserting: bool,
    /// Rows inside the window under the table, under its tab bar, as last
    /// drawn: what a page of its diff scrolls by.
    pub window_rows: u16,
    /// The second window, in a double; None shows one window.
    pub other: Option<OtherWindow>,
    /// What Ctrl+F zoomed, while it is zoomed.
    pub zoom: Option<Zoom>,
    /// The command palette, while Ctrl+P has it open.
    pub palette: Option<Palette>,
    /// In a double, the side of the window the cursor drives, the one that
    /// last had the keys: 0 on the left, 1 on the right.
    pub side: usize,
    /// Where the double is kept. None keeps it in memory only.
    layout_file: Option<PathBuf>,
    /// The window the cursor drove when the double was kept, put back once
    /// its tree is listed.
    returning: Option<OtherWindow>,
    /// The tree whose agent opens once this workspace's agents are read:
    /// an inbox entry chosen from another workspace.
    pub arriving: Option<(PathBuf, crate::inbox::Kind)>,
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
    /// The row of the start a tree's agent comes from, whose steps the
    /// window under the table shows as they end; None for the assistant's
    /// and a scratch chat, which open in a popup.
    pub key: Option<PathBuf>,
    /// Why its start failed, shown in the window until Esc.
    pub failed: Option<String>,
    /// Its start leases a tree, a fresh one or a free one, so it goes
    /// through every step; one in a tree already held only starts the agent.
    pub leases: bool,
    /// In a double, the side of the window it was asked from, which it
    /// stays in wherever the keys go; None with one window.
    pub side: Option<usize>,
}

/// A tree an agent was just started in, and the agents already there, so
/// the new one is the one whose chat opens.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Awaiting {
    path: PathBuf,
    known: Vec<Option<u32>>,
    since: Instant,
}

/// An agent asked to write its handoff to `file`, and the tree it hands on.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Handing {
    path: PathBuf,
    tree: String,
    from: Recipient,
    file: PathBuf,
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
/// one, or the one a new agent starts on; or, for a fresh tree, that what
/// is typed on its base line names no branch to cut it from.
fn branch_problem(prompt: &Prompt) -> Option<String> {
    if prompt.is_base_unmatched() {
        return Some(format!(
            "no branch matches \"{}\" to cut the tree from",
            prompt.base_typed.trim()
        ));
    }
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

    /// The keel run working in `tree`, if any.
    pub fn run_of(&self, tree: &Tree) -> Option<&crate::keel::Run> {
        self.runs.get(&tree.path)
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
        Self::shelf_of(tree) == Some(Shelf::Parked)
    }

    /// Where the tree is set aside, if anywhere.
    pub fn shelf_of(tree: &Tree) -> Option<Shelf> {
        tree.sidecar.as_ref().and_then(|s| s.shelf())
    }

    /// The bases the list groups trees under: the main checkout's branch,
    /// which it heads, and each one a held tree was cut from, the pool's
    /// first and the rest by name.
    pub fn bases_in_use(&self) -> Vec<String> {
        let main = self
            .trees
            .iter()
            .find(|t| t.is_main)
            .and_then(|t| t.branch.clone());
        let mut bases: Vec<String> = self
            .trees
            .iter()
            .filter(|t| t.is_held() && !t.is_main)
            // A pool whose base is unknown still lists its trees, under ''.
            .map(|t| self.base_of(t).unwrap_or_default())
            .chain(main)
            .collect();
        bases.sort_by_key(|b| (Some(b) != self.base.as_ref(), b.clone()));
        bases.dedup();
        bases
    }

    /// The group `tree` is listed under: Parked or Backburner for one set
    /// aside, even when it waits on you, the free trees, else the base it
    /// was cut from. None for the main checkout, which heads its branch's
    /// group rather than being a row; its agents are started and shown on
    /// that heading.
    pub fn group_of(&self, tree: &Tree) -> Option<Group> {
        if tree.is_main {
            return None;
        }
        Some(match Self::shelf_of(tree) {
            Some(Shelf::Parked) => Group::Parked,
            Some(Shelf::Backburner) => Group::Backburner,
            None if !tree.is_held() => Group::Free,
            None => Group::Base(self.base_of(tree).unwrap_or_default()),
        })
    }

    /// The group is folded to its heading. Parked starts folded, the rest
    /// open.
    pub fn is_folded(&self, group: &Group) -> bool {
        match group {
            Group::Parked => !self.is_parked_open,
            _ => self.folded.contains(group),
        }
    }

    fn toggle_fold(&mut self, group: &Group) {
        match group {
            Group::Parked => self.is_parked_open = !self.is_parked_open,
            _ => {
                if !self.folded.remove(group) {
                    self.folded.insert(group.clone());
                }
            }
        }
    }

    /// `tree` is one the filter looks for, by number, branch, holder,
    /// ticket, PR or agent.
    fn is_found(tree: &Tree, needle: &str) -> bool {
        needle.is_empty()
            || [
                Some(&tree.name),
                tree.branch.as_ref(),
                tree.holder.as_ref(),
                tree.sidecar.as_ref().and_then(|s| s.ticket.as_ref()),
                tree.ship.as_ref().map(|s| &s.label),
            ]
            .into_iter()
            .flatten()
            .chain(tree.agents.iter().map(|a| &a.name))
            .any(|field| field.to_lowercase().contains(needle))
    }

    /// The list as it shows: each group under its heading, the bases first,
    /// the pool's leading, then Backburner, the free trees and Parked.
    /// Within a group, trees waiting on you come first, then the most recently active, and
    /// trees with no activity in pool order last. A folded group shows its
    /// heading alone; a filter opens every group and leaves out those it
    /// finds nothing in, but the main checkout's branch keeps its heading.
    pub fn rows(&self) -> Vec<ListRow<'_>> {
        let needle = self.filter.to_lowercase();
        let bases = self.bases_in_use();
        let rank = |group: &Group| match group {
            Group::Base(base) => bases.iter().position(|b| b == base).unwrap_or(0),
            Group::Backburner => bases.len(),
            Group::Free => bases.len() + 1,
            Group::Parked => bases.len() + 2,
        };
        let mut trees: Vec<(Group, &Tree)> = self
            .trees
            .iter()
            .filter(|t| Self::is_found(t, &needle))
            .filter_map(|t| Some((self.group_of(t)?, t)))
            .collect();
        trees.sort_by_cached_key(|(group, t)| (rank(group), self.order_in_group(t)));
        let main = self
            .trees
            .iter()
            .find(|t| t.is_main)
            .and_then(|t| t.branch.clone());
        let groups = bases.iter().map(|b| Group::Base(b.clone())).chain([
            Group::Backburner,
            Group::Free,
            Group::Parked,
        ]);
        let mut rows = Vec::new();
        for group in groups {
            let members: Vec<&Tree> = trees
                .iter()
                .filter(|(g, _)| *g == group)
                .map(|(_, t)| *t)
                .collect();
            let is_mains = matches!(&group, Group::Base(b) if Some(b) == main.as_ref());
            if members.is_empty() && !is_mains {
                continue;
            }
            let is_folded = needle.is_empty() && self.is_folded(&group);
            rows.push(ListRow::Heading {
                waiting: members.iter().filter(|t| self.needs_you(t)).count(),
                count: members.len(),
                is_folded,
                group,
            });
            if !is_folded {
                rows.extend(members.into_iter().map(ListRow::Tree));
            }
        }
        rows
    }

    /// Where a tree goes within its group: waiting on you first, then the
    /// most recently active, the rest keeping pool order.
    pub fn order_in_group(&self, tree: &Tree) -> (bool, std::cmp::Reverse<Option<u64>>) {
        (
            !self.needs_you(tree),
            std::cmp::Reverse(self.active_at(tree)),
        )
    }

    /// The trees the list shows, in its order: those in open groups.
    pub fn visible(&self) -> Vec<&Tree> {
        self.rows()
            .into_iter()
            .filter_map(|row| match row {
                ListRow::Tree(tree) => Some(tree),
                ListRow::Heading { .. } => None,
            })
            .collect()
    }

    /// The row of the first tree, where the cursor starts, else the first.
    fn first_tree_row(&self) -> usize {
        self.rows()
            .iter()
            .position(|row| matches!(row, ListRow::Tree(_)))
            .unwrap_or(0)
    }

    /// The group whose heading the cursor is on.
    pub fn current_heading(&self) -> Option<Group> {
        match self.rows().get(self.selected) {
            Some(ListRow::Heading { group, .. }) => Some(group.clone()),
            _ => None,
        }
    }

    /// The main checkout, when the cursor is on the heading it leads: where
    /// `n` starts an agent that needs no tree, and Enter opens its chat.
    pub fn main_under_cursor(&self) -> Option<&Tree> {
        let Some(Group::Base(base)) = self.current_heading() else {
            return None;
        };
        self.trees
            .iter()
            .find(|t| t.is_main && t.branch.as_ref() == Some(&base))
    }

    /// `z` and `x`: sets the tree under the cursor aside on `shelf`, Parked
    /// or Backburner, or brings it back from there. Only a leased tree can
    /// be set aside. The cursor follows the tree to its new group, which
    /// opens for it if folded, since its trees are worked on; Parked alone
    /// stays folded, its trees out of sight.
    fn toggle_shelf(&mut self, shelf: Shelf) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        let name = tree.name.clone();
        if tree.is_main || !tree.is_held() {
            let verb = match shelf {
                Shelf::Parked => "park",
                Shelf::Backburner => "put on the Backburner",
            };
            self.message = Some(format!("tree {name} is not leased: nothing to {verb}"));
            return Outcome::Continue;
        }
        let path = tree.path.clone();
        let mut record = tree
            .sidecar
            .as_deref()
            .cloned()
            .unwrap_or_else(Sidecar::leased_now);
        let to = (record.shelf() != Some(shelf)).then_some(shelf);
        record.shelve(to);
        self.message = Some(match to {
            Some(Shelf::Parked) => format!("parked tree {name}; Z opens Parked"),
            Some(Shelf::Backburner) => format!("tree {name} is on the Backburner"),
            None => format!("tree {name} is back from {}", shelf.name()),
        });
        if let Some(tree) = self.trees.iter_mut().find(|t| t.path == path) {
            tree.sidecar = Some(Box::new(record.clone()));
        }
        if let Some(group) = self
            .trees
            .iter()
            .find(|t| t.path == path)
            .and_then(|t| self.group_of(t))
            && group != Group::Parked
        {
            self.folded.remove(&group);
        }
        let last = self.rows().len().saturating_sub(1);
        self.selected = self.selected.min(last);
        self.reselect(Some(path.clone()));
        Outcome::Record { path, record }
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

    /// The tree the window under the table shows: the one under the
    /// cursor, or the main checkout on its heading.
    pub fn window_tree(&self) -> Option<&Tree> {
        self.current().or(self.main_under_cursor())
    }

    /// The agent the window shows the chat of: the tree's leading agent
    /// that offers one, as Enter opens it.
    pub fn window_agent(&self) -> Option<&Agent> {
        Self::chat_agent(self.window_tree()?)
    }

    /// The agent a window on `tree` shows the chat of.
    pub fn chat_agent(tree: &Tree) -> Option<&Agent> {
        Self::agents_of(tree)
            .into_iter()
            .find(|agent| agent.attach.is_some())
    }

    /// The tabs the window has for its tree.
    pub fn window_tabs(&self) -> Vec<WindowTab> {
        match self.window_tree() {
            Some(tree) => self.tabs_of(tree),
            None => vec![WindowTab::Info],
        }
    }

    /// The tabs a window on `tree` has: the chat where an agent offers one,
    /// the diff and PR of a held tree, the run while keel has one holding
    /// it, a shell in a held tree, and its info.
    pub fn tabs_of(&self, tree: &Tree) -> Vec<WindowTab> {
        let mut tabs = Vec::new();
        if Self::chat_agent(tree).is_some() {
            tabs.push(WindowTab::Chat);
        }
        // The main checkout's heading stands for it, and `d` and `v` work
        // only on a tree's own row.
        if tree.is_held() && !tree.is_main {
            tabs.extend([WindowTab::Diff, WindowTab::Pr]);
        }
        if crate::keel::holding(&self.runs, &tree.path).is_some() {
            tabs.push(WindowTab::Run);
        }
        // A shell is kept running only while its tree is held.
        if self.keeps_shell(&tree.name) {
            tabs.push(WindowTab::Shell);
        }
        tabs.push(WindowTab::Info);
        tabs
    }

    /// The tab the window shows: the one picked where its tree has it,
    /// else its info.
    pub fn shown_tab(&self) -> WindowTab {
        if self.window_tabs().contains(&self.tab) {
            self.tab
        } else {
            WindowTab::Info
        }
    }

    /// The second window of a double, its tree and the tab it shows; None
    /// while there is no double, or its tree has left the pool.
    pub fn other_window(&self) -> Option<(&Tree, WindowTab)> {
        let other = self.other.as_ref()?;
        let tree = self.trees.iter().find(|t| t.path == other.path)?;
        let tab = if self.tabs_of(tree).contains(&other.tab) {
            other.tab
        } else {
            WindowTab::Info
        };
        Some((tree, tab))
    }

    /// The terminal the second window shows, kept running behind the one
    /// with the keys: as `window_chat` has it for the cursor's window.
    pub fn other_chat(&self) -> Option<(Vec<String>, String, String)> {
        let (tree, tab) = self.other_window()?;
        Self::terminal_of(tree, tab)
    }

    /// The plugin whose screen hints the second window's terminal is read
    /// by, as `window_plugin` has it.
    pub fn other_plugin(&self) -> Option<&str> {
        match self.other_window()? {
            (tree, WindowTab::Chat) => Self::chat_agent(tree)?.plugin.as_deref(),
            _ => None,
        }
    }

    /// Ctrl+P: opens the command palette.
    pub fn open_palette(&mut self) {
        self.palette = Some(Palette::default());
    }

    /// The commands the palette offers for what has been typed: the tree
    /// under the cursor's first, then the pool's, then, once something is
    /// typed, each tree to open; the best answers to it first.
    pub fn commands(&self) -> Vec<Command> {
        let query = self.palette.as_ref().map_or("", |p| p.query.as_str());
        let mut all = Vec::new();
        if let Some(tree) = self.current() {
            for (label, key) in TREE_COMMANDS {
                all.push(Command {
                    label: label.replace("{}", &tree.name),
                    key: if key == ' ' {
                        "Space".to_string()
                    } else {
                        key.to_string()
                    },
                    run: Run::Key(KeyEvent::from(KeyCode::Char(key))),
                });
            }
        }
        for (label, key) in POOL_COMMANDS {
            all.push(Command {
                label: label.to_string(),
                key: key.to_string(),
                run: Run::Key(key_event(key)),
            });
        }
        if !query.trim().is_empty() {
            // Every tree, those a filter or a folded group keeps out of the
            // list too: opening one brings it back into it.
            for tree in self.trees.iter().filter(|t| !t.is_main) {
                let branch = tree.branch.as_deref().unwrap_or("(detached)");
                all.push(Command {
                    label: format!("Open tree {} · {branch}", tree.name),
                    key: String::new(),
                    run: Run::Open(tree.name.clone()),
                });
            }
            let mut ranked: Vec<(u8, Command)> = all
                .into_iter()
                .filter_map(|command| Some((rank(&command.label, query)?, command)))
                .collect();
            // A tree whose number is what was typed comes before any other.
            let exact = |command: &Command| command.run == Run::Open(query.trim().to_string());
            ranked.sort_by_key(|(score, command)| (!exact(command), *score));
            return ranked.into_iter().map(|(_, command)| command).collect();
        }
        all
    }

    /// The keys while the palette is open: type to narrow it, Up and Down
    /// to pick, Enter to run the command picked, Esc to close it.
    fn handle_palette_key(&mut self, key: KeyEvent) -> Outcome {
        let count = self.commands().len();
        let Some(palette) = self.palette.as_mut() else {
            return Outcome::Continue;
        };
        match key.code {
            KeyCode::Esc => self.palette = None,
            KeyCode::Down => palette.selected = (palette.selected + 1).min(count.saturating_sub(1)),
            KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Backspace => {
                palette.query.pop();
                palette.selected = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                palette.query.push(c);
                palette.selected = 0;
            }
            KeyCode::Enter => {
                let selected = palette.selected;
                let picked = self.commands().into_iter().nth(selected);
                self.palette = None;
                if let Some(command) = picked {
                    // A command runs as its key does on the table.
                    self.is_window_focused = false;
                    return match command.run {
                        Run::Key(key) => self.handle_key(key),
                        Run::Open(name) => {
                            let path = self
                                .trees
                                .iter()
                                .find(|t| t.name == name)
                                .map(|t| t.path.clone());
                            if let Some(path) = path {
                                self.reveal(path);
                            }
                            self.handle_key(KeyEvent::from(KeyCode::Enter))
                        }
                    };
                }
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// Ctrl+F: zooms `what`, the table from the table and the windows from
    /// a window with the keys, or zooms back out.
    pub fn toggle_zoom(&mut self, what: Zoom) {
        self.zoom = match self.zoom {
            Some(_) => None,
            None => Some(what),
        };
    }

    /// Ctrl+\: two windows, the one the cursor drives on the right and the
    /// tree it was on kept on the left, with the cursor moved to the next
    /// tree down; or back to one, keeping the window that had the keys.
    pub fn toggle_double(&mut self) {
        // A double whose other tree has left the pool counts as none, so
        // this opens a new one rather than only forgetting it.
        let was_double = self.other_window().is_some();
        self.other = None;
        self.side = 0;
        if was_double {
            // Alone, a window keeps the keys only where it takes typing,
            // as Ctrl+Q gives them to one window.
            if !matches!(
                self.shown_tab(),
                WindowTab::Chat | WindowTab::Shell | WindowTab::Diff | WindowTab::Pr
            ) {
                self.is_window_focused = false;
            }
            self.save_layout();
            return;
        }
        let Some(tree) = self.window_tree() else {
            return;
        };
        let (path, tab) = (tree.path.clone(), self.tab);
        let rows = self.rows();
        let next = (self.selected + 1..rows.len())
            .chain((0..self.selected).rev())
            .find(|&at| matches!(rows[at], ListRow::Tree(_)));
        let Some(at) = next else {
            self.message = Some("one tree in the list: nothing to put beside it".into());
            self.save_layout();
            return;
        };
        self.other = Some(OtherWindow { path, tab });
        self.side = 1;
        self.selected = at;
        self.save_layout();
    }

    /// The window on `side` takes the keys: where the cursor drives the
    /// other one, the two change places, so the cursor drives this one and
    /// the tree it was on stays in the other.
    pub fn focus_side(&mut self, side: usize) {
        if side != self.side
            && let Some(other) = self.other.clone()
            && let Some(name) = self
                .trees
                .iter()
                .find(|t| t.path == other.path)
                .map(|t| t.name.clone())
            && let Some(here) = self.window_tree().map(|t| t.path.clone())
        {
            let tab = self.tab;
            self.select_tree(&name);
            // A filter or a folded group hides its tree from the cursor:
            // the windows stay as they are rather than both show this one.
            if self.window_tree().is_none_or(|t| t.path != other.path) {
                self.message = Some(format!(
                    "tree {name} is out of the list; clear the filter or open its group to give it the keys"
                ));
                return;
            }
            self.other = Some(OtherWindow { path: here, tab });
            self.tab = other.tab;
            self.side = side;
            self.save_layout();
        }
        self.is_window_focused = true;
        self.is_inserting = false;
        self.sync_window();
    }

    /// Ctrl+Q: with one window, gives it the keys where it has something to
    /// take them, else gives them back to the table; with two, moves them
    /// on round the table, the left window and the right one.
    pub fn cycle_keys(&mut self) {
        self.is_inserting = false;
        if self.other_window().is_some() {
            match (self.is_window_focused, self.side) {
                (false, _) => self.focus_side(0),
                (true, 0) => self.focus_side(1),
                (true, _) => self.is_window_focused = false,
            }
            return;
        }
        if self.is_window_focused {
            self.is_window_focused = false;
            return;
        }
        let can_take = match self.shown_tab() {
            WindowTab::Chat | WindowTab::Shell => self.window_chat().is_some(),
            WindowTab::Diff | WindowTab::Pr => true,
            WindowTab::Run | WindowTab::Info => false,
        };
        if can_take {
            self.is_window_focused = true;
            self.sync_window();
        }
    }

    /// Keeps the double in `file`, opening it as that file left it.
    pub fn keep_layout_in(&mut self, file: PathBuf) {
        let layout = crate::state::read::<KeptLayout>(&file);
        self.other = layout.other;
        self.side = layout.side;
        self.returning = layout.cursor.filter(|_| self.other.is_some());
        self.layout_file = Some(file);
    }

    /// Keeps the double as it stands: both windows' trees and tabs, so a
    /// restart opens it as it was left. main calls it as the cursor moves.
    pub fn save_layout(&self) {
        let Some(file) = &self.layout_file else {
            return;
        };
        let cursor = self
            .other
            .as_ref()
            .and(self.window_tree())
            .map(|tree| OtherWindow {
                path: tree.path.clone(),
                tab: self.tab,
            });
        let layout = KeptLayout {
            other: self.other.clone(),
            cursor,
            side: self.side,
        };
        crate::state::write(file, &layout);
    }

    /// Puts the cursor back where a double was left, once its tree is
    /// listed and the agents read have sorted the list, which moves rows
    /// under the cursor until then.
    fn return_cursor(&mut self) {
        let Some(cursor) = self.returning.clone().filter(|_| self.is_agents_read) else {
            return;
        };
        let Some(name) = self
            .trees
            .iter()
            .find(|t| t.path == cursor.path)
            .map(|t| t.name.clone())
        else {
            return;
        };
        self.select_tree(&name);
        self.tab = cursor.tab;
        self.returning = None;
    }

    /// A shell left running in tree `name` stays while the tree is held,
    /// and ends once it is returned or destroyed.
    pub fn keeps_shell(&self, name: &str) -> bool {
        self.trees.iter().any(|t| t.name == name && t.is_held())
    }

    /// The plugin whose screen hints the window's terminal is read by: the
    /// window agent's on the chat tab, and none for a shell.
    pub fn window_plugin(&self) -> Option<&str> {
        match self.shown_tab() {
            WindowTab::Chat => self.window_agent()?.plugin.as_deref(),
            _ => None,
        }
    }

    /// The window has the keys on `tab`.
    pub fn has_keys_on(&self, tab: WindowTab) -> bool {
        self.is_window_focused && self.shown_tab() == tab
    }

    /// The terminal the window shows on its chat or shell tab, as its
    /// command, its agent and its tree: the window agent's chat while it
    /// runs, or a shell in the tree. A cold agent is left closed, since
    /// attaching to it would start it again; Enter does that on purpose.
    pub fn window_chat(&self) -> Option<(Vec<String>, String, String)> {
        Self::terminal_of(self.window_tree()?, self.shown_tab())
    }

    /// The terminal a window on `tree` shows on `tab`, as `window_chat`
    /// has it.
    fn terminal_of(tree: &Tree, tab: WindowTab) -> Option<(Vec<String>, String, String)> {
        match tab {
            WindowTab::Chat => {
                let agent = Self::chat_agent(tree)?;
                if agent.status == Some(Status::Cold) {
                    return None;
                }
                Some((agent.attach.clone()?, agent.name.clone(), tree.name.clone()))
            }
            WindowTab::Shell => {
                let dir = tree.path.display().to_string().replace('\'', "'\\''");
                let command = format!("cd '{dir}' && exec \"${{SHELL:-sh}}\"");
                Some((
                    vec!["sh".into(), "-c".into(), command],
                    SHELL.into(),
                    tree.name.clone(),
                ))
            }
            _ => None,
        }
    }

    /// Opens what the window's tab shows for the tree under the cursor, once
    /// the cursor has rested there: its diff, or its PR as last read, read
    /// now where it never has been. `v` reads it afresh.
    pub fn sync_window(&mut self) {
        let Some(tree) = self.window_tree() else {
            return;
        };
        let path = tree.path.clone();
        match self.shown_tab() {
            WindowTab::Diff if self.view.as_ref().is_none_or(|v| v.path != path) => {
                self.open_diff();
            }
            WindowTab::Pr if self.pr_popup.as_ref().is_none_or(|p| p.path != path) => {
                self.pr_popup = Some(PrPopup::new(path.clone()));
                if !self.prs.contains_key(&path) {
                    self.read_pr(path);
                }
            }
            _ => {}
        }
    }

    /// Moves the cursor to tree `name`, so the window shows the chat opened
    /// on it, unless a filter or a folded group hides it.
    pub fn select_tree(&mut self, name: &str) {
        // The main checkout has no row of its own: its base's heading
        // stands for it.
        if let Some(main) = self.trees.iter().find(|t| t.name == name && t.is_main) {
            let branch = main.branch.clone();
            let heading = self.rows().iter().position(|row| {
                matches!(row, ListRow::Heading { group: Group::Base(base), .. } if branch.as_ref() == Some(base))
            });
            if let Some(at) = heading {
                self.selected = at;
            }
            return;
        }
        let path = self
            .trees
            .iter()
            .find(|t| t.name == name)
            .map(|t| t.path.clone());
        self.reselect(path);
    }

    /// The tree under the cursor; None on a heading.
    pub fn current(&self) -> Option<&Tree> {
        match self.rows().get(self.selected) {
            Some(ListRow::Tree(tree)) => Some(tree),
            _ => None,
        }
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

    /// Shows the cached listing until a live one lands, with what places
    /// each tree read first: its sidecar names its group, and its agents and
    /// their reports its place in it. Left to the git clock, they would land
    /// a moment after the first frame, and the trees would show in pool
    /// order under their bases and then move; read here, they cost about a
    /// third of a second before it.
    pub fn set_cached(&mut self, mut trees: Vec<Tree>) {
        crate::cli::read_agents(&mut trees);
        self.apply_reports(crate::reports::Reader::default().read());
        self.replace_trees(trees);
        self.is_agents_read = true;
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
        self.is_work_read = true;
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
        self.return_cursor();
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
        let rows = self.rows();
        let at = cursor.and_then(|c| {
            rows.iter()
                .position(|row| matches!(row, ListRow::Tree(t) if t.path == c))
        });
        let last = rows.len().saturating_sub(1);
        let has_tree = rows.iter().any(|row| matches!(row, ListRow::Tree(_)));
        drop(rows);
        // The first listing puts the cursor on the first tree, not a heading.
        self.selected = match at {
            Some(at) => at,
            None if !self.is_cursor_placed => self.first_tree_row(),
            None => self.selected.min(last),
        };
        self.is_cursor_placed |= has_tree;
        self.return_cursor();
    }

    /// The chat on its way is `job`'s: its window's start, or, for a popup
    /// with no row, the start that opened it; never a scratch chat's.
    fn owns_opening(&self, job: &Job) -> bool {
        self.opening.as_ref().is_some_and(|o| match &o.key {
            Some(key) => *key == job.key,
            None => o.tree != SCRATCH,
        })
    }

    /// A start on row `key` finished `step`.
    pub fn job_step(&mut self, key: PathBuf, step: &'static str) {
        self.steps.entry(key).or_default().push(step);
    }

    /// The start shown in the window under the table, while there is one.
    pub fn window_start(&self) -> Option<&Opening> {
        self.opening.as_ref().filter(|o| o.key.is_some())
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
                // A start shown in the window stays there, saying which
                // step failed and why, until Esc.
                // Another start's window, or a chat on its way for
                // something else, is left as it is.
                if self.owns_opening(&job) {
                    match self.opening.as_mut().filter(|o| o.key.is_some()) {
                        Some(opening) => opening.failed = Some(last_line(&output).to_string()),
                        None => self.opening = None,
                    }
                }
            }
            self.jobs.insert(path, JobState::Failed(job.kind));
            self.message = Some(format!(
                "{} of tree {} failed: {}  (its output is in the pane below)",
                job.kind.noun(),
                job.tree.name,
                last_line(&output)
            ));
        } else if job.kind == Kind::Start
            && task.is_some()
            && self
                .opening
                .as_ref()
                .is_none_or(|o| o.key.as_ref() != Some(&job.key))
        {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            self.message = Some(if is_assistant {
                "the assistant started on your message; a opens its chat".to_string()
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
            // on the popup means it is opened later, with `a`.
            let known = self
                .trees
                .iter()
                .find(|t| t.path == path)
                .map(|t| t.agents.iter().map(|a| a.pid).collect())
                .unwrap_or_default();
            if self.window_start().is_some() {
                self.message = Some(format!(
                    "agent started in tree {}; its chat opens here",
                    job.tree.name
                ));
            }
            let owns = self.owns_opening(&job);
            match (self.opening.as_mut().filter(|_| owns), job.attach.clone()) {
                (None, _) => {
                    self.message = Some(if is_assistant {
                        "the assistant started; a opens its chat".to_string()
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
        } else if job.kind == Kind::Handoff {
            self.jobs
                .insert(path.clone(), JobState::Done(job.kind, Instant::now()));
            self.message = Some(format!(
                "tree {}'s work is with a new agent; Enter opens its chat  ({})",
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
        let last = self.rows().len().saturating_sub(1);
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

    /// A diff read, or the coloured copy of one already shown, which keeps
    /// the counts it was asked at; a copy of a diff dropped since, by `^R`
    /// or moved counts, is dropped too.
    pub fn diff_loaded(&mut self, path: PathBuf, layout: Result<Layout, String>) {
        let work = match self.diff_requests.remove(&path) {
            Some(work) => work,
            None => match self.diffs.get(&path) {
                Some(loaded) => loaded.work,
                None => return,
            },
        };
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

    /// The keys a window takes in Normal, on a tab with no keys of its own:
    /// Esc or q gives them back to the table, the arrows step through its
    /// tabs, and on a chat or shell i or Enter starts Insert while j, k, d
    /// and u scroll it.
    fn handle_window_key(&mut self, key: KeyEvent) -> Outcome {
        let is_terminal = matches!(self.shown_tab(), WindowTab::Chat | WindowTab::Shell);
        let half = isize::try_from(self.window_rows / 2).map_or(1, |h| h.max(1));
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.is_window_focused = false,
            KeyCode::Left | KeyCode::Right => self.step_tab(key.code == KeyCode::Right),
            KeyCode::Char('i') | KeyCode::Enter if is_terminal => {
                self.is_inserting = self.window_chat().is_some();
            }
            KeyCode::Char('j') | KeyCode::Down if is_terminal => return Outcome::ScrollChat(1),
            KeyCode::Char('k') | KeyCode::Up if is_terminal => return Outcome::ScrollChat(-1),
            KeyCode::Char('d') if is_terminal => return Outcome::ScrollChat(half),
            KeyCode::Char('u') if is_terminal => return Outcome::ScrollChat(-half),
            _ => {}
        }
        Outcome::Continue
    }

    /// The wheel over the window's diff or PR, with the keys or without:
    /// three lines a step, as the arrows move them, leaving the keys where
    /// they are. A chat takes the wheel itself.
    pub fn wheel_window(&mut self, is_down: bool) {
        let key = KeyEvent::from(if is_down { KeyCode::Down } else { KeyCode::Up });
        for _ in 0..3 {
            match self.shown_tab() {
                WindowTab::Diff if self.view.is_some() => self.handle_view_key(key),
                WindowTab::Pr if self.pr_popup.is_some() => self.handle_pr_key(key),
                _ => return,
            };
        }
    }

    /// The window with the keys is in Insert, on a chat or shell: every key
    /// but treetop's few Ctrl keys goes to it. A chat floating where its
    /// window cannot show has no Normal to show, so it is always in Insert.
    pub fn is_inserting(&self) -> bool {
        (self.is_inserting || self.chat_floats())
            && self.is_window_focused
            && matches!(self.shown_tab(), WindowTab::Chat | WindowTab::Shell)
    }

    /// The window cannot show as last drawn, over the Issues screen or on a
    /// terminal too short for it, so a tree's chat with the keys floats in
    /// a popup instead.
    pub fn chat_floats(&self) -> bool {
        self.is_issues || self.window_rows == 0
    }

    /// Gives the window under the table the keys in Insert, so what is
    /// typed goes to its chat: Enter on a tree, the inbox, or a click.
    pub fn insert_in_window(&mut self) {
        self.is_window_focused = true;
        self.is_inserting = true;
    }

    /// Steps the window to its next tab, or its last, in Normal.
    fn step_tab(&mut self, is_next: bool) {
        let tabs = self.window_tabs();
        let at = tabs
            .iter()
            .position(|t| *t == self.shown_tab())
            .unwrap_or(0);
        let step = if is_next { 1 } else { tabs.len() - 1 };
        self.tab = tabs[(at + step) % tabs.len()];
        self.is_inserting = false;
    }

    /// Shows `tab` in the window under the table with the keys, where the
    /// tree under the cursor has it.
    fn show_tab(&mut self, tab: WindowTab) {
        if self.window_tabs().contains(&tab) {
            self.tab = tab;
            self.is_window_focused = true;
            self.is_inserting = false;
        }
    }

    /// Opens the PR popup on the tree under the cursor, reading its PR
    /// afresh: comments and reviews land without a push, so the last read
    /// shows only until this one comes in.
    fn open_pr_view(&mut self) {
        let Some(tree) = self.current() else { return };
        if !tree.is_held() {
            self.message = Some(format!("tree {} is available: it has no PR", tree.name));
            return;
        }
        let path = tree.path.clone();
        self.pr_popup = Some(PrPopup::new(path.clone()));
        self.read_pr(path);
    }

    fn read_pr(&mut self, path: PathBuf) {
        self.prs.entry(path.clone()).or_default().is_reading = true;
        self.pr_wanted = Some(path);
    }

    /// The PR the popup wants read: the tree, its branch, and the ship
    /// plugin that reads it. Where none can, the popup says so instead.
    pub fn wanted_pr(&mut self) -> Option<(PathBuf, String, Plugin)> {
        let path = self.pr_wanted.take()?;
        let branch = self
            .trees
            .iter()
            .find(|t| t.path == path)
            .and_then(|t| t.branch.clone());
        let plugin = ship::answering(&self.ship_plugins, "view").cloned();
        let why = match (branch, plugin) {
            (Some(branch), Some(plugin)) => return Some((path, branch, plugin)),
            (None, _) => "the tree is on no branch, so it has no PR",
            (_, None) => "none of this repository's ship plugins can read a PR whole",
        };
        self.prs.insert(
            path,
            PrRead {
                pr: Some(Err(why.to_string())),
                is_reading: false,
            },
        );
        None
    }

    /// A PR read for the popup, or why it could not be.
    pub fn pr_loaded(&mut self, path: PathBuf, read: Result<Option<PrView>, plugin::Error>) {
        let read = read.map_err(|err| match err.code {
            plugin::Code::Auth => {
                "gh is not signed in, so treetop cannot read this PR. Run gh auth login."
                    .to_string()
            }
            _ => err.to_string(),
        });
        self.prs.insert(
            path.clone(),
            PrRead {
                pr: Some(read),
                is_reading: false,
            },
        );
        if let Some(popup) = self.pr_popup.as_mut().filter(|p| p.path == path) {
            let pr = self.prs.get(&path).and_then(|r| r.pr.clone()?.ok()?);
            match pr {
                Some(pr) => clamp(popup, &pr),
                None => popup.tab = Tab::Overview,
            }
        }
    }

    fn handle_pr_key(&mut self, key: KeyEvent) -> Outcome {
        let Some(path) = self.pr_popup.as_ref().map(|p| p.path.clone()) else {
            return Outcome::Continue;
        };
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('c') => return Outcome::Quit,
                KeyCode::Char('r') => self.read_pr(path),
                KeyCode::Char('q' | 'Q') => self.cycle_keys(),
                KeyCode::Char('f' | 'F') => self.toggle_zoom(Zoom::Windows),
                KeyCode::Char('p' | 'P') => self.open_palette(),
                _ => {}
            }
            return Outcome::Continue;
        }
        // The arrows step the window's tabs, as on every tab in Normal; h
        // and l step the PR's own.
        if matches!(key.code, KeyCode::Left | KeyCode::Right) {
            self.step_tab(key.code == KeyCode::Right);
            return Outcome::Continue;
        }
        // Esc gives the keys back to the table; the PR stays in its tab.
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
            self.is_window_focused = false;
            return Outcome::Continue;
        }
        let pr = self.prs.get(&path).and_then(|r| r.pr.clone()?.ok()?);
        let Some(pr) = pr else {
            return Outcome::Continue;
        };
        let Some(popup) = self.pr_popup.as_mut() else {
            return Outcome::Continue;
        };
        let tabs = pr_view::tabs(&pr);
        let at = tabs.iter().position(|t| *t == popup.tab).unwrap_or(0);
        // Anything but a scroll moves off the item, so starts it at its top.
        if !matches!(
            key.code,
            KeyCode::Char('d' | 'u' | 'f' | 'F' | 'O') | KeyCode::Enter
        ) {
            popup.offset = 0;
        }
        let index = popup.tab.index();
        let last = pr_view::last_row(&pr, popup.tab, popup.nitpicks);
        let step = |popup: &mut PrPopup, delta: isize| {
            popup.selected[index] = popup.selected[index].saturating_add_signed(delta).min(last);
        };
        match key.code {
            KeyCode::Char(c @ '1'..='4') => {
                let wanted = usize::from(c as u8 - b'1');
                if let Some(tab) = tabs.get(wanted) {
                    popup.tab = *tab;
                }
            }
            KeyCode::Char('l') => popup.tab = tabs[(at + 1) % tabs.len()],
            KeyCode::Char('h') => {
                popup.tab = tabs[(at + tabs.len() - 1) % tabs.len()];
            }
            KeyCode::Down | KeyCode::Char('j') => step(popup, 1),
            KeyCode::Up | KeyCode::Char('k') => step(popup, -1),
            KeyCode::Char('d') if popup.tab == Tab::Overview => step(popup, SCROLL),
            KeyCode::Char('u') if popup.tab == Tab::Overview => step(popup, -SCROLL),
            KeyCode::Char('d') => popup.offset += SCROLL.unsigned_abs(),
            KeyCode::Char('u') => popup.offset = popup.offset.saturating_sub(SCROLL.unsigned_abs()),
            KeyCode::Char('g') => popup.selected[index] = 0,
            KeyCode::Char('G') => step(popup, isize::MAX),
            KeyCode::Char('z') if popup.tab == Tab::CodeRabbit => {
                popup.nitpicks = !popup.nitpicks;
                clamp(popup, &pr);
            }
            KeyCode::Char('O') => {
                if let Some(url) = pr.url.clone() {
                    return Outcome::OpenUrl(url);
                }
            }
            KeyCode::Enter if popup.tab == Tab::Checks => {
                if let Some(url) = pr.checks.get(popup.selected()).and_then(|c| c.url.clone()) {
                    return Outcome::OpenUrl(url);
                }
            }
            KeyCode::Char('f') => {
                let selected = popup.selected();
                let (what, text) = match popup.tab {
                    Tab::Overview => return Outcome::Continue,
                    Tab::Checks => match pr.checks.get(selected) {
                        Some(check) => match pr_view::check_message(&pr, check) {
                            Some(text) => (format!("{} on #{}", check.name, pr.number), text),
                            None => {
                                self.message = Some(format!("{} has not failed", check.name));
                                return Outcome::Continue;
                            }
                        },
                        None => return Outcome::Continue,
                    },
                    Tab::Comments => match pr.threads.get(selected) {
                        Some(thread) => (
                            format!("{}'s comment", thread.author),
                            pr_view::thread_message(&pr, thread),
                        ),
                        None => return Outcome::Continue,
                    },
                    Tab::CodeRabbit => match pr_view::findings(&pr, popup.nitpicks).get(selected) {
                        Some(finding) => (
                            format!(
                                "CodeRabbit on {}",
                                pr_view::place(finding.path.as_deref(), finding.lines.as_deref())
                            ),
                            pr_view::finding_message(&pr, finding),
                        ),
                        None => return Outcome::Continue,
                    },
                };
                return self.send_to_tree(&path, &what, text);
            }
            KeyCode::Char('F') if popup.tab == Tab::CodeRabbit => {
                let open = pr_view::actionable(&pr);
                if open.is_empty() {
                    self.message = Some("CodeRabbit has nothing open to send".into());
                    return Outcome::Continue;
                }
                let what = format!("{} CodeRabbit comments", open.len());
                let text = pr_view::findings_message(&pr, &open);
                return self.send_to_tree(&path, &what, text);
            }
            _ => {}
        }
        Outcome::Continue
    }

    /// `text` to the agents in the tree at `path`, saying `what` went.
    fn send_to_tree(&mut self, path: &PathBuf, what: &str, text: String) -> Outcome {
        let Some(tree) = self.trees.iter().find(|t| &t.path == path) else {
            return Outcome::Continue;
        };
        let to = Self::recipients(tree);
        if to.is_empty() {
            self.message = Some(format!("no agent in tree {} to send it to", tree.name));
            return Outcome::Continue;
        }
        self.message = Some(format!("sent {what} to the agent in tree {}", tree.name));
        Outcome::Send { to, text }
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
                KeyCode::Char('q' | 'Q') => self.cycle_keys(),
                KeyCode::Char('f' | 'F') => self.toggle_zoom(Zoom::Windows),
                KeyCode::Char('p' | 'P') => self.open_palette(),
                _ => {}
            }
            return Outcome::Continue;
        }
        // The arrows step the window's tabs, as on every tab in Normal; h
        // and l scroll the diff sideways.
        if matches!(key.code, KeyCode::Left | KeyCode::Right) {
            self.step_tab(key.code == KeyCode::Right);
            return Outcome::Continue;
        }
        // Esc gives the keys back to the table; the diff stays in its tab.
        if matches!(key.code, KeyCode::Esc | KeyCode::Tab | KeyCode::Char('q')) {
            self.is_window_focused = false;
            return Outcome::Continue;
        }
        let (len, starts) = match self.diffs.get(&path).map(|d| &d.layout) {
            Some(Ok(layout)) => (layout.rows.len(), layout.file_starts.clone()),
            _ => (0, Vec::new()),
        };
        // The window under its title bar, less the file header pinned at
        // its top and the key bar at its bottom.
        let page = isize::try_from(self.window_rows.saturating_sub(3)).map_or(1, |p| p.max(1));
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
            KeyCode::Char('l') => view.hscroll = view.hscroll.saturating_add(8),
            KeyCode::Char('h') => view.hscroll = view.hscroll.saturating_sub(8),
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
        Self::chat_of(tree).unwrap_or_else(|| Outcome::Enter(tree.clone()))
    }

    /// The chat of the tree's leading agent that has one, as Enter opens it.
    fn chat_of(tree: &Tree) -> Option<Outcome> {
        Self::agents_of(tree).into_iter().find_map(|agent| {
            Some(Outcome::Chat {
                command: agent.attach.clone()?,
                agent: agent.name.clone(),
                tree: tree.name.clone(),
            })
        })
    }

    /// The chat of an agent `n` started: at once when its start printed the
    /// id to attach to, else once it has shown up in its tree with a chat to
    /// open. Gives up after AWAIT_CHAT, saying so.
    pub fn ready_chat(&mut self) -> Option<Outcome> {
        // A tree's chat opens in the window under the table, on its tree, so
        // a new tree's waits until the pool lists it, as long as AWAIT_CHAT.
        if let Some(Outcome::Chat { tree, .. }) = &self.started_chat
            && self.window_start().is_some()
            && !self.trees.iter().any(|t| &t.name == tree)
        {
            let since = *self.started_at.get_or_insert_with(Instant::now);
            if since.elapsed() < AWAIT_CHAT {
                return None;
            }
        }
        if let Some(chat) = self.started_chat.take() {
            self.opening = None;
            self.awaiting_chat = None;
            self.started_at = None;
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
            .filter(|t| !t.is_main && !shown.iter().any(|s| s.path == t.path))
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
        // A window with the keys holds them in Normal too, so an ask never
        // swaps out the chat or shell being read there.
        !self.is_window_focused
            && self.prompt.is_none()
            && self.picker.is_none()
            && self.port_menu.is_none()
            && self.phone.is_none()
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
            && self.rail.is_none()
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
        // Until agents and reports have both been read, an ask still under
        // way looks finished, and forgetting it would open it again.
        if !self.is_read() {
            return None;
        }
        let before = self.asked.len();
        self.asked
            .retain(|key| asks.iter().any(|(.., command)| command.join(" ") == *key));
        if self.asked.len() != before {
            self.save_asked();
        }
        if !self.is_free() {
            return None;
        }
        let (path, tree, agent, command) = asks
            .into_iter()
            .find(|(.., command)| !self.asked.contains(&command.join(" ")))?;
        self.mark_asked(&command);
        self.reveal(path);
        Some(Outcome::Chat {
            command,
            agent,
            tree,
        })
    }

    /// Puts the cursor on the tree at `path`, clearing a filter that hides
    /// it and opening its folded group.
    fn reveal(&mut self, path: PathBuf) {
        if !self.visible().iter().any(|t| t.path == path) {
            self.filter.clear();
            let group = self
                .trees
                .iter()
                .find(|t| t.path == path)
                .and_then(|t| self.group_of(t));
            if let Some(group) = group.filter(|g| self.is_folded(g)) {
                self.toggle_fold(&group);
            }
        }
        self.reselect(Some(path));
    }

    /// The agent running the chat `command` has had its ask on screen.
    pub fn has_asked(&self, command: &str) -> bool {
        self.asked.contains(command)
    }

    /// Keeps the inbox's marks in `file`, starting from those it holds.
    pub fn keep_inbox_in(&mut self, file: PathBuf) {
        self.inbox = crate::state::read(&file);
        self.inbox_file = Some(file);
    }

    fn save_inbox(&mut self) {
        if let Some(file) = &self.inbox_file {
            self.inbox.write(file);
        }
    }

    /// The list is on screen with nothing over it but a dialog, and the
    /// inbox beside it, so a click can open an entry. Elsewhere the mouse
    /// is left to the terminal, so text selects as it always has.
    pub fn wants_mouse(&self) -> bool {
        // A chat or shell read in Normal takes the wheel, to scroll it, and
        // a click, to type to it.
        let is_reading = self.is_window_focused
            && matches!(self.shown_tab(), WindowTab::Chat | WindowTab::Shell);
        (!self.inbox.is_hidden || self.rail.is_some() || is_reading)
            && !self.is_issues
            && !self.has_keys_on(WindowTab::Diff)
            && !self.has_keys_on(WindowTab::Pr)
            && self.prompt.is_none()
            && self.picker.is_none()
            && self.port_menu.is_none()
            && self.phone.is_none()
            && self.scratch_menu.is_none()
            && self.opening.is_none()
            && !self.is_help
    }

    /// The keys while the inbox has them: move, open, dismiss, and back to
    /// the list.
    fn handle_inbox_key(&mut self, key: KeyEvent, at: usize) -> Outcome {
        let last = crate::inbox::shown(self).len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.rail = Some((at + 1).min(last)),
            KeyCode::Up | KeyCode::Char('k') => self.rail = Some(at.saturating_sub(1)),
            KeyCode::Home | KeyCode::Char('g') => self.rail = Some(0),
            KeyCode::End | KeyCode::Char('G') => self.rail = Some(last),
            KeyCode::Enter => return self.open_entry(at),
            KeyCode::Char('x') => {
                if let Some((entry, _)) = crate::inbox::shown(self).into_iter().nth(at) {
                    self.inbox.dismissed.insert(entry.key);
                    self.save_inbox();
                    self.rail = Some(at.min(last.saturating_sub(1)));
                }
            }
            KeyCode::Char('E') => self.toggle_inbox(),
            // Tab opens Issues from the list, but here it only gives the
            // keys back, as Esc does.
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('e') | KeyCode::Left => self.rail = None,
            KeyCode::Char('?') => self.is_help = true,
            KeyCode::Char('q') => return self.quit(),
            _ => {}
        }
        Outcome::Continue
    }

    /// `E`: hides the inbox, giving the list its width back, or shows it.
    fn toggle_inbox(&mut self) {
        self.inbox.is_hidden = !self.inbox.is_hidden;
        self.rail = None;
        self.save_inbox();
    }

    /// Opens the inbox's entry at `at`, as Enter on its tree's row would,
    /// and marks it seen. A PR's entry puts the cursor on its tree, with
    /// its checks in the detail pane; another workspace's switches there
    /// first.
    pub fn open_entry(&mut self, at: usize) -> Outcome {
        let Some((entry, _)) = crate::inbox::shown(self).into_iter().nth(at) else {
            return Outcome::Continue;
        };
        self.rail = None;
        self.inbox.seen.insert(entry.key.clone());
        self.save_inbox();
        if let Some(away) = entry.away {
            return Outcome::SwitchTo {
                checkout: away.checkout,
                tree: entry.path,
                kind: entry.kind,
            };
        }
        self.open_tree(entry.path, entry.kind)
    }

    /// Puts the cursor on the tree at `path` and opens what an inbox entry
    /// of `kind` for it opens: for a PR, its checks in the detail pane;
    /// else its agent, as Enter on its row does.
    fn open_tree(&mut self, path: PathBuf, kind: crate::inbox::Kind) -> Outcome {
        self.reveal(path.clone());
        let Some(tree) = self.trees.iter().find(|t| t.path == path) else {
            return Outcome::Continue;
        };
        match kind {
            crate::inbox::Kind::Failed => {
                self.message = Some(format!(
                    "tree {}'s PR failed its checks; f sends the failure to its agent",
                    tree.name
                ));
                Outcome::Continue
            }
            crate::inbox::Kind::Merged => {
                self.message = Some(format!("tree {}'s PR merged; r returns it", tree.name));
                Outcome::Continue
            }
            _ => Self::chat_of(tree).unwrap_or_else(|| Outcome::Enter(tree.clone())),
        }
    }

    /// What the inbox entry another workspace switched here for opens, once
    /// the agents are read and nothing else is on screen, the cursor going
    /// to its tree either way: its chat, or inside tmux the pane its agent
    /// runs in. Outside tmux an agent with no chat is left for `t`, rather
    /// than a shell opening by itself.
    pub fn arrival(&mut self) -> Option<Outcome> {
        if !self.is_agents_read || !self.is_free() {
            return None;
        }
        let (path, kind) = self.arriving.take()?;
        if !self.trees.iter().any(|t| t.path == path) {
            self.message = Some(format!("{} is no longer in this pool", path.display()));
            return None;
        }
        match self.open_tree(path, kind) {
            Outcome::Enter(tree) if !self.is_in_tmux => {
                self.message = Some(format!(
                    "tree {} has no chat to open here; t goes to it",
                    tree.name
                ));
                None
            }
            Outcome::Continue => None,
            outcome => Some(outcome),
        }
    }

    /// The chat running `command` is on screen, so whatever its agent asks
    /// now has been seen and does not open again when it closes.
    pub fn saw_chat(&mut self, command: &[String]) {
        if self.asks().iter().any(|(.., c)| c.as_slice() == command)
            && !self.asked.contains(&command.join(" "))
        {
            self.mark_asked(command);
        }
    }

    /// The ask in the chat `command` has been on screen: it does not open
    /// again here, and its inbox entry is seen from every workspace, since
    /// the asks seen are kept per workspace but the inbox is one.
    fn mark_asked(&mut self, command: &[String]) {
        let command = command.join(" ");
        let keys: Vec<String> = crate::inbox::entries(self)
            .into_iter()
            .filter(|e| e.asking.as_deref() == Some(command.as_str()))
            .map(|e| e.key)
            .collect();
        self.asked.insert(command);
        self.save_asked();
        if !keys.is_empty() {
            self.inbox.seen.extend(keys);
            self.save_inbox();
        }
    }

    /// Takes the ports this machine shares, as the refresher read them.
    pub fn apply_shares(&mut self, shares: Vec<Shared>) {
        self.shares = shares
            .into_iter()
            .map(|share| (share.port, share))
            .collect();
    }

    /// How far `port` is shared: None where it is not, Some(false) for the
    /// tailnet, as `p` shares it, Some(true) for the internet, as `P` does.
    pub fn reach_of(&self, port: u16) -> Option<bool> {
        self.shares.get(&port).map(|share| share.is_public)
    }

    /// Takes the latest release's tag, kept only when it is newer.
    pub fn apply_release(&mut self, tag: String) {
        self.newer = crate::update::is_newer(&tag).then_some(tag);
    }

    /// Agents and reports have both been read, so the pool is known in full.
    pub fn is_read(&self) -> bool {
        self.is_agents_read && self.last_states.is_some()
    }

    /// Takes where the main checkout stands after a catch-up, and says so on
    /// the status line when it moved.
    pub fn apply_base(&mut self, standing: crate::base::Standing) {
        self.behind = standing.behind();
        if let crate::base::Standing::Forwarded(n) = standing {
            let branch = self
                .trees
                .iter()
                .find(|t| t.is_main)
                .and_then(|t| t.branch.clone())
                .unwrap_or_else(|| "main checkout".into());
            let commits = if n == 1 { "commit" } else { "commits" };
            self.message = Some(format!("{branch} fast-forwarded {n} {commits}"));
        }
    }

    /// Keeps the asks seen in `file`, starting from those it already holds.
    pub fn keep_asked_in(&mut self, file: PathBuf) {
        self.asked = crate::state::read(&file);
        self.asked_file = Some(file);
    }

    fn save_asked(&self) {
        if let Some(file) = &self.asked_file {
            crate::state::write(file, &self.asked);
        }
    }

    /// The chat of the next tree down the list, as filtered, that has one,
    /// after tree `from`, wrapping past the end. The cursor moves to that
    /// tree, so leaving the chat lands on it. None when no other tree has a
    /// chat to open.
    pub fn next_chat(&mut self, from: &str) -> Option<Outcome> {
        let visible = self.visible();
        let start = visible.iter().position(|t| t.name == from)?;
        let (path, agent, command, tree) = (1..visible.len())
            .map(|step| (start + step) % visible.len())
            .find_map(|index| {
                let tree = visible[index];
                let agent = Self::agents_of(tree)
                    .into_iter()
                    .find(|a| a.attach.is_some())?;
                Some((
                    tree.path.clone(),
                    agent.name.clone(),
                    agent.attach.clone()?,
                    tree.name.clone(),
                ))
            })?;
        self.reselect(Some(path));
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
                        State::Failed => was.is_some_and(|w| w != State::Failed),
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
            let source = tree.sidecar.as_ref().and_then(|s| s.source.clone());
            self.landed.insert(tree.path.clone());
            self.landings.push(Job {
                key: PathBuf::from(format!("(merge of {})", tree.path.display())),
                landing: Some(Pick {
                    id: ticket,
                    checkout: self.checkout.clone(),
                    source,
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

    /// Starts the tree's stopped or failed agent again, in its tree, with its
    /// conversation and the flags it was started with, rather than a fresh
    /// dispatch that would lose them.
    fn resume(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        let (name, path) = (tree.name.clone(), tree.path.clone());
        self.message = if let Some(agent) = tree.agents.iter().find(|a| a.pid.is_some()) {
            Some(format!(
                "{} is running in tree {name}; Enter opens its chat",
                agent.name
            ))
        } else if self.jobs.get(&path).is_some_and(|s| s.is_active()) {
            Some(format!("tree {name} already has an action queued"))
        } else {
            let job = Job::new(Kind::Resume, tree.clone());
            self.jobs.insert(path, JobState::Queued(Kind::Resume));
            return Outcome::Queue(vec![job]);
        };
        Outcome::Continue
    }

    /// `h`: asks the agent in the tree under the cursor to write a handoff,
    /// the prompt of a new agent to take its work over in the same tree,
    /// for when its context has filled. Once written, the handoff opens in
    /// the compose box to be read over before the new agent starts on it.
    /// `h` again while it is written stops waiting.
    fn hand_off(&mut self) -> Outcome {
        let Some(tree) = self.current() else {
            return Outcome::Continue;
        };
        let (name, path) = (tree.name.clone(), tree.path.clone());
        if let Some(handing) = &self.handing {
            self.message = Some(if handing.path == path {
                self.handing = None;
                format!("stopped waiting for tree {name}'s handoff")
            } else {
                format!(
                    "tree {}'s handoff is being written; one at a time",
                    handing.tree
                )
            });
            return Outcome::Continue;
        }
        let agent = Self::agents_of(tree).into_iter().next();
        let why = if tree.is_main || !tree.is_held() {
            format!("no agent's work in tree {name} to hand off")
        } else if tree.sidecar.is_none() {
            format!("treetop did not lease tree {name}, so it cannot hand its work on")
        } else if self.jobs.get(&path).is_some_and(|s| s.is_active()) {
            format!("tree {name} already has an action queued")
        } else {
            match agent {
                None => format!("no agent in tree {name} to hand off from"),
                Some(agent) if agent.pid.is_none() => {
                    format!("{} is not running; R starts it again first", agent.name)
                }
                Some(agent) => match Recipient::of(agent, &name) {
                    Some(from) if from.plugin.is_some() && from.job.is_some() => {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map_or(0, |since| since.as_secs());
                        let file = handoff::file(&path, now);
                        let text = handoff::request(&file);
                        self.message = Some(format!(
                            "asking {} for a handoff; it opens here once written, h again stops waiting",
                            from.agent
                        ));
                        self.handing = Some(Handing {
                            path,
                            tree: name,
                            from: from.clone(),
                            file,
                            since: Instant::now(),
                        });
                        return Outcome::Send {
                            to: vec![from],
                            text,
                        };
                    }
                    _ => format!("{} has no chat to ask for a handoff", agent.name),
                },
            }
        };
        self.message = Some(why);
        Outcome::Continue
    }

    /// The tree at `path` is waiting on its agent's handoff.
    pub fn is_handing_off(&self, path: &PathBuf) -> bool {
        self.handing.as_ref().is_some_and(|h| &h.path == path)
    }

    /// The file to read the handoff from, once its agent has stopped working
    /// and you are free on the list to read it over. Gives up after
    /// `handoff::WAIT`, saying so.
    pub fn wanted_handoff(&mut self) -> Option<PathBuf> {
        let handing = self.handing.as_ref()?;
        if handing.since.elapsed() >= handoff::WAIT {
            self.message = Some(format!(
                "{} wrote no handoff to {}; h asks again",
                handing.from.agent,
                handing.file.display()
            ));
            self.handing = None;
            return None;
        }
        let is_working = self
            .trees
            .iter()
            .filter(|t| t.path == handing.path)
            .flat_map(|t| &t.agents)
            .any(|a| a.job == handing.from.job && a.status == Some(Status::Busy));
        (!is_working && self.is_free()).then(|| handing.file.clone())
    }

    /// Opens the handoff read from its file in the compose box, to read
    /// over, change, or send to a new agent in its tree.
    pub fn handoff_written(&mut self, text: String) {
        let Some(handing) = self.handing.take() else {
            return;
        };
        let Some(tree) = self.trees.iter().find(|t| t.path == handing.path).cloned() else {
            self.message = Some(format!("tree {} has left the pool", handing.tree));
            return;
        };
        self.prompt = Some(Prompt::handoff(tree, handing.from, text.trim().to_string()));
    }

    /// The request for a handoff never reached `agent` in `tree`, so there
    /// is nothing to wait for.
    pub fn handoff_unsent(&mut self, agent: &str, tree: &str) {
        if self
            .handing
            .as_ref()
            .is_some_and(|h| h.from.agent == agent && h.tree == tree)
        {
            self.handing = None;
        }
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
        let at = path.and_then(|p| {
            self.rows()
                .iter()
                .position(|row| matches!(row, ListRow::Tree(t) if t.path == p))
        });
        if let Some(at) = at {
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
    /// starting on the pool's base and stepping through the repository's
    /// branches.
    fn open_dispatch(&mut self) {
        let mut prompt = Prompt::dispatch(Target::NewTree, self.user.clone());
        prompt.default_base = self.base.clone();
        let mut bases = crate::branch::bases(&self.checkout);
        if let Some(base) = &self.base {
            bases.retain(|b| b != base);
            bases.insert(0, base.clone());
        }
        prompt.bases = bases;
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
    /// a free one is leased for it, a held one keeps its branch. On the main
    /// checkout's heading it starts there, on whatever it has checked out.
    fn open_start(&mut self) {
        let Some(tree) = self.current().or(self.main_under_cursor()).cloned() else {
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
            Edit::Editor => Outcome::Editor,
        }
    }

    fn submit(&mut self, prompt: Prompt) -> Outcome {
        let text = prompt.text.trim().to_string();
        // Only a branch you named goes; one that only follows the task is
        // named when the agent starts, from what the task says.
        let branch = prompt.branch.clone().filter(|_| prompt.names_branch());
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
                self.message = Some(if tree.is_main {
                    "starting an agent in the main checkout...".to_string()
                } else {
                    format!("starting an agent in tree {}...", tree.name)
                });
                self.start(*tree, launch)
            }
            Purpose::Handoff { tree, from } => self.take_over(*tree, from, text),
            Purpose::Dispatch { target } => match target {
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
        // never an idle start. A start from the compose box shows in the
        // window under the table, its steps as they end, and its chat opens
        // there once it runs; the assistant's opens only when idle, as it
        // answers in the background.
        let is_idle = launch.task.trim().is_empty();
        if launch.ticket.is_none() && (is_idle || !launch.is_assistant) {
            self.opening = Some(Opening {
                agent: launch
                    .name
                    .clone()
                    .or_else(|| crate::dispatch::session_name(&launch.task))
                    .unwrap_or_else(|| "new agent".into()),
                tree: tree.name.clone(),
                key: (!launch.is_assistant).then(|| tree.path.clone()),
                failed: None,
                leases: launch.is_new_tree || !tree.is_held(),
                side: self.other_window().is_some().then_some(self.side),
            });
            self.steps.remove(&tree.path);
            self.started_at = None;
        }
        let job = Job {
            launch: Some(launch),
            ..Job::new(Kind::Start, tree)
        };
        self.jobs
            .insert(job.key.clone(), JobState::Queued(Kind::Start));
        Outcome::Queue(vec![job])
    }

    /// Queues a new agent in `tree` on `handoff`, under the name of `from`,
    /// whose work it takes over, and which stops once it runs.
    fn take_over(&mut self, tree: Tree, from: Recipient, handoff: String) -> Outcome {
        if handoff.is_empty() {
            self.message = Some("nothing to hand off".into());
            return Outcome::Continue;
        }
        if self.jobs.get(&tree.path).is_some_and(|s| s.is_active()) {
            self.message = Some(format!("tree {} already has an action queued", tree.name));
            return Outcome::Continue;
        }
        let (Some(plugin), Some(id)) = (from.plugin, from.job) else {
            return Outcome::Continue;
        };
        self.message = Some(format!(
            "starting a new agent in tree {} on {}'s handoff...",
            tree.name, from.agent
        ));
        let job = Job {
            launch: Some(Launch {
                name: Some(from.agent.clone()),
                ..Launch::agent(handoff, false)
            }),
            replaces: Some(Replaced {
                agent: from.agent,
                plugin,
                id,
            }),
            ..Job::new(Kind::Handoff, tree)
        };
        self.jobs
            .insert(job.key.clone(), JobState::Queued(Kind::Handoff));
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
    /// `id`, in issue source `source`, at once on screen; returns the tree
    /// and its record for the program to write.
    pub fn attach(&mut self, id: String, source: String) -> Option<(PathBuf, Sidecar)> {
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
        record.source = Some(source);
        tree.sidecar = Some(Box::new(record.clone()));
        self.message = Some(format!("tree {name} works on {id}"));
        Some((path, record))
    }

    /// The Issues screen was left without an issue chosen.
    pub fn stop_attaching(&mut self) {
        self.attaching = None;
    }

    /// The tree a ticket has been picked up in, by its sidecar.
    pub fn tree_of_ticket(&self, id: &str) -> Option<&Tree> {
        self.trees.iter().find(|t| {
            t.sidecar
                .as_ref()
                .and_then(|s| s.ticket.as_deref())
                .is_some_and(|ticket| ticket.eq_ignore_ascii_case(id))
        })
    }

    /// Picks up issue `id`, in issue source `source`, from the Issues screen:
    /// a new agent on it in a fresh tree, back on the trees. An issue already in a tree is not
    /// picked up twice; the cursor goes to its tree instead.
    pub fn pick(&mut self, id: String, source: String) -> Outcome {
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
        self.start(tree, Launch::pick(id, self.checkout.clone(), Some(source)))
    }

    /// Opens the port the tree under the cursor serves in the browser,
    /// shares it with the phone or the internet or takes that share down
    /// again, or shows the code of one already up, as `reach` says; where
    /// the tree serves several, such as a frontend and a backend, it offers
    /// a menu of them first.
    fn choose_port(&mut self, reach: Reach) -> Outcome {
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
            [(port, _)] => self.use_port(*port, tree_name, reach),
            _ => {
                self.port_menu = Some(PortMenu {
                    tree: tree_name,
                    ports,
                    selected: 0,
                    reach,
                });
                Outcome::Continue
            }
        }
    }

    /// Opens `port` of tree `tree` in the browser, or shares it with the
    /// phone or the internet, showing its code once it has an address.
    fn use_port(&mut self, port: u16, tree: String, reach: Reach) -> Outcome {
        let shared = self.shares.get(&port).cloned();
        let is_public = match reach {
            Reach::Browser => return Outcome::Browse { port, tree },
            // A share already up costs no command: the shares listing
            // carries the address its code is drawn from.
            Reach::Code => {
                let Some(share) = shared else {
                    self.message = Some(format!(
                        ":{port} is not shared; p shares it with your phone, P with the internet"
                    ));
                    return Outcome::Continue;
                };
                let code = Code::new(&share.address).map(|code| (share.address, code));
                self.phone = Some(Phone {
                    tree,
                    port,
                    shared: Some(code),
                    is_public: share.is_public,
                });
                return Outcome::Continue;
            }
            // Both keys toggle what they turn on. The unshare command takes
            // the tailnet share down with the public one, so either key off
            // leaves the port local again.
            Reach::Phone if shared.is_some() => return self.stop_sharing(port),
            Reach::Public if shared.is_some_and(|share| share.is_public) => {
                return self.stop_sharing(port);
            }
            Reach::Phone => false,
            Reach::Public => true,
        };
        self.phone = Some(Phone {
            tree,
            port,
            shared: None,
            is_public,
        });
        Outcome::Share { port, is_public }
    }

    /// Takes `port` off the phone and the internet both, as one unshare
    /// command does, and says so while it runs.
    fn stop_sharing(&mut self, port: u16) -> Outcome {
        self.phone = None;
        self.message = Some(format!("unsharing :{port}..."));
        Outcome::Unshare(port)
    }

    /// What sharing `port` with the phone came to: the code of its address,
    /// or why there is none. Ignored once its popup is closed or shows
    /// another port.
    pub fn set_shared(&mut self, port: u16, shared: Result<String, String>) {
        if let Some(phone) = self.phone.as_mut().filter(|p| p.port == port) {
            phone.shared = Some(shared.and_then(|url| Code::new(&url).map(|code| (url, code))));
        }
    }

    /// Keys while the phone's code is shown: any of its ways out closes it,
    /// and `x` takes the port down again once its share is up, whether it
    /// is on the phone or the internet.
    fn handle_phone_key(&mut self, key: KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'p' | 'P' | 'Q') => {
                self.phone = None;
            }
            KeyCode::Char('x') => {
                if let Some(phone) = self.phone.take_if(|p| matches!(p.shared, Some(Ok(_)))) {
                    return self.stop_sharing(phone.port);
                }
            }
            _ => {}
        }
        Outcome::Continue
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
            KeyCode::Esc | KeyCode::Char('q' | 'o' | 'p' | 'P' | 'Q') => {
                self.port_menu = None;
                return Outcome::Continue;
            }
            _ => return Outcome::Continue,
        };
        match self.port_menu.take() {
            Some(menu) => self.use_port(menu.ports[chosen].0, menu.tree, menu.reach),
            None => Outcome::Continue,
        }
    }

    /// Quits, or waits for running actions to finish first.
    /// Restarts on the treetop installed over this one, once no action is
    /// under way, since a restart would cut one off.
    fn restart(&mut self) -> Outcome {
        let to = if self.replaced.is_some() {
            Outcome::Restart
        } else if let Some(tag) = self.newer.clone().filter(|_| !self.is_updating) {
            self.message = Some(format!("installing {tag}..."));
            Outcome::Update
        } else if self.is_updating {
            return Outcome::Continue;
        } else {
            self.message = Some(format!(
                "treetop {} is the latest",
                crate::update::current()
            ));
            return Outcome::Continue;
        };
        match self.active_jobs() {
            0 => to,
            n => {
                self.message = None;
                self.message = Some(format!("{n} action(s) still running; U again once done"));
                Outcome::Continue
            }
        }
    }

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
        // The palette opens over everything, a diff or a PR with the keys too.
        if self.palette.is_some() {
            return self.handle_palette_key(key);
        }
        if self.view.is_some() && self.has_keys_on(WindowTab::Diff) {
            return self.handle_view_key(key);
        }
        if self.pr_popup.is_some() && self.has_keys_on(WindowTab::Pr) {
            return self.handle_pr_key(key);
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
        if self.phone.is_some() {
            return self.handle_phone_key(key);
        }
        if self.scratch_menu.is_some() {
            return self.handle_scratch_key(key);
        }
        // A start shown in the window under the table holds no keys: Esc
        // hides it, and the start carries on; the rest work as ever, so a
        // second start can follow the first at once.
        // Esc hides it from the table; a window with the keys takes its own.
        if self.window_start().is_some() && key.code == KeyCode::Esc && !self.is_window_focused {
            self.opening = None;
            self.awaiting_chat = None;
            self.started_chat = None;
            self.started_at = None;
            return Outcome::Continue;
        }
        if let Some(opening) = self.opening.clone().filter(|o| o.key.is_none()) {
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
                // Ctrl+Q moves the keys on to the window under the table,
                // or round the two of a double, once nothing else on screen
                // waits on them.
                KeyCode::Char('q' | 'Q') => {
                    let is_free = self.reviews.is_empty()
                        && self.pending.is_none()
                        && self.shipping.is_none()
                        && !self.is_help
                        && !self.is_filtering
                        && self.rail.is_none();
                    // Zoomed to the table there is no window to give the
                    // keys to, as before treetop had windows.
                    if is_free && self.zoom != Some(Zoom::Table) {
                        self.cycle_keys();
                    }
                    Outcome::Continue
                }
                // Ctrl+P: the command palette, once nothing else on screen
                // waits on the keys, so no command lands in a dialog.
                KeyCode::Char('p' | 'P') => {
                    let is_free = self.reviews.is_empty()
                        && self.pending.is_none()
                        && self.shipping.is_none()
                        && !self.is_help
                        && !self.is_filtering
                        && self.rail.is_none();
                    if is_free {
                        self.open_palette();
                    }
                    Outcome::Continue
                }
                // Ctrl+F: the table, or the window with the keys, zoomed.
                KeyCode::Char('f' | 'F') => {
                    let what = if self.is_window_focused {
                        Zoom::Windows
                    } else {
                        Zoom::Table
                    };
                    self.toggle_zoom(what);
                    Outcome::Continue
                }
                // Ctrl+\: two windows under the table, or one again.
                // Without the terminal's extended keys it comes as Ctrl+4.
                KeyCode::Char('\\' | '4') => {
                    self.toggle_double();
                    Outcome::Continue
                }
                _ => Outcome::Continue,
            };
        }
        if self.is_help {
            self.is_help = false;
            return Outcome::Continue;
        }
        // A window with the keys but nothing to type into, its info or run,
        // or a chat not open yet, keeps them from the table, so typing meant
        // for it never runs a table command.
        if self.is_window_focused {
            return self.handle_window_key(key);
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
            self.selected = self.first_tree_row();
            return Outcome::Continue;
        }
        if !self.is_quitting {
            self.message = None;
        }
        if let Some(at) = self.rail {
            return self.handle_inbox_key(key, at);
        }
        if key.code == KeyCode::Char('w') {
            return Outcome::Workspaces;
        }
        if matches!(key.code, KeyCode::Left | KeyCode::Right) {
            self.step_tab(key.code == KeyCode::Right);
            return Outcome::Continue;
        }
        if key.code == KeyCode::Char('C') {
            return Outcome::Scratches;
        }
        // On a heading, Enter folds its group or opens it again; on the main
        // checkout's, it opens the chat of an agent working there instead.
        if key.code == KeyCode::Enter
            && let Some(group) = self.current_heading()
        {
            if let Some(chat) = self.main_under_cursor().and_then(Self::chat_of) {
                return chat;
            }
            self.toggle_fold(&group);
            return Outcome::Continue;
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
            KeyCode::Char('e') => {
                // Zoomed to the windows, the inbox has no room beside the
                // table: it zooms back out to show itself.
                if self.zoom == Some(Zoom::Windows) {
                    self.zoom = None;
                }
                self.rail = Some(0);
            }
            KeyCode::Char('E') => self.toggle_inbox(),
            KeyCode::Char('m') => self.open_message(),
            KeyCode::Char('q') => return self.quit(),
            KeyCode::Char('U') => return self.restart(),
            KeyCode::Char('?') => self.is_help = true,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = self.first_tree_row();
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
            KeyCode::Char('o') => return self.choose_port(Reach::Browser),
            KeyCode::Char('p') => return self.choose_port(Reach::Phone),
            KeyCode::Char('P') => return self.choose_port(Reach::Public),
            KeyCode::Char('Q') => return self.choose_port(Reach::Code),
            KeyCode::Char('O') => return self.open_pr(),
            KeyCode::Char('f') => return self.read_failure(),
            KeyCode::Char('R') => return self.resume(),
            KeyCode::Char('h') => return self.hand_off(),
            KeyCode::Char('s') => return self.ship(),
            KeyCode::Char('z') => return self.toggle_shelf(Shelf::Parked),
            KeyCode::Char('x') => return self.toggle_shelf(Shelf::Backburner),
            KeyCode::Char('X') => {
                self.toggle_fold(&Group::Backburner);
                let last = self.rows().len().saturating_sub(1);
                self.selected = self.selected.min(last);
            }
            KeyCode::Char('Z') => {
                self.is_parked_open = !self.is_parked_open;
                let last = self.rows().len().saturating_sub(1);
                self.selected = self.selected.min(last);
            }
            // The assistant's chat, where it answers.
            KeyCode::Char('a') => return self.converse(String::new()),
            KeyCode::Char('n') => self.open_start(),
            KeyCode::Char('N') => self.open_dispatch(),
            KeyCode::Char('d') => {
                self.open_diff();
                self.show_tab(WindowTab::Diff);
            }
            KeyCode::Char('v') => {
                self.open_pr_view();
                self.show_tab(WindowTab::Pr);
            }
            KeyCode::Char('b') => self.open_rename(),
            KeyCode::Char('r') => {
                // Destroy is asked inside the return dialog, so it takes
                // every tree a destroy could, available ones too.
                let trees = self.targets(Kind::Destroy);
                if !trees.is_empty() {
                    let runs = trees
                        .iter()
                        .filter_map(|t| crate::keel::holding(&self.runs, &t.path))
                        .map(|run| (run.tree.clone(), run.clone()))
                        .collect();
                    self.pending = Some(Pending { trees, runs });
                }
            }
            _ => {}
        }
        Outcome::Continue
    }
}

/// Rows `d` and `u` scroll the PR popup by.
const SCROLL: isize = 10;

/// The selection kept on a row the PR has, after a read or a fold.
fn clamp(popup: &mut PrPopup, pr: &PrView) {
    let tabs = pr_view::tabs(pr);
    if !tabs.contains(&popup.tab) {
        popup.tab = Tab::Overview;
    }
    for tab in tabs {
        let index = tab.index();
        popup.selected[index] =
            popup.selected[index].min(pr_view::last_row(pr, tab, popup.nitpicks));
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

    /// Puts the cursor on tree `name`.
    fn select(app: &mut App, name: &str) {
        let path = app
            .trees
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .path
            .clone();
        app.reselect(Some(path));
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some(name),
            "tree {name} is listed"
        );
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
        for name in ["4", "8", "7"] {
            select(&mut app, name);
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
        select(&mut app, "7");
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
            context: None,
            spend: None,
            label: None,
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

        select(&mut app, "4");
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
        select(&mut app, "8");
        press(&mut app, KeyCode::Char('i'));
        assert!(app.is_issues);
        assert_eq!(app.open_issue.as_deref(), Some("END-799"));
        app.is_issues = false;
        app.open_issue = None;
        select(&mut app, "7");
        press(&mut app, KeyCode::Char('i'));
        assert!(!app.is_issues, "an unleased tree works on nothing");
        assert!(app.message.as_deref().unwrap().contains("is not leased"));
    }

    #[test]
    fn an_issue_chosen_for_a_tree_is_recorded_in_its_sidecar() {
        let mut app = app();
        dispatched(&mut app, "/p/8/app", "j1");
        select(&mut app, "8");
        press(&mut app, KeyCode::Char('i'));
        assert!(app.is_issues, "a tree on no issue chooses one");
        assert_eq!(app.choose_issue_for.as_deref(), Some("8"));
        let (path, record) = app.attach("END-42".into(), "personal".into()).unwrap();
        assert_eq!(path, PathBuf::from("/p/8/app"));
        assert_eq!(record.ticket.as_deref(), Some("END-42"));
        assert_eq!(record.source.as_deref(), Some("personal"));
        assert!(!app.is_issues, "back on the trees");
        let tree = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert_eq!(
            tree.sidecar.as_ref().unwrap().ticket.as_deref(),
            Some("END-42"),
            "shown at once"
        );
        assert_eq!(app.message.as_deref(), Some("tree 8 works on END-42"));
        assert_eq!(
            app.attach("END-43".into(), "work".into()),
            None,
            "chosen once"
        );
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
        select(&mut app, "8");
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.choose_issue_for.as_deref(), Some("8"));
        assert!(matches!(
            app.pick("END-9".into(), "work".into()),
            Outcome::Queue(_)
        ));
        app.returned(&PathBuf::from("/p/8/app"));
        assert_eq!(
            app.attach("END-7".into(), "work".into()),
            None,
            "picking up ended the choice"
        );
        let tree = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert!(tree.sidecar.is_none(), "the free tree carries no issue");
    }

    #[test]
    fn an_issue_chosen_for_a_tree_returned_meanwhile_goes_nowhere() {
        let mut app = app();
        select(&mut app, "8");
        press(&mut app, KeyCode::Char('I'));
        app.returned(&PathBuf::from("/p/8/app"));
        assert_eq!(app.attach("END-7".into(), "work".into()), None);
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
        let job = queued(app.pick("END-812".into(), "work".into())).remove(0);
        assert!(!app.is_issues, "back on the trees");
        let launch = job.launch.unwrap();
        assert!(launch.is_new_tree);
        assert_eq!(
            launch.ticket,
            Some(crate::jobs::Pick {
                id: "END-812".into(),
                checkout: PathBuf::from("/repo"),
                source: Some("work".into()),
            })
        );
        dispatched(&mut app, "/p/8/app", "j1");
        let mut sidecar = *app.trees[2].sidecar.clone().unwrap();
        sidecar.ticket = Some("END-799".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/p/8/app"), sidecar)]));
        assert_eq!(app.pick("end-799".into(), "work".into()), Outcome::Continue);
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
        // Esc gives the keys back to the table; the diff stays in its tab.
        press(&mut app, KeyCode::Esc);
        assert!(!app.is_window_focused);
        assert!(app.view.is_some());
        with_work(&mut app, 2);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            app.wanted_diff(),
            Some((PathBuf::from("/p/4/app"), None)),
            "stale on reopen"
        );
    }

    #[test]
    fn a_coloured_copy_keeps_its_diffs_counts_and_one_dropped_since_is_dropped() {
        let mut app = app();
        with_work(&mut app, 1);
        let path = PathBuf::from("/p/4/app");
        press(&mut app, KeyCode::Char('d'));
        app.wanted_diff();
        app.diff_loaded(path.clone(), two_files());
        app.diff_loaded(path.clone(), two_files());
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(
            app.wanted_diff(),
            None,
            "the coloured copy is as fresh as the plain one"
        );
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        app.diff_loaded(path.clone(), two_files());
        assert_eq!(
            app.wanted_diff(),
            Some((path, None)),
            "a copy of a diff dropped for reading again is not shown"
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

    /// The list as rows read: `▾name count` for an open heading, `▸` for a
    /// folded one, else the tree's name.
    fn shape(app: &App) -> Vec<String> {
        app.rows()
            .into_iter()
            .map(|row| match row {
                ListRow::Tree(tree) => tree.name.clone(),
                ListRow::Heading {
                    group,
                    count,
                    is_folded,
                    ..
                } => {
                    let name = match group {
                        Group::Base(base) => base,
                        Group::Backburner => "backburner".into(),
                        Group::Free => "free".into(),
                        Group::Parked => "parked".into(),
                    };
                    format!("{}{name} {count}", if is_folded { "▸" } else { "▾" })
                }
            })
            .collect()
    }

    #[test]
    fn the_list_groups_trees_under_headings_that_enter_folds() {
        let mut app = app();
        app.base = Some("development".into());
        assert_eq!(
            shape(&app),
            ["▾development 2", "4", "8", "▾free 1", "7"],
            "even one base has its heading"
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
            shape(&app),
            ["▾development 1", "8", "▾main 1", "4", "▾free 1", "7"],
            "tree 4 waits on you, first in its own group"
        );
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("8"),
            "the cursor starts on a tree"
        );
        press(&mut app, KeyCode::Down);
        assert_eq!(app.current(), None, "on main's heading");
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            shape(&app),
            ["▾development 1", "8", "▸main 1", "▾free 1", "7"]
        );
        assert!(matches!(app.rows()[2], ListRow::Heading { waiting: 1, .. }));
        app.filter = "end-1".into();
        assert_eq!(
            shape(&app),
            ["▾main 1", "4"],
            "a filter opens what it finds"
        );
        app.filter.clear();
        press(&mut app, KeyCode::Enter);
        assert_eq!(shape(&app)[2], "▾main 1", "Enter opens it again");
    }

    /// The fixture's pool under the main checkout, on `development`, with
    /// the cursor on the heading it leads.
    fn app_on_main_heading() -> App {
        let mut app = app_with_main();
        let main = app.trees.iter().position(|t| t.is_main).unwrap();
        app.trees[main].branch = Some("development".into());
        app.base = Some("development".into());
        app.selected = 0;
        assert_eq!(
            app.current_heading(),
            Some(Group::Base("development".into()))
        );
        app
    }

    #[test]
    fn n_on_the_main_checkouts_heading_starts_an_agent_there_on_its_own_branch() {
        let mut app = app_on_main_heading();
        press(&mut app, KeyCode::Char('n'));
        assert!(
            !app.prompt.as_ref().unwrap().names_branch(),
            "the main checkout keeps what it has checked out"
        );
        type_in(&mut app, "why is the build slow");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(job.kind, Kind::Start);
        assert!(job.tree.is_main, "no tree is leased for it");
        let launch = job.launch.unwrap();
        assert!(!launch.is_new_tree);
        assert_eq!(launch.branch, None);
        assert_eq!(
            app.message.as_deref(),
            Some("starting an agent in the main checkout...")
        );
        app.selected = 3;
        assert!(
            app.current_heading().is_some(),
            "on the free trees' heading"
        );
        press(&mut app, KeyCode::Char('n'));
        assert!(app.prompt.is_none(), "no other heading starts one");
    }

    #[test]
    fn enter_on_the_main_checkouts_heading_opens_its_agents_chat_else_folds() {
        let mut app = app_on_main_heading();
        let main = app.trees.iter().find(|t| t.is_main).unwrap().path.clone();
        let mut assistant = attachable(assistant::NAME);
        assistant.status = Some(Status::Idle);
        app.apply_agents(&HashMap::from([(main.clone(), vec![assistant.clone()])]));
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            shape(&app)[0],
            "▸development 2",
            "the assistant is not its agent, so Enter folds"
        );
        press(&mut app, KeyCode::Enter);
        let mut own = attachable("why is the build slow");
        own.status = Some(Status::Busy);
        app.apply_agents(&HashMap::from([(main, vec![assistant, own])]));
        assert!(matches!(
            press(&mut app, KeyCode::Enter),
            Outcome::Chat { agent, tree, .. } if agent == "why is the build slow" && tree == "main"
        ));
        assert_eq!(shape(&app)[0], "▾development 2", "left open");
    }

    #[test]
    fn the_main_checkout_heads_its_branchs_group_and_is_no_row() {
        let mut app = app_with_main();
        let main = app.trees.iter().position(|t| t.is_main).unwrap();
        app.trees[main].branch = Some("development".into());
        app.base = Some("development".into());
        let mut own = attachable("old session");
        own.status = Some(Status::Waiting);
        let path = app.trees[main].path.clone();
        app.apply_agents(&HashMap::from([(path, vec![own])]));
        assert_eq!(shape(&app), ["▾development 2", "4", "8", "▾free 1", "7"]);
        assert_eq!(
            app.next_ask(),
            None,
            "nothing in the main checkout is asked"
        );
        app.trees.retain(|t| t.is_main);
        assert_eq!(
            shape(&app),
            ["▾development 0"],
            "its heading stays with no trees"
        );
    }

    #[test]
    fn trees_sort_waiting_first_then_by_their_agents_last_work() {
        let names =
            |app: &App| -> Vec<String> { app.visible().iter().map(|t| t.name.clone()).collect() };
        let mut app = app();
        assert_eq!(
            names(&app),
            ["4", "8", "7"],
            "held in pool order, then free"
        );
        dispatched(&mut app, "/p/4/app", "old");
        dispatched(&mut app, "/p/8/app", "new");
        app.apply_reports(vec![
            report("old", State::Working, "/p/4/app", 500),
            report("new", State::Working, "/p/8/app", 10),
        ]);
        assert_eq!(names(&app), ["8", "4", "7"]);
        let quiet = Agent {
            active_at: Some(1_000),
            ..agent("quiet")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![quiet])]));
        assert_eq!(names(&app), ["4", "8", "7"], "an agent's own time counts");
        app.apply_reports(vec![
            report("old", State::Working, "/p/4/app", 500),
            report("new", State::Blocked, "/p/8/app", 10),
        ]);
        assert_eq!(names(&app), ["8", "4", "7"]);
    }

    #[test]
    fn z_parks_a_tree_out_of_sight_and_z_again_brings_it_back() {
        let mut app = app();
        let Outcome::Record { path, record } = press(&mut app, KeyCode::Char('z')) else {
            panic!("z records the park");
        };
        assert_eq!(path, PathBuf::from("/p/4/app"));
        assert!(record.parked);
        assert_eq!(
            app.message.as_deref(),
            Some("parked tree 4; Z opens Parked")
        );
        assert_eq!(shape(&app), ["▾ 1", "8", "▾free 1", "7", "▸parked 1"]);
        app.filter = "end-1".into();
        assert_eq!(shape(&app), ["▾parked 1", "4"], "a filter still finds it");
        app.filter.clear();
        press(&mut app, KeyCode::Char('Z'));
        assert_eq!(shape(&app), ["▾ 1", "8", "▾free 1", "7", "▾parked 1", "4"]);
        press(&mut app, KeyCode::End);
        let Outcome::Record { record, .. } = press(&mut app, KeyCode::Char('z')) else {
            panic!("z records it back");
        };
        assert!(!record.parked);
        assert_eq!(shape(&app), ["▾ 2", "4", "8", "▾free 1", "7"]);
        app.reselect(Some(PathBuf::from("/p/7/app")));
        assert!(
            matches!(press(&mut app, KeyCode::Char('z')), Outcome::Continue),
            "an available tree has nothing to park"
        );
    }

    #[test]
    fn a_workspace_opened_from_its_cache_lists_shelved_trees_in_their_groups_at_once() {
        let root = std::env::temp_dir().join(format!("treetop-cached-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let listing: Vec<String> = [("4", Some(Shelf::Backburner)), ("8", Some(Shelf::Parked)), ("9", None)]
            .into_iter()
            .map(|(name, shelf)| {
                let tree = root.join(name);
                std::fs::create_dir_all(tree.join(".git")).unwrap();
                let mut record = Sidecar::leased_now();
                record.shelve(shelf);
                crate::sidecar::write(&tree, &record).unwrap();
                format!(
                    r#"{{"name":"{name}","status":"leased","branch":"b{name}","lease_holder":"me","path":"{}","leased_at":null,"processes":[]}}"#,
                    tree.display()
                )
            })
            .collect();
        let mut app = App::new(None);
        app.set_cached(parse(&format!("[{}]", listing.join(","))).unwrap());
        assert_eq!(
            shape(&app),
            ["▾ 1", "9", "▾backburner 1", "4", "▸parked 1"],
            "before the git clock has read a single sidecar"
        );
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("9"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn x_puts_a_tree_on_the_backburner_with_the_cursor_and_x_again_brings_it_back() {
        let mut app = app();
        let Outcome::Record { path, record } = press(&mut app, KeyCode::Char('x')) else {
            panic!("x records the backburner");
        };
        assert_eq!(path, PathBuf::from("/p/4/app"));
        assert_eq!(record.shelf(), Some(Shelf::Backburner));
        assert_eq!(app.message.as_deref(), Some("tree 4 is on the Backburner"));
        assert_eq!(
            shape(&app),
            ["▾ 1", "8", "▾backburner 1", "4", "▾free 1", "7"],
            "between the trees in hand and the free ones, open"
        );
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("4"),
            "the cursor follows it"
        );
        press(&mut app, KeyCode::Char('X'));
        assert_eq!(shape(&app), ["▾ 1", "8", "▸backburner 1", "▾free 1", "7"]);
        press(&mut app, KeyCode::Char('X'));
        select(&mut app, "4");
        let Outcome::Record { record, .. } = press(&mut app, KeyCode::Char('z')) else {
            panic!("z parks it from the Backburner");
        };
        assert_eq!(
            record.shelf(),
            Some(Shelf::Parked),
            "on one shelf at a time"
        );
        assert!(!record.backburner);
        assert_eq!(shape(&app), ["▾ 1", "8", "▾free 1", "7", "▸parked 1"]);
        press(&mut app, KeyCode::Char('Z'));
        select(&mut app, "4");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("4"));
        let Outcome::Record { record, .. } = press(&mut app, KeyCode::Char('x')) else {
            panic!("x records it back");
        };
        assert_eq!(record.shelf(), None);
        assert_eq!(
            app.message.as_deref(),
            Some("tree 4 is back from Backburner")
        );
        assert_eq!(shape(&app), ["▾ 2", "4", "8", "▾free 1", "7"]);
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("4"),
            "and back with it"
        );
    }

    #[test]
    fn x_opens_a_backburner_folded_while_it_was_empty_so_the_cursor_follows() {
        let mut app = app();
        press(&mut app, KeyCode::Char('X'));
        select(&mut app, "4");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(
            shape(&app),
            ["▾ 1", "8", "▾backburner 1", "4", "▾free 1", "7"]
        );
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("4"));
    }

    #[test]
    fn x_back_into_a_folded_base_opens_it_so_the_cursor_follows() {
        let mut app = app();
        press(&mut app, KeyCode::Char('x'));
        app.selected = 0;
        press(&mut app, KeyCode::Enter);
        assert_eq!(shape(&app), ["▸ 1", "▾backburner 1", "4", "▾free 1", "7"]);
        select(&mut app, "4");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(shape(&app), ["▾ 2", "4", "8", "▾free 1", "7"]);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("4"));
    }

    #[test]
    fn a_parked_tree_that_needs_you_stays_parked_at_the_top_of_parked() {
        let mut app = app();
        press(&mut app, KeyCode::Char('z'));
        select(&mut app, "8");
        press(&mut app, KeyCode::Char('z'));
        let parked = |job: Option<&str>| {
            let mut record = Sidecar::leased_now();
            record.parked = true;
            record.job = job.map(Into::into);
            record
        };
        app.apply_sidecars(HashMap::from([
            (PathBuf::from("/p/4/app"), parked(None)),
            (PathBuf::from("/p/8/app"), parked(Some("ask"))),
        ]));
        app.apply_reports(vec![report("ask", State::Blocked, "/p/8/app", 1)]);
        assert_eq!(shape(&app), ["▾free 1", "7", "▸parked 2"]);
        assert!(
            app.rows().iter().any(|row| matches!(
                row,
                ListRow::Heading {
                    group: Group::Parked,
                    waiting: 1,
                    ..
                }
            )),
            "the folded heading says one waits on you"
        );
        press(&mut app, KeyCode::Char('Z'));
        assert_eq!(
            shape(&app),
            ["▾free 1", "7", "▾parked 2", "8", "4"],
            "first, and still set aside"
        );
        assert_eq!(app.needing_you(), 1);
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
    fn a_base_typed_that_names_no_branch_keeps_the_box_open() {
        let mut app = app();
        app.base = Some("development".into());
        press(&mut app, KeyCode::Char('N'));
        app.prompt.as_mut().unwrap().bases = vec!["development".into(), "main".into()];
        type_in(&mut app, "fix the login");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        type_in(&mut app, "nope");
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert!(app.prompt.is_some(), "kept open to fix");
        assert_eq!(
            app.message.as_deref(),
            Some("no branch matches \"nope\" to cut the tree from")
        );
    }

    #[test]
    fn a_fresh_tree_is_leased_from_the_base_picked() {
        let mut app = app();
        app.base = Some("development".into());
        press(&mut app, KeyCode::Char('N'));
        let prompt = app.prompt.as_mut().unwrap();
        assert_eq!(prompt.default_base.as_deref(), Some("development"));
        prompt.bases = vec!["development".into(), "main".into()];
        type_in(&mut app, "fix the login");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Right);
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(job.launch.unwrap().base.as_deref(), Some("main"));
    }

    #[test]
    fn d_on_an_available_tree_says_there_is_nothing_to_diff() {
        let mut app = app();
        select(&mut app, "7");
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
        app.window_rows = 6;
        press(&mut app, KeyCode::Char('d'));
        app.wanted_diff();
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
            "a page is the window's inside less the diff's bar, its pinned header and its key bar"
        );
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(scroll(&app), 14);
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(scroll(&app), 14, "never past the last row");
        press(&mut app, KeyCode::Char('q'));
        assert!(
            !app.is_window_focused && !app.is_quitting && app.view.is_some(),
            "q gives the table the keys, and leaves treetop and the diff open"
        );
        app.wheel_window(false);
        assert_eq!(scroll(&app), 11, "the wheel: three lines a step");
        assert!(!app.is_window_focused, "and the table keeps the keys");
    }

    #[test]
    fn the_arrows_step_the_window_tabs_from_the_diff_and_the_pr_and_l_scrolls_the_diff() {
        let mut app = app();
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.shown_tab(), WindowTab::Pr);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.shown_tab(), WindowTab::Diff, "← from the PR");
        app.sync_window();
        app.wanted_diff();
        app.diff_loaded(PathBuf::from("/p/4/app"), two_files());
        let hscroll = |app: &App| app.view.as_ref().unwrap().hscroll;
        press(&mut app, KeyCode::Char('l'));
        assert_eq!(hscroll(&app), 8, "l scrolls the diff sideways");
        press(&mut app, KeyCode::Right);
        assert_eq!(app.shown_tab(), WindowTab::Pr, "→ from the diff");
        assert_eq!(hscroll(&app), 8, "rather than scroll it");
        assert!(app.is_window_focused, "the window keeps the keys");
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
            context: None,
            spend: None,
            label: None,
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
    fn the_main_checkout_leads_with_neither_the_assistant_nor_a_stopped_session() {
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
        let names = |app: &App| -> Vec<String> {
            let main = app.trees.iter().find(|t| t.is_main).unwrap();
            App::agents_of(main)
                .iter()
                .map(|a| a.name.clone())
                .collect()
        };
        assert_eq!(names(&app), ["fix the export"]);
        app.reselect(Some(main.clone()));
        assert_ne!(
            app.current().map(|t| t.is_main),
            Some(true),
            "the main checkout is no row to put the cursor on"
        );
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
        select(&mut app, "7");
        assert!(matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "7"));
        select(&mut app, "8");
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "8"),
            "an agent with no chat is in a pane, which going to the tree finds"
        );
        select(&mut app, "4");
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
        select(&mut app, "7");
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

    #[test]
    fn p_shares_the_port_and_shows_its_address_as_a_code() {
        let mut app = app();
        let process = |pid, name: &str, ports| Process {
            pid,
            name: name.into(),
            ports,
        };
        app.apply_processes(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![process(1, "node", vec![5173])],
        )]));
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Outcome::Share {
                port: 5173,
                is_public: false
            }
        );
        let phone = app.phone.as_ref().unwrap();
        assert_eq!((phone.tree.as_str(), phone.shared.as_ref()), ("4", None));
        assert_eq!(app.next_ask(), None, "the code keeps chats shut");
        // An answer for a port no longer shown is dropped.
        app.set_shared(8000, Ok("https://box.ts.net:8000".into()));
        assert_eq!(app.phone.as_ref().unwrap().shared, None);
        let url = "https://box.tail1.ts.net:5173";
        app.set_shared(5173, Ok(url.into()));
        let shared = app.phone.as_ref().unwrap().shared.clone();
        assert_eq!(shared, Some(Ok((url.into(), Code::new(url).unwrap()))));
        assert_eq!(press(&mut app, KeyCode::Char('j')), Outcome::Continue);
        assert!(app.phone.is_some(), "only a way out closes it");
        press(&mut app, KeyCode::Esc);
        assert!(app.phone.is_none());

        press(&mut app, KeyCode::Char('p'));
        app.set_shared(5173, Err("tether: tailscale is not running".into()));
        assert_eq!(
            app.phone.as_ref().unwrap().shared,
            Some(Err("tether: tailscale is not running".into()))
        );
        press(&mut app, KeyCode::Char('p'));
        assert!(app.phone.is_none(), "p closes it again");

        // A tree serving several asks which, as `o` does, then shares it.
        app.apply_processes(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![
                process(1, "uvicorn", vec![8000]),
                process(2, "node", vec![5173]),
            ],
        )]));
        assert_eq!(press(&mut app, KeyCode::Char('p')), Outcome::Continue);
        assert_eq!(app.port_menu.as_ref().unwrap().reach, Reach::Phone);
        assert_eq!(
            press(&mut app, KeyCode::Char('2')),
            Outcome::Share {
                port: 8000,
                is_public: false
            }
        );
        assert_eq!(app.phone.as_ref().unwrap().port, 8000);
    }

    #[test]
    fn shift_p_publishes_the_port_and_x_takes_it_down() {
        let mut app = app();
        app.apply_processes(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![Process {
                pid: 1,
                name: "node".into(),
                ports: vec![5173],
            }],
        )]));
        assert_eq!(
            press(&mut app, KeyCode::Char('P')),
            Outcome::Share {
                port: 5173,
                is_public: true
            }
        );
        assert!(app.phone.as_ref().unwrap().is_public);
        assert_eq!(
            press(&mut app, KeyCode::Char('x')),
            Outcome::Continue,
            "nothing to take down before it is up"
        );
        assert!(app.phone.is_some());
        let url = "https://box.tail1.ts.net/";
        app.set_shared(5173, Ok(url.into()));
        assert_eq!(press(&mut app, KeyCode::Char('x')), Outcome::Unshare(5173));
        assert!(app.phone.is_none());

        // `x` takes the phone's tailnet share down as well.
        press(&mut app, KeyCode::Char('p'));
        app.set_shared(5173, Ok("https://box.tail1.ts.net:5173".into()));
        assert_eq!(press(&mut app, KeyCode::Char('x')), Outcome::Unshare(5173));
        assert!(app.phone.is_none());
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Char('P'));
        assert!(app.phone.is_none(), "P closes it too");
    }

    #[test]
    fn p_and_shift_p_toggle_a_share_and_shift_q_shows_the_code_of_one_up() {
        let mut app = app();
        app.apply_processes(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![Process {
                pid: 1,
                name: "node".into(),
                ports: vec![5173],
            }],
        )]));
        assert_eq!(press(&mut app, KeyCode::Char('Q')), Outcome::Continue);
        assert!(app.phone.is_none(), "nothing to show while it is local");
        assert_eq!(
            app.message.as_deref(),
            Some(":5173 is not shared; p shares it with your phone, P with the internet")
        );

        let url = "https://box.tail1.ts.net:5173";
        app.apply_shares(vec![Shared {
            port: 5173,
            is_public: false,
            address: url.into(),
        }]);
        assert_eq!(press(&mut app, KeyCode::Char('Q')), Outcome::Continue);
        let phone = app.phone.as_ref().unwrap();
        assert_eq!(
            phone.shared,
            Some(Ok((url.into(), Code::new(url).unwrap()))),
            "no command runs: the listing carries the address"
        );
        assert!(!phone.is_public);
        press(&mut app, KeyCode::Esc);

        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Outcome::Unshare(5173),
            "a second p takes the share down"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('P')),
            Outcome::Share {
                port: 5173,
                is_public: true
            },
            "P puts a port already on the phone on the internet"
        );
        press(&mut app, KeyCode::Esc);

        app.apply_shares(vec![Shared {
            port: 5173,
            is_public: true,
            address: "https://box.tail1.ts.net".into(),
        }]);
        assert_eq!(
            press(&mut app, KeyCode::Char('P')),
            Outcome::Unshare(5173),
            "a second P takes it off the internet"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('p')),
            Outcome::Unshare(5173),
            "p takes a public port down too, as one unshare does both"
        );
    }

    fn type_in(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn capital_n_with_no_task_starts_an_idle_agent_and_opens_its_chat_once_it_shows() {
        let mut app = app();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![agent("old")],
        )]));
        press(&mut app, KeyCode::Char('N'));
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
        let job = queued(press(&mut app, KeyCode::Char('a'))).remove(0);
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
        let job = queued(press(&mut app, KeyCode::Char('a'))).remove(0);
        assert!(app.opening.is_some());
        assert_eq!(
            press(&mut app, KeyCode::Char('q')),
            Outcome::Continue,
            "keys wait"
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.opening.is_none());
        app.job_finished(started(job), true, String::new());
        assert_eq!(app.ready_chat(), None, "dismissed, it opens only with a");
        assert!(app.message.as_deref().unwrap().contains("a opens its chat"));
        let job = queued(press(&mut app, KeyCode::Char('a'))).remove(0);
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
        press(&mut app, KeyCode::Char('N'));
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
    fn a_start_shows_in_the_window_and_its_chat_opens_once_its_tree_is_listed() {
        let mut app = app();
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "fix login");
        let mut job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(
            app.window_start().and_then(|o| o.key.clone()),
            Some(job.key.clone())
        );
        job.tree.path = PathBuf::from("/p/12/app");
        job.tree.name = "12".into();
        job.attach = Some(vec!["claude".into(), "attach".into(), "new1".into()]);
        app.job_finished(job.clone(), true, String::new());
        // Its tree is not listed yet, so the window has nowhere to show it.
        assert_eq!(app.ready_chat(), None);
        let mut trees = app.trees.clone();
        trees.push(Tree {
            name: "12".into(),
            path: "/p/12/app".into(),
            ..app.trees[0].clone()
        });
        app.set_trees(trees);
        assert!(matches!(
            app.ready_chat(),
            Some(Outcome::Chat { tree, .. }) if tree == "12"
        ));
        assert!(app.window_start().is_none());
    }

    #[test]
    fn a_failed_start_stays_in_the_window_until_esc_and_esc_hides_one_under_way() {
        let mut app = app();
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "fix login");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        app.job_finished(job, false, "lease: no tree is free".into());
        assert_eq!(
            app.window_start().and_then(|o| o.failed.clone()).as_deref(),
            Some("lease: no tree is free")
        );
        press(&mut app, KeyCode::Esc);
        assert!(app.opening.is_none());
        // Esc while it runs hides it, and the start carries on unopened.
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "fix login");
        let mut job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        press(&mut app, KeyCode::Esc);
        assert!(app.opening.is_none());
        job.attach = Some(vec!["claude".into(), "attach".into(), "new2".into()]);
        app.job_finished(job, true, String::new());
        assert_eq!(app.ready_chat(), None, "a hidden start opens no chat");
    }

    #[test]
    fn esc_closes_the_bar_and_queues_nothing() {
        let mut app = app();
        press(&mut app, KeyCode::Char('N'));
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
    fn the_main_checkout_is_no_row_so_never_returned_or_destroyed() {
        let mut app = app_with_main();
        assert!(app.visible().iter().all(|t| !t.is_main));
        for _ in 0..8 {
            press(&mut app, KeyCode::Up);
            assert_ne!(app.current().map(|t| t.is_main), Some(true));
        }
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
        select(&mut app, "8");
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
    fn u_installs_a_newer_release_and_restarts_never_mid_action() {
        let mut app = app();
        assert_eq!(press(&mut app, KeyCode::Char('U')), Outcome::Continue);
        assert!(app.message.as_deref().unwrap().contains("is the latest"));

        app.apply_release("v0.0.1".into());
        assert_eq!(app.newer, None, "an older release is no update");
        app.apply_release("v999.0.0".into());
        assert_eq!(press(&mut app, KeyCode::Char('U')), Outcome::Update);
        app.is_updating = true;
        assert_eq!(
            press(&mut app, KeyCode::Char('U')),
            Outcome::Continue,
            "once"
        );
        app.is_updating = false;

        app.replaced = Some(PathBuf::from("/home/me/.local/bin/treetop"));
        app.jobs
            .insert(PathBuf::from("/p/4/app"), JobState::Queued(Kind::Return));
        assert_eq!(press(&mut app, KeyCode::Char('U')), Outcome::Continue);
        assert!(app.message.as_deref().unwrap().contains("still running"));

        app.jobs.clear();
        assert_eq!(press(&mut app, KeyCode::Char('U')), Outcome::Restart);
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
        select(&mut app, "8");
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
        assert_eq!(app.visible()[0].name, "8", "it sorts first");
        select(&mut app, "8");
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
        select(&mut app, "4");
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
    fn n_starts_an_agent_in_the_tree_under_the_cursor() {
        let mut app = app();
        select(&mut app, "7");
        press(&mut app, KeyCode::Char('n'));
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
        assert_eq!(launch.branch, None, "named as it starts, from the task");
        select(&mut app, "4");
        press(&mut app, KeyCode::Char('n'));
        type_in(&mut app, "add tests");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!(job.tree.name, "4");
        assert_eq!(
            job.launch.unwrap().branch,
            None,
            "a held tree keeps its branch"
        );
        select(&mut app, "4");
        press(&mut app, KeyCode::Char('n'));
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
    fn asking_starts_the_assistant_in_the_main_checkout() {
        let mut app = app_with_main();
        press(&mut app, KeyCode::Char(':'));
        type_in(&mut app, "what needs me?");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert!(job.tree.is_main);
        let launch = job.launch.clone().unwrap();
        assert!(launch.is_assistant);
        assert!(launch.task.starts_with("what needs me?"), "{}", launch.task);
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
        press(&mut app, KeyCode::Char(':'));
        type_in(&mut app, "status?");
        match press(&mut app, KeyCode::Enter) {
            Outcome::Send { to, text } => {
                assert!(text.starts_with("status?"), "{text}");
                assert_eq!(to[0].command, ["claude", "attach", assistant::NAME]);
                assert_eq!(to[0].tree, "main");
            }
            other => panic!("expected the message sent as m sends, got {other:?}"),
        }
        assert!(app.opening.is_none(), "no chat opens");
        assert!(
            matches!(press(&mut app, KeyCode::Char('a')), Outcome::Chat { agent, .. } if agent == assistant::NAME),
            "a opens its chat"
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
        assert!(matches!(
            press(&mut app, KeyCode::Char('a')),
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
    fn a_shell_read_in_normal_takes_the_wheel_with_the_inbox_hidden() {
        let mut app = app();
        app.inbox.is_hidden = true;
        app.tab = WindowTab::Shell;
        assert_eq!(app.shown_tab(), WindowTab::Shell);
        assert!(!app.wants_mouse(), "the table leaves the mouse alone");
        app.is_window_focused = true;
        assert!(app.wants_mouse(), "Normal on a shell scrolls it");
        app.tab = WindowTab::Info;
        assert!(!app.wants_mouse(), "nothing to scroll on the info");
    }

    #[test]
    fn an_ask_waits_while_a_window_shows_its_chat_in_normal() {
        let mut app = app();
        let asking = Agent {
            status: Some(Status::Waiting),
            ..attachable("asks")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/8/app"), vec![asking])]));
        app.apply_reports(Vec::new());
        for tab in [WindowTab::Chat, WindowTab::Shell] {
            app.tab = tab;
            app.is_window_focused = true;
            assert_eq!(app.next_ask(), None, "never over a chat being read");
        }
        app.is_window_focused = false;
        assert_eq!(chat_of(app.next_ask()).as_deref(), Some("asks"));
    }

    #[test]
    fn an_ask_a_filter_hides_still_opens_and_the_filter_clears() {
        let mut app = app();
        let asking = Agent {
            status: Some(Status::Waiting),
            ..attachable("asks")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/8/app"), vec![asking])]));
        app.apply_reports(Vec::new());
        app.filter = "end-1".into();
        assert_eq!(chat_of(app.next_ask()).as_deref(), Some("asks"));
        assert!(app.filter.is_empty());
        assert_eq!(app.current().unwrap().name, "8");
    }

    #[test]
    fn an_ask_seen_before_a_workspace_switch_stays_shut_on_coming_back() {
        let file = std::env::temp_dir()
            .join(format!("treetop-asked-{}", std::process::id()))
            .join("asked-app.json");
        let _ = std::fs::remove_file(&file);
        let waiting = || {
            HashMap::from([(
                PathBuf::from("/p/8/app"),
                vec![Agent {
                    status: Some(Status::Waiting),
                    ..attachable("asks")
                }],
            )])
        };
        let mut app = app();
        app.keep_asked_in(file.clone());
        app.apply_agents(&waiting());
        app.apply_reports(Vec::new());
        assert_eq!(chat_of(app.next_ask()).as_deref(), Some("asks"));

        // Coming back builds the workspace afresh.
        let mut back = tests::app();
        back.keep_asked_in(file.clone());
        assert_eq!(back.next_ask(), None, "nothing is read yet");
        back.apply_agents(&waiting());
        back.apply_reports(Vec::new());
        assert_eq!(back.next_ask(), None, "already seen, it stays shut");

        // Once it stops asking, its next ask opens again.
        back.apply_agents(&HashMap::new());
        assert_eq!(back.next_ask(), None);
        back.apply_agents(&waiting());
        assert_eq!(chat_of(back.next_ask()).as_deref(), Some("asks"));
        let _ = std::fs::remove_file(&file);
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
    fn h_asks_the_agent_for_a_handoff_shows_it_once_written_and_hands_its_tree_on() {
        let mut app = app();
        app.apply_sidecars(HashMap::from([(
            PathBuf::from("/p/4/app"),
            Sidecar::leased_now(),
        )]));
        let working = |status| Agent {
            job: Some("0000aaaa".into()),
            plugin: Some("claude".into()),
            status: Some(status),
            ..attachable("fix login")
        };
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![working(Status::Idle)],
        )]));
        select(&mut app, "4");
        let Outcome::Send { to, text } = press(&mut app, KeyCode::Char('h')) else {
            panic!("expected the agent to be asked");
        };
        assert_eq!(to[0].agent, "fix login");
        assert!(text.contains("/p/4/app/.treetop-handoff-"), "{text}");
        assert!(app.is_handing_off(&PathBuf::from("/p/4/app")));
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![working(Status::Busy)],
        )]));
        assert_eq!(app.wanted_handoff(), None, "not while it writes");
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![working(Status::Idle)],
        )]));
        let file = app.wanted_handoff().expect("read once it has stopped");
        assert!(
            text.contains(&file.display().to_string()),
            "it asked for this file"
        );
        app.handoff_written("Carry on with the login redirect.\n".into());
        assert!(!app.is_handing_off(&PathBuf::from("/p/4/app")));
        assert!(
            app.prompt.as_ref().is_some_and(Prompt::is_handoff),
            "read over first"
        );
        type_in(&mut app, " Add the test.");
        let job = queued(press(&mut app, KeyCode::Enter)).remove(0);
        assert_eq!((job.kind, job.tree.name.as_str()), (Kind::Handoff, "4"));
        let launch = job.launch.unwrap();
        assert_eq!(
            launch.task,
            "Carry on with the login redirect. Add the test."
        );
        assert_eq!(
            launch.name.as_deref(),
            Some("fix login"),
            "the row keeps its name"
        );
        assert!(
            !launch.is_new_tree,
            "the same tree, with its branch and work"
        );
        assert_eq!(
            job.replaces,
            Some(Replaced {
                agent: "fix login".into(),
                plugin: "claude".into(),
                id: "0000aaaa".into(),
            })
        );
    }

    #[test]
    fn a_handoff_request_that_never_reached_its_agent_is_not_waited_on() {
        let mut app = app();
        app.apply_sidecars(HashMap::from([(
            PathBuf::from("/p/4/app"),
            Sidecar::leased_now(),
        )]));
        let idle = Agent {
            job: Some("0000aaaa".into()),
            plugin: Some("claude".into()),
            status: Some(Status::Idle),
            ..attachable("fix login")
        };
        app.apply_agents(&HashMap::from([(PathBuf::from("/p/4/app"), vec![idle])]));
        select(&mut app, "4");
        press(&mut app, KeyCode::Char('h'));
        app.handoff_unsent("someone else", "4");
        assert!(
            app.is_handing_off(&PathBuf::from("/p/4/app")),
            "not its request"
        );
        app.handoff_unsent("fix login", "4");
        assert!(!app.is_handing_off(&PathBuf::from("/p/4/app")));
        assert_eq!(app.wanted_handoff(), None);
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
        select(&mut app, "8");
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
    fn capital_r_resumes_a_tree_whose_agent_stopped_but_not_one_that_runs() {
        let mut app = app();
        let job = queued(press(&mut app, KeyCode::Char('R'))).remove(0);
        assert_eq!((job.kind, job.tree.name.as_str()), (Kind::Resume, "4"));
        assert_eq!(
            press(&mut app, KeyCode::Char('R')),
            Outcome::Continue,
            "once at a time"
        );
        app.jobs.clear();
        app.apply_agents(&HashMap::from([(
            PathBuf::from("/p/4/app"),
            vec![attachable("0908e7f3")],
        )]));
        assert_eq!(press(&mut app, KeyCode::Char('R')), Outcome::Continue);
        assert_eq!(
            app.message.as_deref(),
            Some("0908e7f3 is running in tree 4; Enter opens its chat")
        );
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
    fn v_opens_the_trees_pr_and_f_sends_what_is_selected_to_its_agent() {
        use crate::coderabbit::{Finding, Kind as Found};
        use crate::ship::{BotReview, PrView};
        let mut app = app();
        let path = PathBuf::from("/p/4/app");
        press(&mut app, KeyCode::Char('v'));
        assert_eq!(
            app.pr_popup.as_ref().map(|p| p.path.clone()),
            Some(path.clone())
        );
        assert_eq!(app.wanted_pr(), None, "no ship plugin can read it");
        assert_eq!(
            app.prs[&path].pr,
            Some(Err(
                "none of this repository's ship plugins can read a PR whole".into()
            ))
        );
        app.ship_plugins = vec![Plugin::Builtin("github")];
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(app.prs[&path].is_reading, "^R reads it again");
        assert!(
            app.prs[&path].pr.is_some(),
            "the last read shows while ^R reads again"
        );
        let (asked, branch, _) = app.wanted_pr().expect("github reads it");
        assert_eq!((asked, branch.as_str()), (path.clone(), "feature/end-1-a"));
        app.pr_loaded(
            path.clone(),
            Err(plugin::Error::new(plugin::Code::Auth, "run gh auth login")),
        );
        assert_eq!(
            app.prs[&path].pr,
            Some(Err(
                "gh is not signed in, so treetop cannot read this PR. Run gh auth login.".into()
            ))
        );

        let finding = |kind, title: &str, resolved, prompt: Option<&str>| Finding {
            kind,
            path: Some("src/a.rs".into()),
            lines: Some("12".into()),
            title: title.into(),
            body: "Why.".into(),
            prompt: prompt.map(str::to_string),
            resolved,
            ..Finding::default()
        };
        let pr = PrView {
            number: 37,
            bot: Some(BotReview {
                findings: vec![
                    finding(
                        Found::Issue,
                        "Guard it.",
                        false,
                        Some("Return early at src/a.rs:12."),
                    ),
                    finding(Found::Suggestion, "Done already.", true, None),
                    finding(Found::Nitpick, "Name it.", false, None),
                ],
                ..BotReview::default()
            }),
            ..PrView::default()
        };
        app.pr_loaded(
            path.clone(),
            Ok(Some(PrView {
                bot: None,
                ..pr.clone()
            })),
        );
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(
            app.pr_popup.as_ref().unwrap().tab,
            Tab::Overview,
            "no CodeRabbit tab here"
        );

        app.pr_loaded(path.clone(), Ok(Some(pr)));
        press(&mut app, KeyCode::Char('4'));
        assert_eq!(app.pr_popup.as_ref().unwrap().tab, Tab::CodeRabbit);
        assert_eq!(press(&mut app, KeyCode::Char('f')), Outcome::Continue);
        assert_eq!(
            app.message.as_deref(),
            Some("no agent in tree 4 to send it to")
        );
        app.apply_agents(&HashMap::from([(
            path.clone(),
            vec![attachable("0908e7f3")],
        )]));
        let Outcome::Send { to, text } = press(&mut app, KeyCode::Char('f')) else {
            panic!("f sends the selected finding");
        };
        assert_eq!(to.len(), 1);
        assert_eq!(
            text,
            "CodeRabbit flagged this on #37. Fix it if it still holds, and push.\n\n\
             src/a.rs:12: Guard it.\n\nReturn early at src/a.rs:12."
        );
        assert_eq!(
            app.message.as_deref(),
            Some("sent CodeRabbit on src/a.rs:12 to the agent in tree 4")
        );
        let Outcome::Send { text, .. } = press(&mut app, KeyCode::Char('F')) else {
            panic!("F sends every open finding");
        };
        assert!(
            text.starts_with("CodeRabbit left 1 open comment on #37.")
                && !text.contains("Done already.")
                && !text.contains("Name it."),
            "neither the resolved one nor the nitpick: {text}"
        );

        press(&mut app, KeyCode::Char('G'));
        assert_eq!(
            app.pr_popup.as_ref().unwrap().selected(),
            1,
            "the nitpick is folded"
        );
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(
            app.pr_popup.as_ref().unwrap().selected(),
            2,
            "z lists the nitpick"
        );
        // Esc gives the keys back to the table; the PR stays in its tab.
        press(&mut app, KeyCode::Esc);
        assert!(!app.is_window_focused);
        assert!(app.pr_popup.is_some());
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(
            app.pr_popup.as_ref().unwrap().selected(),
            2,
            "keys go to the table now, not the PR"
        );
    }

    #[test]
    fn a_merge_is_followed_up_once_and_names_what_it_leaves_ready() {
        let mut app = app();
        app.checkout = PathBuf::from("/p/main");
        let mut record = Sidecar::leased_now();
        record.ticket = Some("END-1".into());
        record.source = Some("personal".into());
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
                source: Some("personal".into()),
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

    /// Tree 4 with an agent to chat with, in `status`.
    fn with_chat(status: Status) -> App {
        let mut app = app();
        let tree = app.trees.iter_mut().find(|t| t.name == "4").unwrap();
        tree.agents = vec![Agent {
            status: Some(status),
            ..attachable("fixer")
        }];
        select(&mut app, "4");
        app
    }

    #[test]
    fn the_window_opens_a_running_agents_chat_and_never_a_cold_one() {
        let app = with_chat(Status::Busy);
        assert_eq!(
            app.window_chat(),
            Some((
                vec!["claude".into(), "attach".into(), "fixer".into()],
                "fixer".into(),
                "4".into()
            ))
        );
        // Attaching to a cold agent starts it again, so the window leaves it
        // to Enter.
        assert_eq!(with_chat(Status::Cold).window_chat(), None);
        // Left from the chat wraps round to the info tab, which opens no chat.
        let mut on_info = with_chat(Status::Busy);
        press(&mut on_info, KeyCode::Left);
        assert_eq!(on_info.tab, WindowTab::Info);
        assert_eq!(on_info.window_chat(), None, "the info tab opens no chat");
        press(&mut on_info, KeyCode::Right);
        assert_eq!(on_info.tab, WindowTab::Chat);
    }

    #[test]
    fn ctrl_q_gives_the_window_the_keys_where_it_has_a_chat_and_never_quits() {
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let mut app = with_chat(Status::Idle);
        assert_eq!(app.handle_key(ctrl_q), Outcome::Continue);
        assert!(app.is_window_focused);
        let mut app = with_chat(Status::Idle);
        select(&mut app, "8");
        assert_eq!(app.handle_key(ctrl_q), Outcome::Continue);
        assert!(
            !app.is_window_focused,
            "tree 8 has no chat to take the keys"
        );
        let mut app = with_chat(Status::Idle);
        app.rail = Some(0);
        app.handle_key(ctrl_q);
        assert!(!app.is_window_focused, "the inbox keeps the keys it has");
    }

    #[test]
    fn ctrl_q_lands_in_normal_where_the_keys_scroll_and_step_and_i_or_enter_insert() {
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let mut app = with_chat(Status::Idle);
        app.window_rows = 20;
        app.handle_key(ctrl_q);
        assert!(app.is_window_focused && !app.is_inserting(), "Normal");
        assert_eq!(press(&mut app, KeyCode::Char('j')), Outcome::ScrollChat(1));
        assert_eq!(press(&mut app, KeyCode::Char('k')), Outcome::ScrollChat(-1));
        assert_eq!(press(&mut app, KeyCode::Char('d')), Outcome::ScrollChat(10));
        assert_eq!(
            press(&mut app, KeyCode::Char('u')),
            Outcome::ScrollChat(-10)
        );
        press(&mut app, KeyCode::Right);
        assert_eq!(app.shown_tab(), WindowTab::Diff);
        assert!(app.is_window_focused, "the window keeps the keys");
        press(&mut app, KeyCode::Char('i'));
        assert!(!app.is_inserting(), "nothing to type into on the diff");
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        assert_eq!(app.shown_tab(), WindowTab::Info);
        press(&mut app, KeyCode::Char('i'));
        assert!(!app.is_inserting, "nothing to type into on the info");
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Char('i'));
        assert!(app.is_inserting(), "i on the chat");
        // Stepping away and back lands in Normal again.
        app.tab = WindowTab::Info;
        assert!(!app.is_inserting());
        press(&mut app, KeyCode::Right);
        assert_eq!(app.shown_tab(), WindowTab::Chat);
        assert!(!app.is_inserting(), "a tab stepped to is in Normal");
        app.handle_key(ctrl_q);
        assert!(!app.is_window_focused, "Ctrl+Q from Normal: the table");
        app.handle_key(ctrl_q);
        assert!(app.is_window_focused && !app.is_inserting(), "and back");
        press(&mut app, KeyCode::Enter);
        assert!(app.is_inserting(), "Enter on the chat");
        // Coming back with Ctrl+Q is Normal, whatever the window was in.
        app.handle_key(ctrl_q);
        assert!(!app.is_window_focused);
        app.handle_key(ctrl_q);
        assert!(app.is_window_focused && !app.is_inserting());
    }

    #[test]
    fn a_chat_floating_where_its_window_cannot_show_takes_the_keys_from_ctrl_q() {
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        let mut app = with_chat(Status::Idle);
        app.window_rows = 20;
        app.handle_key(ctrl_q);
        assert!(!app.is_inserting(), "drawn in the window: Normal");
        // Too short for the window, the chat floats with no mode to show.
        app.window_rows = 0;
        assert!(app.is_inserting(), "floating: every key to the chat");
        app.window_rows = 20;
        app.is_issues = true;
        assert!(app.is_inserting(), "over the Issues screen too");
    }

    #[test]
    fn a_chat_opened_on_the_main_checkout_puts_the_cursor_on_its_heading() {
        let mut app = app_on_main_heading();
        select(&mut app, "8");
        let main = app.trees.iter().find(|t| t.is_main).unwrap().name.clone();
        app.select_tree(&main);
        assert_eq!(app.window_tree().map(|t| t.name.clone()), Some(main));
        app.select_tree("4");
        assert_eq!(app.window_tree().map(|t| t.name.as_str()), Some("4"));
    }

    /// A keel run of three steps holding the tree at `path`, on its second.
    fn run_in(path: &str) -> crate::keel::Run {
        let step = |title: &str, state: &str| crate::keel::Step {
            title: title.into(),
            state: state.into(),
        };
        crate::keel::Run {
            tree: path.into(),
            playbook: "issue".into(),
            goal: "fix the login".into(),
            state: "active".into(),
            steps: vec![
                step("Audit it", "done"),
                step("Build it", "doing"),
                step("Verify it", "pending"),
            ],
            current: Some(2),
            attention: 0,
            stale: false,
        }
    }

    #[test]
    fn each_tree_has_the_tabs_it_can_show_and_the_keys_step_through_them() {
        use WindowTab::*;
        let mut app = with_chat(Status::Busy);
        assert_eq!(app.window_tabs(), [Chat, Diff, Pr, Shell, Info]);
        app.runs.insert("/p/4/app".into(), run_in("/p/4/app"));
        assert_eq!(app.window_tabs(), [Chat, Diff, Pr, Run, Shell, Info]);
        for wanted in [Diff, Pr, Run, Shell, Info, Chat] {
            press(&mut app, KeyCode::Right);
            assert_eq!(app.shown_tab(), wanted);
        }
        // A free tree has nothing to diff, no PR and no agent, and no shell
        // to keep running in it: `t` opens one there.
        select(&mut app, "7");
        assert_eq!(app.window_tabs(), [Info]);
        app.tab = Diff;
        assert_eq!(app.shown_tab(), Info, "a tab it has not got shows its info");
    }

    #[test]
    fn the_shell_tab_opens_a_shell_in_the_tree() {
        // A quote in the tree's path must not break out of the command.
        let dir = std::env::temp_dir().join(format!("treetop-shell-{}/it's", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // As pwd names it, where the temp dir is behind a symlink.
        let dir = dir.canonicalize().unwrap();
        let mut app = app();
        select(&mut app, "8");
        app.trees.iter_mut().find(|t| t.name == "8").unwrap().path = dir.clone();
        app.tab = WindowTab::Shell;
        let (command, agent, tree) = app.window_chat().unwrap();
        assert_eq!((agent.as_str(), tree.as_str()), (SHELL, "8"));
        // The user's $SHELL is run in the tree: pwd stands in for it.
        let ran = std::process::Command::new(&command[0])
            .args(&command[1..])
            .env("SHELL", "pwd")
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout).trim(),
            dir.display().to_string(),
            "{command:?}"
        );
    }

    #[test]
    fn the_diff_and_pr_tabs_follow_the_cursor_and_a_pr_read_once_is_not_read_again() {
        let mut app = app();
        press(&mut app, KeyCode::Char('d'));
        assert!(app.is_window_focused, "d gives the diff the keys");
        press(&mut app, KeyCode::Esc);
        select(&mut app, "8");
        app.sync_window();
        assert_eq!(
            app.view.as_ref().map(|v| v.path.clone()),
            Some("/p/8/app".into())
        );
        app.tab = WindowTab::Pr;
        app.sync_window();
        assert_eq!(
            app.pr_wanted,
            Some("/p/8/app".into()),
            "read the first time"
        );
        app.pr_wanted = None;
        select(&mut app, "4");
        app.sync_window();
        assert_eq!(app.pr_wanted, Some("/p/4/app".into()));
        app.pr_wanted = None;
        select(&mut app, "8");
        app.sync_window();
        assert_eq!(
            app.pr_popup.as_ref().map(|p| p.path.clone()),
            Some("/p/8/app".into())
        );
        assert_eq!(app.pr_wanted, None, "tree 8's PR was read already");
    }

    #[test]
    fn the_main_checkout_and_a_finished_run_get_no_tab_they_cannot_fill() {
        use WindowTab::*;
        // The heading stands for the main checkout, which has no PR and no
        // diff of a branch of its own, so d and v leave it alone.
        let app = app_on_main_heading();
        assert_eq!(app.window_tabs(), [Shell, Info]);
        let mut app = with_chat(Status::Busy);
        let mut done = run_in("/p/4/app");
        done.state = "done".into();
        app.runs.insert("/p/4/app".into(), done);
        assert!(
            !app.window_tabs().contains(&Run),
            "a done run holds nothing"
        );
    }

    #[test]
    fn only_the_chat_tab_reads_its_screen_by_the_agents_plugin() {
        let mut app = with_chat(Status::Busy);
        let tree = app.trees.iter_mut().find(|t| t.name == "4").unwrap();
        tree.agents[0].plugin = Some("claude".into());
        assert_eq!(app.window_plugin(), Some("claude"));
        // A shell is no agent's chat: what it prints is its own.
        app.tab = WindowTab::Shell;
        assert_eq!(app.window_plugin(), None);
    }

    #[test]
    fn a_shell_left_running_is_kept_while_its_tree_is_held() {
        let app = app();
        assert!(app.keeps_shell("4"));
        assert!(!app.keeps_shell("7"), "a returned tree's shell ends");
        assert!(!app.keeps_shell("12"), "so does a destroyed one's");
    }

    fn ctrl(app: &mut App, c: char) {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }

    fn cursor_on(app: &App) -> String {
        app.window_tree()
            .map(|t| t.name.clone())
            .unwrap_or_default()
    }

    fn other_on(app: &App) -> Option<String> {
        app.other_window().map(|(t, _)| t.name.clone())
    }

    #[test]
    fn ctrl_backslash_puts_the_next_tree_beside_this_one_and_the_cursor_drives_it() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        assert_eq!(
            other_on(&app).as_deref(),
            Some("4"),
            "tree 4 stays on the left"
        );
        assert_eq!((cursor_on(&app).as_str(), app.side), ("8", 1));
        // Moving the cursor to tree 7, past the available group's heading,
        // changes only the window it drives.
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        assert_eq!(cursor_on(&app), "7");
        assert_eq!(other_on(&app).as_deref(), Some("4"));
        // Without the terminal's extended keys, Ctrl+\ comes as Ctrl+4.
        ctrl(&mut app, '4');
        assert_eq!(other_on(&app), None, "one window again");
    }

    #[test]
    fn ctrl_q_moves_the_keys_round_the_table_and_both_windows() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        ctrl(&mut app, 'q');
        assert!(app.is_window_focused);
        assert_eq!(app.side, 0, "the left window has the keys");
        assert_eq!(cursor_on(&app), "4", "and the cursor drives it");
        assert_eq!(other_on(&app).as_deref(), Some("8"));
        ctrl(&mut app, 'q');
        assert_eq!((app.side, cursor_on(&app).as_str()), (1, "8"));
        assert!(app.is_window_focused);
        ctrl(&mut app, 'q');
        assert!(!app.is_window_focused, "back to the table");
        // Back to one window, the one that last had the keys stays.
        ctrl(&mut app, 'q');
        ctrl(&mut app, '\\');
        assert_eq!((cursor_on(&app).as_str(), other_on(&app)), ("4", None));
    }

    #[test]
    fn a_double_opens_again_as_it_was_left() {
        let file = std::env::temp_dir().join(format!("treetop-layout-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&file);
        let mut app = with_chat(Status::Busy);
        app.keep_layout_in(file.clone());
        ctrl(&mut app, '\\');
        assert_eq!(cursor_on(&app), "8");
        app.save_layout();
        // At start the pool is listed before its agents are read, and the
        // agents then sort it: the cursor goes back to its tree after both.
        let mut again = App::new(None);
        again.keep_layout_in(file.clone());
        let trees = app.trees.clone();
        again.set_trees(trees);
        let mut busy = attachable("fixer");
        busy.status = Some(Status::Waiting);
        again.apply_agents(&HashMap::from([(PathBuf::from("/p/8/app"), vec![busy])]));
        assert_eq!(other_on(&again).as_deref(), Some("4"));
        assert_eq!(cursor_on(&again), "8", "both windows as they were left");
        assert_eq!(again.side, 1);
        ctrl(&mut again, '\\');
        let mut single = with_chat(Status::Busy);
        single.keep_layout_in(file.clone());
        assert_eq!(other_on(&single), None, "one window, as it was left");
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn a_double_keeps_its_trees_when_one_is_hidden_and_starts_afresh_once_one_is_gone() {
        // Hidden by a filter, the other window's tree keeps its place, and
        // the keys stay where they were.
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        app.filter = "end-2".into();
        app.reselect(Some("/p/8/app".into()));
        ctrl(&mut app, 'q');
        assert_eq!(other_on(&app).as_deref(), Some("4"));
        assert_eq!(cursor_on(&app), "8");
        assert!(!app.is_window_focused);
        assert!(app.message.as_deref().unwrap_or("").contains("tree 4"));
        // Its tree gone from the pool, Ctrl+\ opens a new double at once.
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        app.trees.retain(|t| t.name != "4");
        select(&mut app, "8");
        ctrl(&mut app, '\\');
        assert_eq!(other_on(&app).as_deref(), Some("8"));
    }

    #[test]
    fn ctrl_backslash_with_one_tree_listed_says_so_and_keeps_one_window() {
        let mut app = with_chat(Status::Busy);
        app.trees.retain(|t| t.name == "4");
        select(&mut app, "4");
        ctrl(&mut app, '\\');
        assert_eq!(other_on(&app), None);
        assert!(app.message.is_some());
    }

    #[test]
    fn a_window_with_no_typing_to_take_keeps_the_keys_from_the_table() {
        // Tree 7 is free: its window has its info, which takes no typing.
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        select(&mut app, "7");
        ctrl(&mut app, 'q');
        ctrl(&mut app, 'q');
        assert!(app.is_window_focused);
        assert_eq!(cursor_on(&app), "7");
        for key in [KeyCode::Up, KeyCode::Char('n'), KeyCode::Enter] {
            assert_eq!(press(&mut app, key), Outcome::Continue);
        }
        assert_eq!(cursor_on(&app), "7", "the table did not move");
        assert!(app.prompt.is_none(), "n started nothing");
        press(&mut app, KeyCode::Esc);
        assert!(!app.is_window_focused, "Esc gives the keys back");
    }

    #[test]
    fn back_to_one_window_the_keys_stay_only_where_there_is_typing_to_take() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        select(&mut app, "7");
        ctrl(&mut app, 'q');
        ctrl(&mut app, 'q');
        assert!(app.is_window_focused, "tree 7's info window has the keys");
        ctrl(&mut app, '\\');
        assert!(
            !app.is_window_focused,
            "alone, an info window gives them back"
        );
    }

    #[test]
    fn going_round_the_windows_keeps_the_tab_each_was_picked_on() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        app.tab = WindowTab::Shell;
        select(&mut app, "7");
        assert_eq!(app.shown_tab(), WindowTab::Info, "a free tree has no shell");
        for _ in 0..3 {
            ctrl(&mut app, 'q');
        }
        select(&mut app, "8");
        assert_eq!(app.shown_tab(), WindowTab::Shell, "the shell tab was kept");
    }

    #[test]
    fn each_start_minds_its_own_window_and_esc_forgets_the_wait() {
        let mut app = app();
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "one");
        let one = queued(press(&mut app, KeyCode::Enter)).remove(0);
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "two");
        let mut two = queued(press(&mut app, KeyCode::Enter)).remove(0);
        // The first start failing leaves the second's window be.
        app.job_finished(one, false, "lease: no tree is free".into());
        let shown = app.window_start().cloned().unwrap();
        assert_eq!((shown.agent.as_str(), shown.failed), ("two", None));
        two.attach = Some(vec!["claude".into(), "attach".into(), "two".into()]);
        two.tree.name = "8".into();
        two.tree.path = "/p/8/app".into();
        app.job_finished(two, true, String::new());
        assert!(matches!(
            app.ready_chat(),
            Some(Outcome::Chat { agent, tree, .. }) if agent == "two" && tree == "8"
        ));
        // A start whose tree is not listed yet waits for it; Esc forgets
        // that wait, so the next start waits afresh.
        press(&mut app, KeyCode::Char('N'));
        type_in(&mut app, "three");
        let mut three = queued(press(&mut app, KeyCode::Enter)).remove(0);
        three.attach = Some(vec!["claude".into(), "attach".into(), "three".into()]);
        three.tree.name = "12".into();
        app.job_finished(three, true, String::new());
        assert_eq!(app.ready_chat(), None);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.started_at, None);
    }

    #[test]
    fn ctrl_f_zooms_the_table_from_the_table_and_the_windows_from_a_window() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, Some(Zoom::Table));
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, None, "again zooms back out");
        // From a diff with the keys, the windows.
        press(&mut app, KeyCode::Char('d'));
        assert!(app.is_window_focused);
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, Some(Zoom::Windows));
        assert!(app.is_window_focused, "the diff keeps the keys");
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, None);
        // From the PR with the keys, the windows too.
        app.tab = WindowTab::Pr;
        app.sync_window();
        assert!(app.has_keys_on(WindowTab::Pr));
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, Some(Zoom::Windows));
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, None);
        // From a window with nothing to type into, the windows too.
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, '\\');
        select(&mut app, "7");
        ctrl(&mut app, 'q');
        ctrl(&mut app, 'q');
        assert!(app.is_window_focused);
        assert_eq!(app.shown_tab(), WindowTab::Info);
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, Some(Zoom::Windows));
    }

    #[test]
    fn e_zoomed_to_the_windows_zooms_out_to_show_the_inbox() {
        let mut app = with_chat(Status::Busy);
        app.zoom = Some(Zoom::Windows);
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.rail, Some(0), "the inbox has the keys");
        assert_eq!(app.zoom, None, "and room to show itself");
    }

    #[test]
    fn zoomed_to_the_table_ctrl_q_gives_no_hidden_window_the_keys() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, '\\');
        ctrl(&mut app, 'f');
        assert_eq!(app.zoom, Some(Zoom::Table));
        ctrl(&mut app, 'q');
        assert!(!app.is_window_focused, "the table keeps the keys");
    }

    fn type_into_palette(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn the_palette_finds_a_command_by_its_words_and_runs_it_as_its_key_does() {
        let mut app = with_chat(Status::Busy);
        // From a chat with the keys, as from the table.
        app.is_window_focused = true;
        ctrl(&mut app, 'p');
        assert!(app.palette.is_some());
        type_into_palette(&mut app, "ret");
        let first = app.commands().into_iter().next().unwrap();
        assert_eq!(
            (first.label.as_str(), first.key.as_str()),
            ("Return tree 4 to the pool", "r")
        );
        press(&mut app, KeyCode::Enter);
        assert!(app.palette.is_none());
        assert!(app.pending.is_some(), "the return dialog, as r opens it");
        assert!(!app.is_window_focused, "it ran on the table");
        // A command that starts with the word typed comes before one that
        // has it further in, wherever it sits in the list.
        app.pending = None;
        ctrl(&mut app, 'p');
        type_into_palette(&mut app, "issue");
        let first = app.commands().into_iter().next().unwrap();
        assert_eq!(first.label, "Issues");
    }

    #[test]
    fn a_tree_number_offers_to_open_that_tree() {
        let mut app = with_chat(Status::Busy);
        ctrl(&mut app, 'p');
        type_into_palette(&mut app, "8");
        let first = app.commands().into_iter().next().unwrap();
        assert_eq!(first.label, "Open tree 8 · feature/end-2-b");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("8"));
        // Nothing typed, the pool's commands follow the tree's; Esc closes.
        ctrl(&mut app, 'p');
        let all = app.commands();
        assert_eq!(all[0].label, "Return tree 8 to the pool");
        assert!(
            all.iter()
                .any(|c| c.label == "New agent in a fresh tree" && c.key == "N")
        );
        assert!(
            !all.iter().any(|c| c.label.contains(" · ")),
            "trees to open only once typed"
        );
        type_into_palette(&mut app, "zzzz");
        assert!(app.commands().is_empty());
        press(&mut app, KeyCode::Esc);
        assert!(app.palette.is_none());
    }

    #[test]
    fn the_palette_takes_the_keys_over_a_diff_and_waits_out_a_dialog() {
        // Opened from a diff with the keys, the palette takes what is typed.
        let mut app = with_chat(Status::Busy);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.has_keys_on(WindowTab::Diff));
        ctrl(&mut app, 'p');
        type_into_palette(&mut app, "ret");
        assert_eq!(app.palette.as_ref().map(|p| p.query.as_str()), Some("ret"));
        // Over a dialog it does not open, so no command lands in the dialog.
        let mut app = with_chat(Status::Busy);
        press(&mut app, KeyCode::Char('r'));
        assert!(app.pending.is_some());
        ctrl(&mut app, 'p');
        assert!(app.palette.is_none());
    }

    #[test]
    fn the_palette_offers_every_tree_and_an_exact_number_first() {
        // A tree out of the list, behind a filter or in a folded group, is
        // offered too; opening it brings it back so the cursor lands on it.
        let mut app = with_chat(Status::Busy);
        app.filter = "end-1".into();
        select(&mut app, "4");
        ctrl(&mut app, 'p');
        type_into_palette(&mut app, "8");
        assert!(app.commands()[0].label.starts_with("Open tree 8 · "));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("8"));
        assert!(app.filter.is_empty(), "the filter that hid it is cleared");
        let free = app
            .trees
            .iter()
            .find(|t| t.name == "7")
            .and_then(|t| app.group_of(t))
            .unwrap();
        app.folded.insert(free.clone());
        assert!(
            !app.rows()
                .iter()
                .any(|r| matches!(r, ListRow::Tree(t) if t.name == "7"))
        );
        ctrl(&mut app, 'p');
        type_into_palette(&mut app, "7");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("7"));
        assert!(!app.folded.contains(&free), "its group opened");
        let mut trees = app.trees.clone();
        trees.push(Tree {
            name: "48".into(),
            path: "/p/48/app".into(),
            ..app.trees[0].clone()
        });
        app.set_trees(trees);
        ctrl(&mut app, 'p');
        type_into_palette(&mut app, "8");
        let opens: Vec<String> = app
            .commands()
            .into_iter()
            .filter(|c| c.label.contains(" · "))
            .map(|c| c.label)
            .collect();
        assert!(opens[0].starts_with("Open tree 8 · "), "{opens:?}");
    }
}
