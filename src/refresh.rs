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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::agents::{self, Agent};
use crate::cache;
use crate::live;
use crate::plugin::Code;
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
    /// Where each held tree's PR stands, by path, or why no ship plugin
    /// could say.
    Ship(Result<Answer, String>),
}

pub struct Refresher {
    pub updates: Receiver<Update>,
    kick: Sender<()>,
    ship_kick: Sender<()>,
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
            let (kick, ship_kick, paused) = (kick.clone(), ship_kick.clone(), Arc::clone(&paused));
            move || {
                let kicks = Kicks {
                    listing: &kick,
                    ship: &ship_kick,
                };
                fast(&checkout, &updates_tx, &kicks, &paused, &trees);
            }
        });
        Refresher {
            updates,
            kick,
            ship_kick,
            paused,
        }
    }

    /// Lists the pool now rather than at the next trigger.
    pub fn refresh(&self) {
        let _ = self.kick.send(());
    }

    /// Asks the ship plugins now rather than at the next minute.
    pub fn refresh_ship(&self) {
        let _ = self.ship_kick.send(());
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
            let listing = pool::status(checkout).map(|(mut listing, raw)| {
                cache::write(checkout, &raw);
                listing.insert(0, pool::main_tree(checkout));
                listing.clone_into(&mut trees.lock().unwrap_or_else(PoisonError::into_inner));
                listing
            });
            if updates.send(Update::Listing(listing)).is_err() {
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
/// the ship clock, when the shared refs do.
struct Kicks<'a> {
    listing: &'a Sender<()>,
    ship: &'a Sender<()>,
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
                    if watcher.is_refs_changed() {
                        let _ = kicks.ship.send(());
                    }
                    work.extend(live::work(watcher.take(&known, &work)));
                    work.retain(|path, _| known.iter().any(|t| t.is_held() && t.path == *path));
                    sent = updates
                        .send(Update::Work(work.clone()))
                        .and_then(|()| updates.send(Update::Sidecars(sidecars)))
                        .and_then(|()| updates.send(Update::Agents(agents)))
                        .and_then(|()| updates.send(Update::Reports(reports.read())));
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
