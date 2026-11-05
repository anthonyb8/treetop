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
//! `[guard]` names commands of your own that act on the tree they run in,
//! such as stopping its dev stack, which the guard then lets an agent run
//! only in a tree it holds (see `hook`):
//!
//! ```toml
//! [guard]
//! in_tree = ["docker compose down"]
//! ```
//!
//! Issues come from named sources, each a ticket plugin and its settings,
//! which the plugin gets as its `config`. A repository picks its source by
//! its main checkout's path; with a single source, every repository uses it.
//!
//! ```toml
//! [issues.work]
//! plugin = "linear"                 # the default
//! key_env = "LINEAR_API_KEY_WORK"
//! team = "END"
//!
//! [issues.personal]
//! key_env = "LINEAR_API_KEY_PERSONAL"
//! team = "ANT"
//!
//! [repos."~/endo/app"]
//! issues = "work"
//! ship = ["no-mistakes", "github"] # its ship plugins; github alone by default
//! ```
//!
//! `[mobile]` is `treetop serve`'s: the Tailscale login it answers, and
//! the port it listens on, on 127.0.0.1 only:
//!
//! ```toml
//! [mobile]
//! owner = "me@example.com"
//! port = 7777
//! ```
//!
//! `browser` is the command `o` and `O` open a URL with, in place of the
//! desktop's opener, with `{url}` in its words replaced by the URL, else the
//! URL added last:
//!
//! ```toml
//! browser = ["chromium", "--user-data-dir=/home/me/.config/devproxy-browser"]
//! ```
//!
//! `share` is the command `p` shares a port with, and `treetop serve` its
//! own, so the phone can reach it,
//! `tether share` when it names none, with `{port}` in its words replaced by
//! the port, else the port added last. It prints the address the phone opens,
//! which treetop shows as a QR code:
//!
//! ```toml
//! share = ["tether", "share"]
//! ```
//!
//! `publish` is the command `P` puts a port on the internet with, for anyone
//! with the link, `tether share --public` when it names none, and
//! `unpublish` the one that takes it down again, `tether unshare` when it
//! names none; `{port}` works in both as in `share`:
//!
//! ```toml
//! publish = ["tether", "share", "--public"]
//! unpublish = ["tether", "unshare"]
//! ```
//!
//! `shares` is the command the PORTS column reads back which ports are
//! shared from, and which of them are public, `tether shares` when it names
//! none. It takes no port, and prints a line an address, as
//! `https://host:5173 -> http://localhost:5173`, with ` (public)` on the
//! ones the internet can reach:
//!
//! ```toml
//! shares = ["tether", "shares"]
//! ```
//!
//! `[claude]` lists the Claude Code config dirs treetop finds sessions in,
//! for setups that run some sessions under their own `CLAUDE_CONFIG_DIR`,
//! such as keel's `~/.keel/claude` beside `~/.claude`. Each session is
//! attached, resumed and stopped under the dir it came from, and a new agent
//! starts under the first. With none listed, it is `CLAUDE_CONFIG_DIR`,
//! else `~/.claude`, as Claude Code itself picks:
//!
//! ```toml
//! [claude]
//! config_dirs = ["~/.claude", "~/.keel/claude"]
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

/// The guard's settings: what it watches beyond treehouse and treetop.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guard {
    /// Commands that act on the tree they run in, each as the words it
    /// starts with, which an agent may run only in a tree it holds.
    #[serde(default)]
    pub in_tree: Vec<String>,
}

/// A place issues come from: Linear or a ticket plugin, and the settings it
/// is given.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Source {
    #[serde(default = "linear")]
    pub plugin: String,
    /// Everything else in the table, passed to the plugin as its `config`.
    #[serde(flatten)]
    pub settings: BTreeMap<String, toml::Value>,
}

/// The value of `name`, a variable holding an API key: from the environment,
/// else from the `env` file beside the config, which is how a service, with
/// none of a shell's variables, gets its keys without one in a unit file.
pub fn key(name: &str) -> Result<String, String> {
    if let Ok(value) = std::env::var(name)
        && !value.trim().is_empty()
    {
        return Ok(value);
    }
    let file = path().and_then(|p| p.parent().map(|dir| dir.join("env")));
    let found = file.as_deref().map(|f| key_in(f, name)).transpose()?;
    found.flatten().ok_or_else(|| {
        let place = file.map_or("the config directory".into(), |f| f.display().to_string());
        format!(
            "{name} is not set, in the environment or in {place}; it should hold a Linear API key"
        )
    })
}

/// The value of `name` in `file`, of `NAME=value` lines, where `file` exists.
/// Lines may say `export`, quote the value and be commented out with `#`. A
/// file others can read is refused rather than used, since it holds keys.
fn key_in(file: &Path, name: &str) -> Result<Option<String>, String> {
    use std::os::unix::fs::PermissionsExt as _;
    let shown = file.display();
    let metadata = match fs::metadata(file) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{shown}: {err}")),
    };
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(format!(
            "{shown} holds keys and others can read it: run chmod 600 {shown}"
        ));
    }
    let text = fs::read_to_string(file).map_err(|err| format!("{shown}: {err}"))?;
    Ok(text.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (key, value) = line.split_once('=')?;
        let value = value.trim();
        let unquoted = ['"', '\'']
            .iter()
            .find_map(|q| value.strip_prefix(*q)?.strip_suffix(*q))
            .unwrap_or(value);
        (key.trim() == name && !unquoted.is_empty()).then(|| unquoted.to_string())
    }))
}

/// The source treetop reads itself, without a plugin.
pub const LINEAR: &str = "linear";

fn linear() -> String {
    LINEAR.into()
}

impl Source {
    /// The settings as the JSON a plugin request carries.
    pub fn config(&self) -> Value {
        serde_json::to_value(&self.settings).unwrap_or(Value::Null)
    }
}

/// `treetop serve`'s settings: who it answers, and where it listens.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mobile {
    /// The Tailscale login the server answers, as `tailscale serve` hands
    /// it over in `Tailscale-User-Login`; with none, it does not start.
    #[serde(default)]
    pub owner: Option<String>,
    /// The port on 127.0.0.1 it listens on; 7777 when unset.
    #[serde(default)]
    pub port: Option<u16>,
}

/// The Claude Code setups treetop looks in.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claude {
    /// Its config dirs, the first where new agents start; empty for the one
    /// Claude Code uses by default.
    #[serde(default)]
    pub config_dirs: Vec<String>,
}

impl Claude {
    /// The config dirs listed, with `~` expanded and repeats dropped.
    pub fn dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        for dir in self.config_dirs.iter().map(|dir| expand(dir)) {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        dirs
    }
}

/// What a repository uses, keyed by its main checkout's path.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repo {
    /// The name of its issue source.
    #[serde(default)]
    pub issues: Option<String>,
    /// Its ship plugins, in order; None for the default.
    #[serde(default)]
    pub ship: Option<Vec<String>>,
    /// Run in each of its trees before it is returned, after the global
    /// `on_return`: what only this repository needs, such as clearing build
    /// leftovers.
    #[serde(default)]
    pub on_return: Vec<String>,
    /// Run in its main checkout once treetop has fast-forwarded it, so what
    /// trees are seeded from, such as installed packages, keeps up.
    #[serde(default)]
    pub on_catch_up: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub hooks: Hooks,
    #[serde(default)]
    pub guard: Guard,
    #[serde(default)]
    pub issues: BTreeMap<String, Source>,
    #[serde(default)]
    pub repos: BTreeMap<String, Repo>,
    /// Views of your own on the Issues screen, on keys 6 to 9 in name
    /// order: a filter line each, which may say `group:project` or
    /// `group:status`.
    #[serde(default)]
    pub views: BTreeMap<String, String>,
    /// The first message of an agent picking up a ticket, with `{id}`,
    /// `{title}`, `{description}`, `{url}` and `{branch}` filled in.
    #[serde(default)]
    pub prompt: Option<String>,
    /// The command a URL is opened with, run even with no desktop session;
    /// empty for the desktop's opener.
    #[serde(default)]
    pub browser: Vec<String>,
    /// The command a port is shared with for the phone, which prints the
    /// address it is reached at; empty for `tether share`.
    #[serde(default)]
    pub share: Vec<String>,
    /// The command a port is put on the internet with, which prints its
    /// public address; empty for `tether share --public`.
    #[serde(default)]
    pub publish: Vec<String>,
    /// The command a published port is taken off the internet with; empty
    /// for `tether unshare`.
    #[serde(default)]
    pub unpublish: Vec<String>,
    /// The command the shared ports are listed with, to mark them in the
    /// PORTS column; empty for `tether shares`.
    #[serde(default)]
    pub shares: Vec<String>,
    /// `treetop serve`, the web app for a phone.
    #[serde(default)]
    pub mobile: Mobile,
    /// The Claude Code config dirs sessions are found in.
    #[serde(default)]
    pub claude: Claude,
}

/// `path` with a leading `~` as the home directory.
fn expand(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => Path::new(&home).join(rest),
        _ => PathBuf::from(path),
    }
}

impl Config {
    /// The issue source of the repository checked out at `checkout`, with
    /// its name: the one its `[repos]` entry names. A repository has none
    /// unless it names one, so an unrelated repository shows no one's
    /// issues; the error says what to add.
    pub fn source_for(&self, checkout: &Path) -> Result<(&str, &Source), String> {
        if self.issues.is_empty() {
            return Err("no issue source: add an [issues.<name>] table to config.toml".into());
        }
        let named = self
            .repos
            .iter()
            .find(|(path, _)| expand(path) == checkout)
            .and_then(|(_, repo)| repo.issues.as_deref());
        let Some(name) = named else {
            let names: Vec<String> = self.issues.keys().map(|n| format!("\"{n}\"")).collect();
            let names = match names.split_last() {
                Some((last, [])) => last.clone(),
                Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
                None => String::new(),
            };
            return Err(format!(
                "{path} has no issue source: add [repos.\"{path}\"] with issues = {names} to the config",
                path = checkout.display()
            ));
        };
        self.issues
            .get_key_value(name)
            .map(|(name, source)| (name.as_str(), source))
            .ok_or_else(|| format!("no issue source named {name}"))
    }

    /// The `[repos]` entry of the repository checked out at `checkout`.
    pub fn repo(&self, checkout: &Path) -> Option<&Repo> {
        self.repos
            .iter()
            .find(|(path, _)| expand(path) == checkout)
            .map(|(_, repo)| repo)
    }

    /// The ship plugins the repository checked out at `checkout` names,
    /// None when it names none and gets the default.
    pub fn ship_for(&self, checkout: &Path) -> Option<Vec<String>> {
        self.repos
            .iter()
            .find(|(path, _)| expand(path) == checkout)
            .and_then(|(_, repo)| repo.ship.clone())
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
        let config = parse("[guard]\nin_tree = [\"docker compose down\"]\n").unwrap();
        assert_eq!(config.guard.in_tree, ["docker compose down"]);
    }

    #[test]
    fn claude_config_dirs_expand_home_and_drop_repeats() {
        assert!(Config::default().claude.dirs().is_empty());
        let config =
            parse("[claude]\nconfig_dirs = [\"~/.claude\", \"/x/keel\", \"~/.claude\"]\n").unwrap();
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        assert_eq!(
            config.claude.dirs(),
            [home.join(".claude"), PathBuf::from("/x/keel")]
        );
        assert!(parse("[claude]\nconfig_dir = []\n").is_err());
    }

    #[test]
    fn a_repository_takes_the_source_it_names_and_no_other() {
        let config = parse(
            "[issues.work]\nkey_env = \"W\"\nteam = \"END\"\n\
             [issues.personal]\nplugin = \"linear\"\nkey_env = \"P\"\n\
             [repos.\"/home/me/app\"]\nissues = \"work\"\n",
        )
        .unwrap();
        let (name, source) = config.source_for(Path::new("/home/me/app")).unwrap();
        assert_eq!((name, source.plugin.as_str()), ("work", "linear"));
        assert_eq!(
            source.config(),
            serde_json::json!({ "key_env": "W", "team": "END" })
        );
        assert_eq!(
            config.source_for(Path::new("/home/me/other")).unwrap_err(),
            "/home/me/other has no issue source: add [repos.\"/home/me/other\"] with \
             issues = \"personal\" or \"work\" to the config"
        );
        // One source is not every repository's: an unrelated one shows none.
        let one = parse("[issues.only]\nkey_env = \"K\"\n").unwrap();
        assert!(
            one.source_for(Path::new("/anywhere"))
                .unwrap_err()
                .starts_with("/anywhere has no issue source")
        );
        assert!(Config::default().source_for(Path::new("/x")).is_err());
    }

    #[test]
    fn a_repository_names_its_ship_plugins_or_gets_none() {
        let config = parse(
            "[repos.\"/home/me/app\"]\nship = [\"no-mistakes\", \"github\"]\n\
             [repos.\"/home/me/quiet\"]\nship = []\n",
        )
        .unwrap();
        assert_eq!(
            config.ship_for(Path::new("/home/me/app")),
            Some(vec!["no-mistakes".to_string(), "github".to_string()])
        );
        assert_eq!(config.ship_for(Path::new("/home/me/quiet")), Some(vec![]));
        assert_eq!(config.ship_for(Path::new("/home/me/other")), None);
    }

    #[test]
    fn a_key_file_reads_as_a_shell_would_and_only_when_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("treetop-keys-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("env");
        assert_eq!(key_in(&file, "A"), Ok(None), "no file is no key");
        fs::write(
            &file,
            "# A=commented\nA=plain\nexport B=\"quoted\"\nC='single'\nD=\nAA=other\n",
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let read = |name| key_in(&file, name).unwrap();
        assert_eq!(read("A").as_deref(), Some("plain"));
        assert_eq!(read("B").as_deref(), Some("quoted"));
        assert_eq!(read("C").as_deref(), Some("single"));
        assert_eq!(read("D"), None, "empty is unset");
        assert_eq!(read("E"), None);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let why = key_in(&file, "A").unwrap_err();
        assert!(
            why.ends_with(&format!("run chmod 600 {}", file.display())),
            "{why}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn views_are_named_filter_lines() {
        let config = parse("[views]\nbugs = \"label:Bug group:status\"\n").unwrap();
        assert_eq!(config.views["bugs"], "label:Bug group:status");
    }

    #[test]
    fn browser_is_a_command_or_nothing() {
        assert!(Config::default().browser.is_empty());
        let config = parse("browser = [\"chromium\", \"--new-window\", \"{url}\"]\n").unwrap();
        assert_eq!(config.browser, ["chromium", "--new-window", "{url}"]);
    }

    #[test]
    fn a_misspelt_hook_is_an_error() {
        assert!(parse("[hooks]\non_retrun = [\"x\"]\n").is_err());
    }
}
