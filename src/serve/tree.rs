//! One tree as the Tree screen draws it: its card, then what the terminal's
//! detail pane and PR column say about it, and its agent's latest report
//! with the steps that led there.

use serde_json::{Value, json};

use crate::agents::Status;
use crate::app::App;
use crate::pool::Tree;
use crate::reports::{State, Step};

use super::pool::card;

/// Report steps kept on the screen, the newest.
const STEPS: usize = 12;

pub fn tree_json(app: &App, tree: &Tree, now: u64) -> Value {
    let mut ports: Vec<(u16, &str, u32)> = tree
        .processes
        .iter()
        .flat_map(|p| {
            p.ports
                .iter()
                .map(move |&port| (port, p.name.as_str(), p.pid))
        })
        .collect();
    ports.sort_unstable();
    ports.dedup_by_key(|(port, _, _)| *port);
    let says = app.says(tree);
    let report = says.map(|item| {
        let steps = distinct(&item.report.steps);
        let steps: Vec<Value> = steps[steps.len().saturating_sub(STEPS)..]
            .iter()
            .map(|step| {
                json!({
                    "at": step.at,
                    "state": step.state.map(state),
                    "detail": step.detail,
                })
            })
            .collect();
        json!({
            "state": state(item.state),
            "says": item.report.text,
            "steps": steps,
        })
    });
    json!({
        "card": card(app, tree, now),
        "path": tree.path,
        "holder": tree.holder,
        "base": app.base_of(tree).filter(|_| tree.is_held() && !tree.is_main),
        "task": tree.sidecar.as_ref().and_then(|s| s.task.clone()),
        "agents": App::agents_of(tree).iter().map(|a| json!({
            "name": a.name,
            "status": a.status.map(status),
            "has_chat": a.attach.is_some(),
            "active_ago": a.active_at.map(|at| now.saturating_sub(at)),
            "label": a.label,
        })).collect::<Vec<_>>(),
        "ports": ports.iter().map(|(port, name, pid)| json!({
            "port": port,
            "process": name,
            "pid": pid,
            "public": app.shares.get(port).filter(|s| s.is_public).map(|s| &s.address),
        })).collect::<Vec<_>>(),
        "pr": tree.ship.as_ref().map(|s| json!({
            "label": s.label,
            "stage": s.stage,
            "url": s.url,
            "detail": s.detail,
        })),
        // Why no ship plugin could say where its PR stands, when none could.
        "pr_unknown": tree.ship.is_none().then(|| app.ship_error.clone()).flatten(),
        "report": report,
    })
}

/// The steps with each run of the same state and words as one, at its
/// latest: an agent rewrites its status line far more often than it
/// changes it.
fn distinct(steps: &[Step]) -> Vec<&Step> {
    let mut kept: Vec<&Step> = Vec::new();
    for step in steps {
        match kept.last_mut() {
            Some(last) if last.state == step.state && last.detail == step.detail => *last = step,
            _ => kept.push(step),
        }
    }
    kept
}

fn state(state: State) -> &'static str {
    match state {
        State::Blocked => "blocked",
        State::Working => "working",
        State::Done => "done",
        State::Stopped => "stopped",
        State::Failed => "failed",
    }
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Busy => "busy",
        Status::Idle => "idle",
        Status::Waiting => "waiting",
        Status::Cold => "cold",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::Process;
    use crate::serve::pool::tests::{declared, keys, pool};
    use crate::ship::{Ship, Stage};

    #[test]
    fn a_status_line_written_again_is_one_step() {
        let step = |at: i64, state: State, detail: &str| Step {
            at: Some(at),
            state: Some(state),
            detail: detail.into(),
        };
        let steps = [
            step(1, State::Working, "reading"),
            step(2, State::Working, "reading"),
            step(3, State::Working, "testing"),
            step(4, State::Blocked, "testing"),
            step(5, State::Working, "reading"),
        ];
        let kept: Vec<(Option<i64>, &str)> = distinct(&steps)
            .iter()
            .map(|s| (s.at, s.detail.as_str()))
            .collect();
        assert_eq!(
            kept,
            [
                (Some(2), "reading"),
                (Some(3), "testing"),
                (Some(4), "testing"),
                (Some(5), "reading")
            ]
        );
    }

    #[test]
    fn a_tree_has_exactly_the_fields_the_app_reads() {
        let mut app = pool();
        let tree = app.trees.iter_mut().find(|t| t.name == "17").unwrap();
        tree.processes = vec![Process {
            pid: 42,
            name: "vite".into(),
            ports: vec![5173],
        }];
        tree.ship = Some(Box::new(Ship {
            branch: "me/END-123-fix-login".into(),
            stage: Stage::CiFailed,
            label: "#41 ✗ ci".into(),
            url: Some("https://example.com/41".into()),
            head: None,
            detail: vec!["test failed".into()],
            plugin: "github".into(),
        }));
        let mut reports = app.reports.clone();
        reports[0].steps = vec![Step {
            at: Some(100),
            state: Some(State::Working),
            detail: "read the login flow".into(),
        }];
        app.apply_reports(reports);
        let tree = app.trees.iter().find(|t| t.name == "17").unwrap();
        let json = tree_json(&app, tree, 1000);
        assert_eq!(keys(&json), declared("Tree"));
        assert_eq!(keys(&json["agents"][0]), declared("AgentRow"));
        assert_eq!(keys(&json["ports"][0]), declared("Port"));
        let pr: std::collections::BTreeSet<String> =
            declared("Pr").union(&declared("TreePr")).cloned().collect();
        assert_eq!(keys(&json["pr"]), pr);
        assert_eq!(keys(&json["report"]), declared("Report"));
        assert_eq!(keys(&json["report"]["steps"][0]), declared("Step"));
        assert_eq!(json["report"]["state"], "blocked");
        assert_eq!(json["ports"][0]["process"], "vite");
    }
}
