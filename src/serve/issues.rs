//! Issues on the phone: the Issues screen's list, reading and edits as a JSON
//! API, over the same backends the screen uses, with who is working on each
//! issue read from the pool. Nothing here talks to a ticket system itself.

use std::sync::{Arc, Mutex, PoisonError};

use issues::{Backend, Group, Section, View};
use linear::{Change, Detail, Issue, Link, NewIssue, Team};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use tiny_http::Method;

use super::pool::{card, now};
use super::{Asked, Pool, Reply, actions};
use crate::app::App;
use crate::tickets::{self, Own};

/// A call on the Issues screen: `<rest>` of `/api/issues/<rest>`.
pub fn route(pool: &Pool, rest: &str, asked: &Asked) -> Reply {
    let source = asked.query.get("source").cloned();
    if let (Method::Post, Some((id, "pick"))) = (asked.method, rest.split_once('/')) {
        return actions::pick(pool, id, source, asked.body);
    }
    let (backends, own) = match tickets::backends(&pool.checkout) {
        Ok(found) => found,
        Err(why) => return Reply::error(500, &why),
    };
    if (asked.method, rest) == (&Method::Get, "sources") {
        return Reply::json(200, &sources_json(&backends, &own, &views()));
    }
    match chosen(&backends, &own, source.as_deref()) {
        Ok(backend) => answer(backend.as_ref(), rest, asked, &pool.app, &|| pool.ready()),
        Err((status, why)) => Reply::error(status, &why),
    }
}

/// The source asked for by name, else the repository's own, or the status and
/// words that say why there is neither.
fn chosen<'a>(
    backends: &'a [Arc<dyn Backend>],
    own: &Own,
    source: Option<&str>,
) -> Result<&'a Arc<dyn Backend>, (u16, String)> {
    let found = match (source, own) {
        (Some(name), _) => backends.iter().find(|b| b.name() == name),
        (None, Ok(at)) => backends.get(*at),
        (None, Err(why)) => return Err((409, why.clone())),
    };
    let name = source.unwrap_or("");
    found.ok_or_else(|| (404, format!("no issue source {name}")))
}

/// The built-in views then the config's, without any that repeat one before
/// them, as `all` and `by status` do.
fn views() -> Vec<View> {
    let mut views: Vec<View> = Vec::new();
    for view in issues::builtin().into_iter().chain(tickets::views()) {
        let is_new = !views
            .iter()
            .any(|v| v.filters == view.filters && v.group == view.group);
        if is_new {
            views.push(view);
        }
    }
    views
}

fn group_name(group: Group) -> &'static str {
    match group {
        Group::None => "none",
        Group::Project => "project",
        Group::Status => "status",
    }
}

fn sources_json(backends: &[Arc<dyn Backend>], own: &Own, views: &[View]) -> Value {
    let names: Vec<&str> = backends.iter().map(|b| b.name()).collect();
    json!({
        "sources": names,
        "own": own.as_ref().ok().and_then(|at| names.get(*at)),
        "why": own.as_ref().err(),
        "views": views.iter().map(|view| json!({
            "name": view.name,
            "line": issues::line(&view.filters),
            "group": group_name(view.group),
        })).collect::<Vec<_>>(),
    })
}

/// One call on source `backend`, answered from the pool in `app`, which is
/// locked only once the source has answered, so a slow one holds up nothing.
fn answer(
    backend: &dyn Backend,
    rest: &str,
    asked: &Asked,
    app: &Mutex<App>,
    ready: &dyn Fn() -> Vec<String>,
) -> Reply {
    let locked = || app.lock().unwrap_or_else(PoisonError::into_inner);
    let (id, call) = rest.split_once('/').unwrap_or((rest, ""));
    match (asked.method, id, call) {
        (&Method::Get, "", _) => {
            let line = asked.query.get("line").map_or("", String::as_str);
            let group = match asked.query.get("group").map(String::as_str) {
                Some("status") => Group::Status,
                Some("project") => Group::Project,
                _ => Group::None,
            };
            match backend.list(&issues::parse(line)) {
                Ok(issues) => {
                    // A source that cannot say its states still lists; its
                    // groups then keep to their names.
                    let team = backend.team().ok().flatten();
                    let sections = issues::sections(issues, group, team.as_deref());
                    let ready = ready();
                    Reply::json(200, &list_json(&locked(), &ready, &sections))
                }
                Err(why) => Reply::error(502, &why),
            }
        }
        (&Method::Get, "team", _) => match backend.team() {
            Ok(team) => Reply::json(200, &json!({ "team": team.as_deref().map(team_json) })),
            Err(why) => Reply::error(502, &why),
        },
        (&Method::Get, id, "") => match backend.detail(id) {
            Ok(detail) => {
                let ready = ready();
                Reply::json(200, &detail_json(&locked(), &ready, &detail))
            }
            Err(why) => Reply::error(502, &why),
        },
        (&Method::Post, "", _) => create(backend, asked.body, app),
        (&Method::Post, id, "") => update(backend, id, asked.body, app),
        (&Method::Post, id, "comment") => comment(backend, id, asked.body),
        _ => Reply::error(404, "no such call"),
    }
}

/// An issue as a row of the list, with who is working on it.
fn issue_json(app: &App, ready: &[String], issue: &Issue) -> Value {
    let tree = app.tree_of_ticket(&issue.id);
    json!({
        "id": issue.id,
        "title": issue.title,
        "state": issue.state,
        "state_id": issue.state_id,
        "priority": issue.priority,
        "labels": issue.labels,
        "label_ids": issue.label_ids,
        "assignee": issue.assignee,
        "assignee_id": issue.assignee_id,
        "project": issue.project,
        "project_id": issue.project_id,
        "cycle": issue.cycle,
        "url": issue.url,
        "card": tree.map(|tree| card(app, tree, now())),
        "is_ready": tree.is_none() && ready.contains(&issue.id),
    })
}

fn list_json(app: &App, ready: &[String], sections: &[Section]) -> Value {
    let sections: Vec<Value> = sections
        .iter()
        .map(|section| {
            let rows: Vec<Value> = section
                .issues
                .iter()
                .map(|issue| issue_json(app, ready, issue))
                .collect();
            json!({
                "heading": section.heading,
                "is_closed": section.is_closed,
                "issues": rows,
            })
        })
        .collect();
    json!({ "sections": sections })
}

fn link_json(link: &Link) -> Value {
    json!({
        "kind": link.kind,
        "id": link.id,
        "title": link.title,
        "state": link.state,
        "is_done": link.is_done,
    })
}

fn detail_json(app: &App, ready: &[String], detail: &Detail) -> Value {
    let comments: Vec<Value> = detail
        .comments
        .iter()
        .map(|c| json!({ "author": c.author, "body": c.body, "created_at": c.created_at }))
        .collect();
    let links: Vec<Value> = detail.links.iter().map(link_json).collect();
    json!({
        "issue": issue_json(app, ready, &detail.issue),
        "description": detail.issue.description,
        "comments": comments,
        "links": links,
    })
}

fn team_json(team: &Team) -> Value {
    let named = |list: &[linear::Named]| -> Vec<Value> {
        list.iter()
            .map(|n| json!({ "id": n.id, "name": n.name }))
            .collect()
    };
    let states: Vec<Value> = team
        .states
        .iter()
        .map(|s| json!({ "id": s.id, "name": s.name, "is_closed": issues::is_closed(s) }))
        .collect();
    let cycles: Vec<Value> = team
        .cycles
        .iter()
        .map(|c| json!({ "number": c.number, "name": c.name, "is_active": c.is_active }))
        .collect();
    json!({
        "states": states,
        "labels": named(&team.labels),
        "members": named(&team.members),
        "projects": named(&team.projects),
        "cycles": cycles,
        "me": team.me.as_ref().map(|me| json!({ "id": me.id, "name": me.name })),
    })
}

/// A field a body may leave out, keep, or set to null to clear.
fn clearable<'de, D: Deserializer<'de>>(body: D) -> Result<Option<Option<String>>, D::Error> {
    Option::deserialize(body).map(Some)
}

/// A change to an issue. A field left out is left alone.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    title: Option<String>,
    description: Option<String>,
    state_id: Option<String>,
    #[serde(default, deserialize_with = "clearable")]
    assignee_id: Option<Option<String>>,
    label_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "clearable")]
    project_id: Option<Option<String>>,
    priority: Option<u8>,
}

fn update(backend: &dyn Backend, id: &str, body: &[u8], app: &Mutex<App>) -> Reply {
    let edit: Edit = match serde_json::from_slice(body) {
        Ok(edit) => edit,
        Err(err) => return Reply::error(400, &format!("not a change to an issue: {err}")),
    };
    let change = Change {
        title: edit.title,
        description: edit.description,
        state_id: edit.state_id,
        assignee_id: edit.assignee_id,
        label_ids: edit.label_ids,
        project_id: edit.project_id,
        priority: edit.priority,
    };
    if change.is_empty() {
        return Reply::error(400, "nothing to change");
    }
    let changed = backend
        .detail(id)
        .and_then(|detail| backend.update(&detail.issue, &change));
    match changed {
        Ok(issue) => {
            let app = app.lock().unwrap_or_else(PoisonError::into_inner);
            Reply::json(200, &issue_json(&app, &[], &issue))
        }
        Err(why) => Reply::error(502, &why),
    }
}

fn comment(backend: &dyn Backend, id: &str, body: &[u8]) -> Reply {
    #[derive(Deserialize)]
    struct Said {
        body: String,
    }
    let Ok(said) = serde_json::from_slice::<Said>(body) else {
        return Reply::error(400, "comment takes {\"body\": \"...\"}");
    };
    let text = said.body.trim();
    if text.is_empty() {
        return Reply::error(400, "nothing to say");
    }
    let posted = backend
        .detail(id)
        .and_then(|detail| backend.comment(&detail.issue, text));
    match posted {
        Ok(c) => Reply::json(
            200,
            &json!({ "author": c.author, "body": c.body, "created_at": c.created_at }),
        ),
        Err(why) => Reply::error(502, &why),
    }
}

fn create(backend: &dyn Backend, body: &[u8], app: &Mutex<App>) -> Reply {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Fresh {
        title: String,
        description: Option<String>,
        state_id: Option<String>,
        assignee_id: Option<String>,
        #[serde(default)]
        label_ids: Vec<String>,
        project_id: Option<String>,
        priority: Option<u8>,
    }
    let fresh: Fresh = match serde_json::from_slice(body) {
        Ok(fresh) => fresh,
        Err(err) => return Reply::error(400, &format!("not a new issue: {err}")),
    };
    let title = fresh.title.trim();
    if title.is_empty() {
        return Reply::error(400, "an issue needs a title");
    }
    let team = match backend.team() {
        Ok(Some(team)) => team,
        Ok(None) => return Reply::error(400, issues::READ_ONLY),
        Err(why) => return Reply::error(502, &why),
    };
    let new = NewIssue {
        team_id: team.id.clone(),
        title: title.to_string(),
        description: fresh.description.filter(|d| !d.trim().is_empty()),
        state_id: fresh.state_id,
        assignee_id: fresh.assignee_id,
        label_ids: fresh.label_ids,
        project_id: fresh.project_id,
        priority: fresh.priority.filter(|p| *p > 0),
    };
    match backend.create(&new) {
        Ok(issue) => {
            let app = app.lock().unwrap_or_else(PoisonError::into_inner);
            Reply::json(200, &issue_json(&app, &[], &issue))
        }
        Err(why) => Reply::error(502, &why),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use linear::{Comment, Named, State};

    use super::super::pool::tests::{declared, keys, pool as app_of_trees};
    use super::*;
    use crate::sidecar::Sidecar;

    /// A source that keeps what it was asked to change, refuses as told, and
    /// has a team unless it is read-only.
    #[derive(Default)]
    struct Fake {
        is_read_only: bool,
        refuses: Option<String>,
        changes: Mutex<Vec<Change>>,
        created: Mutex<Vec<NewIssue>>,
        listed: Mutex<Vec<issues::Filters>>,
    }

    fn issue(id: &str, state: &str) -> Issue {
        Issue {
            uuid: format!("uuid-{id}"),
            id: id.into(),
            title: format!("{id} title"),
            state: state.into(),
            state_id: Some(state.into()),
            ..Issue::default()
        }
    }

    impl Backend for Fake {
        fn name(&self) -> &str {
            "work"
        }

        fn list(&self, filters: &issues::Filters) -> Result<Vec<Issue>, String> {
            self.listed.lock().unwrap().push(filters.clone());
            Ok(vec![
                issue("END-1", "Doing"),
                issue("END-2", "Done"),
                issue("END-3", "Doing"),
            ])
        }

        fn detail(&self, id: &str) -> Result<Detail, String> {
            Ok(Detail {
                issue: Issue {
                    description: Some("It loops.".into()),
                    ..issue(id, "Doing")
                },
                comments: vec![Comment {
                    author: "Mia".into(),
                    body: "Seen on Safari.".into(),
                    created_at: "2026-10-30T10:00:00.000Z".into(),
                }],
                links: vec![Link {
                    kind: "blocked by".into(),
                    id: "END-9".into(),
                    title: "Store invoices".into(),
                    state: "Done".into(),
                    is_done: true,
                }],
            })
        }

        fn team(&self) -> Result<Option<Arc<Team>>, String> {
            if self.is_read_only {
                return Ok(None);
            }
            let state = |name: &str, kind: &str| State {
                id: name.into(),
                name: name.into(),
                kind: kind.into(),
                position: 0.0,
            };
            Ok(Some(Arc::new(Team {
                id: "team-1".into(),
                key: "END".into(),
                name: "End".into(),
                states: vec![state("Doing", "started"), state("Done", "completed")],
                labels: vec![Named {
                    id: "l1".into(),
                    name: "Bug".into(),
                }],
                members: Vec::new(),
                projects: Vec::new(),
                cycles: Vec::new(),
                me: None,
            })))
        }

        fn update(&self, before: &Issue, change: &Change) -> Result<Issue, String> {
            if let Some(why) = &self.refuses {
                return Err(why.clone());
            }
            assert_eq!(before.uuid, "uuid-END-1", "it changes the issue asked for");
            self.changes.lock().unwrap().push(change.clone());
            Ok(before.clone())
        }

        fn create(&self, new: &NewIssue) -> Result<Issue, String> {
            self.created.lock().unwrap().push(new.clone());
            Ok(issue("END-161", "Doing"))
        }

        fn comment(&self, issue: &Issue, body: &str) -> Result<Comment, String> {
            assert_eq!(issue.uuid, "uuid-END-1");
            Ok(Comment {
                author: "me".into(),
                body: body.into(),
                created_at: "2026-11-01T10:00:00.000Z".into(),
            })
        }
    }

    /// A pool with tree 17 working on END-1.
    fn pool() -> Mutex<App> {
        let mut app = app_of_trees();
        let mut record = Sidecar::leased_now();
        record.ticket = Some("END-1".into());
        app.apply_sidecars(HashMap::from([(PathBuf::from("/pool/17"), record)]));
        Mutex::new(app)
    }

    fn ask(
        backend: &Fake,
        method: &Method,
        rest: &str,
        query: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, Value) {
        let asked = Asked {
            method,
            path: "/api/issues",
            query: query
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            login: Some("me"),
            origin: None,
            host: None,
            body,
        };
        let reply = answer(backend, rest, &asked, &pool(), &|| vec!["END-3".into()]);
        (
            reply.status,
            serde_json::from_slice(&reply.body).unwrap_or(Value::Null),
        )
    }

    #[test]
    fn a_list_comes_grouped_with_who_works_on_what() {
        let fake = Fake::default();
        let (status, json) = ask(
            &fake,
            &Method::Get,
            "",
            &[("line", "mine label:Bug"), ("group", "status")],
            b"",
        );
        assert_eq!(status, 200);
        let asked = fake.listed.lock().unwrap().remove(0);
        assert!(asked.mine, "the line narrows the list");
        assert_eq!(asked.label.as_deref(), Some("Bug"));
        let sections = json["sections"].as_array().unwrap();
        assert_eq!(sections[0]["heading"], "Doing");
        assert_eq!(sections[0]["is_closed"], false);
        assert_eq!(sections[1]["heading"], "Done");
        assert_eq!(sections[1]["is_closed"], true);
        let rows = sections[0]["issues"].as_array().unwrap();
        assert_eq!(rows[0]["card"]["tree"], "17");
        assert_eq!(rows[0]["is_ready"], false);
        assert_eq!(rows[1]["card"], Value::Null);
        assert_eq!(rows[1]["is_ready"], true, "merges left it ready");
    }

    #[test]
    fn a_source_that_cannot_change_issues_says_so() {
        let fake = Fake {
            is_read_only: true,
            ..Fake::default()
        };
        assert_eq!(
            ask(&fake, &Method::Get, "team", &[], b"").1["team"],
            Value::Null
        );
        let (status, json) = ask(&fake, &Method::Post, "", &[], br#"{"title": "x"}"#);
        assert_eq!(
            (status, json["error"].as_str()),
            (400, Some(issues::READ_ONLY))
        );
    }

    #[test]
    fn a_change_touches_only_what_it_names_and_null_clears() {
        let fake = Fake::default();
        let body = br#"{"state_id": "Done", "assignee_id": null}"#;
        assert_eq!(ask(&fake, &Method::Post, "END-1", &[], body).0, 200);
        let body = br#"{"title": "New"}"#;
        assert_eq!(ask(&fake, &Method::Post, "END-1", &[], body).0, 200);
        let changes = fake.changes.lock().unwrap();
        assert_eq!(
            changes[0],
            Change {
                state_id: Some("Done".into()),
                assignee_id: Some(None),
                ..Change::default()
            }
        );
        assert_eq!(changes[1].assignee_id, None, "left out, left alone");
        assert_eq!(changes[1].title.as_deref(), Some("New"));
    }

    #[test]
    fn a_refused_change_says_why_and_nothing_is_not_a_change() {
        let fake = Fake {
            refuses: Some("Linear said no".into()),
            ..Fake::default()
        };
        let (status, json) = ask(&fake, &Method::Post, "END-1", &[], br#"{"title": "x"}"#);
        assert_eq!(
            (status, json["error"].as_str()),
            (502, Some("Linear said no"))
        );
        assert_eq!(ask(&fake, &Method::Post, "END-1", &[], b"{}").0, 400);
    }

    #[test]
    fn a_new_issue_goes_to_the_team_with_what_it_was_given() {
        let fake = Fake::default();
        let body = br#"{"title": " Fix it ", "description": " ", "priority": 0,
                        "label_ids": ["l1"], "assignee_id": null}"#;
        let (status, json) = ask(&fake, &Method::Post, "", &[], body);
        assert_eq!((status, json["id"].as_str()), (200, Some("END-161")));
        assert_eq!(
            fake.created.lock().unwrap()[0],
            NewIssue {
                team_id: "team-1".into(),
                title: "Fix it".into(),
                label_ids: vec!["l1".into()],
                ..NewIssue::default()
            }
        );
        assert_eq!(
            ask(&fake, &Method::Post, "", &[], br#"{"title": " "}"#).0,
            400
        );
    }

    #[test]
    fn views_that_repeat_one_are_dropped_and_the_config_s_own_follow() {
        let root = crate::jobs::tests::config_root("views");
        if !crate::jobs::tests::in_process_of_its_own(
            "serve::issues::tests::views_that_repeat_one_are_dropped_and_the_config_s_own_follow",
            &root,
        ) {
            return;
        }
        std::fs::write(
            root.join("config/treetop/config.toml"),
            "[views]\nagain = \"group:status\"\nbugs = \"label:Bug\"\n",
        )
        .unwrap();
        let names: Vec<String> = views().into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["mine", "all", "by project", "this cycle", "bugs"]);
    }

    #[test]
    fn a_source_is_the_one_named_else_the_repository_s_own() {
        struct Named(&'static str);
        impl Backend for Named {
            fn name(&self) -> &str {
                self.0
            }
            fn list(&self, _: &issues::Filters) -> Result<Vec<Issue>, String> {
                Ok(Vec::new())
            }
            fn detail(&self, _: &str) -> Result<Detail, String> {
                Ok(Detail::default())
            }
        }
        let backends: Vec<Arc<dyn Backend>> =
            vec![Arc::new(Named("personal")), Arc::new(Named("work"))];
        let name = |own: &Own, source| {
            chosen(&backends, own, source).map(|backend| backend.name().to_string())
        };
        assert_eq!(name(&Ok(0), Some("work")), Ok("work".to_string()));
        assert_eq!(name(&Ok(1), None), Ok("work".to_string()));
        let none: Own = Err("no source here".into());
        assert_eq!(name(&none, Some("personal")), Ok("personal".to_string()));
        assert_eq!(name(&none, None), Err((409, "no source here".to_string())));
        assert_eq!(
            name(&Ok(0), Some("gone")),
            Err((404, "no issue source gone".to_string()))
        );
    }

    #[test]
    fn the_json_has_exactly_the_fields_the_app_reads() {
        let fake = Fake::default();
        let (_, list) = ask(&fake, &Method::Get, "", &[("group", "status")], b"");
        assert_eq!(keys(&list), declared("IssueList"));
        let section = &list["sections"][0];
        assert_eq!(keys(section), declared("IssueSection"));
        assert_eq!(keys(&section["issues"][0]), declared("IssueRow"));
        let (_, detail) = ask(&fake, &Method::Get, "END-1", &[], b"");
        assert_eq!(keys(&detail), declared("IssueDetail"));
        assert_eq!(keys(&detail["comments"][0]), declared("IssueComment"));
        assert_eq!(keys(&detail["links"][0]), declared("IssueLink"));
        let (_, comment) = ask(
            &fake,
            &Method::Post,
            "END-1/comment",
            &[],
            br#"{"body": "hi"}"#,
        );
        assert_eq!(keys(&comment), declared("IssueComment"));
        let (_, team) = ask(&fake, &Method::Get, "team", &[], b"");
        assert_eq!(keys(&team["team"]), declared("IssueTeam"));
        let backends: Vec<Arc<dyn Backend>> = vec![Arc::new(Fake::default())];
        let sources = sources_json(&backends, &Ok(0), &views());
        assert_eq!(keys(&sources), declared("IssueSources"));
        assert_eq!(sources["own"], "work");
        assert_eq!(keys(&sources["views"][0]), declared("IssueView"));
        let none = sources_json(&backends, &Err("no source".into()), &[]);
        assert_eq!(none["own"], Value::Null);
        assert_eq!(none["why"], "no source");
    }
}
