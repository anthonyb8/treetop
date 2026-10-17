//! Tickets, from the ticket plugin of the repository's ticket source in
//! `config.toml`: listing them with filters, reading one, the branch its
//! system names for it, and marking it started. The plugin does the talking;
//! this resolves which plugin and which settings, and reads its replies.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use issues::Backend;
pub use issues::Filters;

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

    pub fn get(&self, id: &str) -> Result<Ticket, String> {
        serde_json::from_value(self.call("get", json!({ "id": id }))?)
            .map_err(|err| format!("{}: unreadable ticket: {err}", self.name))
    }

    /// Marks the ticket started and assigned to the key's user.
    pub fn start(&self, id: &str) -> Result<(), String> {
        self.call("start", json!({ "id": id })).map(|_| ())
    }

    pub fn list(&self, filters: &Filters) -> Result<Vec<Ticket>, String> {
        let reply = self.call("list", json!({ "filters": filters }))?;
        serde_json::from_value(reply.get("tickets").cloned().unwrap_or(json!([])))
            .map_err(|err| format!("{}: unreadable tickets: {err}", self.name))
    }
}

/// The prompt an agent picking up `ticket` starts with: `template` from the
/// config with `{id}`, `{title}`, `{description}`, `{url}` and `{branch}`
/// filled in, else the ticket's id and title, its text and its link.
pub fn prompt(template: Option<&str>, ticket: &Ticket) -> String {
    const DEFAULT: &str = "{id}: {title}\n\n{description}\n\n{url}";
    let text = |value: &Option<String>| value.clone().unwrap_or_default();
    let filled = template
        .unwrap_or(DEFAULT)
        .replace("{id}", &ticket.id)
        .replace("{title}", &ticket.title)
        .replace("{description}", &text(&ticket.description))
        .replace("{url}", &text(&ticket.url))
        .replace("{branch}", &text(&ticket.branch));
    // A field the ticket lacks leaves no run of blank lines behind.
    let mut out = String::new();
    for part in filled.split("\n\n").filter(|p| !p.trim().is_empty()) {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(part.trim_end());
    }
    out
}

/// A ticket plugin other than Linear's on the Issues screen: it lists and
/// reads through the plugin contract, and its edits say they cannot.
struct ReadOnly(Source);

/// A ticket as the Issues screen shows an issue.
fn issue(ticket: Ticket) -> linear::Issue {
    linear::Issue {
        uuid: ticket.id.clone(),
        id: ticket.id,
        title: ticket.title,
        state: ticket.state,
        labels: ticket.labels,
        assignee: ticket.assignee,
        project: ticket.project,
        cycle: ticket.cycle,
        url: ticket.url,
        description: ticket.description,
        branch: ticket.branch,
        ..linear::Issue::default()
    }
}

impl Backend for ReadOnly {
    fn name(&self) -> &str {
        &self.0.name
    }

    fn list(&self, filters: &Filters) -> Result<Vec<linear::Issue>, String> {
        Ok(self.0.list(filters)?.into_iter().map(issue).collect())
    }

    fn detail(&self, id: &str) -> Result<linear::Detail, String> {
        Ok(linear::Detail {
            issue: issue(self.0.get(id)?),
            comments: Vec::new(),
        })
    }
}

/// The config's own views for the Issues screen, in name order; none when
/// the config cannot be read, which the screen's source says instead.
pub fn views() -> Vec<issues::View> {
    config::load()
        .map(|config| {
            config
                .views
                .iter()
                .map(|(name, text)| issues::View::parse(name, text))
                .collect()
        })
        .unwrap_or_default()
}

/// Every issue source in the config as a backend for the Issues screen, and
/// the index of the repository's own. Linear's built-in plugin gets the full
/// screen; any other plugin is read-only.
pub fn backends(checkout: &Path) -> Result<(Vec<Arc<dyn Backend>>, usize), String> {
    let config = config::load()?;
    let own = config
        .source_for(checkout)
        .ok()
        .map(|(name, _)| name.to_string());
    let mut backends: Vec<Arc<dyn Backend>> = Vec::new();
    let mut at = 0;
    for (name, source) in &config.issues {
        if own.as_deref() == Some(name.as_str()) {
            at = backends.len();
        }
        let plugin = plugin::named(Kind::Tickets, &source.plugin)
            .ok_or_else(|| format!("[issues.{name}]: no ticket plugin {}", source.plugin))?;
        let setting = |key: &str| {
            source
                .settings
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        if plugin == Plugin::Builtin("linear") {
            backends.push(Arc::new(issues::Linear::new(
                name,
                setting("key_env").as_deref(),
                setting("team"),
            )));
        } else {
            backends.push(Arc::new(ReadOnly(Source {
                name: name.clone(),
                plugin,
                config: source.config(),
            })));
        }
    }
    Ok((backends, at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_fills_the_template_and_drops_what_the_ticket_lacks() {
        let ticket = Ticket {
            id: "END-812".into(),
            title: "Fix the login redirect".into(),
            description: Some("Keep return_to through SSO.".into()),
            url: None,
            ..Ticket::default()
        };
        assert_eq!(
            prompt(None, &ticket),
            "END-812: Fix the login redirect\n\nKeep return_to through SSO."
        );
        assert_eq!(
            prompt(Some("Work on {id}. Use /pickup {id}."), &ticket),
            "Work on END-812. Use /pickup END-812."
        );
    }
}
