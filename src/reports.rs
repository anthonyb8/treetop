//! What background agents report back. Claude Code keeps every background
//! session's progress under `<config>/jobs/<id>/`, the id being the short one
//! `claude --bg` prints, so the inbox reads what is already written and needs
//! nothing from the agents:
//!
//! - `state.json`: its `state` (working, blocked, done or stopped), a one-line
//!   `detail`, the question in `needs` while blocked, the answer in
//!   `output.result` once done, and where it works (`worktreePath`, else `cwd`).
//! - `timeline.jsonl`: one `{at, state, detail}` line per step.
//!
//! `treetop status` hands each job's latest word to the coordinator too.
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

use crate::pool::Tree;

/// How long a finished or stopped job stays in the inbox. One still working
/// or waiting on you stays however old it is.
const RECENT: Duration = Duration::from_secs(24 * 60 * 60);

/// Timeline steps kept per job, the newest.
const STEPS: usize = 30;

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
pub struct Step {
    /// Seconds since the epoch, None when the stamp cannot be read.
    pub at: Option<i64>,
    pub state: Option<State>,
    pub detail: String,
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
    /// When it last wrote, which orders the inbox and marks what is read.
    pub updated: SystemTime,
    /// Its latest steps, oldest first.
    pub steps: Vec<Step>,
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

#[derive(Deserialize)]
struct RawStep {
    #[serde(default)]
    at: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    detail: Option<String>,
}

fn non_empty(text: Option<&str>) -> Option<String> {
    text.map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// The job as a report, from the text of its two files.
fn report(job: &str, state_json: &str, timeline: &str, updated: SystemTime) -> Option<Report> {
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
    let lines: Vec<&str> = timeline.lines().filter(|l| !l.trim().is_empty()).collect();
    let steps = lines[lines.len().saturating_sub(STEPS)..]
        .iter()
        .filter_map(|line| serde_json::from_str::<RawStep>(line).ok())
        .map(|step| Step {
            at: step.at.as_deref().and_then(unix),
            state: step.state.as_deref().and_then(State::parse),
            detail: step.detail.unwrap_or_default(),
        })
        .collect();
    Some(Report {
        job: job.to_string(),
        name: non_empty(raw.name.as_deref()).unwrap_or_else(|| job.to_string()),
        state,
        text: said.or(detail).unwrap_or_default(),
        dir: raw.worktree_path.or(raw.cwd),
        updated,
        steps,
    })
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// The tree a report's job works in: the one whose sidecar names the job,
/// or where its agent runs now. A job is never placed by its directory
/// alone, since a tree that has been returned and handed out again would
/// show the last agent's report.
pub fn tree_of<'a>(report: &Report, trees: &'a [Tree]) -> Option<&'a Tree> {
    let job = Some(report.job.as_str());
    trees.iter().find(|tree| {
        tree.sidecar.as_ref().and_then(|s| s.job.as_deref()) == job
            || tree.agents.iter().any(|a| a.job.as_deref() == job)
    })
}

/// Reads the jobs again only where their files have changed since the last
/// read, since most sit still.
#[derive(Default)]
pub struct Reader {
    known: HashMap<String, (SystemTime, Option<Report>)>,
}

impl Reader {
    /// Every job worth showing: working or waiting on you, or settled within
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
                _ => fs::read_to_string(&state_file).ok().and_then(|state| {
                    let timeline = fs::read_to_string(&timeline_file).unwrap_or_default();
                    report(&job, &state, &timeline, updated)
                }),
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

/// Seconds since the epoch of an RFC 3339 stamp in UTC, as Claude Code writes
/// them (`2026-10-14T00:28:12.336Z`). Anything else is None.
fn unix(stamp: &str) -> Option<i64> {
    let stamp = stamp.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let mut date = date.splitn(3, '-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let time = time.split('.').next()?;
    let mut time = time.splitn(3, ':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
    // Days from the civil date, after Howard Hinnant's `days_from_civil`.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// `HH:MM` in local time.
pub fn clock(secs: i64) -> String {
    let time = secs as libc::time_t;
    // SAFETY: localtime_r reads `time` and fills the zeroed `tm` we own.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&time, &mut tm) }.is_null() {
        return "--:--".into();
    }
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}

/// How long ago, as short as it can be said: `now`, `4m`, `3h`, `2d`.
pub fn age(since: SystemTime) -> String {
    let secs = SystemTime::now()
        .duration_since(since)
        .map_or(0, |age| age.as_secs());
    match secs {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        _ => format!("{}d", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of a job's state as Claude Code writes it, trimmed.
    const BLOCKED: &str = r#"{"state":"blocked","detail":"asking about the branch",
      "name":"end-759-pickup","needs":"Retry the dead jobs now or after deploy?",
      "output":null,"worktreePath":"/p/10/app","cwd":"/home/me/app","tokens":12}"#;

    const TIMELINE: &str = concat!(
        r#"{"at":"2026-10-14T00:26:26.595Z","state":"working","detail":"fixing a query","text":""}"#,
        "\n",
        r#"{"at":"2026-10-14T00:28:12.336Z","state":"blocked","detail":"asking","text":"x"}"#,
        "\n",
        "{not json\n",
    );

    fn read(state: &str) -> Report {
        report("48af0855", state, TIMELINE, SystemTime::UNIX_EPOCH).unwrap()
    }

    #[test]
    fn a_blocked_job_says_its_question_and_works_in_its_tree() {
        let report = read(BLOCKED);
        assert_eq!(report.state, State::Blocked);
        assert_eq!(report.name, "end-759-pickup");
        assert_eq!(report.text, "Retry the dead jobs now or after deploy?");
        assert_eq!(report.dir, Some(PathBuf::from("/p/10/app")));
        assert_eq!(report.steps.len(), 2, "a line that is not json is skipped");
        assert_eq!(report.steps[1].state, Some(State::Blocked));
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
        assert_eq!(report("x", &odd, "", SystemTime::UNIX_EPOCH), None);
    }

    #[test]
    fn reads_utc_stamps() {
        assert_eq!(unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(unix("2026-10-14T00:28:12.336Z"), Some(1_791_937_692));
        assert_eq!(unix("2000-03-01T00:00:00Z"), Some(951_868_800));
        assert_eq!(unix("2026-10-14 00:28"), None);
    }
}
