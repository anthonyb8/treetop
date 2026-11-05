//! keel's runs: what a routed task is doing in each tree, as `keel status
//! --json` prints them. keel writes them to files outside the trees and that
//! command only reads those files, so it is cheap enough to poll. Its output
//! is a JSON array of version 1 runs, whose fields are only ever added (see
//! keel's `docs/status.md`); every field is read by name, extra ones are
//! ignored, and a run that does not read is skipped. keel need not be
//! installed: with no `keel` on PATH there are simply no runs.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::child::{self, End};

/// How long `keel status --json` gets.
const LIMIT: Duration = Duration::from_secs(5);

/// One step of a run's playbook.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Step {
    #[serde(default)]
    pub title: String,
    /// `pending`, `doing`, `done` or `skipped`.
    #[serde(default)]
    pub state: String,
}

/// A routed task in a tree, as keel records it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Run {
    /// The tree's absolute path, its git toplevel, which joins the run to
    /// its tree.
    pub tree: PathBuf,
    /// The playbook the router matched, such as `bug-fix`.
    pub playbook: String,
    #[serde(default)]
    pub goal: String,
    /// `active`, `paused`, `done`, `stopped` or `failed`.
    pub state: String,
    #[serde(default)]
    pub steps: Vec<Step>,
    /// The 1-based step being worked on, else the first still open.
    #[serde(default)]
    pub current: Option<usize>,
    /// How many of its decisions are flagged for attention.
    #[serde(default)]
    pub attention: u64,
    /// Active, but its tree had no live process at keel's last sweep.
    #[serde(default)]
    pub stale: bool,
}

impl Run {
    /// Whether keel keeps its tree: an active or paused run is unfinished
    /// work, even with every change committed and pushed.
    pub fn holds_tree(&self) -> bool {
        matches!(self.state.as_str(), "active" | "paused")
    }

    /// The step being worked on out of the playbook's, such as `3/6`; all
    /// done when no step is open.
    pub fn step(&self) -> String {
        let total = self.steps.len();
        match self.current {
            Some(at) => format!("{at}/{total}"),
            None => format!("{total}/{total}"),
        }
    }

    /// The step being worked on's title, if any.
    pub fn step_title(&self) -> Option<&str> {
        let at = self.current?.checked_sub(1)?;
        self.steps
            .get(at)
            .map(|step| step.title.as_str())
            .filter(|title| !title.is_empty())
    }

    /// How the run reads in a few words: its playbook, step and state, then
    /// what needs looking at.
    pub fn summary(&self) -> String {
        let mut words = vec![format!("{} {}", self.playbook, self.step())];
        if self.state != "active" {
            words.push(self.state.clone());
        }
        if self.attention > 0 {
            words.push(format!("{} need attention", self.attention));
        }
        if self.stale {
            words.push("stale".into());
        }
        words.join(" · ")
    }
}

/// The run that holds the tree at `path`, if any: active or paused, so
/// keel's own sweep and hooks keep the tree.
pub fn holding<'a>(runs: &'a HashMap<PathBuf, Run>, path: &std::path::Path) -> Option<&'a Run> {
    runs.get(path).filter(|run| run.holds_tree())
}

/// Why a tree whose run holds it is not returned by an agent.
pub fn refusal(name: &str, run: &Run) -> String {
    format!(
        "tree {name} has a keel run ({}) and keel keeps it until the run stops; \
         stop it with `keel run stop`, or only the user can return it, from treetop",
        run.summary()
    )
}

/// The runs in `text`, keel's output, by tree. A run that does not read is
/// left out rather than failing the rest.
pub fn parse(text: &str) -> HashMap<PathBuf, Run> {
    let Ok(Value::Array(runs)) = serde_json::from_str::<Value>(text) else {
        return HashMap::new();
    };
    runs.into_iter()
        .filter_map(|run| serde_json::from_value::<Run>(run).ok())
        .map(|run| (run.tree.clone(), run))
        .collect()
}

/// Every run keel lists, by tree; None when keel is not installed, so a
/// setup without it shows nothing about runs. A keel that fails or answers
/// in a shape treetop cannot read lists none.
pub fn status() -> Option<HashMap<PathBuf, Run>> {
    let mut command = Command::new("keel");
    command.args(["status", "--json"]);
    let ran = match child::run(&mut command, None, LIMIT) {
        Ok(ran) => ran,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return None,
        Err(_) => return Some(HashMap::new()),
    };
    match ran.end {
        End::Exited(status) if status.success() => {
            Some(parse(&String::from_utf8_lossy(&ran.stdout)))
        }
        _ => Some(HashMap::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two runs as keel's version 1 prints them, one with fields added since.
    const STATUS: &str = r#"[
      {"version":1,"tree":"/p/3/app","branch":"fix/login","playbook":"bug-fix",
       "goal":"users get two notifications after a retry","state":"active","reason":"",
       "steps":[{"title":"Reproduce it","state":"done","reason":null},
                {"title":"Narrow the cause","state":"doing","reason":null},
                {"title":"Fix it at the root","state":"pending","reason":null}],
       "current":2,"started_at":"2026-11-05T10:00:00Z","updated_at":"2026-11-05T10:20:00Z",
       "stale":false,"attention":2,"done_check":null,"contract_hash":null,"verifier":null,
       "gate":null,"added_later":{"x":1}},
      {"version":1,"tree":"/p/4/app","playbook":"investigation","state":"paused",
       "steps":[{"title":"Read","state":"done"}],"current":null,"stale":true},
      {"version":1,"tree":"/p/5/app"}
    ]"#;

    #[test]
    fn reads_each_run_by_its_tree_and_skips_one_that_does_not_read() {
        let runs = parse(STATUS);
        assert_eq!(runs.len(), 2);
        let fixing = &runs[&PathBuf::from("/p/3/app")];
        assert_eq!(fixing.step(), "2/3");
        assert_eq!(fixing.step_title(), Some("Narrow the cause"));
        assert_eq!(fixing.summary(), "bug-fix 2/3 · 2 need attention");
        assert!(fixing.holds_tree());
        let paused = &runs[&PathBuf::from("/p/4/app")];
        assert_eq!(paused.summary(), "investigation 1/1 · paused · stale");
        assert!(paused.holds_tree());
    }

    #[test]
    fn an_ended_run_does_not_hold_its_tree() {
        let mut runs = parse(STATUS);
        let mut run = runs.remove(&PathBuf::from("/p/3/app")).unwrap();
        for state in ["done", "stopped", "failed"] {
            run.state = state.into();
            assert!(!run.holds_tree(), "{state}");
        }
    }

    #[test]
    fn output_that_is_not_a_list_of_runs_lists_none() {
        assert!(parse("").is_empty());
        assert!(parse("{\"tree\":\"/p\"}").is_empty());
        assert!(parse("not json").is_empty());
    }
}
