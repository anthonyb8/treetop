//! A client for Linear's GraphQL API, as treetop uses it: listing a team's
//! issues with filters, reading one, and marking one started. Requests go
//! over HTTPS from the process itself, with a personal API key, so nothing
//! waits on a process start and the key never shows in a process list.
//!
//! It knows nothing about screens or trees; treetop's ticket plugin and its
//! Issues screen are built on it.

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const API: &str = "https://api.linear.app/graphql";

/// How long one request gets, connecting included.
const LIMIT: Duration = Duration::from_secs(10);

/// Issues one list returns, the most recently updated first.
const PAGE: usize = 100;

/// What went wrong, as a caller decides what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The key is missing, wrong or revoked.
    Auth,
    NotFound,
    /// Linear could not be reached, or did not answer in time.
    Unavailable,
    /// Linear refused the request, or answered in a shape not understood.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    pub message: String,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Error {
            kind,
            message: message.into(),
        }
    }

    fn failed(message: impl Into<String>) -> Self {
        Error::new(ErrorKind::Failed, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// An issue, as much of it as treetop shows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    /// The id people use, such as `END-123`.
    pub id: String,
    pub title: String,
    pub state: String,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub cycle: Option<String>,
    pub url: Option<String>,
    /// Its Markdown text; only a single issue's read carries it.
    pub description: Option<String>,
    /// The branch name Linear gives it.
    pub branch: Option<String>,
}

/// What narrows a list. Each is optional; none lists the team's open issues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Only issues assigned to the key's user.
    pub mine: bool,
    pub label: Option<String>,
    /// A state's name; without one, only open issues are listed.
    pub state: Option<String>,
    /// Part of a project's name.
    pub project: Option<String>,
    /// `current` for the active cycle, else a cycle number.
    pub cycle: Option<String>,
    /// Words the title contains.
    pub query: Option<String>,
}

/// Talks to Linear with one API key.
pub struct Client {
    key: String,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(key: impl Into<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(LIMIT))
            // A GraphQL error comes back as JSON whatever the status, and
            // its message says more than the status does.
            .http_status_as_error(false)
            .build()
            .into();
        Client {
            key: key.into(),
            agent,
        }
    }

    /// The key in the environment variable `name`, or an auth error naming it.
    pub fn from_env(name: &str) -> Result<Self, Error> {
        std::env::var(name)
            .ok()
            .filter(|key| !key.trim().is_empty())
            .map(Client::new)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Auth,
                    format!("{name} is not set; it should hold a Linear API key"),
                )
            })
    }

    /// Runs one GraphQL request and returns its `data`.
    pub fn query(&self, query: &str, variables: Value) -> Result<Value, Error> {
        let body = json!({ "query": query, "variables": variables }).to_string();
        let mut response = self
            .agent
            .post(API)
            .header("Authorization", &self.key)
            .header("Content-Type", "application/json")
            .send(&body)
            .map_err(|err| Error::new(ErrorKind::Unavailable, format!("Linear: {err}")))?;
        let text = response
            .body_mut()
            .read_to_string()
            .map_err(|err| Error::new(ErrorKind::Unavailable, format!("Linear: {err}")))?;
        reply(text.as_bytes())
    }

    /// The team's issues `filter` asks for, the most recently updated first.
    pub fn issues(&self, team: Option<&str>, filter: &Filter) -> Result<Vec<Issue>, Error> {
        let query = format!(
            "query($filter: IssueFilter, $first: Int) {{ issues(filter: $filter, first: $first, \
             orderBy: updatedAt) {{ nodes {{ {FIELDS} }} }} }}"
        );
        let data = self.query(
            &query,
            json!({ "filter": issue_filter(team, filter), "first": PAGE }),
        )?;
        Ok(data
            .pointer("/issues/nodes")
            .and_then(Value::as_array)
            .map(|nodes| nodes.iter().filter_map(|n| issue(n, false)).collect())
            .unwrap_or_default())
    }

    /// One issue, by its id, with its description.
    pub fn issue(&self, id: &str) -> Result<Issue, Error> {
        let query = format!("query($id: String!) {{ issue(id: $id) {{ {FIELDS} description }} }}");
        let data = self.query(&query, json!({ "id": id }))?;
        data.get("issue")
            .and_then(|node| issue(node, true))
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no issue {id}")))
    }

    /// Moves the issue to its team's first started state and assigns it to
    /// the key's user.
    pub fn start(&self, id: &str) -> Result<(), Error> {
        let query = "query($id: String!) { viewer { id } issue(id: $id) { id team { \
                     states(filter: { type: { eq: \"started\" } }) { nodes { id position } } } } }";
        let data = self.query(query, json!({ "id": id }))?;
        let issue = data
            .pointer("/issue/id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no issue {id}")))?;
        let viewer = data
            .pointer("/viewer/id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::failed("Linear gave no user for the key"))?;
        let state = data
            .pointer("/issue/team/states/nodes")
            .and_then(Value::as_array)
            .and_then(|states| {
                states.iter().min_by(|a, b| {
                    let position =
                        |s: &Value| s.get("position").and_then(Value::as_f64).unwrap_or(0.0);
                    position(a).total_cmp(&position(b))
                })
            })
            .and_then(|s| s.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::failed(format!("{id}'s team has no started state")))?;
        let update = "mutation($id: String!, $input: IssueUpdateInput!) { \
                      issueUpdate(id: $id, input: $input) { success } }";
        let done = self.query(
            update,
            json!({ "id": issue, "input": { "stateId": state, "assigneeId": viewer } }),
        )?;
        if done.pointer("/issueUpdate/success") != Some(&Value::Bool(true)) {
            return Err(Error::failed(format!("Linear did not update {id}")));
        }
        Ok(())
    }
}

/// The `data` of a GraphQL reply, or its first error.
fn reply(body: &[u8]) -> Result<Value, Error> {
    let mut reply: Value = serde_json::from_slice(body).map_err(|err| {
        Error::failed(format!(
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
        let kind = if is_auth {
            ErrorKind::Auth
        } else {
            ErrorKind::Failed
        };
        return Err(Error::new(kind, message.to_string()));
    }
    Ok(reply
        .get_mut("data")
        .map(Value::take)
        .unwrap_or(Value::Null))
}

/// The issue fields every read asks for.
const FIELDS: &str = "identifier title url branchName state { name } assignee { name } \
    labels { nodes { name } } project { name } cycle { number name }";

fn issue(node: &Value, with_description: bool) -> Option<Issue> {
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
    Some(Issue {
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

/// The issue filter `filter` and the team ask for: open issues only, unless
/// a state is named.
fn issue_filter(team: Option<&str>, filter: &Filter) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(team) = team {
        out.insert("team".into(), json!({ "key": { "eqIgnoreCase": team } }));
    }
    if filter.mine {
        out.insert("assignee".into(), json!({ "isMe": { "eq": true } }));
    }
    if let Some(label) = &filter.label {
        out.insert(
            "labels".into(),
            json!({ "some": { "name": { "eqIgnoreCase": label } } }),
        );
    }
    match &filter.state {
        Some(state) => out.insert("state".into(), json!({ "name": { "eqIgnoreCase": state } })),
        None => out.insert(
            "state".into(),
            json!({ "type": { "nin": ["completed", "canceled"] } }),
        ),
    };
    if let Some(project) = &filter.project {
        out.insert(
            "project".into(),
            json!({ "name": { "containsIgnoreCase": project } }),
        );
    }
    match filter.cycle.as_deref() {
        Some("current") => {
            out.insert("cycle".into(), json!({ "isActive": { "eq": true } }));
        }
        Some(number) => {
            if let Ok(number) = number.parse::<u32>() {
                out.insert("cycle".into(), json!({ "number": { "eq": number } }));
            }
        }
        None => {}
    }
    if let Some(query) = filter.query.as_deref().filter(|q| !q.trim().is_empty()) {
        out.insert(
            "title".into(),
            json!({ "containsIgnoreCase": query.trim() }),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_becomes_an_issue_filter_open_issues_by_default() {
        let none = issue_filter(Some("END"), &Filter::default());
        assert_eq!(
            none,
            json!({
                "team": { "key": { "eqIgnoreCase": "END" } },
                "state": { "type": { "nin": ["completed", "canceled"] } },
            })
        );
        let all = issue_filter(
            None,
            &Filter {
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
            &Filter {
                cycle: Some("12".into()),
                ..Filter::default()
            },
        );
        assert_eq!(numbered["cycle"], json!({ "number": { "eq": 12 } }));
    }

    #[test]
    fn reads_issues_from_a_reply() {
        let body = br#"{"data":{"issues":{"nodes":[{"identifier":"END-123",
            "title":"Fix login","url":"https://linear.app/x/issue/END-123","branchName":"anthony/end-123-fix-login",
            "state":{"name":"Todo"},"assignee":null,"labels":{"nodes":[{"name":"Bug"}]},
            "project":{"name":"Auth"},"cycle":{"number":12,"name":null}}]}}}"#;
        let data = reply(body).unwrap();
        let issue = issue(&data["issues"]["nodes"][0], false).unwrap();
        assert_eq!(issue.id, "END-123");
        assert_eq!(issue.labels, ["Bug"]);
        assert_eq!(issue.assignee, None);
        assert_eq!(issue.cycle.as_deref(), Some("cycle 12"));
        assert_eq!(issue.branch.as_deref(), Some("anthony/end-123-fix-login"));
    }

    #[test]
    fn an_error_reply_says_what_linear_said() {
        let auth = br#"{"errors":[{"message":"Authentication required","extensions":{"type":"authentication error"}}]}"#;
        let err = reply(auth).unwrap_err();
        assert_eq!(
            (err.kind, err.message.as_str()),
            (ErrorKind::Auth, "Authentication required")
        );
        assert_eq!(reply(b"<html>").unwrap_err().kind, ErrorKind::Failed);
    }

    #[test]
    fn a_missing_key_is_an_auth_error_that_names_the_variable() {
        let Err(err) = Client::from_env("TREETOP_TEST_NO_SUCH_KEY") else {
            panic!("no key, no client");
        };
        assert_eq!(err.kind, ErrorKind::Auth);
        assert!(
            err.message
                .starts_with("TREETOP_TEST_NO_SUCH_KEY is not set")
        );
    }
}
