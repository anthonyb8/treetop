//! Two clocks, which is how htop stays fast. Processes and git state change by
//! the second and cost milliseconds to read, so a fast clock reads processes
//! and their ports every FAST, and git state and agents every GIT_EVERY ticks:
//! git runs as child processes and is most of treetop's idle CPU, and agents
//! move between trees far less often than processes start, so both get half
//! the rate. Even then git only runs for the trees `watch` says changed. Pool state
//! changes only when a tree is leased, returned or destroyed, and `treehouse
//! status` costs seconds of CPU, so the slow clock lists it only when `git
//! worktree list` changes, when asked, and every SLOW as a backstop.
//!
//! A third clock asks the repository's ship plugins where each held tree's
//! PR stands, every SHIP, at once when the shared refs change (a push lands)
//! or when asked, but never twice within SHIP_GAP, and every SHIP_BACKOFF
//! while they cannot be reached, so a forge's rate limit is never at risk.
//!
//! A fourth asks the agent plugins how much of the plan is used, every
//! USAGE, on a thread of its own, since the answer can come over the
//! network and must never hold up the processes.
//!
//! A fifth keeps the main checkout on its upstream: it fetches every BASE
//! and fast-forwards where that is safe, then again whenever the shared refs
//! change, as an agent's fetch or push moves them.
//!
//! A sixth reads which ports are shared with the phone or the internet,
//! every SHARES and whenever `p` or `P` has just changed one, so the PORTS
//! column marks them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::agents::{self, Agent, Status};
use crate::base::{self, Standing};
use crate::cache;
use crate::config;
use crate::live;
use crate::phone::{self, Shared};
use crate::plugin::{Code, Usage};
use crate::pool::{self, Process, Tree, Work};
use crate::reports::{self, Report};
use crate::ship::{self, Answer};
use crate::sidecar::{self, Sidecar};
use crate::watch::Watcher;

const FAST: Duration = Duration::from_secs(2);
const SLOW: Duration = Duration::from_secs(120);
const GIT_EVERY: u32 = 2;
const SHIP: Duration = Duration::from_secs(60);
const SHIP_GAP: Duration = Duration::from_secs(15);
const SHIP_BACKOFF: Duration = Duration::from_secs(300);
const USAGE: Duration = Duration::from_secs(60);
/// How often keel's runs are read: each is a few small files.
const RUNS: Duration = Duration::from_secs(5);
const BASE: Duration = Duration::from_secs(300);
const SHARES: Duration = Duration::from_secs(10);
const RELEASE: Duration = Duration::from_secs(60 * 60);
/// How long a repository's `on_catch_up` may take: an install of its
/// packages can run for minutes.
const CATCH_UP_HOOK: Duration = Duration::from_secs(600);

pub enum Update {
    ListingStarted,
    Listing(Result<Vec<Tree>>),
    Processes(HashMap<PathBuf, Vec<Process>>),
    Work(HashMap<PathBuf, Option<Work>>),
    Agents(HashMap<PathBuf, Vec<Agent>>),
    /// treetop's record of each tree it leased, read with the agents.
    Sidecars(HashMap<PathBuf, Sidecar>),
    /// Every background agent's latest report, read with the agents.
    Reports(Vec<Report>),
    /// How much of the plan the agents run on is used; None when no agent
    /// plugin can say.
    Usage(Option<Usage>),
    /// Where each held tree's PR stands, by path, or why no ship plugin
    /// could say.
    Ship(Result<Answer, String>),
    /// Where the main checkout stands against its upstream after a
    /// catch-up.
    Base(Standing),
    /// Why the repository's `on_catch_up` failed after a fast-forward.
    CatchUpHookFailed(String),
    /// The latest treetop release's tag.
    Release(String),
    /// Every port this machine shares, and whether each is public.
    Shares(Vec<Shared>),
    /// keel's runs, by tree; None when keel is not installed.
    Runs(Option<HashMap<PathBuf, crate::keel::Run>>),
}

pub struct Refresher {
    pub updates: Receiver<Update>,
    kick: Sender<()>,
    ship_kick: Sender<()>,
    shares_kick: Sender<()>,
    paused: Arc<AtomicBool>,
}

impl Refresher {
    /// Starts both clocks for the pool of `checkout`. `known` is the listing
    /// already on screen, from the cache, so the fast clock has trees to probe
    /// before the first slow listing lands.
    pub fn spawn(checkout: PathBuf, known: Vec<Tree>) -> Self {
        let (updates_tx, updates) = mpsc::channel();
        let (kick, kick_rx) = mpsc::channel();
        let (ship_kick, ship_kick_rx) = mpsc::channel();
        let (base_kick, base_kick_rx) = mpsc::channel();
        let (shares_kick, shares_kick_rx) = mpsc::channel();
        let main_busy = Arc::new(AtomicBool::new(false));
        let paused = Arc::new(AtomicBool::new(false));
        let trees = Arc::new(Mutex::new(known));

        thread::spawn({
            let (updates_tx, paused, trees, checkout) = (
                updates_tx.clone(),
                Arc::clone(&paused),
                Arc::clone(&trees),
                checkout.clone(),
            );
            move || slow(&checkout, &updates_tx, &kick_rx, &paused, &trees)
        });
        thread::spawn({
            let (updates_tx, paused, trees, checkout) = (
                updates_tx.clone(),
                Arc::clone(&paused),
                Arc::clone(&trees),
                checkout.clone(),
            );
            move || ship_clock(&checkout, &updates_tx, &ship_kick_rx, &paused, &trees)
        });
        thread::spawn({
            let (updates_tx, paused) = (updates_tx.clone(), Arc::clone(&paused));
            move || usage_clock(&updates_tx, &paused)
        });
        thread::spawn({
            let (updates_tx, paused) = (updates_tx.clone(), Arc::clone(&paused));
            move || release_clock(&updates_tx, &paused)
        });
        thread::spawn({
            let (updates_tx, paused) = (updates_tx.clone(), Arc::clone(&paused));
            move || shares_clock(&updates_tx, &shares_kick_rx, &paused)
        });
        thread::spawn({
            let (updates_tx, paused) = (updates_tx.clone(), Arc::clone(&paused));
            move || runs_clock(&updates_tx, &paused)
        });
        thread::spawn({
            let (updates_tx, paused, main_busy, checkout) = (
                updates_tx.clone(),
                Arc::clone(&paused),
                Arc::clone(&main_busy),
                checkout.clone(),
            );
            move || base_clock(&checkout, &updates_tx, &base_kick_rx, &paused, &main_busy)
        });
        thread::spawn({
            let (kick, ship_kick, paused) = (kick.clone(), ship_kick.clone(), Arc::clone(&paused));
            move || {
                let kicks = Kicks {
                    listing: &kick,
                    ship: &ship_kick,
                    base: &base_kick,
                    main_busy: &main_busy,
                };
                fast(&checkout, &updates_tx, &kicks, &paused, &trees);
            }
        });
        Refresher {
            updates,
            kick,
            ship_kick,
            shares_kick,
            paused,
        }
    }

    /// Lists the pool now rather than at the next trigger.
    pub fn refresh(&self) {
        let _ = self.kick.send(());
    }

    /// A handle that lists the pool now, for a thread that does not hold
    /// the refresher, as `refresh` does.
    pub fn kicker(&self) -> Sender<()> {
        self.kick.clone()
    }

    /// Asks the ship plugins now rather than at the next minute.
    pub fn refresh_ship(&self) {
        let _ = self.ship_kick.send(());
    }

    /// Reads the shared ports now rather than at the next tick, for right
    /// after `p` or `P` has changed one.
    pub fn refresh_shares(&self) {
        let _ = self.shares_kick.send(());
    }

    /// Stops the clocks, for while a shell has the terminal and treetop is
    /// not on screen.
    pub fn set_paused(&self, is_paused: bool) {
        self.paused.store(is_paused, Ordering::Relaxed);
    }
}

fn slow(
    checkout: &std::path::Path,
    updates: &Sender<Update>,
    kicks: &Receiver<()>,
    paused: &AtomicBool,
    trees: &Mutex<Vec<Tree>>,
) {
    loop {
        if !paused.load(Ordering::Relaxed) {
            if updates.send(Update::ListingStarted).is_err() {
                return;
            }
            let is_first = trees
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty();
            let mut listing = pool::status(checkout).map(|(mut listing, raw)| {
                cache::write(checkout, &raw);
                listing.insert(0, pool::main_tree(checkout));
                listing.clone_into(&mut trees.lock().unwrap_or_else(PoisonError::into_inner));
                listing
            });
            // With no cache on screen, the first listing is what the screen
            // first draws, so it comes with what places each tree: its
            // sidecar names its group, and its agents and their reports its
            // place in it. Left to the git clock, they would land after the
            // trees are drawn, and the trees would move. The reports go
            // first, being kept apart from the trees, and the agents again
            // after, so the screen counts them read.
            let mut agents = None;
            if is_first && let Ok(listing) = &mut listing {
                crate::cli::read_agents(listing);
                agents = Some(
                    listing
                        .iter()
                        .map(|t| (t.path.clone(), t.agents.clone()))
                        .collect(),
                );
                if updates
                    .send(Update::Reports(reports::Reader::default().read()))
                    .is_err()
                {
                    return;
                }
            }
            if updates.send(Update::Listing(listing)).is_err() {
                return;
            }
            if let Some(agents) = agents
                && updates.send(Update::Agents(agents)).is_err()
            {
                return;
            }
        }
        if let Err(RecvTimeoutError::Disconnected) = kicks.recv_timeout(SLOW) {
            return;
        }
        // Kicks that queued up while listing are answered by the next one.
        while kicks.try_recv().is_ok() {}
    }
}

/// What the fast clock kicks: the listing, when the worktrees change, and
/// the ship and base clocks, when the shared refs do. It also tells the base
/// clock whether an agent is busy in the main checkout.
struct Kicks<'a> {
    listing: &'a Sender<()>,
    ship: &'a Sender<()>,
    base: &'a Sender<()>,
    main_busy: &'a AtomicBool,
}

fn fast(
    checkout: &std::path::Path,
    updates: &Sender<Update>,
    kicks: &Kicks,
    paused: &AtomicBool,
    trees: &Mutex<Vec<Tree>>,
) {
    let mut seen = None;
    let mut tick: u32 = 0;
    let mut watcher = Watcher::new();
    let mut reports = reports::Reader::default();
    // The latest git counts of every held tree, so a tree that is not read
    // again keeps its counts and an unpushed-only read keeps its changed count.
    let mut work: HashMap<PathBuf, Option<Work>> = HashMap::new();
    loop {
        if !paused.load(Ordering::Relaxed) {
            let known = trees.lock().unwrap_or_else(PoisonError::into_inner).clone();
            // Drained every tick so a burst of writes cannot overflow the queue.
            watcher.drain();
            if !known.is_empty() {
                let processes = live::processes(&known);
                let mut sent = Ok(());
                if tick.is_multiple_of(GIT_EVERY) {
                    let sidecars = sidecar::read_all(&known);
                    let agents = agents::find(&known, &processes, &sidecars);
                    let is_main_busy = agents.get(checkout).is_some_and(|agents| {
                        agents.iter().any(|a| a.status == Some(Status::Busy))
                    });
                    kicks.main_busy.store(is_main_busy, Ordering::Relaxed);
                    if watcher.is_refs_changed() {
                        let _ = kicks.ship.send(());
                        let _ = kicks.base.send(());
                    }
                    // Sent before git runs, which takes seconds across a
                    // pool on its first read: they decide each tree's group
                    // and place in it, which the list must not wait on.
                    sent = updates
                        .send(Update::Sidecars(sidecars))
                        .and_then(|()| updates.send(Update::Agents(agents)))
                        .and_then(|()| updates.send(Update::Reports(reports.read())));
                    work.extend(live::work(watcher.take(&known, &work)));
                    work.retain(|path, _| known.iter().any(|t| t.is_held() && t.path == *path));
                    sent = sent.and_then(|()| updates.send(Update::Work(work.clone())));
                }
                sent = sent.and_then(|()| updates.send(Update::Processes(processes)));
                if sent.is_err() {
                    return;
                }
                tick = tick.wrapping_add(1);
            }
            let worktrees = live::worktrees(checkout);
            if seen.is_some() && worktrees != seen && kicks.listing.send(()).is_err() {
                return;
            }
            seen = worktrees;
        }
        thread::sleep(FAST);
    }
}

/// Keeps the main checkout on its upstream until treetop moves on: a fetch
/// and a catch-up every BASE, and a catch-up alone when kicked, since the
/// refs moved and fetching again would only repeat what moved them.
fn base_clock(
    checkout: &std::path::Path,
    updates: &Sender<Update>,
    kicks: &Receiver<()>,
    paused: &AtomicBool,
    main_busy: &AtomicBool,
) {
    let mut is_fetch_due = true;
    loop {
        if !paused.load(Ordering::Relaxed) {
            if std::mem::take(&mut is_fetch_due) {
                base::fetch(checkout);
            }
            let standing = base::catch_up(checkout, main_busy.load(Ordering::Relaxed));
            if updates.send(Update::Base(standing)).is_err() {
                return;
            }
            if let Standing::Forwarded(_) = standing
                && let Some(why) = run_catch_up_hook(checkout)
                && updates.send(Update::CatchUpHookFailed(why)).is_err()
            {
                return;
            }
        }
        match kicks.recv_timeout(BASE) {
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => is_fetch_due = true,
            // Kicks that queued up while catching up are answered by this one.
            Ok(()) => while kicks.try_recv().is_ok() {},
        }
    }
}

/// Runs the repository's `on_catch_up` in its main checkout. Says why when
/// it failed, from the last line of its log.
fn run_catch_up_hook(checkout: &std::path::Path) -> Option<String> {
    let command = crate::config::load()
        .ok()?
        .repo(checkout)
        .map(|repo| repo.on_catch_up.clone())
        .filter(|command| !command.is_empty())?;
    let mut log = String::new();
    let is_ok =
        crate::jobs::run_hook_for("on_catch_up", &command, checkout, CATCH_UP_HOOK, &mut log);
    (!is_ok).then(|| {
        log.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("on_catch_up hook failed")
            .to_string()
    })
}

/// Asks which treetop release is the latest, every RELEASE, until treetop
/// moves on. One it cannot reach is asked again at the next.
fn release_clock(updates: &Sender<Update>, paused: &AtomicBool) {
    loop {
        if !paused.load(Ordering::Relaxed)
            && let Ok(tag) = crate::update::latest_release()
            && updates.send(Update::Release(tag)).is_err()
        {
            return;
        }
        thread::sleep(RELEASE);
    }
}

/// Reads which ports are shared, every SHARES while treetop is not paused
/// and whenever `p` or `P` has just changed one, until treetop moves on. A
/// machine with no shares command installed says nothing rather than an
/// error a tick: sharing itself reports its own failures.
fn shares_clock(updates: &Sender<Update>, kicks: &Receiver<()>, paused: &AtomicBool) {
    loop {
        if !paused.load(Ordering::Relaxed) {
            let listing = config::load()
                .map(|config| config.shares)
                .unwrap_or_default();
            if let Ok(shares) = phone::shares(&listing)
                && updates.send(Update::Shares(shares)).is_err()
            {
                return;
            }
        }
        if let Err(RecvTimeoutError::Disconnected) = kicks.recv_timeout(SHARES) {
            return;
        }
    }
}

/// Asks how much of the plan is used, every USAGE while treetop is not
/// paused, until treetop moves on.
/// Reads keel's runs until treetop moves on, or stops at once where keel is
/// not installed.
fn runs_clock(updates: &Sender<Update>, paused: &AtomicBool) {
    loop {
        if !paused.load(Ordering::Relaxed) {
            let runs = crate::keel::status();
            let is_absent = runs.is_none();
            if updates.send(Update::Runs(runs)).is_err() || is_absent {
                return;
            }
        }
        thread::sleep(RUNS);
    }
}

fn usage_clock(updates: &Sender<Update>, paused: &AtomicBool) {
    loop {
        if !paused.load(Ordering::Relaxed) && updates.send(Update::Usage(agents::usage())).is_err()
        {
            return;
        }
        thread::sleep(USAGE);
    }
}

/// Asks the ship plugins of the pool's repository where each held tree's PR
/// stands, until treetop moves on. A repository with none, `ship = []`, has
/// nothing asked.
fn ship_clock(
    checkout: &std::path::Path,
    updates: &Sender<Update>,
    kicks: &Receiver<()>,
    paused: &AtomicBool,
    trees: &Mutex<Vec<Tree>>,
) {
    let (plugins, missing) = ship::plugins(checkout);
    if plugins.is_empty() {
        if !missing.is_empty() {
            let why = format!("no ship plugin {}", missing.join(", "));
            let _ = updates.send(Update::Ship(Err(why)));
        }
        return;
    }
    loop {
        let mut wait = SHIP;
        if !paused.load(Ordering::Relaxed) {
            let known = trees.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let asked = ship::asked(&known);
            let answer = ship::status(&plugins, checkout, &asked);
            if answer
                .as_ref()
                .is_err_and(|err| matches!(err.code, Code::Unavailable | Code::Auth))
            {
                wait = SHIP_BACKOFF;
            }
            if updates
                .send(Update::Ship(answer.map_err(|err| err.to_string())))
                .is_err()
            {
                return;
            }
        }
        thread::sleep(SHIP_GAP);
        match kicks.recv_timeout(wait.saturating_sub(SHIP_GAP)) {
            Err(RecvTimeoutError::Disconnected) => return,
            // Kicks that queued up while asking are answered by the next ask.
            _ => while kicks.try_recv().is_ok() {},
        }
    }
}
