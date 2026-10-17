//! Tickets, from the ticket plugin of the repository's ticket source in
//! `config.toml`: listing them with filters, reading one, the branch its
//! system names for it, and marking it started. The plugin does the talking;
//! this resolves which plugin and which settings, and reads its replies.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config;
use crate::plugin::{self, Kind, Plugin};

/// A ticket as a ticket plugin reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ticket {
    /// The id people use, such as `END-123`.
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub cycle: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// The ticket's own text; `get` gives it, `list` may leave it out.
    #[serde(default)]
    pub description: Option<String>,
    /// The branch the ticket system names for it.
    #[serde(default)]
    pub branch: Option<String>,
}

/// What narrows `list`. Each is optional; none lists the team's open tickets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filters {
    /// Only tickets assigned to the key's user.
    #[serde(default)]
    pub mine: bool,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    /// `current` for the active cycle, else a cycle number.
    #[serde(default)]
    pub cycle: Option<String>,
    /// Words the title contains.
    #[serde(default)]
    pub query: Option<String>,
}

/// The repository's ticket source: its name, its plugin and the settings the
/// plugin is given.
#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub name: String,
    plugin: Plugin,
    config: Value,
}

impl Source {
    /// The source of the repository checked out at `checkout`, or what to
    /// set to have one.
    pub fn of(checkout: &Path) -> Result<Self, String> {
        let config = config::load()?;
        let (name, source) = config.source_for(checkout)?;
        let plugin = plugin::named(Kind::Tickets, &source.plugin)
            .ok_or_else(|| format!("no ticket plugin {}", source.plugin))?;
        Ok(Source {
            name: name.to_string(),
            plugin,
            config: source.config(),
        })
    }

    fn call(&self, op: &str, args: Value) -> Result<Value, String> {
        self.plugin
            .call_with(op, args, self.config.clone())
            .map_err(|err| format!("{}: {err}", self.name))
    }

    pub fn list(&self, filters: &Filters) -> Result<Vec<Ticket>, String> {
        let reply = self.call("list", json!({ "filters": filters }))?;
        serde_json::from_value(reply.get("tickets").cloned().unwrap_or(json!([])))
            .map_err(|err| format!("{}: unreadable tickets: {err}", self.name))
    }
}
