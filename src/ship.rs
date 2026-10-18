//! Where each tree's work stands after its agent: its pull request, the
//! checks on it and its review, or a validation pipeline it is going
//! through. Ship plugins say so, so treetop knows no forge, CI or gate of its
//! own: GitHub's, over `gh`, is built in, and a repository names the ones it
//! uses under `[repos]`, in order.
//!
//! ```toml
//! [repos."~/endo/app"]
//! ship = ["no-mistakes", "github"]
//! ```
//!
//! For each branch the first plugin with something current to say wins, so a
//! pipeline's gate shows while it runs and the PR's review once it ends.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config;
use crate::plugin::{self, Code, Error, Kind, Plugin};
use crate::pool::Tree;

/// The plugins a repository with no `ship` entry uses.
pub const DEFAULT: [&str; 1] = ["github"];

/// Where a branch's work stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// Stopped at a step that waits on a person.
    Gate,
    /// A run ended without getting the work through, on its current commit.
    Failed,
    Draft,
    /// Checks are still running on the PR.
    Checks,
    CiFailed,
    /// A reviewer asked for changes.
    Changes,
    /// Open and waiting on review.
    Review,
    Approved,
    Merged,
    Closed,
    /// Nothing current to say, such as a pipeline whose run ended.
    Idle,
    /// Under way; also any stage this treetop does not know.
    #[serde(other)]
    Running,
}

impl Stage {
    /// The work waits on you: a gate to answer, or a failure to fix.
    pub fn needs_you(self) -> bool {
        matches!(
            self,
            Stage::Gate | Stage::Failed | Stage::CiFailed | Stage::Changes
        )
    }
}

/// What a ship plugin says about one branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ship {
    pub branch: String,
    pub stage: Stage,
    /// The plugin's own few characters for the PR column, such as `#12 ✗ ci`.
    #[serde(default)]
    pub label: String,
    /// The PR, or the run, to open in the browser.
    #[serde(default)]
    pub url: Option<String>,
    /// The commit the PR or run is on.
    #[serde(default)]
    pub head: Option<String>,
    /// Lines for the detail pane.
    #[serde(default)]
    pub detail: Vec<String>,
    /// The plugin that said it, filled in by treetop.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub plugin: String,
}

impl Ship {
    /// The PR's number, from its label or its URL, such as `#12`.
    pub fn number(&self) -> Option<String> {
        let from_url = self
            .url
            .as_deref()
            .and_then(|url| url.rsplit_once("/pull/"))
            .map(|(_, n)| n.trim_end_matches('/'))
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
        from_url.map(|n| format!("#{n}"))
    }
}

/// A tree a plugin is asked about: where it is and its branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    pub dir: PathBuf,
    pub branch: String,
}

/// The trees worth asking about: held pool trees on a branch.
pub fn asked(trees: &[Tree]) -> Vec<Asked> {
    trees
        .iter()
        .filter(|t| t.is_held() && !t.is_main)
        .filter_map(|t| {
            Some(Asked {
                dir: t.path.clone(),
                branch: t.branch.clone()?,
            })
        })
        .collect()
}

/// The ship plugins of the repository checked out at `checkout`, in its
/// order, with the names no plugin answers to.
pub fn plugins(checkout: &Path) -> (Vec<Plugin>, Vec<String>) {
    let names = config::load()
        .ok()
        .and_then(|c| c.ship_for(checkout))
        .unwrap_or_else(|| DEFAULT.iter().map(|n| n.to_string()).collect());
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for name in names {
        match plugin::named(Kind::Ship, &name) {
            Some(plugin) => found.push(plugin),
            None => missing.push(name),
        }
    }
    (found, missing)
}

/// What one plugin's `status` says, by branch.
fn ask(plugin: &Plugin, checkout: &Path, trees: &[Asked]) -> Result<HashMap<String, Ship>, Error> {
    let reply = plugin.call("status", json!({ "dir": checkout, "trees": trees }))?;
    let ships: Vec<Ship> =
        serde_json::from_value(reply.get("branches").cloned().unwrap_or(json!([])))
            .map_err(|err| Error::new(Code::Failed, format!("unreadable branches: {err}")))?;
    Ok(ships
        .into_iter()
        .map(|mut ship| {
            ship.plugin = plugin.name().to_string();
            (ship.branch.clone(), ship)
        })
        .collect())
}

/// Each plugin's answer, in the plugins' order, the first non-idle answer
/// for a branch winning, else the last idle one.
pub fn combine(answers: Vec<HashMap<String, Ship>>) -> HashMap<String, Ship> {
    let mut combined: HashMap<String, Ship> = HashMap::new();
    for answer in answers {
        for (branch, ship) in answer {
            let is_settled = combined
                .get(&branch)
                .is_some_and(|s| s.stage != Stage::Idle);
            if !is_settled {
                combined.insert(branch, ship);
            }
        }
    }
    combined
}

/// What the ship plugins said: where each tree stands, by its path, and
/// why each plugin that could not say could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    pub ships: HashMap<PathBuf, Ship>,
    pub failures: Vec<String>,
}

/// Where each tree in `trees` stands, asking every plugin at once. Errs
/// only when every plugin did, with the first one's error.
pub fn status(plugins: &[Plugin], checkout: &Path, trees: &[Asked]) -> Result<Answer, Error> {
    if trees.is_empty() || plugins.is_empty() {
        return Ok(Answer::default());
    }
    let replies: Vec<Result<HashMap<String, Ship>, Error>> = thread::scope(|scope| {
        let handles: Vec<_> = plugins
            .iter()
            .map(|plugin| scope.spawn(move || ask(plugin, checkout, trees)))
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(Error::new(Code::Failed, "the plugin call panicked")))
            })
            .collect()
    });
    let mut answers = Vec::new();
    let mut errors = Vec::new();
    for (plugin, reply) in plugins.iter().zip(replies) {
        match reply {
            Ok(answer) => answers.push(answer),
            Err(err) => errors.push((plugin.name(), err)),
        }
    }
    if answers.is_empty() && !errors.is_empty() {
        return Err(errors.swap_remove(0).1);
    }
    let by_branch = combine(answers);
    Ok(Answer {
        ships: trees
            .iter()
            .filter_map(|t| Some((t.dir.clone(), by_branch.get(&t.branch)?.clone())))
            .collect(),
        failures: errors
            .into_iter()
            .map(|(name, err)| format!("{name}: {err}"))
            .collect(),
    })
}

/// The first plugin of `plugins` that answers `op`.
pub fn answering<'a>(plugins: &'a [Plugin], op: &str) -> Option<&'a Plugin> {
    plugins
        .iter()
        .find(|p| p.description().is_ok_and(|d| d.ops.iter().any(|o| o == op)))
}

/// The plugin named `name` among `plugins`.
pub fn by_name<'a>(plugins: &'a [Plugin], name: &str) -> Option<&'a Plugin> {
    plugins.iter().find(|p| p.name() == name)
}

/// What failed on `ship`'s branch in the tree at `dir`, as its plugin tells
/// it, for the tree's agent.
pub fn logs(plugin: &Plugin, dir: &Path, branch: &str) -> Result<String, Error> {
    let reply = plugin.call("logs", json!({ "dir": dir, "branch": branch }))?;
    Ok(reply
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

/// The message that sends `ship`'s failure, `text` as its plugin told it,
/// to the agent working on it: one line saying what it is, then the text,
/// capped at MESSAGE_CAP.
pub fn message(ship: &Ship, text: &str) -> String {
    let pr = ship.number().unwrap_or_else(|| ship.branch.clone());
    let what = match ship.stage {
        Stage::CiFailed => format!("CI failed on {pr}. Fix it and push."),
        Stage::Changes => format!("Review asked for changes on {pr}. Make them and push."),
        Stage::Gate => format!(
            "The {} run on {} stopped at a gate. Here is what it found:",
            ship.plugin, ship.branch
        ),
        _ => format!(
            "The {} run on {} failed. Fix it and push.",
            ship.plugin, ship.branch
        ),
    };
    format!("{what}\n\n{}", cap(text.trim(), MESSAGE_CAP))
}

/// The most of a failure's text sent to an agent.
pub const MESSAGE_CAP: usize = 16 * 1024;

/// The last `lines` lines of `text`.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

/// `text` cut to its last `bytes` bytes at a line, or a character where one
/// line is longer than that.
pub fn cap(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut start = text.len() - bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let cut = &text[start..];
    match cut.find('\n') {
        Some(at) if at + 1 < cut.len() => &cut[at + 1..],
        _ => cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ship(branch: &str, stage: Stage, plugin: &str) -> Ship {
        Ship {
            branch: branch.into(),
            stage,
            label: format!("{stage:?}"),
            url: None,
            head: None,
            detail: Vec::new(),
            plugin: plugin.into(),
        }
    }

    fn answer(ships: &[Ship]) -> HashMap<String, Ship> {
        ships
            .iter()
            .map(|s| (s.branch.clone(), s.clone()))
            .collect()
    }

    #[test]
    fn the_first_plugin_with_something_current_to_say_wins() {
        let pipeline = answer(&[ship("a", Stage::Gate, "nm"), ship("b", Stage::Idle, "nm")]);
        let forge = answer(&[
            ship("a", Stage::Review, "github"),
            ship("b", Stage::CiFailed, "github"),
            ship("c", Stage::Merged, "github"),
        ]);
        let combined = combine(vec![pipeline, forge]);
        assert_eq!(combined["a"].plugin, "nm");
        assert_eq!(combined["b"].stage, Stage::CiFailed);
        assert_eq!(combined["c"].stage, Stage::Merged);
    }

    #[test]
    fn an_unknown_stage_reads_as_running_and_needing_you_is_a_gate_or_a_failure() {
        let read: Ship =
            serde_json::from_value(json!({"branch": "a", "stage": "warming_up"})).unwrap();
        assert_eq!(read.stage, Stage::Running);
        let needing: Vec<Stage> = [
            Stage::Gate,
            Stage::Failed,
            Stage::Draft,
            Stage::Checks,
            Stage::CiFailed,
            Stage::Changes,
            Stage::Review,
            Stage::Approved,
            Stage::Merged,
            Stage::Closed,
            Stage::Idle,
            Stage::Running,
        ]
        .into_iter()
        .filter(|s| s.needs_you())
        .collect();
        assert_eq!(
            needing,
            [Stage::Gate, Stage::Failed, Stage::CiFailed, Stage::Changes]
        );
    }

    #[test]
    fn a_pr_number_comes_from_its_url() {
        let mut s = ship("a", Stage::Review, "github");
        assert_eq!(s.number(), None);
        s.url = Some("https://github.com/o/r/pull/12".into());
        assert_eq!(s.number().as_deref(), Some("#12"));
    }

    #[test]
    fn a_failure_goes_to_its_agent_with_what_it_is_first() {
        let mut s = ship("a/fix", Stage::CiFailed, "github");
        s.url = Some("https://github.com/o/r/pull/7".into());
        assert_eq!(
            message(&s, "Failing checks: test\n"),
            "CI failed on #7. Fix it and push.\n\nFailing checks: test"
        );
        let gate = ship("a/fix", Stage::Gate, "no-mistakes");
        assert!(
            message(&gate, "r1 warning").starts_with(
                "The no-mistakes run on a/fix stopped at a gate. Here is what it found:"
            )
        );
        let long = "x\n".repeat(MESSAGE_CAP);
        assert!(message(&s, &long).len() < MESSAGE_CAP + 64);
    }

    #[test]
    fn a_cap_keeps_the_end_from_a_line_start() {
        assert_eq!(cap("short", 16), "short");
        assert_eq!(cap("first line\nsecond\nthird", 12), "third");
        assert_eq!(cap("ééééé", 3), "é");
        assert_eq!(tail("1\n2\n3\n4", 2), "3\n4");
    }
}
