//! A held tree keeps its agents awake. Each agent plugin that answers `keep`
//! is handed the background agents in held trees, which it keeps from being
//! retired for sitting idle and starts again when they have gone cold.
//! Returning the tree stops them, which lets them rest.
//!
//! Plugins are asked off the UI thread, one round at a time, and only when
//! the agents to keep change or one of them is cold, since starting an agent
//! takes seconds.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use serde_json::json;

use crate::plugin::{self, Kind};

/// A background agent to keep awake: its plugin, its id there, and whether
/// it is cold now.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Kept {
    pub plugin: String,
    pub id: String,
    pub is_cold: bool,
}

#[derive(Default)]
pub struct Keeper {
    is_busy: Arc<AtomicBool>,
    /// The agents handed over last, so the same awake set is not handed
    /// over again every tick.
    last: BTreeSet<(String, String)>,
}

impl Keeper {
    /// Hands `agents` to their plugins, unless a round is still running, or
    /// the same agents were handed over last and all of them are awake.
    pub fn keep(&mut self, agents: Vec<Kept>) {
        let ids: BTreeSet<(String, String)> = agents
            .iter()
            .map(|a| (a.plugin.clone(), a.id.clone()))
            .collect();
        let is_any_cold = agents.iter().any(|a| a.is_cold);
        if (ids == self.last && !is_any_cold) || self.is_busy.swap(true, Ordering::AcqRel) {
            return;
        }
        self.last = ids.clone();
        let is_busy = Arc::clone(&self.is_busy);
        thread::spawn(move || {
            let mut by_plugin: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (plugin, id) in ids {
                by_plugin.entry(plugin).or_default().push(id);
            }
            for (name, ids) in by_plugin {
                let Some(plugin) = plugin::named(Kind::Agent, &name) else {
                    continue;
                };
                let answers_keep = plugin
                    .description()
                    .is_ok_and(|d| d.ops.iter().any(|op| op == "keep"));
                if answers_keep {
                    // A plugin that cannot keep its agents leaves them as
                    // they are; the next round asks again.
                    let _ = plugin.call("keep", json!({ "ids": ids }));
                }
            }
            is_busy.store(false, Ordering::Release);
        });
    }
}
