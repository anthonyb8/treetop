//! Tickets, from the ticket plugin of the repository's ticket source in
//! `config.toml`: listing them with filters, reading one, the branch its
//! system names for it, and marking it started. The plugin does the talking;
//! this resolves which plugin and which settings, and reads its replies.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

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

/// The filter line's words, split at spaces outside double quotes.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let (mut word, mut is_quoted) = (String::new(), false);
    for c in text.chars() {
        match c {
            '"' => is_quoted = !is_quoted,
            c if c.is_whitespace() && !is_quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The filters a filter line asks for: `mine`, `label:`, `state:`,
/// `project:` and `cycle:` (`current` or a number) take a value, quoted when
/// it has spaces; every other word searches titles.
pub fn parse(text: &str) -> Filters {
    let mut filters = Filters::default();
    let mut query = Vec::new();
    for word in words(text) {
        let (key, value) = word.split_once(':').unwrap_or(("", &word));
        let value = Some(value.to_string()).filter(|v| !v.is_empty());
        match key {
            "label" => filters.label = value,
            "state" => filters.state = value,
            "project" => filters.project = value,
            "cycle" => filters.cycle = value,
            _ if word == "mine" => filters.mine = true,
            _ => query.push(word),
        }
    }
    filters.query = (!query.is_empty()).then(|| query.join(" "));
    filters
}

/// `filters` as the filter line that asks for them, which `parse` reads back.
pub fn line(filters: &Filters) -> String {
    let quoted = |value: &str| {
        if value.contains(' ') {
            format!("\"{value}\"")
        } else {
            value.to_string()
        }
    };
    let mut parts = Vec::new();
    if filters.mine {
        parts.push("mine".to_string());
    }
    for (key, value) in [
        ("label", &filters.label),
        ("state", &filters.state),
        ("project", &filters.project),
        ("cycle", &filters.cycle),
    ] {
        if let Some(value) = value {
            parts.push(format!("{key}:{}", quoted(value)));
        }
    }
    parts.extend(filters.query.clone());
    parts.join(" ")
}

/// Tickets read for a filter, or why they could not be.
pub struct Loaded {
    pub filters: Filters,
    /// The source's name and its tickets.
    pub result: Result<(String, Vec<Ticket>), String>,
}

/// Lists tickets on a thread of its own, so the screen never waits on the
/// network. Only the latest request waiting is read; older ones are stale.
pub struct Loader {
    requests: Sender<Filters>,
    pub results: Receiver<Loaded>,
}

impl Loader {
    pub fn spawn(checkout: PathBuf) -> Self {
        let (requests, queue) = mpsc::channel::<Filters>();
        let (results_tx, results) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mut filters) = queue.recv() {
                while let Ok(newer) = queue.try_recv() {
                    filters = newer;
                }
                let result = Source::of(&checkout)
                    .and_then(|source| Ok((source.name.clone(), source.list(&filters)?)));
                if results_tx.send(Loaded { filters, result }).is_err() {
                    return;
                }
            }
        });
        Loader { requests, results }
    }

    pub fn request(&self, filters: Filters) {
        let _ = self.requests.send(filters);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_line_reads_keys_quoted_values_and_search_words() {
        let filters = parse(r#"mine label:Bug state:"In Review" cycle:current fix login"#);
        assert_eq!(
            filters,
            Filters {
                mine: true,
                label: Some("Bug".into()),
                state: Some("In Review".into()),
                cycle: Some("current".into()),
                query: Some("fix login".into()),
                ..Filters::default()
            }
        );
        assert_eq!(parse(&line(&filters)), filters, "it reads back");
        assert_eq!(parse("  "), Filters::default());
    }
}
