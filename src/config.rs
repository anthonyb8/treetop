//! `~/.config/treetop/config.toml` (or under `$XDG_CONFIG_HOME`), which is
//! optional: with no file, treetop runs as it ships. It holds what belongs to
//! one person's setup rather than to treetop, such as stopping a dev stack
//! when a tree is returned.
//!
//! ```toml
//! [hooks]
//! on_dispatch = ["notify-send", "agent started"]
//! on_return = ["harness", "stack", "down"]
//! ```
//!
//! A hook is a command and its arguments, run in the tree with
//! `TREETOP_TREE`, `TREETOP_BRANCH` and `TREETOP_TICKET` set. A hook that
//! fails is shown in the log, and never stops the start or return it belongs
//! to.
//!
//! Tickets come from named sources, each a ticket plugin and its settings,
//! which the plugin gets as its `config`. A repository picks its source by
//! its main checkout's path; with a single source, every repository uses it.
//!
//! ```toml
//! [tickets.work]
//! plugin = "linear"                 # the default
//! key_env = "LINEAR_API_KEY_WORK"
//! team = "END"
//!
//! [tickets.personal]
//! key_env = "LINEAR_API_KEY_PERSONAL"
//! team = "ANT"
//!
//! [repos."~/endo/app"]
//! tickets = "work"
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hooks {
    /// Run once an agent has started in a tree.
    #[serde(default)]
    pub on_dispatch: Vec<String>,
    /// Run before a tree is returned, after its agents are stopped.
    #[serde(default)]
    pub on_return: Vec<String>,
}

/// A place tickets come from: a ticket plugin and the settings it is given.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Source {
    #[serde(default = "linear")]
    pub plugin: String,
    /// Everything else in the table, passed to the plugin as its `config`.
    #[serde(flatten)]
    pub settings: BTreeMap<String, toml::Value>,
}

fn linear() -> String {
    "linear".into()
}

impl Source {
    /// The settings as the JSON a plugin request carries.
    pub fn config(&self) -> Value {
        serde_json::to_value(&self.settings).unwrap_or(Value::Null)
    }
}

/// What a repository uses, keyed by its main checkout's path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    /// The name of its ticket source.
    #[serde(default)]
    pub tickets: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub tickets: BTreeMap<String, Source>,
    #[serde(default)]
    pub repos: BTreeMap<String, Repo>,
    /// The first message of an agent picking up a ticket, with `{id}`,
    /// `{title}`, `{description}`, `{url}` and `{branch}` filled in.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// `path` with a leading `~` as the home directory.
fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

impl Config {
    /// The ticket source of the repository checked out at `checkout`, with
    /// its name: the one its `[repos]` entry names, else the only one there
    /// is. An error says what to set.
    pub fn source_for(&self, checkout: &Path) -> Result<(&str, &Source), String> {
        let named = self
            .repos
            .iter()
            .find(|(path, _)| expand(path) == checkout)
            .and_then(|(_, repo)| repo.tickets.as_deref());
        let name = match (named, self.tickets.len()) {
            (Some(name), _) => name,
            (None, 1) => self
                .tickets
                .keys()
                .next()
                .map(String::as_str)
                .unwrap_or_default(),
            (None, 0) => {
                return Err("no ticket source: add a [tickets.<name>] table to config.toml".into());
            }
            (None, _) => {
                return Err(format!(
                    "several ticket sources: name one for this repository under [repos.\"{}\"]",
                    checkout.display()
                ));
            }
        };
        self.tickets
            .get_key_value(name)
            .map(|(name, source)| (name.as_str(), source))
            .ok_or_else(|| format!("no ticket source named {name}"))
    }
}

/// `$XDG_CONFIG_HOME/treetop/config.toml`, else `~/.config/treetop/config.toml`.
pub fn path() -> Option<PathBuf> {
    crate::plugin::dir().and_then(|plugins| plugins.parent().map(|dir| dir.join("config.toml")))
}

fn parse(text: &str) -> Result<Config, String> {
    toml::from_str(text).map_err(|err| err.to_string())
}

/// The config, or the default when there is no file. A file that cannot be
/// read or parsed is an error, so a typo is reported rather than ignored.
pub fn load() -> Result<Config, String> {
    let Some(path) = path() else {
        return Ok(Config::default());
    };
    match fs::read_to_string(&path) {
        Ok(text) => parse(&text).map_err(|err| format!("{}: {err}", path.display())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hooks_and_needs_nothing() {
        assert_eq!(parse("").unwrap(), Config::default());
        let config = parse("[hooks]\non_return = [\"harness\", \"stack\", \"down\"]\n").unwrap();
        assert_eq!(config.hooks.on_return, ["harness", "stack", "down"]);
        assert!(config.hooks.on_dispatch.is_empty());
    }

    #[test]
    fn a_repository_takes_the_source_it_names_else_the_only_one() {
        let config = parse(
            "[tickets.work]\nkey_env = \"W\"\nteam = \"END\"\n\
             [tickets.personal]\nplugin = \"linear\"\nkey_env = \"P\"\n\
             [repos.\"/home/me/app\"]\ntickets = \"work\"\n",
        )
        .unwrap();
        let (name, source) = config.source_for(Path::new("/home/me/app")).unwrap();
        assert_eq!((name, source.plugin.as_str()), ("work", "linear"));
        assert_eq!(
            source.config(),
            serde_json::json!({ "key_env": "W", "team": "END" })
        );
        assert!(
            config
                .source_for(Path::new("/home/me/other"))
                .unwrap_err()
                .starts_with("several ticket sources")
        );
        let one = parse("[tickets.only]\nkey_env = \"K\"\n").unwrap();
        assert_eq!(one.source_for(Path::new("/anywhere")).unwrap().0, "only");
        assert!(Config::default().source_for(Path::new("/x")).is_err());
    }

    #[test]
    fn a_misspelt_hook_is_an_error() {
        assert!(parse("[hooks]\non_retrun = [\"x\"]\n").is_err());
    }
}
