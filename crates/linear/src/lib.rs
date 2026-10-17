//! A client for Linear's GraphQL API, as treetop uses it: listing a team's
//! issues with filters, reading one with its comments, reading what a team's
//! issues can be set to, and changing, creating and commenting on issues.
//! Requests go over HTTPS from the process itself, with a personal API key,
//! so nothing waits on a process start and the key never shows in a process
//! list.
//!
//! It knows nothing about screens or trees; treetop's ticket plugin and its
//! Issues screen are built on it.

mod edit;
mod filter;
mod model;

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::{Value, json};

pub use edit::{Change, NewIssue};
pub use filter::{ANY_STATE, Filter, UNASSIGNED};
pub use model::{Comment, Cycle, Detail, Issue, Link, Named, State, Team};

use model::{ISSUE_FIELDS, RawIssue, RawTeam};

const API: &str = "https://api.linear.app/graphql";

/// How long one request gets, connecting included.
const LIMIT: Duration = Duration::from_secs(10);

/// Issues one list returns, the most recently updated first.
const PAGE: usize = 100;

/// The most an image fetched may weigh.
const IMAGE_LIMIT: u64 = 25 * 1024 * 1024;

/// Another issue as a detail names it: its id, title and status.
const LINKED: &str = "identifier title state { name }";

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

/// Talks to Linear with one API key, keeping each team's states, labels,
/// members, projects and cycles once read.
pub struct Client {
    key: String,
    agent: ureq::Agent,
    teams: Mutex<HashMap<String, Arc<Team>>>,
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
            teams: Mutex::default(),
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
             orderBy: updatedAt) {{ nodes {{ {ISSUE_FIELDS} }} }} }}"
        );
        let data = self.query(
            &query,
            json!({ "filter": filter.to_value(team), "first": PAGE }),
        )?;
        let nodes = data
            .pointer("/issues/nodes")
            .cloned()
            .unwrap_or(Value::Array(Vec::new()));
        let raw: Vec<RawIssue> = serde_json::from_value(nodes)
            .map_err(|err| Error::failed(format!("unreadable issues: {err}")))?;
        Ok(raw.into_iter().map(Issue::from).collect())
    }

    /// One issue, by its id or uuid, with its description, comments, and the
    /// issues it is linked to.
    pub fn detail(&self, id: &str) -> Result<Detail, Error> {
        let query = format!(
            "query($id: String!) {{ issue(id: $id) {{ {ISSUE_FIELDS} description \
             comments(first: 100) {{ nodes {{ body createdAt user {{ id name }} }} }} \
             parent {{ {LINKED} }} children(first: 50) {{ nodes {{ {LINKED} }} }} \
             relations(first: 50) {{ nodes {{ type relatedIssue {{ {LINKED} }} }} }} \
             inverseRelations(first: 50) {{ nodes {{ type issue {{ {LINKED} }} }} }} }} }}"
        );
        let data = self.query(&query, json!({ "id": id }))?;
        raw_issue(data.get("issue"), id).map(RawIssue::into_detail)
    }

    /// The file at `url`, such as an image in a description. Linear's own
    /// uploads need the key; it goes to Linear's hosts only, over HTTPS.
    pub fn fetch(&self, url: &str) -> Result<Vec<u8>, Error> {
        let unavailable =
            |err: ureq::Error| Error::new(ErrorKind::Unavailable, format!("{url}: {err}"));
        let mut request = self.agent.get(url);
        if is_linear_host(url) {
            request = request.header("Authorization", &self.key);
        }
        let mut response = request.call().map_err(unavailable)?;
        let status = response.status();
        if !status.is_success() {
            let kind = match status.as_u16() {
                401 | 403 => ErrorKind::Auth,
                404 => ErrorKind::NotFound,
                _ => ErrorKind::Failed,
            };
            return Err(Error::new(kind, format!("{url}: {status}")));
        }
        response
            .body_mut()
            .with_config()
            .limit(IMAGE_LIMIT)
            .read_to_vec()
            .map_err(unavailable)
    }

    /// One issue, by its id, with its description.
    pub fn issue(&self, id: &str) -> Result<Issue, Error> {
        self.detail(id).map(|detail| detail.issue)
    }

    /// The team with key `key`, read once and then kept; `refresh` reads it
    /// again.
    pub fn team(&self, key: &str) -> Result<Arc<Team>, Error> {
        let cached = self
            .teams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key.to_lowercase())
            .cloned();
        if let Some(team) = cached {
            return Ok(team);
        }
        let team = Arc::new(self.read_team(key)?);
        self.teams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.to_lowercase(), Arc::clone(&team));
        Ok(team)
    }

    /// Forgets every team read, so the next `team` reads it again.
    pub fn refresh(&self) {
        self.teams
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Linear prices a query by multiplying each list by its page size, a
    /// list with none counting as 50, against a limit of 10,000; the one team
    /// asked for is a list of one, or its lists would count fifty times over.
    fn read_team(&self, key: &str) -> Result<Team, Error> {
        let query = "query($key: String!) { viewer { id name } \
            issueLabels(first: 250, filter: { team: { null: true } }) { nodes { id name } } \
            teams(first: 1, filter: { key: { eqIgnoreCase: $key } }) { nodes { id key name \
              states { nodes { id name type position } } \
              labels(first: 250) { nodes { id name } } \
              members(first: 250) { nodes { id name } } \
              projects(first: 250) { nodes { id name } } \
              cycles(first: 50) { nodes { id number name isActive } } } } }";
        let mut data = self.query(query, json!({ "key": key }))?;
        let raw = data
            .pointer_mut("/teams/nodes/0")
            .map(Value::take)
            .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no team {key}")))?;
        let raw: RawTeam = serde_json::from_value(raw)
            .map_err(|err| Error::failed(format!("unreadable team {key}: {err}")))?;
        let workspace_labels = model::named_list(data.pointer_mut("/issueLabels").map(Value::take));
        let me = model::one_named(data.pointer_mut("/viewer").map(Value::take));
        Ok(raw.into_team(workspace_labels, me))
    }

    /// Applies `change` to the issue with uuid `uuid`, and returns the issue
    /// as it now is.
    pub fn update(&self, uuid: &str, change: &Change) -> Result<Issue, Error> {
        let query = format!(
            "mutation($id: String!, $input: IssueUpdateInput!) {{ \
             issueUpdate(id: $id, input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        let data = self.query(&query, json!({ "id": uuid, "input": change.input() }))?;
        succeeded(&data, "/issueUpdate/success", uuid)?;
        raw_issue(data.pointer("/issueUpdate/issue"), uuid).map(Issue::from)
    }

    /// Creates `new` and returns it.
    pub fn create(&self, new: &NewIssue) -> Result<Issue, Error> {
        let query = format!(
            "mutation($input: IssueCreateInput!) {{ \
             issueCreate(input: $input) {{ success issue {{ {ISSUE_FIELDS} }} }} }}"
        );
        let data = self.query(&query, json!({ "input": new.input() }))?;
        succeeded(&data, "/issueCreate/success", &new.title)?;
        raw_issue(data.pointer("/issueCreate/issue"), &new.title).map(Issue::from)
    }

    /// Posts `body`, Markdown, as a comment on the issue with uuid `uuid`.
    pub fn comment(&self, uuid: &str, body: &str) -> Result<Comment, Error> {
        let query = "mutation($input: CommentCreateInput!) { commentCreate(input: $input) { \
                     success comment { body createdAt user { id name } } } }";
        let data = self.query(query, json!({ "input": { "issueId": uuid, "body": body } }))?;
        succeeded(&data, "/commentCreate/success", uuid)?;
        let comment = data
            .pointer("/commentCreate/comment")
            .ok_or_else(|| Error::failed("Linear returned no comment"))?;
        Ok(Comment {
            author: comment
                .pointer("/user/name")
                .and_then(Value::as_str)
                .unwrap_or("you")
                .to_string(),
            body: comment
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or(body)
                .to_string(),
            created_at: comment
                .get("createdAt")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }

    /// Moves the issue to its team's first started state and assigns it to
    /// the key's user.
    pub fn start(&self, id: &str) -> Result<(), Error> {
        let query = "query($id: String!) { viewer { id } issue(id: $id) { id team { \
                     states(filter: { type: { eq: \"started\" } }) { nodes { id position } } } } }";
        let data = self.query(query, json!({ "id": id }))?;
        let uuid = data
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
        let change = Change {
            state_id: Some(state.to_string()),
            assignee_id: Some(Some(viewer.to_string())),
            ..Change::default()
        };
        self.update(uuid, &change).map(|_| ())
    }
}

/// Whether `url` is Linear's own, over HTTPS: `linear.app` or a host under
/// it, such as `uploads.linear.app`.
fn is_linear_host(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Userinfo or a port would let `linear.app@elsewhere` pass.
    if authority.contains(['@', ':']) {
        return false;
    }
    let host = authority.to_ascii_lowercase();
    host == "linear.app" || host.ends_with(".linear.app")
}

/// An issue node, or that `what` was not found.
fn raw_issue(node: Option<&Value>, what: &str) -> Result<RawIssue, Error> {
    let node = node
        .filter(|n| !n.is_null())
        .cloned()
        .ok_or_else(|| Error::new(ErrorKind::NotFound, format!("no issue {what}")))?;
    serde_json::from_value(node).map_err(|err| Error::failed(format!("unreadable issue: {err}")))
}

/// A mutation's `success` was true.
fn succeeded(data: &Value, pointer: &str, what: &str) -> Result<(), Error> {
    if data.pointer(pointer) == Some(&Value::Bool(true)) {
        Ok(())
    } else {
        Err(Error::failed(format!(
            "Linear did not apply the change to {what}"
        )))
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

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUE: &str = r#"{"id":"u-123","identifier":"END-123","title":"Fix login",
        "url":"https://linear.app/x/issue/END-123","branchName":"anthony/end-123-fix-login",
        "priority":2,"updatedAt":"2026-10-17T10:00:00.000Z","team":{"id":"t1"},
        "state":{"id":"s1","name":"Todo"},"assignee":null,
        "labels":{"nodes":[{"id":"l1","name":"Bug"}]},"project":{"id":"p1","name":"Auth"},
        "cycle":{"number":12,"name":null}}"#;

    #[test]
    fn reads_an_issue_with_names_to_show_and_ids_to_change() {
        let raw: RawIssue = serde_json::from_str(ISSUE).unwrap();
        let issue = Issue::from(raw);
        assert_eq!(
            (issue.uuid.as_str(), issue.id.as_str()),
            ("u-123", "END-123")
        );
        assert_eq!(
            (issue.labels, issue.label_ids),
            (vec!["Bug".into()], vec!["l1".into()])
        );
        assert_eq!(issue.state_id.as_deref(), Some("s1"));
        assert_eq!((issue.assignee, issue.assignee_id), (None, None));
        assert_eq!(issue.cycle.as_deref(), Some("cycle 12"));
        assert_eq!(issue.priority, 2);
        assert_eq!(issue.team_id.as_deref(), Some("t1"));
    }

    #[test]
    fn a_detail_carries_its_comments_oldest_first() {
        let with_comments = ISSUE.replace(
            r#""cycle":{"number":12,"name":null}}"#,
            r#""cycle":null,"description":"Keep return_to.","comments":{"nodes":[
                {"body":"second","createdAt":"2026-10-17T12:00:00Z","user":{"id":"u2","name":"Bo"}},
                {"body":"first","createdAt":"2026-10-17T11:00:00Z","user":null}]}}"#,
        );
        let raw: RawIssue = serde_json::from_str(&with_comments).unwrap();
        let detail = raw.into_detail();
        assert_eq!(detail.issue.description.as_deref(), Some("Keep return_to."));
        assert_eq!(detail.issue.cycle, None);
        let said: Vec<(&str, &str)> = detail
            .comments
            .iter()
            .map(|c| (c.author.as_str(), c.body.as_str()))
            .collect();
        assert_eq!(said, [("someone", "first"), ("Bo", "second")]);
    }

    #[test]
    fn a_detail_names_its_links_from_its_own_side() {
        // Each linked issue as the detail query asks for it, LINKED.
        let linked = |id: &str| {
            format!(r#"{{"identifier":"{id}","title":"t {id}","state":{{"name":"Todo"}}}}"#)
        };
        let with_links = ISSUE.replace(
            r#""cycle":{"number":12,"name":null}}"#,
            &format!(
                r#""cycle":null,"parent":{},"children":{{"nodes":[{}]}},
                "relations":{{"nodes":[{{"type":"related","relatedIssue":{}}},{{"type":"blocks","relatedIssue":{}}}]}},
                "inverseRelations":{{"nodes":[{{"type":"blocks","issue":{}}},{{"type":"duplicate","issue":{}}}]}}}}"#,
                linked("END-1"),
                linked("END-2"),
                linked("END-3"),
                linked("END-4"),
                linked("END-5"),
                linked("END-6"),
            ),
        );
        let raw: RawIssue = serde_json::from_str(&with_links).unwrap();
        let links: Vec<(String, String)> = raw
            .into_detail()
            .links
            .into_iter()
            .map(|l| (l.kind, l.id))
            .collect();
        let expected = [
            ("parent", "END-1"),
            ("sub-issue", "END-2"),
            ("blocked by", "END-5"),
            ("blocks", "END-4"),
            ("related", "END-3"),
            ("duplicated by", "END-6"),
        ]
        .map(|(kind, id)| (kind.to_string(), id.to_string()));
        assert_eq!(links, expected);
    }

    #[test]
    fn the_key_goes_to_linears_own_hosts_only() {
        assert!(is_linear_host("https://uploads.linear.app/a/b.png"));
        assert!(is_linear_host("https://linear.app/x"));
        assert!(
            !is_linear_host("http://uploads.linear.app/a.png"),
            "not over plain HTTP"
        );
        assert!(!is_linear_host("https://linear.app.evil.com/a.png"));
        assert!(!is_linear_host("https://evillinear.app/a.png"));
        assert!(!is_linear_host("https://linear.app@evil.com/a.png"));
        assert!(!is_linear_host("https://i.imgur.com/a.png"));
    }

    #[test]
    fn reads_a_team_with_states_in_workflow_order_and_workspace_labels() {
        let raw: RawTeam = serde_json::from_str(
            r#"{"id":"t1","key":"END","name":"Endo","states":{"nodes":[
                {"id":"d","name":"Done","type":"completed","position":0},
                {"id":"r","name":"In Review","type":"started","position":2},
                {"id":"p","name":"In Progress","type":"started","position":1},
                {"id":"b","name":"Backlog","type":"backlog","position":5}]},
               "labels":{"nodes":[{"id":"l2","name":"bug"}]},
               "members":{"nodes":[{"id":"u2","name":"Zoe"},{"id":"u1","name":"anthony"}]},
               "projects":{"nodes":[]},"cycles":{"nodes":[{"id":"c","number":14,"name":"","isActive":true}]}}"#,
        )
        .unwrap();
        let team = raw.into_team(
            vec![Named {
                id: "l1".into(),
                name: "Feature".into(),
            }],
            None,
        );
        let states: Vec<&str> = team.states.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(states, ["Backlog", "In Progress", "In Review", "Done"]);
        let labels: Vec<&str> = team.labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(labels, ["bug", "Feature"]);
        let members: Vec<&str> = team.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(members, ["anthony", "Zoe"]);
        assert_eq!(team.cycles[0].name, None, "an empty name is no name");
        assert!(team.cycles[0].is_active);
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
    fn a_failed_mutation_says_so() {
        let data = json!({ "issueUpdate": { "success": false } });
        assert!(succeeded(&data, "/issueUpdate/success", "END-1").is_err());
        let data = json!({ "issueUpdate": { "success": true } });
        assert!(succeeded(&data, "/issueUpdate/success", "END-1").is_ok());
    }

    /// Linear validates a request against its schema before it checks the
    /// key, so with a bad key a request that fits the schema is refused for
    /// authentication and one that does not is refused for what is wrong
    /// with it. Run with `cargo test -p linear -- --ignored`; it needs the
    /// network.
    #[test]
    #[ignore = "calls Linear"]
    fn every_request_fits_linears_schema() {
        let client = Client::new("lin_api_not_a_real_key");
        let all = Filter {
            mine: true,
            assignee: None,
            label: Some("Bug".into()),
            state: Some("Todo".into()),
            project: Some("x".into()),
            cycle: Some("current".into()),
            query: Some("login END-12".into()),
        };
        let others = [
            Filter {
                assignee: Some("Zoe".into()),
                ..Filter::default()
            },
            Filter {
                assignee: Some(UNASSIGNED.into()),
                state: Some(ANY_STATE.into()),
                ..Filter::default()
            },
        ];
        let change = Change {
            title: Some("t".into()),
            description: Some("d".into()),
            state_id: Some("s".into()),
            assignee_id: Some(None),
            label_ids: Some(vec!["l".into()]),
            project_id: Some(Some("p".into())),
            priority: Some(2),
        };
        let new = NewIssue {
            team_id: "t".into(),
            title: "t".into(),
            description: Some("d".into()),
            label_ids: vec!["l".into()],
            priority: Some(3),
            ..NewIssue::default()
        };
        let results = [
            ("issues", client.issues(Some("END"), &all).err()),
            ("detail", client.detail("END-1").err()),
            ("team", client.team("END").err()),
            ("update", client.update("u", &change).err()),
            ("create", client.create(&new).err()),
            ("comment", client.comment("u", "hi").err()),
            ("start", client.start("END-1").err()),
        ];
        for (name, err) in results {
            let err = err.unwrap_or_else(|| panic!("{name} succeeded with a bad key"));
            assert_eq!(err.kind, ErrorKind::Auth, "{name}: {}", err.message);
        }
        // Variables are only read after the key is checked, so the inputs
        // are checked again written into the query, where the schema
        // validates them.
        let filter = |filter: &Filter| {
            format!(
                "{{ issues(filter: {}) {{ nodes {{ id }} }} }}",
                literal(&filter.to_value(Some("END")))
            )
        };
        let inline = [
            ("filter", filter(&all)),
            ("filter by person", filter(&others[0])),
            ("filter of nobody's", filter(&others[1])),
            (
                "change",
                format!(
                    "mutation {{ issueUpdate(id: \"u\", input: {}) {{ success }} }}",
                    literal(&change.input())
                ),
            ),
            (
                "new issue",
                format!(
                    "mutation {{ issueCreate(input: {}) {{ success }} }}",
                    literal(&new.input())
                ),
            ),
        ];
        for (name, query) in inline {
            let err = client.query(&query, json!({})).unwrap_err();
            assert_eq!(err.kind, ErrorKind::Auth, "{name}: {}", err.message);
        }
    }

    /// `value` written as a GraphQL literal: keys bare, everything else as
    /// JSON writes it.
    fn literal(value: &Value) -> String {
        match value {
            Value::Object(map) => {
                let fields: Vec<String> = map
                    .iter()
                    .map(|(key, value)| format!("{key}: {}", literal(value)))
                    .collect();
                format!("{{ {} }}", fields.join(", "))
            }
            Value::Array(items) => {
                let items: Vec<String> = items.iter().map(literal).collect();
                format!("[{}]", items.join(", "))
            }
            other => other.to_string(),
        }
    }

    /// Every read a picker or the list makes, against a real workspace: the
    /// key in `LINEAR_API_KEY_WORK` and the team in `LINEAR_TEST_TEAM`, else
    /// `END`. Linear only prices a query once the key is good, so this is
    /// what catches one too costly to run. It changes nothing. Run with
    /// `cargo test -p linear -- --ignored reads_a_real_workspace`.
    #[test]
    #[ignore = "calls Linear with a real key"]
    fn reads_a_real_workspace() {
        let client = Client::from_env("LINEAR_API_KEY_WORK").expect("LINEAR_API_KEY_WORK is set");
        let team = std::env::var("LINEAR_TEST_TEAM").unwrap_or_else(|_| "END".into());
        let read = client
            .team(&team)
            .unwrap_or_else(|err| panic!("team {team}: {err}"));
        assert!(!read.states.is_empty(), "a team has states");
        let issues = client
            .issues(Some(&team), &Filter::default())
            .unwrap_or_else(|err| panic!("issues: {err}"));
        if let Some(first) = issues.first() {
            client
                .detail(&first.id)
                .unwrap_or_else(|err| panic!("detail of {}: {err}", first.id));
        }
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
