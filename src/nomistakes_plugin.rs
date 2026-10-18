//! The no-mistakes ship plugin, built in: `treetop plugin no-mistakes` serves
//! it, over `no-mistakes axi`, the gate's interface for programs. A
//! repository uses it only when its `ship` names it.
//!
//! It reads a branch's pipeline run from `axi status` in the branch's tree,
//! whoever started it: treetop, the agent, or a `git push` through the gate.
//! While the run is under way it is the tree's stage, a gate waiting on you
//! included; once the run ends it has nothing current to say, so the PR's own
//! stage shows, except for a failed run on the commit the tree is still on.
//!
//! `axi` prints TOON: `key: value` lines nested by two spaces, and tables
//! as a `name[N]{a,b}:` header over one comma-separated row each.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::child::{self, End};
use crate::plugin::{Code, Error, PROTOCOL};
use crate::pool;
use crate::ship::{self, Asked, Ship, Stage};

/// How long one `axi status` or `axi logs` gets.
const AXI_LIMIT: Duration = Duration::from_secs(8);

/// How long `run` waits for the run it started to show.
const RUN_SHOWS: Duration = Duration::from_secs(20);

/// The lines of a step's log an agent gets.
const LOG_LINES: usize = 200;

/// Run statuses after which the run does nothing more.
const ENDED: [&str; 10] = [
    "completed",
    "passed",
    "checks-passed",
    "passed-with-override",
    "passed-with-skips",
    "failed",
    "cancelled",
    "canceled",
    "superseded",
    "aborted",
];

/// A pipeline run as `axi status` shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Run {
    id: String,
    branch: String,
    status: String,
    head: String,
    /// `awaiting_agent` is set: the run is parked at a gate.
    is_parked: bool,
    /// Each step and its status, in order.
    steps: Vec<(String, String)>,
}

/// What `axi status` says: the branch's run, the gate it waits at, or the
/// error it gave instead.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Status {
    run: Option<Run>,
    gate: Option<String>,
    error: Option<String>,
}

/// `value` without the quotes TOON puts round some strings.
fn unquote(value: &str) -> String {
    let value = value.trim();
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
        .to_string()
}

/// Reads the parts of `axi status` this plugin uses, ignoring the rest.
fn parse(text: &str) -> Status {
    let mut status = Status::default();
    let mut block = "";
    // The `step` and `status` columns of the steps table being read.
    let mut steps_columns: Option<(usize, usize)> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start().len();
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if indent == 0 {
            steps_columns = None;
            let (key, value) = line.split_once(':').unwrap_or((line, ""));
            block = match key {
                "run" => {
                    status.run = Some(Run::default());
                    "run"
                }
                "gate" => {
                    status.gate = Some(unquote(value));
                    ""
                }
                "error" => {
                    status.error = Some(unquote(value));
                    ""
                }
                _ => "",
            };
            continue;
        }
        let Some(run) = status.run.as_mut().filter(|_| block == "run") else {
            continue;
        };
        if indent == 2 {
            steps_columns = None;
            if let Some(header) = line.strip_prefix("steps[") {
                let columns: Vec<&str> = header
                    .split_once('{')
                    .and_then(|(_, rest)| rest.split_once('}'))
                    .map(|(columns, _)| columns.split(',').collect())
                    .unwrap_or_default();
                let at = |name: &str| columns.iter().position(|c| *c == name);
                steps_columns = at("step").zip(at("status"));
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = unquote(value);
            match key {
                "id" => run.id = value,
                "branch" => run.branch = value,
                "status" => run.status = value,
                "head_sha" => run.head = value,
                "awaiting_agent" => run.is_parked = !value.is_empty(),
                _ => {}
            }
        } else if let Some((step, state)) = steps_columns {
            let cells: Vec<&str> = line.split(',').collect();
            if let (Some(name), Some(value)) = (cells.get(step), cells.get(state)) {
                run.steps.push((name.to_string(), value.to_string()));
            }
        }
    }
    status
}

impl Run {
    fn has_ended(&self) -> bool {
        ENDED.contains(&self.status.as_str())
    }

    /// The step the run is at: the one running or stopped, else the first
    /// still to come.
    fn current_step(&self) -> Option<&str> {
        let at = |wanted: fn(&str) -> bool| {
            self.steps
                .iter()
                .find(|(_, status)| wanted(status))
                .map(|(step, _)| step.as_str())
        };
        at(|s| !matches!(s, "completed" | "skipped" | "pending")).or_else(|| at(|s| s == "pending"))
    }

    fn failed_step(&self) -> Option<&str> {
        self.steps
            .iter()
            .find(|(_, status)| status == "failed")
            .map(|(step, _)| step.as_str())
    }

    /// Each step with a mark for how it went, for the detail pane.
    fn steps_line(&self) -> String {
        let marked: Vec<String> = self
            .steps
            .iter()
            .map(|(step, status)| {
                let mark = match status.as_str() {
                    "completed" => "✓",
                    "failed" => "✗",
                    "skipped" => "-",
                    "pending" => "·",
                    _ => "…",
                };
                format!("{step} {mark}")
            })
            .collect();
        format!("steps {}", marked.join("  "))
    }
}

/// Where the branch in the tree stands, as its run says, given the commit
/// the tree is on now. None when no run of the branch says anything.
fn ship_of(status: &Status, branch: &str, head: Option<&str>) -> Option<Ship> {
    let run = status.run.as_ref().filter(|r| r.branch == branch)?;
    let step = status
        .gate
        .as_deref()
        .or_else(|| run.current_step())
        .unwrap_or("run")
        .to_string();
    let is_current = head.is_some_and(|h| h == run.head);
    let (stage, label) = if !run.has_ended() {
        if status.gate.is_some() || run.is_parked {
            (Stage::Gate, format!("nm {step} ⏸"))
        } else {
            (Stage::Running, format!("nm {step} …"))
        }
    } else if run.status == "failed" && is_current {
        let failed = run.failed_step().unwrap_or("run");
        (Stage::Failed, format!("nm ✗ {failed}"))
    } else {
        (Stage::Idle, format!("nm {}", run.status))
    };
    Some(Ship {
        branch: branch.to_string(),
        stage,
        label,
        url: None,
        head: Some(run.head.clone()).filter(|h| !h.is_empty()),
        detail: vec![format!("run {} {}", run.id, run.status), run.steps_line()],
        plugin: String::new(),
    })
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(Code::Failed, message)
}

/// Runs `no-mistakes axi` with `args` in `dir` and returns what it printed,
/// whatever its exit: a failed run's status exits 1 and still says it all.
fn axi(dir: &Path, args: &[&str]) -> Result<String, Error> {
    let mut command = Command::new("no-mistakes");
    command.arg("axi").args(args).current_dir(dir);
    let ran = child::run(&mut command, None, AXI_LIMIT).map_err(|err| {
        Error::new(
            Code::Unavailable,
            if err.kind() == std::io::ErrorKind::NotFound {
                "no-mistakes is not installed".to_string()
            } else {
                format!("no-mistakes: {err}")
            },
        )
    })?;
    match ran.end {
        End::Exited(_) => Ok(String::from_utf8_lossy(&ran.stdout).into_owned()),
        End::TimedOut | End::HeldOpen(_) => Err(Error::new(
            Code::Unavailable,
            format!("no-mistakes axi {} took over {AXI_LIMIT:?}", args.join(" ")),
        )),
    }
}

/// The branch's run in the tree at `dir`, as `axi status` there tells it.
fn read(dir: &Path) -> Result<Status, Error> {
    axi(dir, &["status"]).map(|text| parse(&text))
}

fn head(dir: &Path) -> Option<String> {
    pool::git(dir, &["rev-parse", "HEAD"]).map(|h| h.trim().to_string())
}

fn status(args: &Value) -> Result<Value, Error> {
    let trees: Vec<Asked> = serde_json::from_value(args.get("trees").cloned().unwrap_or(json!([])))
        .map_err(|err| failed(format!("invalid trees: {err}")))?;
    let answers: Vec<Result<Option<Ship>, Error>> = thread::scope(|scope| {
        let handles: Vec<_> = trees
            .iter()
            .map(|tree| {
                scope.spawn(move || {
                    let status = read(&tree.dir)?;
                    Ok(ship_of(&status, &tree.branch, head(&tree.dir).as_deref()))
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(failed("reading a run panicked")))
            })
            .collect()
    });
    // One tree it cannot read is no reason to blank the rest; every tree is.
    let mut ships = Vec::new();
    let mut first_error = None;
    for answer in answers {
        match answer {
            Ok(ship) => ships.extend(ship),
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    match first_error {
        Some(err) if ships.is_empty() && !trees.is_empty() => Err(err),
        _ => Ok(json!({ "branches": ships })),
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| failed(format!("no {key} given")))
}

/// The run of `branch` in the tree at `dir`, or why there is none.
fn run_of(dir: &Path, branch: &str) -> Result<Run, Error> {
    let status = read(dir)?;
    if let Some(error) = status.error {
        return Err(Error::new(Code::NotFound, error));
    }
    status
        .run
        .filter(|r| r.branch == branch)
        .ok_or_else(|| Error::new(Code::NotFound, format!("no run of {branch}")))
}

fn logs(args: &Value) -> Result<Value, Error> {
    let dir = Path::new(required(args, "dir")?);
    let run = run_of(dir, required(args, "branch")?)?;
    let step = run
        .failed_step()
        .or_else(|| run.current_step())
        .ok_or_else(|| Error::new(Code::NotFound, "the run has no step to read"))?;
    let log = axi(dir, &["logs", "--step", step, "--full"])?;
    Ok(json!({
        "text": format!(
            "The {step} step of no-mistakes run {}, last {LOG_LINES} lines:\n{}",
            run.id,
            ship::tail(&log, LOG_LINES)
        )
    }))
}

fn attach(args: &Value) -> Result<Value, Error> {
    let dir = Path::new(required(args, "dir")?);
    let run = run_of(dir, required(args, "branch")?)?;
    Ok(json!({ "argv": ["no-mistakes", "attach", "--run", run.id] }))
}

/// Starts `axi run` in the tree, detached, so the run outlives this call,
/// and returns once `axi status` shows a run of the tree's commit under way.
fn run(args: &Value) -> Result<Value, Error> {
    let dir = Path::new(required(args, "dir")?);
    let branch = required(args, "branch")?;
    let intent = required(args, "intent")?;
    let commit = head(dir).ok_or_else(|| failed("the tree has no commit to run"))?;
    let mut child = Command::new("no-mistakes")
        .args(["axi", "run", "--intent", intent])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|err| Error::new(Code::Unavailable, format!("no-mistakes: {err}")))?;
    let deadline = Instant::now() + RUN_SHOWS;
    loop {
        if let Ok(Some(exit)) = child.try_wait()
            && !exit.success()
        {
            return Err(failed(format!(
                "no-mistakes axi run exited with {exit}; run it in the tree to see why"
            )));
        }
        let shown = read(dir)
            .ok()
            .and_then(|s| s.run)
            .filter(|r| r.branch == branch && r.head == commit && !r.has_ended());
        if let Some(run) = shown {
            // Reaped once it ends, which is after this call has returned.
            thread::spawn(move || child.wait());
            return Ok(json!({ "started": true, "id": run.id }));
        }
        if Instant::now() >= deadline {
            thread::spawn(move || child.wait());
            return Err(Error::new(
                Code::Unavailable,
                format!("no run of {branch} showed within {RUN_SHOWS:?}"),
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// Answers one request as the no-mistakes plugin.
pub fn op(op: &str, args: &Value) -> Result<Value, Error> {
    match op {
        "describe" => Ok(json!({
            "protocol": PROTOCOL,
            "name": "no-mistakes",
            "kind": "ship",
            "ops": ["status", "logs", "run", "attach"],
        })),
        "status" => status(args),
        "logs" => logs(args),
        "run" => run(args),
        "attach" => attach(args),
        _ => Err(Error::new(
            Code::Unsupported,
            format!("no-mistakes does not answer {op}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `axi status` of a run cancelled at its review, as it printed it.
    const CANCELLED: &str = r#"run:
  id: "01M4XVQJKVZF7A71VZWGAER38Q"
  branch: development
  status: cancelled
  head: 4a7e0cdf
  head_sha: 4a7e0cdfc98949f0233242950ec63844db8f5269
  findings: none
  steps[9]{step,status,findings,duration_ms}:
    intent,completed,0,42
    rebase,skipped,0,0
    review,failed,0,215694
    test,pending,0,0
    document,pending,0,0
    lint,pending,0,0
    push,pending,0,0
    pr,pending,0,0
    ci,pending,0,0
branch_sync:
  state: user_owned
  local:
    branch: development
    head: 1cfceb8096a0ea33aee849da26087e54deb0f6ff
  pipeline:
    run: "01M4XVQJKVZF7A71VZWGAER38Q"
    status: cancelled
outcome: cancelled
error: "cancelled: aborted by user"
"#;

    const HEAD: &str = "4a7e0cdfc98949f0233242950ec63844db8f5269";

    fn with(status: &str, extra: &str) -> String {
        CANCELLED
            .replace(
                "  status: cancelled\n",
                &format!("  status: {status}\n{extra}"),
            )
            .replace("review,failed", "review,running")
    }

    #[test]
    fn reads_the_run_its_steps_and_ignores_the_rest() {
        let status = parse(CANCELLED);
        let run = status.run.unwrap();
        assert_eq!(run.id, "01M4XVQJKVZF7A71VZWGAER38Q");
        assert_eq!(run.branch, "development");
        assert_eq!(run.status, "cancelled");
        assert_eq!(run.head, HEAD);
        assert_eq!(run.steps.len(), 9);
        assert_eq!(run.steps[2], ("review".to_string(), "failed".to_string()));
        assert_eq!(status.error.as_deref(), Some("cancelled: aborted by user"));
    }

    #[test]
    fn a_running_step_shows_and_a_parked_one_is_a_gate() {
        let running = parse(&with("running", ""));
        let ship = ship_of(&running, "development", Some(HEAD)).unwrap();
        assert_eq!(
            (ship.stage, ship.label.as_str()),
            (Stage::Running, "nm review …")
        );
        let parked = parse(&format!(
            "{}gate: review\n",
            with("running", "  awaiting_agent: parked 2m\n")
        ));
        let ship = ship_of(&parked, "development", Some(HEAD)).unwrap();
        assert_eq!(
            (ship.stage, ship.label.as_str()),
            (Stage::Gate, "nm review ⏸")
        );
        assert!(ship.detail[1].starts_with("steps intent ✓  rebase -  review …  test ·"));
    }

    #[test]
    fn an_ended_run_says_nothing_current_unless_it_failed_on_this_commit() {
        let cancelled = ship_of(&parse(CANCELLED), "development", Some(HEAD)).unwrap();
        assert_eq!(cancelled.stage, Stage::Idle);
        let failed = parse(&CANCELLED.replace("  status: cancelled\n", "  status: failed\n"));
        let ship = ship_of(&failed, "development", Some(HEAD)).unwrap();
        assert_eq!(
            (ship.stage, ship.label.as_str()),
            (Stage::Failed, "nm ✗ review")
        );
        let moved_on = ship_of(&failed, "development", Some("1cfceb80")).unwrap();
        assert_eq!(moved_on.stage, Stage::Idle, "a commit since makes it stale");
        assert_eq!(ship_of(&failed, "other", Some(HEAD)), None);
    }

    #[test]
    fn an_uninitialised_repository_has_no_run() {
        let status = parse(
            "error: repo not initialized (run 'no-mistakes init' first)\nhelp[1]: Run `no-mistakes init`\n",
        );
        assert_eq!(status.run, None);
        assert!(status.error.unwrap().starts_with("repo not initialized"));
    }

    #[test]
    fn describes_itself() {
        let described = op("describe", &json!({})).unwrap();
        assert_eq!(described["kind"], "ship");
        assert_eq!(described["ops"], json!(["status", "logs", "run", "attach"]));
    }
}
