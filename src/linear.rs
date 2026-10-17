//! The Linear ticket plugin, built in: `treetop plugin linear` serves it. It
//! talks to Linear's GraphQL API with `curl`, the request written to curl's
//! stdin as a config file, so the API key never shows in a process list.
//!
//! Its config is the ticket source's table from `config.toml`:
//!
//! ```toml
//! [tickets.work]
//! plugin = "linear"
//! key_env = "LINEAR_API_KEY_WORK"   # the variable holding a personal API key
//! team = "END"                      # the team whose issues `list` shows
//! ```

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::child::{self, End};
use crate::plugin::{Code, Error, PROTOCOL};
use crate::tickets::{Filters, Ticket};

const API: &str = "https://api.linear.app/graphql";

/// How long one API call gets.
const LIMIT: Duration = Duration::from_secs(10);

/// Issues one `list` returns, the most recently updated first.
const PAGE: usize = 100;

#[derive(Debug, Deserialize)]
struct Settings {
    key_env: String,
    #[serde(default)]
    team: Option<String>,
}

fn failed(message: impl Into<String>) -> Error {
    Error::new(Code::Failed, message)
}

/// `text` quoted for a curl config file.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The curl config that posts `body` with `key`.
fn curl_config(key: &str, body: &str) -> String {
    format!(
        "url = {}\nheader = {}\nheader = {}\ndata = {}\n",
        quoted(API),
        quoted("Content-Type: application/json"),
        quoted(&format!("Authorization: {key}")),
        quoted(body)
    )
}

/// The API key the config's `key_env` names.
fn key(settings: &Settings) -> Result<String, Error> {
    std::env::var(&settings.key_env)
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| {
            Error::new(
                Code::Auth,
                format!(
                    "{} is not set; it should hold a Linear API key",
                    settings.key_env
                ),
            )
        })
}

/// Runs one GraphQL request and returns its `data`.
fn graphql(settings: &Settings, query: &str, variables: Value) -> Result<Value, Error> {
    let body = json!({ "query": query, "variables": variables }).to_string();
    let config = curl_config(&key(settings)?, &body);
    let ran = child::run(
        Command::new("curl").args(["--silent", "--show-error", "--config", "-"]),
        Some(config.into_bytes()),
        LIMIT,
    )
    .map_err(|err| Error::new(Code::Unavailable, format!("curl: {err}")))?;
    match ran.end {
        End::Exited(status) if status.success() => {}
        End::Exited(_) => {
            let stderr = String::from_utf8_lossy(&ran.stderr).trim().to_string();
            return Err(Error::new(Code::Unavailable, stderr));
        }
        End::TimedOut | End::HeldOpen(_) => {
            return Err(Error::new(
                Code::Unavailable,
                "Linear did not answer in time",
            ));
        }
    }
    reply(&ran.stdout)
}

/// The `data` of a GraphQL reply, or its first error.
fn reply(stdout: &[u8]) -> Result<Value, Error> {
    let mut reply: Value = serde_json::from_slice(stdout).map_err(|err| {
        failed(format!(
            "Linear answered in a shape treetop cannot read: {err}"
        ))
    })?;
    if let Some(first) = reply.get("errors").and_then(|e| e.get(0)) {
        let message = first
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Linear refused the request");
        let is_auth = first
            .pointer("/extensions/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.contains("authentication"));
        let code = if is_auth { Code::Auth } else { Code::Failed };
        return Err(Error::new(code, message.to_string()));
    }
    Ok(reply
        .get_mut("data")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

/// The issue fields every op reads.
const FIELDS: &str = "identifier title url branchName state { name } assignee { name } \
    labels { nodes { name } } project { name } cycle { number name }";

fn ticket(node: &Value, with_description: bool) -> Option<Ticket> {
    let text = |pointer: &str| {
        node.pointer(pointer)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let cycle = node.get("cycle").filter(|c| !c.is_null()).map(|c| {
        c.get("name")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("cycle {}", c.get("number").unwrap_or(&Value::Null)))
    });
    Some(Ticket {
        id: text("/identifier")?,
        title: text("/title").unwrap_or_default(),
        state: text("/state/name").unwrap_or_default(),
        labels: node
            .pointer("/labels/nodes")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|l| l.get("name").and_then(Value::as_str).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        assignee: text("/assignee/name"),
        project: text("/project/name"),
        cycle,
        url: text("/url"),
        description: if with_description {
            text("/description")
        } else {
            None
        },
        branch: text("/branchName"),
    })
}

/// The issue filter `filters` and the team ask for: open issues only,
/// unless a state is named.
fn issue_filter(team: Option<&str>, filters: &Filters) -> Value {
    let mut filter = serde_json::Map::new();
    if let Some(team) = team {
        filter.insert("team".into(), json!({ "key": { "eqIgnoreCase": team } }));
    }
    if filters.mine {
        filter.insert("assignee".into(), json!({ "isMe": { "eq": true } }));
    }
    if let Some(label) = &filters.label {
        filter.insert(
            "labels".into(),
            json!({ "some": { "name": { "eqIgnoreCase": label } } }),
        );
    }
    match &filters.state {
        Some(state) => filter.insert("state".into(), json!({ "name": { "eqIgnoreCase": state } })),
        None => filter.insert(
            "state".into(),
            json!({ "type": { "nin": ["completed", "canceled"] } }),
        ),
    };
    if let Some(project) = &filters.project {
        filter.insert(
            "project".into(),
            json!({ "name": { "containsIgnoreCase": project } }),
        );
    }
    match filters.cycle.as_deref() {
        Some("current") => {
            filter.insert("cycle".into(), json!({ "isActive": { "eq": true } }));
        }
        Some(number) => {
            if let Ok(number) = number.parse::<u32>() {
                filter.insert("cycle".into(), json!({ "number": { "eq": number } }));
            }
        }
        None => {}
    }
    if let Some(query) = filters.query.as_deref().filter(|q| !q.trim().is_empty()) {
        filter.insert(
            "title".into(),
            json!({ "containsIgnoreCase": query.trim() }),
        );
    }
    Value::Object(filter)
}

fn list(settings: &Settings, args: &Value) -> Result<Value, Error> {
    let filters: Filters = match args.get("filters").filter(|f| !f.is_null()) {
        Some(filters) => serde_json::from_value(filters.clone())
            .map_err(|err| failed(format!("invalid filters: {err}")))?,
        None => Filters::default(),
    };
    let query = format!(
        "query($filter: IssueFilter, $first: Int) {{ issues(filter: $filter, first: $first, \
         orderBy: updatedAt) {{ nodes {{ {FIELDS} }} }} }}"
    );
    let data = graphql(
        settings,
        &query,
        json!({ "filter": issue_filter(settings.team.as_deref(), &filters), "first": PAGE }),
    )?;
    let tickets: Vec<Ticket> = data
        .pointer("/issues/nodes")
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(|n| ticket(n, false)).collect())
        .unwrap_or_default();
    Ok(json!({ "tickets": tickets }))
}

fn get_ticket(settings: &Settings, id: &str) -> Result<Ticket, Error> {
    let query = format!("query($id: String!) {{ issue(id: $id) {{ {FIELDS} description }} }}");
    let data = graphql(settings, &query, json!({ "id": id }))?;
    data.get("issue")
        .and_then(|issue| ticket(issue, true))
        .ok_or_else(|| Error::new(Code::NotFound, format!("no issue {id}")))
}

/// Moves the issue to its team's first started state and assigns it to the
/// key's user.
fn start(settings: &Settings, id: &str) -> Result<Value, Error> {
    let query = "query($id: String!) { viewer { id } issue(id: $id) { id team { \
                 states(filter: { type: { eq: \"started\" } }) { nodes { id position } } } } }";
    let data = graphql(settings, query, json!({ "id": id }))?;
    let issue = data
        .pointer("/issue/id")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::new(Code::NotFound, format!("no issue {id}")))?;
    let viewer = data
        .pointer("/viewer/id")
        .and_then(Value::as_str)
        .ok_or_else(|| failed("Linear gave no user for the key"))?;
    let state = data
        .pointer("/issue/team/states/nodes")
        .and_then(Value::as_array)
        .and_then(|states| {
            states.iter().min_by(|a, b| {
                let position = |s: &Value| s.get("position").and_then(Value::as_f64).unwrap_or(0.0);
                position(a).total_cmp(&position(b))
            })
        })
        .and_then(|s| s.get("id"))
        .and_then(Value::as_str)
        .ok_or_else(|| failed(format!("{id}'s team has no started state")))?;
    let update = "mutation($id: String!, $input: IssueUpdateInput!) { \
                  issueUpdate(id: $id, input: $input) { success } }";
    let done = graphql(
        settings,
        update,
        json!({ "id": issue, "input": { "stateId": state, "assigneeId": viewer } }),
    )?;
    if done.pointer("/issueUpdate/success") != Some(&Value::Bool(true)) {
        return Err(failed(format!("Linear did not update {id}")));
    }
    Ok(json!({}))
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, Error> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| failed(format!("no {key} given")))
}

/// Answers one request as the Linear plugin, with the ticket source's table
/// as `config`.
pub fn op(op: &str, args: &Value, config: &Value) -> Result<Value, Error> {
    if op == "describe" {
        return Ok(json!({
            "protocol": PROTOCOL,
            "name": "linear",
            "kind": "tickets",
            "ops": ["list", "get", "branch", "start"],
        }));
    }
    let settings: Settings = serde_json::from_value(config.clone()).map_err(|err| {
        failed(format!(
            "the ticket source needs key_env, the variable holding a Linear API key: {err}"
        ))
    })?;
    match op {
        "list" => list(&settings, args),
        "get" => Ok(json!(get_ticket(&settings, required(args, "id")?)?)),
        "branch" => {
            let ticket = get_ticket(&settings, required(args, "id")?)?;
            Ok(json!({ "branch": ticket.branch }))
        }
        "start" => start(&settings, required(args, "id")?),
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
    fn quotes_for_curl_so_a_body_with_quotes_and_lines_survives() {
        assert_eq!(quoted(r#"a "b" \ c"#), r#""a \"b\" \\ c""#);
        assert_eq!(quoted("x\ny"), r#""x\ny""#);
        let config = curl_config("lin_api_1", r#"{"query":"q"}"#);
        assert!(config.contains(r#"header = "Authorization: lin_api_1""#));
        assert!(config.contains(r#"data = "{\"query\":\"q\"}""#));
    }

    #[test]
    fn filters_become_an_issue_filter_open_issues_by_default() {
        let none = issue_filter(Some("END"), &Filters::default());
        assert_eq!(
            none,
            json!({
                "team": { "key": { "eqIgnoreCase": "END" } },
                "state": { "type": { "nin": ["completed", "canceled"] } },
            })
        );
        let all = issue_filter(
            None,
            &Filters {
                mine: true,
                label: Some("Bug".into()),
                state: Some("In Review".into()),
                project: Some("checkout".into()),
                cycle: Some("current".into()),
                query: Some(" login ".into()),
            },
        );
        assert_eq!(all["assignee"], json!({ "isMe": { "eq": true } }));
        assert_eq!(all["labels"]["some"]["name"]["eqIgnoreCase"], "Bug");
        assert_eq!(
            all["state"],
            json!({ "name": { "eqIgnoreCase": "In Review" } })
        );
        assert_eq!(all["cycle"], json!({ "isActive": { "eq": true } }));
        assert_eq!(all["title"], json!({ "containsIgnoreCase": "login" }));
        let numbered = issue_filter(
            None,
            &Filters {
                cycle: Some("12".into()),
                ..Filters::default()
            },
        );
        assert_eq!(numbered["cycle"], json!({ "number": { "eq": 12 } }));
    }

    #[test]
    fn reads_issues_from_a_reply() {
        let reply_text = br#"{"data":{"issues":{"nodes":[{"identifier":"END-123",
            "title":"Fix login","url":"https://linear.app/x/issue/END-123","branchName":"anthony/end-123-fix-login",
            "state":{"name":"Todo"},"assignee":null,"labels":{"nodes":[{"name":"Bug"}]},
            "project":{"name":"Auth"},"cycle":{"number":12,"name":null}}]}}}"#;
        let data = reply(reply_text).unwrap();
        let ticket = ticket(&data["issues"]["nodes"][0], false).unwrap();
        assert_eq!(ticket.id, "END-123");
        assert_eq!(ticket.labels, ["Bug"]);
        assert_eq!(ticket.assignee, None);
        assert_eq!(ticket.cycle.as_deref(), Some("cycle 12"));
        assert_eq!(ticket.branch.as_deref(), Some("anthony/end-123-fix-login"));
    }

    #[test]
    fn an_error_reply_says_what_linear_said() {
        let auth = br#"{"errors":[{"message":"Authentication required","extensions":{"type":"authentication error"}}]}"#;
        let err = reply(auth).unwrap_err();
        assert_eq!(
            (err.code, err.message.as_str()),
            (Code::Auth, "Authentication required")
        );
        assert_eq!(reply(b"<html>").unwrap_err().code, Code::Failed);
    }

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
}
