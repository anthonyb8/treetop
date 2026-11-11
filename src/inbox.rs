//! What wants you across this workspace and the others: each tree whose
//! agent needs you, died or finished, and each whose PR failed its checks or
//! merged. Another workspace's background agents are placed in its trees by
//! their reports, and its PRs and its agents waiting in a pane come from a
//! slower read of its pool. It is a view of how things stand now, not a log
//! of what happened, so an agent already waiting when treetop opens is in it.
//!
//! Of those, what is waiting on you, an agent asking, a PR whose checks
//! failed and an agent that died, is what the banner and the tmux status
//! line show until it stops being true. What counts is decided here for the
//! phone's pushes too, so the desktop and the phone never disagree. Which
//! entries the phone has opened or dismissed is kept under treetop's state
//! directory.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::agents::Status;
use crate::app::App;
use crate::assistant;
use crate::pool::Tree;
use crate::reports::{Report, State};
use crate::ship::{Ship, Stage};

/// An agent's state as far as the inbox and pushes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    NeedsYou,
    Done,
    Died,
    Other,
}

/// What the tree's agent last came to: waiting on you, at a prompt or with
/// a question; finished; its process dead; else nothing worth saying. None
/// when no report places an agent in it.
pub fn said(app: &App, tree: &Tree) -> Option<Said> {
    let waiting = tree
        .agents
        .iter()
        .any(|a| a.status == Some(Status::Waiting) && !assistant::is_assistant(&a.name));
    match app.says(tree).map(|i| i.state) {
        Some(State::Blocked) => Some(Said::NeedsYou),
        _ if waiting => Some(Said::NeedsYou),
        Some(State::Done) => Some(Said::Done),
        Some(State::Failed) => Some(Said::Died),
        Some(_) => Some(Said::Other),
        None => None,
    }
}

/// The PR stages worth telling you about: its checks failed, or it merged.
pub fn pr_kind(stage: Stage) -> Option<Kind> {
    match stage {
        Stage::CiFailed | Stage::Failed => Some(Kind::Failed),
        Stage::Merged => Some(Kind::Merged),
        _ => None,
    }
}

/// What an entry is about, in the order the inbox groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    NeedsYou,
    /// Its PR's checks failed.
    Failed,
    /// Its agent's process died.
    Died,
    Done,
    /// Its PR merged.
    Merged,
}

impl Kind {
    fn rank(self) -> u8 {
        match self {
            Kind::NeedsYou => 0,
            Kind::Failed | Kind::Died => 1,
            Kind::Done | Kind::Merged => 2,
        }
    }
}

/// Another workspace's entry: where it lives, so opening it switches there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Away {
    pub name: String,
    pub checkout: PathBuf,
}

/// One thing in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What seen and dismissed remember it by: the same until it changes,
    /// so an agent that asks again comes back.
    pub key: String,
    pub kind: Kind,
    /// None for the workspace on screen.
    pub away: Option<Away>,
    /// The tree's name, its number in the pool.
    pub tree: String,
    pub path: PathBuf,
    /// The ticket and branch, as `END-123 fix-login`.
    pub title: Option<String>,
    /// What it is about, on one line: the question, the answer, the PR.
    pub what: String,
    /// When it happened, where that is known; a PR's stage carries no time.
    pub at: Option<SystemTime>,
    /// The chat command of the agent asking, so an ask its chat already
    /// showed counts as seen.
    pub asking: Option<String>,
}

/// Another workspace's trees as its cache last listed them, which is where
/// its agents' reports are placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    pub away: Away,
    pub trees: Vec<Tree>,
    /// treetop's record of each tree it leased there, by path: a tree whose
    /// record names a job is that job's.
    pub sidecars: std::collections::HashMap<PathBuf, crate::sidecar::Sidecar>,
}

/// How long another workspace goes between reads of its agents and PRs,
/// slower than the open workspace's own, since each read runs its ship
/// plugins.
const READ_EVERY: Duration = Duration::from_secs(180);

/// A workspace not opened for this long is not read.
const STALE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Rounds of a reading made off the screen's thread, every so often,
/// until this is dropped.
pub struct Reader<T> {
    pub rounds: Receiver<T>,
    /// Dropped with the reader, which wakes its thread to stop at once
    /// rather than after its next wait and read.
    _stop: mpsc::Sender<()>,
}

/// Runs `read` now and then every `every`, sending each round, until the
/// reader it returns is dropped.
fn spawn_every<T: Send + 'static>(
    every: Duration,
    mut read: impl FnMut() -> T + Send + 'static,
) -> Reader<T> {
    let (sender, rounds) = mpsc::channel();
    let (stop, stopped) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        loop {
            if sender.send(read()).is_err() {
                return;
            }
            // Waits out `every`, unless the reader is dropped first.
            if let Err(mpsc::RecvTimeoutError::Disconnected) | Ok(()) = stopped.recv_timeout(every)
            {
                return;
            }
        }
    });
    Reader {
        rounds,
        _stop: stop,
    }
}

/// The workspaces worth reading from the one at `here`: every other one
/// opened within STALE of `now`, in seconds since the epoch. With only
/// `here`, none.
fn due(
    workspaces: Vec<crate::workspaces::Workspace>,
    here: &Path,
    now: u64,
) -> Vec<crate::workspaces::Workspace> {
    workspaces
        .into_iter()
        .filter(|w| w.checkout != here && now.saturating_sub(w.opened) <= STALE.as_secs())
        .collect()
}

/// Reads each other workspace opened within STALE, every READ_EVERY: its
/// trees with their agents and PRs, by checkout. With no other workspace
/// due it reads nothing. A switch drops it, which stops it.
pub fn spawn_reader(here: PathBuf) -> Reader<HashMap<PathBuf, Vec<Tree>>> {
    spawn_every(READ_EVERY, move || {
        due(crate::workspaces::list(), &here, secs(SystemTime::now()))
            .into_iter()
            .filter_map(|w| {
                let mut trees = crate::cache::read(&w.checkout)?;
                crate::cli::read_agents(&mut trees);
                crate::cli::read_ship(&w.checkout, &mut trees);
                Some((w.checkout, trees))
            })
            .collect()
    })
}

/// The other workspaces' pools, from their caches.
pub fn other_pools(here: &Path) -> Vec<Pool> {
    crate::workspaces::list()
        .into_iter()
        .filter(|w| w.checkout != here)
        .filter_map(|w| {
            let trees = crate::cache::read(&w.checkout)?;
            Some(Pool {
                sidecars: crate::sidecar::read_all(&trees),
                trees,
                away: Away {
                    name: w.name(),
                    checkout: w.checkout,
                },
            })
        })
        .collect()
}

fn secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The first line of `text`, for an entry's one line.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// The tree's ticket and branch, as `END-123 fix-login`, the ticket from
/// `sidecar`, treetop's record of the tree, where it has one.
fn title_of(tree: &Tree, sidecar: Option<&crate::sidecar::Sidecar>) -> Option<String> {
    let ticket = sidecar.and_then(|s| s.ticket.as_deref());
    crate::serve::title(tree.branch.as_deref(), ticket)
}

/// What a died agent's entry says, wherever it lives.
const DIED: &str = "its agent died; R resumes it";

/// Every entry, unsorted: the trees on screen, then the other workspaces'
/// agents placed in their trees.
pub fn entries(app: &App) -> Vec<Entry> {
    let mut entries = Vec::new();
    for tree in app.trees.iter().filter(|t| t.is_held() && !t.is_main) {
        let item = app.says(tree);
        let agent = App::agents_of(tree).into_iter().next();
        let kind = match said(app, tree) {
            Some(Said::NeedsYou) => Some(Kind::NeedsYou),
            Some(Said::Done) => Some(Kind::Done),
            Some(Said::Died) => Some(Kind::Died),
            _ => None,
        };
        if let Some(kind) = kind {
            let what = match kind {
                Kind::NeedsYou => item
                    .as_ref()
                    .filter(|i| i.state == State::Blocked)
                    .map_or_else(
                        || "waiting at a prompt in its chat".into(),
                        |i| first_line(i.text()),
                    ),
                Kind::Died => DIED.into(),
                _ => item
                    .as_ref()
                    .map_or_else(String::new, |i| first_line(i.text())),
            };
            let at = item.as_ref().map(|i| i.report.updated);
            let stamp = item.as_ref().map_or_else(
                || agent.map_or_else(String::new, |a| a.name.clone()),
                |i| format!("{}@{}", i.report.job, secs(i.report.updated)),
            );
            entries.push(Entry {
                key: format!("{kind:?}:{}:{stamp}", tree.path.display()),
                kind,
                away: None,
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, tree.sidecar.as_deref()),
                what,
                at,
                asking: (kind == Kind::NeedsYou)
                    .then(|| agent.and_then(|a| a.attach.as_ref()).map(|c| c.join(" ")))
                    .flatten(),
            });
        }
        if let Some(ship) = &tree.ship
            && let Some(kind) = pr_kind(ship.stage)
        {
            entries.push(Entry {
                key: pr_key(kind, tree, ship),
                kind,
                away: None,
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, tree.sidecar.as_deref()),
                what: ship.label.clone(),
                at: None,
                asking: None,
            });
        }
    }
    entries.extend(away_entries(app));
    entries
}

/// What a waiting entry is about: its kind and tree, not the moment it
/// last said so, so an agent still asking is the one ask however often it
/// writes its report, and comes back as a new one only once it has stopped.
pub type About = (Kind, PathBuf);

impl Entry {
    pub fn about(&self) -> About {
        (self.kind, self.path.clone())
    }
}

/// What waits on you, across every workspace read: agents asking, PRs
/// whose checks failed and agents that died, the oldest first. Each is as
/// old as it has waited: since treetop first saw it, or since it happened
/// where that was earlier. Each leaves once it stops being true, so there
/// is nothing to dismiss.
pub fn waiting(app: &App) -> Vec<Entry> {
    let mut waiting: Vec<Entry> = entries(app)
        .into_iter()
        .filter(|e| matches!(e.kind, Kind::NeedsYou | Kind::Failed | Kind::Died))
        .map(|mut e| {
            e.at = app.waiting_since(&e.about()).or(e.at);
            e
        })
        .collect();
    waiting.sort_by(|a, b| {
        // Where no time is known yet, as before treetop has looked, last.
        a.at.is_none()
            .cmp(&b.at.is_none())
            .then(a.at.cmp(&b.at))
            .then(a.away.is_some().cmp(&b.away.is_some()))
            .then(a.tree.cmp(&b.tree))
    });
    waiting
}

/// Where an entry's tree is, for a line naming several: its number, after
/// its workspace's name when that is another.
pub fn place_of(entry: &Entry) -> String {
    match &entry.away {
        Some(away) => format!("{}:{}", away.name, entry.tree),
        None => entry.tree.clone(),
    }
}

/// What a PR entry is remembered by: the commit its PR is on, so a new push
/// that fails again is new, else its label.
fn pr_key(kind: Kind, tree: &Tree, ship: &Ship) -> String {
    let at = ship.head.as_deref().unwrap_or(&ship.label);
    format!("{kind:?}:{}:{at}", tree.path.display())
}

/// The other workspaces' entries: their background agents that need you,
/// died or finished, each placed in the tree its report says it works in;
/// then, from the last read of each pool, its PRs that failed or merged and
/// its agents waiting on you that no report speaks for.
fn away_entries(app: &App) -> Vec<Entry> {
    let mut entries = reported_away(app);
    for pool in &app.pools {
        let Some(live) = app.away_live.get(&pool.away.checkout) else {
            continue;
        };
        for tree in pool.trees.iter().filter(|t| t.is_held() && !t.is_main) {
            let title = title_of(tree, pool.sidecars.get(&tree.path));
            let entry = |kind: Kind, key: String, what: String| Entry {
                key,
                kind,
                away: Some(pool.away.clone()),
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title.clone(),
                what,
                at: None,
                asking: None,
            };
            let read = live.iter().find(|t| t.path == tree.path);
            if let Some(ship) = read.and_then(|t| t.ship.as_deref())
                && let Some(kind) = pr_kind(ship.stage)
            {
                entries.push(entry(kind, pr_key(kind, tree, ship), ship.label.clone()));
            }
            let is_asking = entries
                .iter()
                .any(|e| e.kind == Kind::NeedsYou && e.path == tree.path);
            let waiting = read
                .into_iter()
                .flat_map(|t| &t.agents)
                .find(|a| a.status == Some(Status::Waiting) && !assistant::is_assistant(&a.name));
            if let Some(agent) = waiting.filter(|_| !is_asking) {
                let key = format!(
                    "{:?}:{}:{}@{}",
                    Kind::NeedsYou,
                    tree.path.display(),
                    agent.name,
                    agent.active_at.unwrap_or(0)
                );
                let mut waits = entry(
                    Kind::NeedsYou,
                    key,
                    "waiting at a prompt in its pane".into(),
                );
                waits.at = agent
                    .active_at
                    .map(|at| UNIX_EPOCH + Duration::from_secs(at));
                entries.push(waits);
            }
        }
    }
    entries
}

/// The other workspaces' background agents that need you, died or
/// finished, each placed in the tree its report says it works in.
fn reported_away(app: &App) -> Vec<Entry> {
    app.reports
        .iter()
        .filter(|r| !assistant::is_assistant(&r.name))
        .filter_map(|report| {
            let kind = match report.state {
                State::Blocked => Kind::NeedsYou,
                State::Done => Kind::Done,
                State::Failed => Kind::Died,
                State::Working | State::Stopped => return None,
            };
            let (pool, tree) = place(report, &app.pools)?;
            Some(Entry {
                key: format!(
                    "{kind:?}:{}:{}@{}",
                    tree.path.display(),
                    report.job,
                    secs(report.updated)
                ),
                kind,
                away: Some(pool.away.clone()),
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, pool.sidecars.get(&tree.path)),
                what: if kind == Kind::Died {
                    DIED.into()
                } else {
                    first_line(&report.text)
                },
                at: Some(report.updated),
                asking: None,
            })
        })
        .collect()
}

/// The other workspace's held tree `report`'s agent works in: the one its
/// directory is in, as agents are placed: one whose sidecar names a job only
/// for that job, else only if it wrote since that tree's lease began, since
/// a tree returned and handed out again is not still the old agent's.
pub fn place<'a>(report: &Report, pools: &'a [Pool]) -> Option<(&'a Pool, &'a Tree)> {
    let dir = report.dir.as_deref()?;
    let wrote = secs(report.updated);
    pools.iter().find_map(|pool| {
        let tree = pool.trees.iter().find(|t| {
            if !t.is_held() || !dir.starts_with(&t.path) {
                return false;
            }
            // A tree whose sidecar names a job is that job's alone.
            match pool.sidecars.get(&t.path).and_then(|s| s.job.as_deref()) {
                Some(job) => job == report.job,
                None => t
                    .leased_at
                    .as_deref()
                    .and_then(crate::pool::seconds)
                    .is_some_and(|began| wrote >= began),
            }
        })?;
        Some((pool, tree))
    })
}

/// The entries in the order the inbox shows them, with whether each has
/// been seen: unseen first, what needs you, then what failed, then what is
/// done, newest first in each; seen ones after, the same way. Dismissed
/// ones are left out.
pub fn shown(app: &App) -> Vec<(Entry, bool)> {
    let mut shown: Vec<(Entry, bool)> = entries(app)
        .into_iter()
        .filter(|e| !app.inbox.dismissed.contains(&e.key))
        .map(|e| {
            let seen = app.inbox.seen.contains(&e.key)
                || e.asking.as_ref().is_some_and(|c| app.has_asked(c));
            (e, seen)
        })
        .collect();
    shown.sort_by(|(a, a_seen), (b, b_seen)| {
        a_seen
            .cmp(b_seen)
            .then(a.kind.rank().cmp(&b.kind.rank()))
            .then(b.at.cmp(&a.at))
            .then(a.away.is_some().cmp(&b.away.is_some()))
            .then(a.tree.cmp(&b.tree))
    });
    shown
}

/// What you have done with the inbox, kept across runs: the entries opened
/// and dismissed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Marks {
    #[serde(default)]
    pub seen: BTreeSet<String>,
    #[serde(default)]
    pub dismissed: BTreeSet<String>,
}

/// How many keys each set keeps. An entry's key changes when it does, so
/// old keys pile up; past this many the set starts again, which at worst
/// shows a few old entries as new once.
const KEPT: usize = 1_000;

impl Marks {
    /// Writes the marks to `file`, keeping those another treetop wrote
    /// there since this one read it, so an entry opened on the phone is not
    /// unopened again by the desktop's next write, or the other way round.
    pub fn write(&mut self, file: &Path) {
        let mine = std::mem::take(self);
        *self = change(file, |marks| {
            marks.seen.extend(mine.seen);
            marks.dismissed.extend(mine.dismissed);
        });
    }
}

/// Applies `change` to the marks in `file` as they stand and writes them
/// back, returning them. A lock beside the file is held throughout, so
/// another write at the same moment, from this treetop or the desktop's or
/// the phone's server, waits for this one rather than writing over it.
pub fn change(file: &Path, change: impl FnOnce(&mut Marks)) -> Marks {
    // Without the lock, as where the directory cannot be written, the
    // write goes ahead unguarded: a lost mark only shows an entry again.
    let _lock = lock_beside(file);
    let mut marks: Marks = crate::state::read(file);
    change(&mut marks);
    for set in [&mut marks.seen, &mut marks.dismissed] {
        if set.len() > KEPT {
            set.clear();
        }
    }
    crate::state::write(file, &marks);
    marks
}

/// An exclusive lock on a file beside `file`, released when dropped.
fn lock_beside(file: &Path) -> Option<std::fs::File> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(file.with_extension("json.lock"))
        .ok()?;
    lock.lock().ok()?;
    Some(lock)
}

/// The file the marks are kept in. One for every workspace, since an
/// entry opened in one is seen from all.
pub fn file() -> Option<PathBuf> {
    Some(crate::state::dir()?.join("inbox.json"))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::agents::{Agent, Source};
    use crate::app::Outcome;
    use crate::ship::Ship;
    use crate::sidecar::Sidecar;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use std::fs;
    use std::time::Duration;

    fn agent(name: &str, job: &str, status: Status) -> Agent {
        Agent {
            name: name.into(),
            pid: Some(1),
            status: Some(status),
            source: Source::Plugin,
            is_background: true,
            attach: Some(vec!["claude".into(), "attach".into(), job.into()]),
            job: Some(job.into()),
            plugin: None,
            active_at: None,
            context: None,
            label: None,
            spend: None,
        }
    }

    pub fn tree(path: &str, name: &str, branch: &str, agents: Vec<Agent>) -> Tree {
        Tree {
            name: name.into(),
            status: "leased".into(),
            branch: Some(branch.into()),
            holder: Some("claude:x".into()),
            path: PathBuf::from(path),
            leased_at: None,
            processes: Vec::new(),
            agents,
            work: None,
            is_main: false,
            sidecar: None,
            ship: None,
        }
    }

    pub fn report(job: &str, dir: &str, state: State, text: &str, ago: u64) -> Report {
        Report {
            job: job.into(),
            name: job.into(),
            state,
            text: text.into(),
            dir: Some(PathBuf::from(dir)),
            updated: SystemTime::now() - Duration::from_secs(ago),
            steps: Vec::new(),
        }
    }

    /// A pool with one tree of each kind, and another workspace whose
    /// agent waits on you in its tree 12.
    pub fn app() -> App {
        let mut app = App::new(None);
        let mut asking = tree(
            "/pool/4/app",
            "4",
            "me/anb-41-inbox-rail",
            vec![agent("inbox-rail", "j4", Status::Idle)],
        );
        asking.sidecar = Some(Box::new(Sidecar {
            ticket: Some("ANB-41".into()),
            ..Sidecar::leased_now()
        }));
        let mut failing = tree(
            "/pool/7/app",
            "7",
            "me/fix-login",
            vec![agent("fix-login", "j7", Status::Busy)],
        );
        failing.ship = Some(Box::new(Ship {
            branch: "me/fix-login".into(),
            stage: Stage::CiFailed,
            label: "#212 ✗ e2e".into(),
            url: None,
            head: None,
            detail: Vec::new(),
            plugin: "github".into(),
        }));
        app.trees = vec![
            asking,
            tree(
                "/pool/2/app",
                "2",
                "me/help-columns",
                vec![agent("help-columns", "j2", Status::Idle)],
            ),
            failing,
            tree(
                "/pool/9/app",
                "9",
                "me/port-menu",
                vec![agent("port-menu", "j9", Status::Cold)],
            ),
        ];
        app.is_loaded = true;
        app.is_agents_read = true;
        app.pools = vec![Pool {
            sidecars: Default::default(),
            away: Away {
                name: "endodeals-web".into(),
                checkout: "/code/endodeals-web".into(),
            },
            trees: vec![Tree {
                leased_at: Some("2026-01-01T09:00:00-05:00".into()),
                ..tree("/other/12/web", "12", "me/END-88-checkout-tax", Vec::new())
            }],
        }];
        app.apply_reports(vec![
            report(
                "j4",
                "/pool/4/app",
                State::Blocked,
                "Where should the inbox sit?",
                120,
            ),
            report(
                "j2",
                "/pool/2/app",
                State::Done,
                "Done: opened PR #31",
                1_100,
            ),
            report("j9", "/pool/9/app", State::Failed, "", 1_900),
            report(
                "j12",
                "/other/12/web/src",
                State::Blocked,
                "Reseed the rates?",
                360,
            ),
        ]);
        app
    }

    /// Settles everything in `app` that waits on you: the asks and the
    /// dead agent reported, tree 7's failed PR and the other workspace.
    pub fn nothing_waits(app: &mut App) {
        app.reports.clear();
        app.trees.retain(|t| t.name != "7");
        app.pools.clear();
        assert!(waiting(app).is_empty(), "{:?}", waiting(app));
    }

    fn order(app: &App) -> Vec<(Kind, String, bool)> {
        shown(app)
            .into_iter()
            .map(|(e, seen)| (e.kind, e.tree, seen))
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) -> Outcome {
        app.handle_key(KeyEvent::from(code))
    }

    #[test]
    fn what_needs_you_comes_first_then_what_failed_then_what_is_done() {
        let app = app();
        assert_eq!(
            order(&app),
            [
                (Kind::NeedsYou, "4".into(), false),
                (Kind::NeedsYou, "12".into(), false),
                (Kind::Died, "9".into(), false),
                (Kind::Failed, "7".into(), false),
                (Kind::Done, "2".into(), false),
            ]
        );
        let away = &shown(&app)[1].0;
        assert_eq!(
            away.away.as_ref().map(|a| a.name.as_str()),
            Some("endodeals-web")
        );
        assert_eq!(away.title.as_deref(), Some("END-88 checkout-tax"));
    }

    /// Presses `e` until it does what `wanted` looks for, at most once
    /// round everything waiting.
    fn press_e_until(app: &mut App, wanted: impl Fn(&App, &Outcome) -> bool) -> Outcome {
        for _ in 0..=waiting(app).len() {
            let outcome = press(app, KeyCode::Char('e'));
            if wanted(app, &outcome) {
                return outcome;
            }
        }
        panic!("e never got there: {:?}", order(app));
    }

    #[test]
    fn e_goes_to_the_oldest_that_waits_and_again_round_the_rest() {
        let mut app = app();
        // Tree 9's agent died 1,900s ago, the oldest.
        assert_eq!(press(&mut app, KeyCode::Char('e')), Outcome::Continue);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("9"));
        assert!(app.message.as_deref().unwrap().contains("R resumes"));
        assert!(
            matches!(press(&mut app, KeyCode::Char('e')), Outcome::SwitchTo { ref tree, .. } if tree == Path::new("/other/12/web")),
            "then the ask of 360s ago, in another workspace"
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('e')),
            Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "j4".into()],
                agent: "inbox-rail".into(),
                tree: "4".into(),
            },
            "then the ask of 120s ago here"
        );
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("4"));
        assert_eq!(press(&mut app, KeyCode::Char('e')), Outcome::Continue);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("7"));
        assert!(app.message.as_deref().unwrap().contains("f sends"));
        assert_eq!(press(&mut app, KeyCode::Char('e')), Outcome::Continue);
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("9"),
            "and round again"
        );
    }

    #[test]
    fn an_ask_whose_chat_opened_for_you_counts_as_seen() {
        let mut app = app();
        app.saw_chat(&["claude".into(), "attach".into(), "j4".into()]);
        assert_eq!(
            order(&app).last(),
            Some(&(Kind::NeedsYou, "4".into(), true))
        );
    }

    #[test]
    fn a_dismissed_entry_stays_gone_across_runs_until_its_agent_asks_again() {
        let file = std::env::temp_dir().join(format!("treetop-inbox-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        // The phone dismisses it.
        let key = shown(&app())[0].0.key.clone();
        change(&file, |marks| {
            marks.dismissed.insert(key);
        });

        let mut again = self::app();
        again.keep_inbox_in(file.clone());
        assert!(!order(&again).iter().any(|(_, tree, _)| tree == "4"));

        let mut reports = again.reports.clone();
        reports[0] = report("j4", "/pool/4/app", State::Blocked, "And the strip?", 5);
        again.apply_reports(reports);
        assert_eq!(order(&again)[0], (Kind::NeedsYou, "4".into(), false));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn what_the_phone_dismissed_survives_the_desktop_writing_its_own_marks() {
        let file =
            std::env::temp_dir().join(format!("treetop-inbox-both-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let mut desktop = app();
        desktop.keep_inbox_in(file.clone());
        let mut phone = Marks::default();
        phone.dismissed.insert("from the phone".into());
        phone.write(&file);

        desktop.saw_chat(&["claude".into(), "attach".into(), "j4".into()]);

        let kept: Marks = crate::state::read(&file);
        assert!(kept.dismissed.contains("from the phone"));
        assert_eq!(kept.seen.len(), 1, "{kept:?}");
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn another_workspaces_entry_switches_there_and_opens_its_agent() {
        let mut app = app();
        assert_eq!(
            press_e_until(&mut app, |_, o| matches!(o, Outcome::SwitchTo { .. })),
            Outcome::SwitchTo {
                checkout: "/code/endodeals-web".into(),
                tree: "/other/12/web".into(),
                kind: Kind::NeedsYou,
            }
        );

        // The pool of endodeals-web, as the switch opens it.
        let mut there = App::new(None);
        there.arriving = Some(("/other/12/web".into(), Kind::NeedsYou));
        there.trees = vec![tree(
            "/other/12/web",
            "12",
            "me/END-88-checkout-tax",
            vec![agent("checkout-tax", "j12", Status::Waiting)],
        )];
        assert_eq!(
            there.arrival(),
            None,
            "nothing opens before the agents are read"
        );
        there.is_agents_read = true;
        assert_eq!(
            there.arrival(),
            Some(Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "j12".into()],
                agent: "checkout-tax".into(),
                tree: "12".into(),
            })
        );
        assert_eq!(there.current().map(|t| t.name.as_str()), Some("12"));
        assert_eq!(there.arrival(), None, "it opens once");
    }

    #[test]
    fn a_report_from_before_its_trees_lease_is_not_that_trees() {
        let mut app = app();
        app.pools[0].trees[0].leased_at = Some("2099-01-01T09:00:00-05:00".into());
        assert!(!order(&app).iter().any(|(_, tree, _)| tree == "12"));
    }

    #[test]
    fn a_tree_whose_sidecar_names_another_job_is_not_that_reports() {
        let mut app = app();
        let named = |job: &str| Sidecar {
            job: Some(job.into()),
            ..Sidecar::leased_now()
        };
        app.pools[0].sidecars = [("/other/12/web".into(), named("someone-else"))].into();
        assert!(!order(&app).iter().any(|(_, tree, _)| tree == "12"));
        app.pools[0].sidecars = [("/other/12/web".into(), named("j12"))].into();
        assert!(order(&app).iter().any(|(_, tree, _)| tree == "12"));
    }

    #[test]
    fn another_workspaces_entry_names_the_ticket_its_sidecar_records() {
        let mut app = app();
        app.pools[0].trees[0].branch = Some("me/checkout-tax".into());
        app.pools[0].sidecars = [(
            "/other/12/web".into(),
            Sidecar {
                job: Some("j12".into()),
                ticket: Some("END-88".into()),
                ..Sidecar::leased_now()
            },
        )]
        .into();
        let away = shown(&app)
            .into_iter()
            .find(|(e, _)| e.tree == "12")
            .unwrap()
            .0;
        assert_eq!(away.title.as_deref(), Some("END-88 checkout-tax"));
    }

    #[test]
    fn an_ask_seen_in_one_workspace_is_seen_from_every_other() {
        let file =
            std::env::temp_dir().join(format!("treetop-inbox-asked-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let mut here = app();
        here.keep_inbox_in(file.clone());
        here.saw_chat(&["claude".into(), "attach".into(), "j4".into()]);

        // Another workspace's treetop: its own asked file, the same inbox.
        let mut there = app();
        there.keep_inbox_in(file.clone());
        assert!(!there.has_asked("claude attach j4"));
        assert_eq!(
            order(&there).last(),
            Some(&(Kind::NeedsYou, "4".into(), true))
        );
        let _ = fs::remove_file(&file);
    }

    /// endodeals-web's last read: tree 7's PR failing on commit `abc`, and
    /// tree 5's agent waiting in a tmux pane with no report.
    fn read_elsewhere(app: &mut App, head: &str) {
        let mut failing = tree("/other/7/web", "7", "me/END-91-fix-login", Vec::new());
        failing.ship = Some(Box::new(Ship {
            branch: "me/END-91-fix-login".into(),
            stage: Stage::CiFailed,
            label: "#218 ✗ ci".into(),
            url: None,
            head: Some(head.into()),
            detail: Vec::new(),
            plugin: "github".into(),
        }));
        let pane = Agent {
            attach: None,
            job: None,
            is_background: false,
            active_at: Some(1_793_900_000),
            ..agent("claude", "", Status::Waiting)
        };
        let waiting = tree("/other/5/web", "5", "me/seo-meta", vec![pane]);
        app.pools[0]
            .trees
            .extend([failing.clone(), waiting.clone()]);
        app.away_live = [("/code/endodeals-web".into(), vec![failing, waiting])].into();
    }

    fn entry_of(app: &App, tree: &str) -> Entry {
        shown(app)
            .into_iter()
            .map(|(e, _)| e)
            .find(|e| e.tree == tree && e.away.is_some())
            .unwrap_or_else(|| panic!("no entry for {tree} in {:?}", order(app)))
    }

    #[test]
    fn another_workspaces_failed_pr_lands_on_its_tree_there() {
        let mut app = app();
        read_elsewhere(&mut app, "abc");
        let failed = entry_of(&app, "7");
        assert_eq!(
            (failed.kind, failed.what.as_str()),
            (Kind::Failed, "#218 ✗ ci")
        );
        assert_eq!(failed.title.as_deref(), Some("END-91 fix-login"));

        assert_eq!(
            press_e_until(&mut app, |_, o| {
                matches!(
                    o,
                    Outcome::SwitchTo {
                        kind: Kind::Failed,
                        ..
                    }
                )
            }),
            Outcome::SwitchTo {
                checkout: "/code/endodeals-web".into(),
                tree: "/other/7/web".into(),
                kind: Kind::Failed,
            }
        );

        // endodeals-web, as the switch opens it.
        let mut there = App::new(None);
        there.trees = vec![tree("/other/7/web", "7", "me/END-91-fix-login", Vec::new())];
        there.is_agents_read = true;
        there.arriving = Some(("/other/7/web".into(), Kind::Failed));
        assert_eq!(there.arrival(), None, "no chat opens for a PR");
        assert_eq!(there.current().map(|t| t.name.as_str()), Some("7"));
        assert!(
            there.message.as_deref().unwrap().contains("f sends"),
            "{:?}",
            there.message
        );
    }

    #[test]
    fn an_agent_waiting_in_a_pane_elsewhere_needs_you_once() {
        let mut app = app();
        read_elsewhere(&mut app, "abc");
        let waiting = entry_of(&app, "5");
        assert_eq!(
            (waiting.kind, waiting.what.as_str()),
            (Kind::NeedsYou, "waiting at a prompt in its pane")
        );
        // Tree 12's agent asks through its report; a pane waiting there too
        // is the same ask, not a second entry.
        let mut asking = app.pools[0].trees[0].clone();
        asking.agents = vec![agent("checkout-tax", "j12", Status::Waiting)];
        app.away_live
            .get_mut(&PathBuf::from("/code/endodeals-web"))
            .unwrap()
            .push(asking);
        let twelves = order(&app).iter().filter(|(_, t, _)| t == "12").count();
        assert_eq!(twelves, 1);
    }

    #[test]
    fn a_pane_agent_elsewhere_is_gone_to_in_tmux_and_left_for_t_outside() {
        let pane = Agent {
            attach: None,
            ..agent("claude", "", Status::Waiting)
        };
        let arrive = |is_in_tmux: bool| {
            let mut there = App::new(None);
            there.is_in_tmux = is_in_tmux;
            there.trees = vec![tree("/other/5/web", "5", "me/seo-meta", vec![pane.clone()])];
            there.is_agents_read = true;
            there.arriving = Some(("/other/5/web".into(), Kind::NeedsYou));
            (there.arrival(), there.message)
        };
        assert!(matches!(arrive(true).0, Some(Outcome::Enter(t)) if t.name == "5"));
        let (outcome, message) = arrive(false);
        assert_eq!(outcome, None, "no shell opens by itself");
        assert!(message.unwrap().contains("t goes to it"));
    }

    #[test]
    fn a_dismissed_pr_comes_back_when_a_new_push_fails_again() {
        let away_seven = |app: &App| {
            shown(app)
                .into_iter()
                .find(|(e, _)| e.tree == "7" && e.away.is_some())
                .map(|(_, seen)| seen)
        };
        let mut app = app();
        read_elsewhere(&mut app, "abc");
        let failed = entry_of(&app, "7");
        app.inbox.dismissed.insert(failed.key);
        assert_eq!(away_seven(&app), None, "dismissed");
        // The next read finds a new push, failing too.
        let live = app
            .away_live
            .get_mut(&PathBuf::from("/code/endodeals-web"))
            .unwrap();
        live[0].ship.as_mut().unwrap().head = Some("def".into());
        assert_eq!(
            away_seven(&app),
            Some(false),
            "back, unseen, on the new push"
        );
    }

    #[test]
    fn a_dropped_reader_stops_at_once_rather_than_after_its_next_round() {
        let reads = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = std::sync::Arc::clone(&reads);
        // Far longer than the test may take, so only stopping at once,
        // which drops `read` and its count, gets the count back to one.
        let reader = spawn_every(Duration::from_secs(3600), move || {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        reader.rounds.recv().unwrap();
        drop(reader);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::sync::Arc::strong_count(&reads) > 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "the reader's thread is still waiting out its round"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn only_other_workspaces_opened_this_week_are_read() {
        let now = 1_793_900_000;
        let opened = |checkout: &str, days_ago: u64| crate::workspaces::Workspace {
            checkout: checkout.into(),
            opened: now - days_ago * 86_400,
        };
        let here = Path::new("/code/treetop");
        assert!(due(vec![opened("/code/treetop", 0)], here, now).is_empty());
        let read: Vec<PathBuf> = due(
            vec![
                opened("/code/treetop", 0),
                opened("/code/endodeals-web", 2),
                opened("/code/lander", 30),
            ],
            here,
            now,
        )
        .into_iter()
        .map(|w| w.checkout)
        .collect();
        assert_eq!(read, [PathBuf::from("/code/endodeals-web")]);
    }

    #[test]
    fn the_bell_rings_once_for_what_starts_waiting_and_tmux_says_what_waits() {
        // Treetop opening: its trees listed, its agents' reports not yet read.
        let read = app();
        let mut app = App::new(None);
        app.trees = read.trees.clone();
        app.pools = read.pools.clone();
        app.is_loaded = true;
        app.is_agents_read = true;
        assert_eq!(app.watch_waiting(), None, "no look before the reports");
        app.apply_reports(read.reports.clone());
        // What already waits once they are read rings nothing.
        let (is_new, line) = app.watch_waiting().unwrap();
        assert!(!is_new);
        assert_eq!(
            line.as_deref(),
            Some("● 4 waiting · 9 endodeals-web:12 4 7"),
            "the oldest first; tree 7's failed PR since treetop saw it"
        );
        assert_eq!(app.watch_waiting(), Some((false, line)), "nor again");
        // Tree 4's agent, still asking, writes its report again: the same
        // ask, so no bell.
        let mut reports = app.reports.clone();
        reports[0] = report("j4", "/pool/4/app", State::Blocked, "Still: where?", 1);
        app.apply_reports(reports);
        assert_eq!(app.watch_waiting().map(|(is_new, _)| is_new), Some(false));
        // Tree 2's agent comes to ask something, and the other workspace's,
        // back at work a moment, asks again: the bell rings for them here,
        // with no news besides.
        let mut reports = app.reports.clone();
        reports[3].state = State::Working;
        app.apply_reports(reports.clone());
        app.watch_waiting();
        reports[1] = report("j2", "/pool/2/app", State::Blocked, "Which column?", 5);
        reports[3].state = State::Blocked;
        assert_eq!(app.apply_reports(reports), Vec::<String>::new());
        let (is_new, line) = app.watch_waiting().unwrap();
        assert!(is_new, "a new ask rings");
        assert_eq!(
            line.as_deref(),
            Some("● 5 waiting · 9 endodeals-web:12 4 2 7"),
            "the oldest first"
        );
        // Once nothing waits, the status line clears.
        nothing_waits(&mut app);
        assert_eq!(app.watch_waiting(), Some((false, None)));
    }

    #[test]
    fn e_keeps_its_place_across_a_switch_of_workspace() {
        let mut here = app();
        press(&mut here, KeyCode::Char('e'));
        assert!(matches!(
            press(&mut here, KeyCode::Char('e')),
            Outcome::SwitchTo { .. }
        ));
        // The switch starts the next workspace afresh, with what `e` had
        // been through carried over; here the same trees, read from there.
        let mut there = app();
        there.keep_watch(here.take_watch());
        assert!(
            matches!(press(&mut there, KeyCode::Char('e')), Outcome::Chat { ref tree, .. } if tree == "4"),
            "on to the one after tree 12, not back to the first"
        );
        assert_eq!(
            there.watch_waiting().map(|(is_new, _)| is_new),
            Some(false),
            "and the switch rings nothing"
        );
    }

    #[test]
    fn e_from_a_chat_in_normal_leaves_it_for_what_waits() {
        let mut app = app();
        app.select_tree("2");
        app.tab = crate::app::WindowTab::Chat;
        app.is_window_focused = true;
        assert_eq!(press(&mut app, KeyCode::Char('e')), Outcome::Continue);
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("9"),
            "the oldest, tree 9's dead agent"
        );
        assert!(!app.is_window_focused, "the window gives the keys up");
    }

    #[test]
    fn e_with_nothing_waiting_says_so() {
        let mut app = app();
        nothing_waits(&mut app);
        assert_eq!(press(&mut app, KeyCode::Char('e')), Outcome::Continue);
        assert_eq!(app.message.as_deref(), Some("nothing is waiting on you"));
    }

    #[test]
    fn the_banner_takes_the_mouse_only_while_something_waits() {
        let mut app = app();
        assert!(app.wants_mouse(), "for a click on the banner");
        nothing_waits(&mut app);
        assert!(!app.wants_mouse(), "text selects in the terminal as ever");
    }
}
