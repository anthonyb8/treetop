//! What wants you across this workspace and the others: each tree whose
//! agent needs you, died or finished, and each whose PR failed its checks or
//! merged. Another workspace's background agents are placed in its trees by
//! their reports, and its PRs and its agents waiting in a pane come from a
//! slower read of its pool. It is a view of how things stand now, not a log
//! of what happened, so an agent already waiting when treetop opens is in it.
//!
//! Of those, what is waiting on you, an agent asking, a PR whose checks
//! failed and an agent that died, is what the count, its notifications
//! and the tmux status line show until it stops being true. What counts is decided here for the
//! phone's pushes too, so the desktop and the phone never disagree.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::agents::Status;
use crate::app::App;
use crate::assistant;
use crate::pool::Tree;
use crate::reports::{Report, State};
use crate::ship::Stage;

/// An agent's state as far as what waits and pushes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    NeedsYou,
    Done,
    Died,
    Other,
}

/// What the tree's agents last came to, by the reports placed in it and
/// its agents' running sessions: waiting on you, at a prompt or with a
/// question; finished; its process dead; else nothing worth saying. None
/// when neither a report nor a finished turn speaks for it. The same rules
/// as for another workspace's tree, through `judge`.
pub fn said(app: &App, tree: &Tree) -> Option<Said> {
    judge(&placed(app, tree), tree, &app.trees).0
}

/// The reports placed in `tree`, one of this workspace's, the assistant's
/// left out.
fn placed<'a>(app: &'a App, tree: &Tree) -> Vec<&'a Report> {
    app.reports
        .iter()
        .filter(|r| !assistant::is_assistant(&r.name))
        .filter(|r| crate::reports::tree_of(r, &app.trees).is_some_and(|t| t.path == tree.path))
        .collect()
}

/// What `tree` comes to from `reports`, the ones placed in it, and its
/// agents, each report read by the running sessions in `live`; with the
/// report that speaks for it, where one does. Each agent's newest report is
/// its word. In order: an agent asking, by its report or at a prompt, needs
/// you; else one whose report says it died is dead; else a busy lead keeps
/// the tree working, whatever another agent reported; else a done report,
/// or a lead idle after its turn, is done.
fn judge<'a>(
    reports: &[&'a Report],
    tree: &Tree,
    live: &[Tree],
) -> (Option<Said>, Option<&'a Report>) {
    let mut newest: Vec<(&Report, State)> = Vec::new();
    for report in reports {
        match newest.iter_mut().find(|(r, _)| r.job == report.job) {
            Some(kept) if kept.0.updated >= report.updated => {}
            Some(kept) => *kept = (report, crate::app::report_state(report, live)),
            None => newest.push((report, crate::app::report_state(report, live))),
        }
    }
    let newest_in = |state: State| {
        newest
            .iter()
            .filter(|(_, s)| *s == state)
            .max_by_key(|(r, _)| r.updated)
            .map(|(r, _)| *r)
    };
    if let Some(report) = newest_in(State::Blocked) {
        return (Some(Said::NeedsYou), Some(report));
    }
    let agents = || {
        tree.agents
            .iter()
            .filter(|a| !assistant::is_assistant(&a.name))
    };
    if agents().any(|a| a.status == Some(Status::Waiting)) {
        return (Some(Said::NeedsYou), None);
    }
    if let Some(report) = newest_in(State::Failed) {
        return (Some(Said::Died), Some(report));
    }
    // Claude Code marks a finished turn idle; one agent still busy leads.
    let lead = App::agents_of(tree).into_iter().next();
    if lead.is_some_and(|a| a.status == Some(Status::Busy)) {
        return (Some(Said::Other), None);
    }
    let latest = newest.iter().max_by_key(|(r, _)| r.updated);
    if let Some((report, State::Done)) = latest {
        return (Some(Said::Done), Some(*report));
    }
    if lead.is_some_and(|a| a.status == Some(Status::Idle) && a.active_at.is_some()) {
        return (Some(Said::Done), None);
    }
    (latest.map(|_| Said::Other), None)
}

/// An entry's line and time for what `judge` said of a tree, from the
/// report that speaks for it, else what its agents show.
fn judged_entry(
    said: Said,
    report: Option<&Report>,
    tree: &Tree,
    live: &[Tree],
) -> Option<(Kind, String, Option<SystemTime>)> {
    let agent_at = || {
        App::agents_of(tree)
            .first()
            .and_then(|a| a.active_at)
            .map(|at| UNIX_EPOCH + Duration::from_secs(at))
    };
    let text = |report: &Report| first_line(crate::app::Item::new(report, live).text());
    Some(match said {
        Said::NeedsYou => (
            Kind::NeedsYou,
            // The same words from any workspace, so what `x` put away stays away.
            report.map_or_else(|| AT_A_PROMPT.into(), text),
            report.map(|r| r.updated).or_else(agent_at),
        ),
        Said::Died => (Kind::Died, DIED.into(), report.map(|r| r.updated)),
        Said::Done => (
            Kind::Done,
            report.map_or_else(|| "finished its turn".into(), text),
            report.map(|r| r.updated).or_else(agent_at),
        ),
        Said::Other => return None,
    })
}

/// The PR stages worth telling you about: its checks failed, or it merged.
pub fn pr_kind(stage: Stage) -> Option<Kind> {
    match stage {
        Stage::CiFailed | Stage::Failed => Some(Kind::Failed),
        Stage::Merged => Some(Kind::Merged),
        _ => None,
    }
}

/// What an entry is about.
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

/// Another workspace's entry: where it lives, so opening it switches there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Away {
    pub name: String,
    pub checkout: PathBuf,
}

/// One thing that wants you, waiting on you or finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
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
                let mut trees = with_main(&w.checkout, crate::cache::read(&w.checkout)?);
                crate::cli::read_agents(&mut trees);
                crate::cli::read_ship(&w.checkout, &mut trees);
                Some((w.checkout, trees))
            })
            .collect()
    })
}

/// A workspace's trees as its cache lists them, with its main checkout at
/// `checkout` first, as the workspace on screen has it, since an agent can
/// work there too.
pub fn with_main(checkout: &Path, mut trees: Vec<Tree>) -> Vec<Tree> {
    trees.insert(0, crate::pool::main_tree(checkout));
    trees
}

/// The other workspaces' pools, from their caches.
pub fn other_pools(here: &Path) -> Vec<Pool> {
    crate::workspaces::list()
        .into_iter()
        .filter(|w| w.checkout != here)
        .filter_map(|w| {
            let trees = with_main(&w.checkout, crate::cache::read(&w.checkout)?);
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
    for tree in app.trees.iter().filter(|t| t.is_held()) {
        let (said, report) = judge(&placed(app, tree), tree, &app.trees);
        if let Some((kind, what, at)) =
            said.and_then(|said| judged_entry(said, report, tree, &app.trees))
        {
            entries.push(Entry {
                kind,
                away: None,
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, tree.sidecar.as_deref()),
                what,
                at,
            });
        }
        if let Some(ship) = &tree.ship
            && let Some(kind) = pr_kind(ship.stage)
        {
            entries.push(Entry {
                kind,
                away: None,
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, tree.sidecar.as_deref()),
                what: ship.label.clone(),
                at: None,
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

/// What `x` keeps of the entry `about` is, to put it away until it changes:
/// its kind, tree and the time it happened, which a new question, a new
/// finish or a rewritten report moves; a PR, with no time, by its line. The
/// same from any workspace. None once gone.
pub fn put_away_key(app: &App, about: &About) -> Option<String> {
    entries(app)
        .into_iter()
        .find(|e| e.about() == *about)
        .map(|e| key_of(&e))
}

fn key_of(entry: &Entry) -> String {
    let what = entry
        .at
        .map_or_else(|| entry.what.clone(), |at| secs(at).to_string());
    key(entry.kind, &entry.path, &what)
}

fn key(kind: Kind, path: &Path, what: &str) -> String {
    format!("{kind:?} {} {what}", path.display())
}

/// What an ask at a prompt says where no report speaks for it.
const AT_A_PROMPT: &str = "waiting at a prompt";

/// The keys put away of asks at a prompt with no time, whose tree, as last
/// read, no longer has an agent waiting: answered, so the next ask there is
/// a new one, though nothing else would tell the two apart.
pub fn answered_keys(app: &App) -> Vec<String> {
    let read = app.trees.iter().chain(app.away_live.values().flatten());
    read.filter(|t| !t.agents.iter().any(|a| a.status == Some(Status::Waiting)))
        .map(|t| key(Kind::NeedsYou, &t.path, AT_A_PROMPT))
        .filter(|k| app.put_away.contains(k))
        .collect()
}

/// The entries put away as they stand, which leave the notifications.
pub fn put_away(app: &App) -> std::collections::HashSet<About> {
    if app.put_away.is_empty() {
        return Default::default();
    }
    entries(app)
        .into_iter()
        .filter(|e| app.put_away.contains(&key_of(e)))
        .map(|e| e.about())
        .collect()
}

/// What waits on you, across every workspace read: agents asking, PRs
/// whose checks failed and agents that died, the oldest first. Each is as
/// old as it has waited: since treetop first saw it, or since it happened
/// where that was earlier. Each leaves once it stops being true, so there
/// is nothing to dismiss.
pub fn needs_action(app: &App) -> Vec<Entry> {
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

/// What is done: agents that finished, by their report or idle after a
/// turn, and PRs that merged. None of it waits on you.
pub fn done(app: &App) -> Vec<Entry> {
    entries(app)
        .into_iter()
        .filter(|e| matches!(e.kind, Kind::Done | Kind::Merged))
        .collect()
}

/// Where an entry's tree is, for a line naming several: its number, after
/// its workspace's name when that is another.
pub fn place_of(entry: &Entry) -> String {
    match &entry.away {
        Some(away) => format!("{}:{}", away.name, entry.tree),
        None => entry.tree.clone(),
    }
}

/// The other workspaces' entries, each tree judged as one here is: by the
/// reports placed in it and, from the last read of its pool, its agents
/// and its PR. Until a pool is read, its reports alone speak.
fn away_entries(app: &App) -> Vec<Entry> {
    let mut entries = Vec::new();
    for pool in &app.pools {
        let live = app.away_live.get(&pool.away.checkout);
        let live = live.map_or(&[][..], Vec::as_slice);
        for tree in pool.trees.iter().filter(|t| t.is_held()) {
            let entry = |kind: Kind, what: String, at: Option<SystemTime>| Entry {
                kind,
                away: Some(pool.away.clone()),
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, pool.sidecars.get(&tree.path)),
                what,
                at,
            };
            let read = live.iter().find(|t| t.path == tree.path);
            let reports: Vec<&Report> = app
                .reports
                .iter()
                .filter(|r| !assistant::is_assistant(&r.name))
                .filter(|r| {
                    place(r, std::slice::from_ref(pool)).is_some_and(|(_, t)| t.path == tree.path)
                })
                .collect();
            // Its agents as the read found them; unread, none are known.
            let unread = Tree {
                agents: Vec::new(),
                ..tree.clone()
            };
            let agents = read.unwrap_or(&unread);
            let (said, report) = judge(&reports, agents, live);
            if let Some((kind, what, at)) =
                said.and_then(|said| judged_entry(said, report, agents, live))
            {
                entries.push(entry(kind, what, at));
            }
            if let Some(ship) = read.and_then(|t| t.ship.as_deref())
                && let Some(kind) = pr_kind(ship.stage)
            {
                entries.push(entry(kind, ship.label.clone(), None));
            }
        }
    }
    entries
}

/// The other workspace's held tree `report`'s agent works in: the one its
/// directory is in, as agents are placed: one whose sidecar names a job only
/// for that job, else only if it wrote since that tree's lease began, since
/// a tree returned and handed out again is not still the old agent's. Its
/// main checkout, never leased, takes what no tree does.
pub fn place<'a>(report: &Report, pools: &'a [Pool]) -> Option<(&'a Pool, &'a Tree)> {
    let dir = report.dir.as_deref()?;
    let wrote = secs(report.updated);
    pools.iter().find_map(|pool| {
        let is_in = |t: &Tree| t.is_held() && dir.starts_with(&t.path);
        let tree = pool.trees.iter().find(|t| {
            if t.is_main || !is_in(t) {
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
        });
        let tree = match tree {
            Some(tree) => tree,
            None if pool.trees.iter().any(|t| !t.is_main && is_in(t)) => return None,
            None => pool.trees.iter().find(|t| t.is_main && is_in(t))?,
        };
        Some((pool, tree))
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::agents::{Agent, Source};
    use crate::app::Outcome;
    use crate::ship::Ship;
    use crate::sidecar::Sidecar;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
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
            answered_at: None,
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
        assert!(needs_action(app).is_empty(), "{:?}", needs_action(app));
    }

    fn order(app: &App) -> Vec<(Kind, String)> {
        needs_action(app)
            .into_iter()
            .map(|e| (e.kind, e.tree))
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) -> Outcome {
        app.handle_key(KeyEvent::from(code))
    }

    #[test]
    fn what_waits_is_oldest_first_and_what_is_done_never_waits() {
        let app = app();
        assert_eq!(
            order(&app),
            [
                (Kind::Died, "9".into()),
                (Kind::NeedsYou, "12".into()),
                (Kind::NeedsYou, "4".into()),
                (Kind::Failed, "7".into()),
            ],
            "tree 2's finished agent is not among them"
        );
        let away = &needs_action(&app)[1];
        assert_eq!(
            away.away.as_ref().map(|a| a.name.as_str()),
            Some("endodeals-web")
        );
        assert_eq!(away.title.as_deref(), Some("END-88 checkout-tax"));
    }

    /// Opens the notifications with `e` and the `at`th item with Enter.
    fn open_nth(app: &mut App, at: usize) -> Outcome {
        press(app, KeyCode::Char('e'));
        for _ in 0..at {
            press(app, KeyCode::Down);
        }
        press(app, KeyCode::Enter)
    }

    /// Opens each item that needs action in turn until one does what
    /// `wanted` looks for.
    fn open_until(app: &mut App, wanted: impl Fn(&App, &Outcome) -> bool) -> Outcome {
        for at in 0..needs_action(app).len() {
            let outcome = open_nth(app, at);
            if wanted(app, &outcome) {
                return outcome;
            }
        }
        panic!("never got there: {:?}", order(app));
    }

    #[test]
    fn enter_opens_each_that_needs_action_where_it_waits() {
        let mut app = app();
        // Tree 9's agent died 1,900s ago, the oldest.
        assert_eq!(open_nth(&mut app, 0), Outcome::Continue);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("9"));
        assert!(app.message.as_deref().unwrap().contains("R resumes"));
        assert!(
            matches!(open_nth(&mut app, 1), Outcome::SwitchTo { ref tree, .. } if tree == Path::new("/other/12/web")),
            "then the ask of 360s ago, in another workspace"
        );
        assert_eq!(
            open_nth(&mut app, 2),
            Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "j4".into()],
                agent: "inbox-rail".into(),
                tree: "4".into(),
            },
            "then the ask of 120s ago here"
        );
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("4"));
        assert_eq!(open_nth(&mut app, 3), Outcome::Continue);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("7"));
        assert!(app.message.as_deref().unwrap().contains("f sends"));
        assert_eq!(open_nth(&mut app, 0), Outcome::Continue);
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("9"),
            "opened again"
        );
    }

    #[test]
    fn another_workspaces_entry_switches_there_and_opens_its_agent() {
        let mut app = app();
        assert_eq!(
            open_until(&mut app, |_, o| matches!(o, Outcome::SwitchTo { .. })),
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
        assert!(!order(&app).iter().any(|(_, tree)| tree == "12"));
    }

    #[test]
    fn a_tree_whose_sidecar_names_another_job_is_not_that_reports() {
        let mut app = app();
        let named = |job: &str| Sidecar {
            job: Some(job.into()),
            ..Sidecar::leased_now()
        };
        app.pools[0].sidecars = [("/other/12/web".into(), named("someone-else"))].into();
        assert!(!order(&app).iter().any(|(_, tree)| tree == "12"));
        app.pools[0].sidecars = [("/other/12/web".into(), named("j12"))].into();
        assert!(order(&app).iter().any(|(_, tree)| tree == "12"));
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
        let away = needs_action(&app)
            .into_iter()
            .find(|e| e.tree == "12")
            .unwrap();
        assert_eq!(away.title.as_deref(), Some("END-88 checkout-tax"));
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
            answered_at: None,
            ..agent("claude", "", Status::Waiting)
        };
        let waiting = tree("/other/5/web", "5", "me/seo-meta", vec![pane]);
        app.pools[0]
            .trees
            .extend([failing.clone(), waiting.clone()]);
        app.away_live = [("/code/endodeals-web".into(), vec![failing, waiting])].into();
    }

    fn entry_of(app: &App, tree: &str) -> Entry {
        needs_action(app)
            .into_iter()
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
            open_until(&mut app, |_, o| {
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
            (Kind::NeedsYou, "waiting at a prompt")
        );
        // Tree 12's agent asks through its report; a pane waiting there too
        // is the same ask, not a second entry.
        let mut asking = app.pools[0].trees[0].clone();
        asking.agents = vec![agent("checkout-tax", "j12", Status::Waiting)];
        app.away_live
            .get_mut(&PathBuf::from("/code/endodeals-web"))
            .unwrap()
            .push(asking);
        let twelves = order(&app).iter().filter(|(_, t)| t == "12").count();
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
    fn enter_opens_the_item_under_the_cursor_and_an_opened_done_one_leaves() {
        let mut app = app();
        press(&mut app, KeyCode::Char('e'));
        assert!(app.is_notes_shown());
        // Down past the dead agent to the other workspace's ask.
        press(&mut app, KeyCode::Char('j'));
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::SwitchTo { ref tree, .. } if tree == Path::new("/other/12/web"))
        );
        assert!(!app.is_notes_shown(), "closed on the way");
        // The last is tree 2's finish: opened, it leaves Done.
        press(&mut app, KeyCode::Char('e'));
        for _ in 0..10 {
            press(&mut app, KeyCode::Down);
        }
        assert!(
            matches!(press(&mut app, KeyCode::Enter), Outcome::Chat { ref tree, .. } if tree == "2")
        );
        let (needs, done) = app.notes_list();
        assert_eq!((needs.len(), done.len()), (4, 0), "what needs action stays");
        // Esc closes.
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Esc);
        assert!(!app.is_notes_shown());
    }

    #[test]
    fn x_puts_one_away_until_it_changes_across_a_restart() {
        let dir = std::env::temp_dir().join(format!("treetop-put-away-{}", std::process::id()));
        let file = dir.join("put-away.json");
        let mut first = app();
        first.put_away_file = Some(file.clone());
        let one = &mut first;
        press(one, KeyCode::Char('e'));
        // Tree 9's dead agent, the first.
        press(one, KeyCode::Char('x'));
        let (needs, _) = one.notes_list();
        assert!(!needs.iter().any(|e| e.tree == "9"), "{needs:?}");
        assert_eq!(needs.len(), 3, "out of the count too");
        assert!(one.is_notes_shown(), "the rest still open");
        // Started again, it is still away.
        let mut again = app();
        again.put_away = crate::state::read(&file);
        assert!(!again.notes_list().0.iter().any(|e| e.tree == "9"));
        // Its agent resumed and died again: a new item, back in the list.
        let mut reports = again.reports.clone();
        let nine = reports.iter_mut().find(|r| r.job == "j9").unwrap();
        nine.updated = SystemTime::now();
        again.apply_reports(reports);
        assert!(again.notes_list().0.iter().any(|e| e.tree == "9"));
        // An ask put away comes back once the agent asks something new.
        press(&mut again, KeyCode::Char('e'));
        let four = again
            .notes_list()
            .0
            .iter()
            .position(|e| e.tree == "4")
            .unwrap();
        for _ in 0..four {
            press(&mut again, KeyCode::Char('j'));
        }
        press(&mut again, KeyCode::Char('x'));
        assert!(!again.notes_list().0.iter().any(|e| e.tree == "4"));
        let mut reports = again.reports.clone();
        reports[0] = report("j4", "/pool/4/app", State::Blocked, "And the colour?", 10);
        again.apply_reports(reports);
        assert!(again.notes_list().0.iter().any(|e| e.tree == "4"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_ask_put_away_from_another_workspace_stays_away_there() {
        // Tree 12's agent sits at a prompt in its tmux pane, no report, its
        // last reply known, or not, as for an agent read from a holder file.
        for replied in [Some(secs_ago(60)), None] {
            put_away_there(|| Agent {
                active_at: replied,
                answered_at: None,
                ..agent("checkout-tax", "j12", Status::Waiting)
            });
        }
    }

    /// Puts away tree 12's ask from another workspace, `asking` its agent,
    /// and checks it is away in tree 12's own workspace too.
    fn put_away_there(asking: impl Fn() -> Agent) {
        let mut here = app();
        here.reports.retain(|r| r.job != "j12");
        let mut there = here.pools[0].trees[0].clone();
        there.agents = vec![asking()];
        here.away_live = [("/code/endodeals-web".into(), vec![there])].into();
        press(&mut here, KeyCode::Char('e'));
        let at = here
            .notes_list()
            .0
            .iter()
            .position(|e| e.tree == "12")
            .unwrap();
        for _ in 0..at {
            press(&mut here, KeyCode::Down);
        }
        press(&mut here, KeyCode::Char('x'));
        assert!(!here.notes_list().0.iter().any(|e| e.tree == "12"));
        // Switched there, the same ask, now in its own workspace.
        let mut there = app();
        there.reports.retain(|r| r.job != "j12");
        there.pools.clear();
        there.trees.push(tree(
            "/other/12/web",
            "12",
            "me/END-88-checkout-tax",
            vec![asking()],
        ));
        assert!(there.notes_list().0.iter().any(|e| e.tree == "12"));
        there.put_away = here.put_away.clone();
        assert!(
            !there.notes_list().0.iter().any(|e| e.tree == "12"),
            "{:?}",
            there.put_away
        );
    }

    #[test]
    fn an_ask_with_no_time_put_away_comes_back_once_asked_again() {
        // Tree 2's agent, from a holder file, sits at a prompt; no report,
        // and no time for its last reply.
        let mut app = app();
        app.reports.retain(|r| r.job != "j2");
        let set = |app: &mut App, status: Status| {
            let two = app.trees.iter_mut().find(|t| t.name == "2").unwrap();
            two.agents[0].status = Some(status);
            app.watch_waiting();
        };
        let shown = |app: &App| app.notes_list().0.iter().any(|e| e.tree == "2");
        set(&mut app, Status::Waiting);
        app.notes = Some(0);
        let at = app
            .notes_list()
            .0
            .iter()
            .position(|e| e.tree == "2")
            .unwrap();
        app.notes = Some(at);
        press(&mut app, KeyCode::Char('x'));
        assert!(!shown(&app), "put away");
        set(&mut app, Status::Waiting);
        assert!(!shown(&app), "still the same ask");
        // Answered, it works; then it asks again.
        set(&mut app, Status::Busy);
        set(&mut app, Status::Waiting);
        assert!(shown(&app), "a new ask");
    }

    /// Seconds since the epoch, `ago` seconds back.
    fn secs_ago(ago: u64) -> u64 {
        secs(SystemTime::now()) - ago
    }

    /// Whether tree `name` is in Needs action, and whether in Done.
    fn listed(app: &App, name: &str) -> (bool, bool) {
        let is_in = |list: Vec<Entry>| list.iter().any(|e| e.tree == name);
        (is_in(needs_action(app)), is_in(done(app)))
    }

    #[test]
    fn a_blocked_report_holds_until_its_session_works_past_it() {
        // Tree 4's agent asked ten minutes ago.
        let mut app = app();
        let mut reports = app.reports.clone();
        reports[0] = report("j4", "/pool/4/app", State::Blocked, "Which DB?", 600);
        app.apply_reports(reports);
        let mut now = |status: Status, active_ago: u64| {
            let four = &mut app.trees[0].agents[0];
            four.status = Some(status);
            four.active_at = Some(secs_ago(active_ago));
            listed(&app, "4")
        };
        assert_eq!(
            now(Status::Idle, 590),
            (true, false),
            "its turn ended a moment after it asked: it still waits"
        );
        assert_eq!(
            now(Status::Idle, 60),
            (false, true),
            "answered, it worked well past the report and stopped: done"
        );
        assert_eq!(
            now(Status::Idle, 500),
            (false, true),
            "answered at once, a short turn and stopped: done"
        );
        assert_eq!(now(Status::Busy, 5), (false, false), "working on it");
        assert_eq!(
            now(Status::Waiting, 5),
            (true, false),
            "asking again, however late, it waits"
        );
        // Answered seconds after it asked, and done as quick: the answer,
        // not the time, tells it from the ask's own turn.
        let four = &mut app.trees[0].agents[0];
        four.status = Some(Status::Idle);
        four.active_at = Some(secs_ago(590));
        assert_eq!(listed(&app, "4"), (true, false), "not yet answered");
        app.trees[0].agents[0].answered_at = Some(secs_ago(595));
        assert_eq!(listed(&app, "4"), (false, true), "answered, and finished");
    }

    #[test]
    fn what_finished_says_what_it_said_not_that_it_waits_at_a_prompt() {
        // Tree 4 asked ten minutes ago, was answered, and its session has
        // since worked and gone idle.
        let mut app = app();
        let mut reports = app.reports.clone();
        reports[0] = report(
            "j4",
            "/pool/4/app",
            State::Blocked,
            "Where should the inbox sit?",
            600,
        );
        app.apply_reports(reports);
        let four = app.trees.iter_mut().find(|t| t.name == "4").unwrap();
        four.agents[0].active_at = Some(secs_ago(30));
        let four = done(&app).into_iter().find(|e| e.tree == "4").unwrap();
        assert_eq!(four.what, "Where should the inbox sit?");
    }

    #[test]
    fn another_workspaces_stale_ask_no_longer_needs_you() {
        let mut app = app();
        assert!(needs_action(&app).iter().any(|e| e.tree == "12"));
        // endodeals-web's read: tree 12's session has worked past its ask
        // and sits idle.
        let mut answered = agent("checkout-tax", "j12", Status::Idle);
        answered.active_at = Some(secs_ago(30));
        app.away_live = [(
            "/code/endodeals-web".into(),
            vec![tree(
                "/other/12/web",
                "12",
                "me/END-88-checkout-tax",
                vec![answered],
            )],
        )]
        .into();
        assert!(
            !needs_action(&app).iter().any(|e| e.tree == "12"),
            "{:?}",
            needs_action(&app)
        );
        assert!(
            done(&app)
                .iter()
                .any(|e| e.tree == "12" && e.away.is_some())
        );
    }

    #[test]
    fn a_tree_with_an_agent_still_busy_is_not_done() {
        let mut app = app();
        app.reports.retain(|r| r.job != "j2");
        let two = app.trees.iter_mut().find(|t| t.name == "2").unwrap();
        two.agents[0].active_at = Some(secs_ago(30));
        let mut busy = agent("helper", "jx", Status::Busy);
        busy.active_at = Some(secs_ago(5));
        two.agents.push(busy);
        assert_eq!(listed(&app, "2"), (false, false));
        // So with a report saying it is done.
        let mut reports = app.reports.clone();
        reports.push(report("j2", "/pool/2/app", State::Done, "Done", 60));
        app.apply_reports(reports);
        assert_eq!(listed(&app, "2"), (false, false));
    }

    #[test]
    fn an_agent_asking_after_its_report_said_done_needs_action_only() {
        let mut app = app();
        // Tree 2's report here says done, and its agent has since asked.
        let two = app.trees.iter_mut().find(|t| t.name == "2").unwrap();
        two.agents[0].status = Some(Status::Waiting);
        assert_eq!(listed(&app, "2"), (true, false));
        // The same in another workspace: tree 12's report says done, and
        // that workspace's read finds its agent asking.
        let mut reports = app.reports.clone();
        reports[3] = report("j12", "/other/12/web/src", State::Done, "Seeded.", 360);
        app.apply_reports(reports);
        let asking = agent("checkout-tax", "j12", Status::Waiting);
        app.away_live = [(
            "/code/endodeals-web".into(),
            vec![tree(
                "/other/12/web",
                "12",
                "me/END-88-checkout-tax",
                vec![asking],
            )],
        )]
        .into();
        assert_eq!(listed(&app, "12"), (true, false));
    }

    #[test]
    fn an_answered_ask_no_longer_opens_its_chat_by_itself() {
        let mut app = app();
        let mut reports = app.reports.clone();
        reports[0] = report(
            "j4",
            "/pool/4/app",
            State::Blocked,
            "Where should the inbox sit?",
            600,
        );
        app.apply_reports(reports);
        assert!(app.next_ask().is_some(), "an ask waiting opens its chat");
        let mut app = self::app();
        let mut reports = app.reports.clone();
        reports[0] = report(
            "j4",
            "/pool/4/app",
            State::Blocked,
            "Where should the inbox sit?",
            600,
        );
        app.apply_reports(reports);
        // Answered: its session has run a turn since and sits idle.
        let four = app.trees.iter_mut().find(|t| t.name == "4").unwrap();
        four.agents[0].active_at = Some(secs_ago(30));
        assert!(app.next_ask().is_none(), "{:?}", app.next_ask());
    }

    #[test]
    fn another_workspaces_tree_with_an_agent_still_busy_is_not_done() {
        let mut app = app();
        let mut reports = app.reports.clone();
        reports[3] = report("j12", "/other/12/web/src", State::Done, "Seeded.", 360);
        app.apply_reports(reports);
        let mut done = agent("checkout-tax", "j12", Status::Idle);
        done.active_at = Some(secs_ago(300));
        let mut busy = agent("helper", "jx", Status::Busy);
        busy.active_at = Some(secs_ago(5));
        app.away_live = [(
            "/code/endodeals-web".into(),
            vec![tree(
                "/other/12/web",
                "12",
                "me/END-88-checkout-tax",
                vec![done, busy],
            )],
        )]
        .into();
        assert_eq!(listed(&app, "12"), (false, false));
    }

    #[test]
    fn the_table_does_not_mark_a_tree_done_while_its_lead_is_busy() {
        let mut app = app();
        let two = app.trees.iter_mut().find(|t| t.name == "2").unwrap();
        let mut busy = agent("helper", "jx", Status::Busy);
        busy.active_at = Some(secs_ago(5));
        two.agents.push(busy);
        let row = crate::ui::tests::screen_lines(&app, 200, 40)
            .into_iter()
            .find(|l| l.contains("me/help-columns"))
            .unwrap();
        assert!(!row.contains("✓ helper"), "{row}");
        assert!(row.contains("helper"), "{row}");
    }

    #[test]
    fn another_workspace_follows_the_same_rules_as_this_one() {
        let in_away = |app: &App, tree: &str| {
            let is_in = |list: Vec<Entry>| list.iter().any(|e| e.tree == tree && e.away.is_some());
            (is_in(needs_action(app)), is_in(done(app)))
        };
        let read = |app: &mut App, agents: Vec<Agent>| {
            app.away_live = [(
                "/code/endodeals-web".into(),
                vec![tree(
                    "/other/12/web",
                    "12",
                    "me/END-88-checkout-tax",
                    agents,
                )],
            )]
            .into();
        };
        // A done report beside another agent asking: Needs action only.
        let mut app = app();
        let mut reports = app.reports.clone();
        reports[3] = report("j12", "/other/12/web/src", State::Done, "Seeded.", 360);
        app.apply_reports(reports);
        let mut done = agent("checkout-tax", "j12", Status::Idle);
        done.active_at = Some(secs_ago(300));
        read(&mut app, vec![done, agent("helper", "jx", Status::Waiting)]);
        assert_eq!(in_away(&app, "12"), (true, false));
        // An agent idle after its turn, with no report: Done.
        let mut app = self::app();
        app.reports.retain(|r| r.job != "j12");
        let mut finished = agent("checkout-tax", "j12", Status::Idle);
        finished.active_at = Some(secs_ago(60));
        read(&mut app, vec![finished]);
        assert_eq!(in_away(&app, "12"), (false, true));
    }

    #[test]
    fn an_ask_in_a_tree_holds_though_another_agent_there_reports_done_later() {
        // Tree 4: agent j4 asked, and a second agent there reported done
        // after it.
        let mut app = app();
        let mut reports = app.reports.clone();
        reports.push(report("j4b", "/pool/4/app/sub", State::Done, "Tidied.", 30));
        app.apply_reports(reports);
        let four = app.trees.iter_mut().find(|t| t.name == "4").unwrap();
        four.agents.push(agent("tidy", "j4b", Status::Idle));
        assert_eq!(listed(&app, "4"), (true, false));
        // The same in another workspace: tree 12's agent asked, a second
        // one there reported done.
        let mut reports = app.reports.clone();
        reports.push(report(
            "j12b",
            "/other/12/web/lib",
            State::Done,
            "Seeded.",
            30,
        ));
        app.apply_reports(reports);
        app.pools[0].sidecars.clear();
        let is_in = |list: Vec<Entry>| list.iter().filter(|e| e.tree == "12").count();
        assert_eq!((is_in(needs_action(&app)), is_in(done(&app))), (1, 0));
    }

    #[test]
    fn an_ask_beside_a_newer_done_report_shows_as_needing_you_everywhere() {
        let mut app = app();
        let mut reports = app.reports.clone();
        reports.push(report("j4b", "/pool/4/app/sub", State::Done, "Tidied.", 30));
        app.apply_reports(reports);
        let four = app.trees.iter_mut().find(|t| t.name == "4").unwrap();
        four.agents.push(agent("tidy", "j4b", Status::Idle));
        let four = app.trees.iter().find(|t| t.name == "4").unwrap();
        assert!(
            app.needs_you(four),
            "the header count and the phone count it"
        );
        let row = crate::ui::tests::screen_lines(&app, 200, 40)
            .into_iter()
            .find(|l| l.contains("me/anb-41-inbox-rail"))
            .unwrap();
        assert!(row.contains("● inbox-rail"), "{row}");
    }

    #[test]
    fn another_workspaces_tree_with_a_dead_agent_and_one_asking_has_one_entry() {
        let mut app = app();
        let mut reports = app.reports.clone();
        reports[3] = report("j12", "/other/12/web/src", State::Failed, "", 360);
        app.apply_reports(reports);
        app.away_live = [(
            "/code/endodeals-web".into(),
            vec![tree(
                "/other/12/web",
                "12",
                "me/END-88-checkout-tax",
                vec![agent("helper", "jx", Status::Waiting)],
            )],
        )]
        .into();
        let twelve: Vec<Kind> = needs_action(&app)
            .into_iter()
            .filter(|e| e.tree == "12")
            .map(|e| e.kind)
            .collect();
        assert_eq!(twelve, [Kind::NeedsYou]);
    }

    #[test]
    fn the_table_marks_a_tree_as_its_lists_have_it() {
        let row = |status: Status, tree: &str, branch: &str| {
            let mut app = app();
            let t = app.trees.iter_mut().find(|t| t.name == tree).unwrap();
            t.agents.push(agent("helper", "jx", status));
            crate::ui::tests::screen_lines(&app, 200, 40)
                .into_iter()
                .find(|l| l.contains(branch))
                .unwrap()
        };
        let asking = row(Status::Waiting, "2", "me/help-columns");
        assert!(asking.contains("● helper"), "{asking}");
        let over_death = row(Status::Waiting, "9", "me/port-menu");
        assert!(over_death.contains("● helper"), "{over_death}");
    }

    #[test]
    fn the_main_checkouts_heading_marks_what_its_cold_agent_left() {
        let heading = |state: State| {
            let mut app = app();
            let mut main = tree(
                "/repo/app",
                "main",
                "trunk",
                vec![agent("on-main", "jm", Status::Cold)],
            );
            main.is_main = true;
            main.status = "checkout".into();
            app.trees.insert(0, main);
            let mut reports = app.reports.clone();
            reports.push(report("jm", "/repo/app", state, "Which?", 60));
            app.apply_reports(reports);
            crate::ui::tests::screen_lines(&app, 200, 40)
                .into_iter()
                .find(|l| l.contains("main checkout"))
                .unwrap()
        };
        let died = heading(State::Failed);
        assert!(died.contains("✗ on-main"), "{died}");
        let asking = heading(State::Blocked);
        assert!(asking.contains("● on-main"), "{asking}");
    }

    #[test]
    fn a_stale_ask_in_another_workspace_is_not_counted_as_needing_you() {
        // Tree 12's agent asked six minutes ago and has run a turn since.
        let mut app = app();
        let mut there = app.pools[0].trees[0].clone();
        there.agents = vec![Agent {
            active_at: Some(secs_ago(30)),
            answered_at: None,
            ..agent("checkout-tax", "j12", Status::Idle)
        }];
        app.away_live = [("/code/endodeals-web".into(), vec![there])].into();
        assert_eq!(listed(&app, "12"), (false, true));
        assert_eq!(app.elsewhere(), 0, "the header counts what the list has");
    }

    #[test]
    fn the_main_checkouts_heading_shows_no_cold_agent_with_nothing_to_say() {
        let mut app = app();
        let mut main = tree(
            "/repo/app",
            "main",
            "trunk",
            vec![agent("on-main", "jm", Status::Cold)],
        );
        main.is_main = true;
        main.status = "checkout".into();
        app.trees.insert(0, main);
        let mut reports = app.reports.clone();
        reports.push(report("jm", "/repo/app", State::Working, "Building", 60));
        app.apply_reports(reports);
        let heading = crate::ui::tests::screen_lines(&app, 200, 40)
            .into_iter()
            .find(|l| l.contains("main checkout"))
            .unwrap();
        assert!(!heading.contains("on-main"), "{heading}");
    }

    /// Tree 2 here and tree 12 in endodeals-web, each with `agents` and
    /// the reports `reports` gives for a tree's directory, judged: (needs
    /// action, done) for each, which the same rules make the same.
    fn here_and_away(
        agents: impl Fn(&str) -> Vec<Agent>,
        reports: impl Fn(&str) -> Vec<Report>,
    ) -> ((bool, bool), (bool, bool)) {
        let mut app = app();
        app.reports
            .retain(|r| !["j2", "j12"].contains(&r.job.as_str()));
        let mut all = app.reports.clone();
        all.extend(reports("/pool/2/app"));
        all.extend(reports("/other/12/web/src").into_iter().map(|r| Report {
            job: format!("{}-away", r.job),
            ..r
        }));
        app.apply_reports(all);
        let two = app.trees.iter_mut().find(|t| t.name == "2").unwrap();
        two.agents = agents("");
        app.away_live = [(
            "/code/endodeals-web".into(),
            vec![tree(
                "/other/12/web",
                "12",
                "me/END-88-checkout-tax",
                agents("-away"),
            )],
        )]
        .into();
        let away = |list: Vec<Entry>| list.iter().any(|e| e.tree == "12" && e.away.is_some());
        (
            listed(&app, "2"),
            (away(needs_action(&app)), away(done(&app))),
        )
    }

    #[test]
    fn a_tree_comes_to_the_same_here_and_in_another_workspace() {
        let with = |name: &str, job: &str, status: Status, ago: Option<u64>| {
            let mut a = agent(name, job, status);
            a.active_at = ago.map(secs_ago);
            a
        };
        // A died report beside a busy agent's newer working one: dead.
        let (here, away) = here_and_away(
            |s| {
                vec![
                    with("a", &format!("ja{s}"), Status::Cold, None),
                    with("b", &format!("jb{s}"), Status::Busy, Some(5)),
                ]
            },
            |dir| {
                vec![
                    report("ja", dir, State::Failed, "", 600),
                    report("jb", dir, State::Working, "Building", 30),
                ]
            },
        );
        assert_eq!((here, away), ((true, false), (true, false)), "died");
        // A stopped report, its session idle after a turn: done.
        let (here, away) = here_and_away(
            |s| vec![with("a", &format!("ja{s}"), Status::Idle, Some(20))],
            |dir| vec![report("ja", dir, State::Stopped, "", 30)],
        );
        assert_eq!((here, away), ((false, true), (false, true)), "stopped");
        // A newer stopped report over an older done one, idle: done.
        let (here, away) = here_and_away(
            |s| {
                vec![
                    with("a", &format!("ja{s}"), Status::Idle, Some(20)),
                    with("b", &format!("jb{s}"), Status::Idle, Some(20)),
                ]
            },
            |dir| {
                vec![
                    report("ja", dir, State::Done, "Done", 300),
                    report("jb", dir, State::Stopped, "", 30),
                ]
            },
        );
        assert_eq!(
            (here, away),
            ((false, true), (false, true)),
            "done then stopped"
        );
    }

    #[test]
    fn a_session_idle_after_a_turn_is_done_not_needing_you() {
        // Tree 2's agent sits idle with no report.
        let mut app = app();
        app.reports.retain(|r| r.job != "j2");
        assert_eq!(listed(&app, "2"), (false, false), "no turn yet");
        app.trees[1].agents[0].active_at = Some(secs_ago(30));
        assert_eq!(listed(&app, "2"), (false, true), "idle after a turn");
        // A background job left on working whose session went idle is done.
        let mut reports = app.reports.clone();
        reports.push(report(
            "j2",
            "/pool/2/app",
            State::Working,
            "Running the tests",
            300,
        ));
        app.apply_reports(reports);
        assert_eq!(listed(&app, "2"), (false, true), "its turn over");
        app.trees[1].agents[0].status = Some(Status::Busy);
        assert_eq!(listed(&app, "2"), (false, false), "still working");
    }

    #[test]
    fn an_agent_asking_in_the_main_checkout_needs_action() {
        // An agent started on the main heading, as `n` there starts one.
        let mut app = app();
        let mut main = tree(
            "/repo/app",
            "main",
            "main",
            vec![agent("on-main", "jm", Status::Waiting)],
        );
        main.is_main = true;
        main.status = "checkout".into();
        app.trees.push(main);
        assert_eq!(listed(&app, "main"), (true, false), "at a prompt");
        let main = app.trees.last().unwrap().clone();
        assert!(app.needs_you(&main), "the header counts what the list has");
        app.trees.last_mut().unwrap().agents[0].status = Some(Status::Idle);
        app.trees.last_mut().unwrap().agents[0].active_at = Some(secs_ago(30));
        assert_eq!(listed(&app, "main"), (false, true), "idle after its turn");
    }

    #[test]
    fn opening_what_waits_in_the_main_checkout_lands_on_it() {
        // Its agent died, which opens no chat: the cursor still goes there.
        let mut app = app();
        let mut main = tree(
            "/repo/app",
            "main",
            "main",
            vec![agent("on-main", "jm", Status::Cold)],
        );
        main.is_main = true;
        main.status = "checkout".into();
        app.trees.insert(0, main);
        let mut reports = app.reports.clone();
        reports.push(report("jm", "/repo/app", State::Failed, "", 60));
        app.apply_reports(reports);
        nothing_else_waits(&mut app, "main");
        app.select_tree("2");
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.window_tree().map(|t| t.name.as_str()), Some("main"));
    }

    #[test]
    fn an_agent_asking_in_another_workspaces_main_checkout_needs_action() {
        let mut app = app();
        let pool = &mut app.pools[0];
        pool.trees = with_main(&pool.away.checkout, std::mem::take(&mut pool.trees));
        let mut reports = app.reports.clone();
        reports.push(report(
            "jw",
            "/code/endodeals-web/src",
            State::Blocked,
            "Which rates?",
            60,
        ));
        app.apply_reports(reports);
        assert!(
            needs_action(&app)
                .iter()
                .any(|e| e.tree == "main" && e.away.is_some() && e.what == "Which rates?"),
            "{:?}",
            needs_action(&app)
        );
        // A report from one of its trees is still that tree's.
        assert_eq!(listed(&app, "12"), (true, false));
    }

    /// Clears all that waits in `app` but what is in tree `name`.
    fn nothing_else_waits(app: &mut App, name: &str) {
        let path = app
            .trees
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .path
            .clone();
        app.reports
            .retain(|r| r.dir.as_deref().is_some_and(|d| d.starts_with(&path)));
        app.trees.retain(|t| t.name != "7");
        app.pools.clear();
    }

    #[test]
    fn a_merged_pr_is_done_and_a_failed_one_needs_action() {
        let mut app = app();
        assert_eq!(listed(&app, "7"), (true, false), "its checks failed");
        app.trees[2].ship.as_mut().unwrap().stage = Stage::Merged;
        assert_eq!(listed(&app, "7"), (false, true), "merged");
    }

    #[test]
    fn e_from_a_chat_in_normal_opens_the_notifications_over_the_table() {
        let mut app = app();
        app.select_tree("2");
        app.tab = crate::app::WindowTab::Chat;
        app.is_window_focused = true;
        assert_eq!(press(&mut app, KeyCode::Char('e')), Outcome::Continue);
        assert!(!app.is_window_focused, "the window gives the keys up");
        assert!(app.is_notes_shown());
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert_eq!(
            app.current().map(|t| t.name.as_str()),
            Some("9"),
            "the oldest, tree 9's dead agent"
        );
    }

    #[test]
    fn enter_with_nothing_listed_opens_nothing() {
        let mut app = app();
        nothing_waits(&mut app);
        press(&mut app, KeyCode::Char('e'));
        assert!(app.is_notes_shown(), "open, saying there is nothing");
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert!(app.is_notes_shown());
    }

    #[test]
    fn the_count_takes_the_mouse_only_while_it_shows() {
        let mut app = app();
        assert!(app.wants_mouse(), "for a click on the count");
        nothing_waits(&mut app);
        assert!(!app.wants_mouse(), "text selects in the terminal as ever");
    }
}
