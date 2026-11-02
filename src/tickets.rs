//! Issue sources: each `[issues.<name>]` table in `config.toml` as a backend,
//! which is how everything in treetop reads and changes issues. Linear is
//! called directly; any other source goes through its ticket plugin, on the
//! wire format below. A source's backend is built once a process, so its
//! client and team are shared by whatever asks. Keys come from `config::key`.
//!
//! ```toml
//! [issues.work]
//! plugin = "linear"                 # the default
//! key_env = "LINEAR_API_KEY_WORK"   # the variable holding a personal API key
//! team = "END"                      # the team whose issues are listed
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use serde::Deserialize;
use serde_json::{Value, json};

use issues::Backend;
pub use issues::Filters;
use linear::Issue;

use crate::config;
use crate::plugin::{self, Kind, Plugin};

/// A ticket as a ticket plugin reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct Ticket {
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
    /// The tickets it is linked to; `get` may give them, `list` leaves them
    /// out.
    #[serde(default)]
    pub links: Vec<Link>,
}

/// Another ticket a ticket is linked to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
struct Link {
    pub id: String,
    /// `blocks`, `blocked by` or `related`, from the ticket's side.
    pub kind: String,
    /// It is done or cancelled, so it holds nothing up.
    #[serde(default)]
    pub done: bool,
}

/// A source on a ticket plugin other than Linear: it reads and marks through
/// the plugin contract, and its edits say they cannot.
#[derive(Debug, Clone, PartialEq)]
struct Plugged {
    name: String,
    plugin: Plugin,
    config: Value,
}

impl Plugged {
    fn call(&self, op: &str, args: Value) -> Result<Value, String> {
        self.plugin
            .call_with(op, args, self.config.clone())
            .map_err(|err| format!("{}: {err}", self.name))
    }
}

/// The prompt an agent picking up `ticket` starts with: `template` from the
/// config with `{id}`, `{title}`, `{description}`, `{url}` and `{branch}`
/// filled in, else the ticket's id and title, its text and its link.
pub fn prompt(template: Option<&str>, ticket: &Issue) -> String {
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

/// A ticket as an issue.
fn issue(ticket: Ticket) -> Issue {
    Issue {
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

impl Backend for Plugged {
    fn name(&self) -> &str {
        &self.name
    }

    fn list(&self, filters: &Filters) -> Result<Vec<Issue>, String> {
        let reply = self.call("list", json!({ "filters": filters }))?;
        let tickets: Vec<Ticket> =
            serde_json::from_value(reply.get("tickets").cloned().unwrap_or(json!([])))
                .map_err(|err| format!("{}: unreadable tickets: {err}", self.name))?;
        Ok(tickets.into_iter().map(issue).collect())
    }

    fn detail(&self, id: &str) -> Result<linear::Detail, String> {
        let ticket: Ticket = serde_json::from_value(self.call("get", json!({ "id": id }))?)
            .map_err(|err| format!("{}: unreadable ticket: {err}", self.name))?;
        let links = ticket
            .links
            .iter()
            .map(|link| linear::Link {
                kind: link.kind.clone(),
                id: link.id.clone(),
                title: String::new(),
                state: String::new(),
                is_done: link.done,
            })
            .collect();
        Ok(linear::Detail {
            issue: issue(ticket),
            comments: Vec::new(),
            links,
        })
    }

    fn start(&self, id: &str) -> Result<(), String> {
        self.call("start", json!({ "id": id })).map(|_| ())
    }

    fn finish(&self, id: &str) -> Result<(), String> {
        self.call("finish", json!({ "id": id })).map(|_| ())
    }

    fn finishes(&self) -> bool {
        self.config.get("finish").and_then(Value::as_bool) == Some(true)
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

/// The repository's own issue source, by its place among the backends, or
/// why it has none.
pub type Own = Result<usize, String>;

/// Each source's backend, by everything that makes it what it is, its key
/// too, so a change to the config or a key added to the `env` file takes
/// effect without one source being built twice.
static BUILT: LazyLock<Mutex<HashMap<String, Arc<dyn Backend>>>> = LazyLock::new(Mutex::default);

/// The backend of source `name`: its plugin where it has one, which is how a
/// plugin of your own named `linear` replaces the built-in client.
fn build(
    name: &str,
    source: &config::Source,
    key: Result<String, String>,
) -> Result<Arc<dyn Backend>, String> {
    if let Some(plugin) = plugin::named(Kind::Tickets, &source.plugin) {
        return Ok(Arc::new(Plugged {
            name: name.to_string(),
            plugin,
            config: source.config(),
        }));
    }
    if source.plugin != config::LINEAR {
        return Err(format!(
            "[issues.{name}]: no ticket plugin {}",
            source.plugin
        ));
    }
    let team = source.settings.get("team").and_then(|t| t.as_str());
    let finishes = source.settings.get("finish").and_then(toml::Value::as_bool);
    Ok(Arc::new(issues::Linear::new(
        name,
        key,
        team.map(str::to_string),
        finishes == Some(true),
    )))
}

/// The API key source `name` names in `key_env`, or why it has none.
fn key_of(name: &str, source: &config::Source) -> Result<String, String> {
    let key_env = source.settings.get("key_env").and_then(|k| k.as_str());
    let key_env = key_env.ok_or_else(|| {
        format!("[issues.{name}] needs key_env, the variable holding a Linear API key")
    })?;
    config::key(key_env)
}

fn backend(name: &str, source: &config::Source) -> Result<Arc<dyn Backend>, String> {
    let api_key = key_of(name, source);
    let made_of = format!(
        "{name}\0{}\0{}\0{api_key:?}",
        source.plugin,
        source.config()
    );
    let mut built = BUILT.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(backend) = built.get(&made_of) {
        return Ok(Arc::clone(backend));
    }
    let backend = build(name, source, api_key)?;
    built.insert(made_of, Arc::clone(&backend));
    Ok(backend)
}

/// The issue source of the repository checked out at `checkout`, or what to
/// set to have one. Only that source is built, so a broken table for another
/// one does not stand in the way.
pub fn own(checkout: &Path) -> Result<Arc<dyn Backend>, String> {
    let config = config::load()?;
    let (name, source) = config.source_for(checkout)?;
    backend(name, source)
}

/// The issue source called `name`, or the repository's own for None, which is
/// what a tree picked up before sources were recorded works from.
pub fn named(name: Option<&str>, checkout: &Path) -> Result<Arc<dyn Backend>, String> {
    let Some(name) = name else {
        return own(checkout);
    };
    let config = config::load()?;
    let source = config
        .issues
        .get(name)
        .ok_or_else(|| format!("no issue source {name} in the config"))?;
    backend(name, source)
}

/// Every issue source in the config as a backend for the Issues screen, and
/// the index of the repository's own, or why it has none.
pub fn backends(checkout: &Path) -> Result<(Vec<Arc<dyn Backend>>, Own), String> {
    let config = config::load()?;
    // The backends follow the config's order, so the repository's source
    // is at its place there.
    let own = config
        .source_for(checkout)
        .map(|(name, _)| config.issues.keys().position(|n| n == name).unwrap_or(0));
    let backends = config
        .issues
        .iter()
        .map(|(name, source)| backend(name, source))
        .collect::<Result<_, _>>()?;
    Ok((backends, own))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_fills_the_template_and_drops_what_the_ticket_lacks() {
        let ticket = Issue {
            id: "END-812".into(),
            title: "Fix the login redirect".into(),
            description: Some("Keep return_to through SSO.".into()),
            url: None,
            ..Issue::default()
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

    /// A source on a ticket plugin that knows END-1, which blocks END-2
    /// (blocked by nothing else), END-3 (blocked by END-9, still open) and
    /// END-4 (done already).
    fn linked_source(test: &str) -> Plugged {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("treetop-tickets-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tracker");
        let reply = |links: &str| {
            format!(
                r#"{{\"ok\":true,\"result\":{{\"id\":\"$id\",\"title\":\"t\",\"links\":[{links}]}}}}"#
            )
        };
        let script = format!(
            "#!/bin/sh\nid=$(sed 's/.*\"id\": *\"\\([^\"]*\\)\".*/\\1/')\ncase $id in\n\
             END-1) echo \"{}\" ;;\nEND-2) echo \"{}\" ;;\nEND-3) echo \"{}\" ;;\n\
             *) echo \"{}\" ;;\nesac\n",
            reply(
                r#"{\"id\":\"END-2\",\"kind\":\"blocks\"},{\"id\":\"END-3\",\"kind\":\"blocks\"},{\"id\":\"END-4\",\"kind\":\"blocks\",\"done\":true},{\"id\":\"END-7\",\"kind\":\"related\"}"#
            ),
            reply(r#"{\"id\":\"END-1\",\"kind\":\"blocked by\"}"#),
            reply(
                r#"{\"id\":\"END-1\",\"kind\":\"blocked by\"},{\"id\":\"END-9\",\"kind\":\"blocked by\"}"#
            ),
            r#"{\"ok\":true,\"result\":{\"tickets\":[{\"id\":\"END-1\",\"title\":\"t\",\"state\":\"Todo\"}]}}"#,
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        Plugged {
            name: "test".into(),
            plugin: Plugin::External {
                name: "tracker".into(),
                path,
            },
            config: json!({ "finish": true }),
        }
    }

    fn source(table: &str) -> config::Source {
        toml::from_str(table).unwrap()
    }

    /// ANB-26: where a key comes from, in a process of its own for the sake
    /// of the environment, and with a config directory of its own.
    #[test]
    fn a_key_comes_from_the_shell_else_the_env_file_and_its_lack_says_where_it_looked() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = crate::jobs::tests::config_root("key");
        if !crate::jobs::tests::in_process_of_its_own(
            "tickets::tests::a_key_comes_from_the_shell_else_the_env_file_and_its_lack_says_where_it_looked",
            &root,
        ) {
            return;
        }
        let env = root.join("config/treetop/env");
        let linear = source("key_env = \"TREETOP_TEST_KEY\"");
        let without = backend("work", &linear).unwrap();
        assert_eq!(
            without.list(&Filters::default()).unwrap_err(),
            format!(
                "TREETOP_TEST_KEY is not set, in the environment or in {}; \
                 it should hold a Linear API key",
                env.display()
            )
        );
        std::fs::write(&env, "TREETOP_TEST_KEY=from-the-file\n").unwrap();
        std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            key_of("work", &linear).as_deref(),
            Ok("from-the-file"),
            "a service has only the file"
        );
        let with = backend("work", &linear).unwrap();
        assert!(!Arc::ptr_eq(&without, &with), "a key added takes effect");
        assert!(
            Arc::ptr_eq(&with, &backend("work", &linear).unwrap()),
            "whatever asks shares one client"
        );
        // SAFETY: this process runs only this test, on this thread.
        unsafe { std::env::set_var("TREETOP_TEST_KEY", "from-the-shell") };
        assert_eq!(key_of("work", &linear).as_deref(), Ok("from-the-shell"));
        assert!(key_of("work", &source("")).unwrap_err().contains("key_env"));
    }

    #[test]
    fn a_source_on_a_plugin_nobody_has_says_which_table() {
        let nobody = source("plugin = \"nobody\"");
        let why = build("work", &nobody, Err(String::new())).err().unwrap();
        assert_eq!(why, "[issues.work]: no ticket plugin nobody");
    }

    #[test]
    fn a_source_on_a_plugin_lists_and_reads_but_its_edits_say_they_cannot() {
        let source = linked_source("plugged");
        let listed = source.list(&Filters::default()).unwrap();
        assert_eq!(listed[0].id, "END-1");
        assert_eq!(listed[0].state, "Todo");
        assert_eq!(source.detail("END-2").unwrap().links[0].kind, "blocked by");
        let change = linear::Change::default();
        assert_eq!(
            source.update(&Issue::default(), &change),
            Err(issues::READ_ONLY.to_string())
        );
        assert_eq!(
            source.comment(&Issue::default(), "hello"),
            Err(issues::READ_ONLY.to_string())
        );
        assert_eq!(
            source.create(&linear::NewIssue::default()),
            Err(issues::READ_ONLY.to_string())
        );
    }

    #[test]
    fn a_merge_readies_what_it_alone_blocked() {
        let source = linked_source("ready");
        assert_eq!(source.ready_after("END-1").unwrap(), ["END-2"]);
        assert!(source.finishes());
    }
}
