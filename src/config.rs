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

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::Deserialize;

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

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub hooks: Hooks,
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
    fn a_misspelt_hook_is_an_error() {
        assert!(parse("[hooks]\non_retrun = [\"x\"]\n").is_err());
    }
}
