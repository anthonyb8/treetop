//! What background agents report back. Claude Code keeps every background
//! session's progress under `<config>/jobs/<id>/`, the id being the short one
//! `claude --bg` prints, so treetop reads what is already written and needs
//! nothing from the agents. `state.json` holds its `state` (working, blocked,
//! done or stopped), a one-line `detail`, the question in `needs` while
//! blocked, the answer in `output.result` once done, and where it works
//! (`worktreePath`, else `cwd`); `timeline.jsonl` gains a line per step, so
//! its time says when the job last moved.
//!
//! treetop rings the bell when a job comes to need you or finishes, and
//! `treetop status` hands each job's latest word to the coordinator.
//!
//! These files are internal and undocumented, as the session files are, so
//! every field is read by name and anything unexpected leaves a job out
//! rather than failing.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::Deserialize;
use serde_json::Value;

/// How long a finished or stopped job is still reported. One still working
/// or waiting on you is reported however old it is.
const RECENT: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum State {
    /// Waiting on you: a question, a decision, a permission.
    Blocked,
    Working,
    Done,
    /// Stopped by someone; `claude attach` opens it again.
    Stopped,
}

impl State {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "blocked" => Some(State::Blocked),
            "working" => Some(State::Working),
            "done" => Some(State::Done),
            "stopped" => Some(State::Stopped),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            State::Blocked => "needs you",
            State::Working => "working",
            State::Done => "done",
            State::Stopped => "stopped",
        }
    }

    /// Whether the job has finished its turn, so its report is something to
    /// read rather than something still moving.
    pub fn is_settled(self) -> bool {
        matches!(self, State::Done | State::Stopped)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// The job's id, which `claude attach` takes.
    pub job: String,
    pub name: String,
    pub state: State,
    /// What it has to say: the question while blocked, the answer once done,
    /// else what it is doing now.
    pub text: String,
    /// Where it works: the tree it took, else where it was started.
    pub dir: Option<PathBuf>,
    /// When it last wrote.
    pub updated: SystemTime,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Raw {
    state: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    detail: Option<String>,
    #[serde(default)]
    needs: Option<Value>,
    #[serde(default)]
    output: Option<Value>,
    #[serde(default)]
    worktree_path: Option<PathBuf>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

fn non_empty(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// The job as a report, from the text of its `state.json`.
fn report(job: &str, state_json: &str, updated: SystemTime) -> Option<Report> {
    let raw: Raw = serde_json::from_str(state_json).ok()?;
    let state = State::parse(&raw.state)?;
    let detail = non_empty(raw.detail.as_deref());
    let said = match state {
        State::Blocked => non_empty(raw.needs.as_ref().and_then(Value::as_str)),
        State::Done => non_empty(
            raw.output
                .as_ref()
                .and_then(|o| o.get("result"))
                .and_then(Value::as_str),
        ),
        State::Working | State::Stopped => None,
    };
    Some(Report {
        job: job.to_string(),
        name: non_empty(raw.name.as_deref()).unwrap_or_else(|| job.to_string()),
        state,
        text: said.or(detail).unwrap_or_default(),
        dir: raw.worktree_path.or(raw.cwd),
        updated,
    })
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Reads the jobs again only where their files have changed since the last
/// read, since most sit still.
#[derive(Default)]
pub struct Reader {
    known: HashMap<String, (SystemTime, Option<Report>)>,
}

impl Reader {
    /// Every job worth reporting: working or waiting on you, or settled within
    /// RECENT.
    pub fn read(&mut self) -> Vec<Report> {
        let Some(jobs) = crate::agents::config_dir().map(|c| c.join("jobs")) else {
            return Vec::new();
        };
        let Ok(entries) = fs::read_dir(&jobs) else {
            return Vec::new();
        };
        let now = SystemTime::now();
        let mut seen = HashMap::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            let Some(job) = dir.file_name().and_then(|n| n.to_str()).map(str::to_string) else {
                continue;
            };
            let (state_file, timeline_file) = (dir.join("state.json"), dir.join("timeline.jsonl"));
            let Some(updated) = [modified(&state_file), modified(&timeline_file)]
                .into_iter()
                .flatten()
                .max()
            else {
                continue;
            };
            let report = match self.known.remove(&job) {
                Some((at, report)) if at == updated => report,
                _ => fs::read_to_string(&state_file)
                    .ok()
                    .and_then(|state| report(&job, &state, updated)),
            };
            seen.insert(job, (updated, report));
        }
        self.known = seen;
        self.known
            .values()
            .filter_map(|(_, report)| report.clone())
            .filter(|r| {
                !r.state.is_settled()
                    || now.duration_since(r.updated).is_ok_and(|age| age <= RECENT)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a job's state as Claude Code writes it, trimmed.
    const BLOCKED: &str = r#"{"state":"blocked","detail":"asking about the branch",
      "name":"end-759-pickup","needs":"Retry the dead jobs now or after deploy?",
      "output":null,"worktreePath":"/p/10/app","cwd":"/home/me/app","tokens":12}"#;

    fn read(state: &str) -> Report {
        report("48af0855", state, SystemTime::UNIX_EPOCH).unwrap()
    }

    #[test]
    fn a_blocked_job_says_its_question_and_works_in_its_tree() {
        let report = read(BLOCKED);
        assert_eq!(report.state, State::Blocked);
        assert_eq!(report.name, "end-759-pickup");
        assert_eq!(report.text, "Retry the dead jobs now or after deploy?");
        assert_eq!(report.dir, Some(PathBuf::from("/p/10/app")));
    }

    #[test]
    fn a_done_job_says_its_result_else_its_detail() {
        let done = BLOCKED
            .replace(r#""state":"blocked""#, r#""state":"done""#)
            .replace(r#""output":null"#, r#""output":{"result":"Tests green."}"#);
        assert_eq!(read(&done).text, "Tests green.");
        let bare = BLOCKED.replace(r#""state":"blocked""#, r#""state":"done""#);
        assert_eq!(read(&bare).text, "asking about the branch");
    }

    #[test]
    fn a_job_with_no_tree_works_where_it_started_and_an_unknown_state_is_left_out() {
        let in_place = BLOCKED.replace(r#""worktreePath":"/p/10/app","#, "");
        assert_eq!(read(&in_place).dir, Some(PathBuf::from("/home/me/app")));
        let odd = BLOCKED.replace(r#""state":"blocked""#, r#""state":"pondering""#);
        assert_eq!(report("x", &odd, SystemTime::UNIX_EPOCH), None);
    }
}
