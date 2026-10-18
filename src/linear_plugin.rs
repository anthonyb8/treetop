//! The Linear ticket plugin, built in: `treetop plugin linear` serves it, on
//! the `linear` crate's client.
//!
//! Its config is the issue source's table from `config.toml`:
//!
//! ```toml
//! [issues.work]
//! plugin = "linear"
//! key_env = "LINEAR_API_KEY_WORK"   # the variable holding a personal API key
//! team = "END"                      # the team whose issues `list` shows
//! ```

use linear::{Client, Detail, ErrorKind, Filter, Issue};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::plugin::{Code, Error, PROTOCOL};
use crate::tickets::{Filters, Link, Ticket};

#[derive(Debug, Deserialize)]
struct Settings {
    key_env: String,
    #[serde(default)]
    team: Option<String>,
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(Code::Failed, message)
}

/// A client's error as the plugin contract says it.
fn contract(err: linear::Error) -> Error {
    let code = match err.kind {
        ErrorKind::Auth => Code::Auth,
        ErrorKind::NotFound => Code::NotFound,
        ErrorKind::Unavailable => Code::Unavailable,
        ErrorKind::Failed => Code::Failed,
    };
    Error::new(code, err.message)
}

fn ticket(issue: Issue) -> Ticket {
    Ticket {
        id: issue.id,
        title: issue.title,
        state: issue.state,
        labels: issue.labels,
        assignee: issue.assignee,
        project: issue.project,
        cycle: issue.cycle,
        url: issue.url,
        description: issue.description,
        branch: issue.branch,
        links: Vec::new(),
    }
}

/// A ticket with the tickets it blocks, is blocked by and relates to.
fn linked(detail: Detail) -> Ticket {
    let links = detail
        .links
        .iter()
        .filter(|l| matches!(l.kind.as_str(), "blocks" | "blocked by" | "related"))
        .map(|l| Link {
            id: l.id.clone(),
            kind: l.kind.clone(),
            done: l.is_done,
        })
        .collect();
    Ticket {
        links,
        ..ticket(detail.issue)
    }
}

fn filter(filters: Filters) -> Filter {
    Filter {
        mine: filters.mine,
        assignee: filters.assignee,
        label: filters.label,
        state: filters.state,
        project: filters.project,
        cycle: filters.cycle,
        query: filters.query,
    }
}

fn list(client: &Client, settings: &Settings, args: &Value) -> Result<Value, Error> {
    let filters: Filters = match args.get("filters").filter(|f| !f.is_null()) {
        Some(filters) => serde_json::from_value(filters.clone())
            .map_err(|err| failed(format!("invalid filters: {err}")))?,
        None => Filters::default(),
    };
    let issues = client
        .issues(settings.team.as_deref(), &filter(filters))
        .map_err(contract)?;
    let tickets: Vec<Ticket> = issues.into_iter().map(ticket).collect();
    Ok(json!({ "tickets": tickets }))
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| failed(format!("no {key} given")))
}

/// Answers one request as the Linear plugin, with the issue source's table
/// as `config`.
pub fn op(op: &str, args: &Value, config: &Value) -> Result<Value, Error> {
    if op == "describe" {
        return Ok(json!({
            "protocol": PROTOCOL,
            "name": "linear",
            "kind": "tickets",
            "ops": ["list", "get", "branch", "start", "finish"],
        }));
    }
    let settings: Settings = serde_json::from_value(config.clone()).map_err(|err| {
        failed(format!(
            "the issue source needs key_env, the variable holding a Linear API key: {err}"
        ))
    })?;
    let client = Client::from_env(&settings.key_env).map_err(contract)?;
    match op {
        "list" => list(&client, &settings, args),
        "get" => {
            let detail = client.detail(required(args, "id")?).map_err(contract)?;
            Ok(json!(linked(detail)))
        }
        "branch" => {
            let issue = client.issue(required(args, "id")?).map_err(contract)?;
            Ok(json!({ "branch": issue.branch }))
        }
        "start" => {
            client.start(required(args, "id")?).map_err(contract)?;
            Ok(json!({}))
        }
        "finish" => {
            client.finish(required(args, "id")?).map_err(contract)?;
            Ok(json!({}))
        }
        _ => Err(Error::new(
            Code::Unsupported,
            format!("linear does not answer {op}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_key_is_an_auth_error_that_names_the_variable() {
        let config = json!({ "key_env": "TREETOP_TEST_NO_SUCH_KEY", "team": "END" });
        let err = op("list", &json!({}), &config).unwrap_err();
        assert_eq!(err.code, Code::Auth);
        assert!(
            err.message
                .starts_with("TREETOP_TEST_NO_SUCH_KEY is not set")
        );
        let err = op("list", &json!({}), &json!({})).unwrap_err();
        assert!(err.message.contains("key_env"));
    }

    #[test]
    fn describes_itself_without_a_key() {
        let described = op("describe", &json!({}), &json!({})).unwrap();
        assert_eq!(described["kind"], "tickets");
    }
}
