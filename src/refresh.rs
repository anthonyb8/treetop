//! Two clocks, which is how htop stays fast. Processes and git state change by
//! the second and cost milliseconds to read, so a fast clock reads processes
//! every FAST and git state every GIT_EVERY ticks: git runs as child processes
//! and is most of treetop's idle CPU, so it gets half the rate. Pool state
//! changes only when a tree is leased, returned or destroyed, and `treehouse
//! status` costs seconds of CPU, so the slow clock lists it only when `git
//! worktree list` changes, when asked, and every SLOW as a backstop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use anyhow::Result;

use crate::cache;
use crate::live;
use crate::pool::{self, Process, Tree, Work};

const FAST: Duration = Duration::from_secs(2);
const SLOW: Duration = Duration::from_secs(120);
const GIT_EVERY: u32 = 2;

pub enum Update {
    ListingStarted,
    Listing(Result<Vec<Tree>>),
    Processes(HashMap<PathBuf, Vec<Process>>),
    Work(HashMap<PathBuf, Option<Work>>),
}

pub struct Refresher {
    pub updates: Receiver<Update>,
    kick: Sender<()>,
    paused: Arc<AtomicBool>,
}

impl Refresher {
    /// Starts both clocks for the pool of `checkout`. `known` is the listing
    /// already on screen, from the cache, so the fast clock has trees to probe
    /// before the first slow listing lands.
    pub fn spawn(checkout: PathBuf, known: Vec<Tree>) -> Self {
        let (updates_tx, updates) = mpsc::channel();
        let (kick, kick_rx) = mpsc::channel();
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
            let (kick, paused) = (kick.clone(), Arc::clone(&paused));
            move || fast(&checkout, &updates_tx, &kick, &paused, &trees)
        });
        Refresher {
            updates,
            kick,
            paused,
        }
    }

    /// Lists the pool now rather than at the next trigger.
    pub fn refresh(&self) {
        let _ = self.kick.send(());
    }

    /// Stops both clocks, for while a shell has the terminal and treetop is
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
            let listing = pool::status(checkout).map(|(listing, raw)| {
                cache::write(checkout, &raw);
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

fn fast(
    checkout: &std::path::Path,
    updates: &Sender<Update>,
    kick: &Sender<()>,
    paused: &AtomicBool,
    trees: &Mutex<Vec<Tree>>,
) {
    let mut seen = None;
    let mut tick: u32 = 0;
    loop {
        if !paused.load(Ordering::Relaxed) {
            let known = trees.lock().unwrap_or_else(PoisonError::into_inner).clone();
            if !known.is_empty() {
                let mut sent = updates.send(Update::Processes(live::processes(&known)));
                if tick.is_multiple_of(GIT_EVERY) {
                    sent = sent.and_then(|()| updates.send(Update::Work(live::work(&known))));
                }
                if sent.is_err() {
                    return;
                }
                tick = tick.wrapping_add(1);
            }
            let worktrees = live::worktrees(checkout);
            if seen.is_some() && worktrees != seen && kick.send(()).is_err() {
                return;
            }
            seen = worktrees;
        }
        thread::sleep(FAST);
    }
}
