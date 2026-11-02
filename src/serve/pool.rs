//! The pool as the phone draws it: the list's groups in the list's order,
//! with the trees waiting on you lifted out above them, and each tree as a
//! card carrying what its row shows. Built from the screen's own `App`, so
//! the marks, order and grouping are the list's, not a second reading.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::agents::Status;
use crate::app::{App, Group};
use crate::pool::Tree;
use crate::reports::State;

/// The whole Pool screen.
pub fn pool_json(app: &App, workspace: &str) -> Value {
    let now = now();
    let main = app.trees.iter().find(|t| t.is_main);
    let mut needs_you = Vec::new();
    let mut groups = Vec::new();
    for base in app.bases_in_use() {
        let group = Group::Base(base.clone());
        let mut cards = Vec::new();
        for tree in members(app, &group) {
            if app.needs_you(tree) {
                needs_you.push(card(app, tree, now));
            } else {
                cards.push(card(app, tree, now));
            }
        }
        let checkout = main
            .filter(|m| m.branch.as_deref() == Some(base.as_str()))
            .map(|m| json!({ "ports": m.ports(), "git": git(m) }));
        if cards.is_empty() && checkout.is_none() {
            continue;
        }
        groups.push(json!({
            "base": base,
            "checkout": checkout,
            "cards": cards,
        }));
    }
    let parked: Vec<Value> = members(app, &Group::Parked)
        .into_iter()
        .map(|t| card(app, t, now))
        .collect();
    json!({
        "workspace": workspace,
        // The listing on show is the last one saved, not yet a live one.
        "is_stale": app.is_cached,
        "error": app.error,
        "needs_you": needs_you,
        "groups": groups,
        "free": members(app, &Group::Free).len(),
        "parked": parked,
        "bases": app.bases_in_use(),
    })
}

/// Seconds since the epoch, which `active_ago` counts back from.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The trees of `group` in the list's order, folded or not.
fn members<'a>(app: &'a App, group: &Group) -> Vec<&'a Tree> {
    let mut trees: Vec<&Tree> = app
        .trees
        .iter()
        .filter(|t| app.group_of(t).as_ref() == Some(group))
        .collect();
    trees.sort_by_cached_key(|t| app.order_in_group(t));
    trees
}

/// One tree as a card: what its row in the list says.
pub fn card(app: &App, tree: &Tree, now: u64) -> Value {
    let agents = App::agents_of(tree);
    let says = app.says(tree);
    let mark = match (says.as_ref().map(|i| i.state), agents.first()) {
        (Some(State::Blocked), _) => "waiting",
        (Some(State::Done), _) => "done",
        (Some(State::Failed), _) => "failed",
        (_, Some(agent)) => match agent.status {
            Some(Status::Waiting) => "waiting",
            Some(Status::Busy) => "busy",
            Some(Status::Idle) => "idle",
            Some(Status::Cold) => "cold",
            None => "unknown",
        },
        (_, None) if tree.is_held() && app.is_agents_read => "no-agent",
        (_, None) => "none",
    };
    let more = agents
        .iter()
        .skip(1)
        .filter(|a| a.status != Some(Status::Cold))
        .count();
    let ticket = tree.sidecar.as_ref().and_then(|s| s.ticket.clone());
    json!({
        "tree": tree.name,
        "title": title(tree.branch.as_deref(), ticket.as_deref()),
        "branch": tree.branch,
        "ticket": ticket,
        "mark": mark,
        "agent": agents.first().map(|a| a.name.clone()),
        "more": more,
        "says": says.map(|i| i.report.text.clone()).filter(|t| !t.is_empty()),
        "needs_you": app.needs_you(tree),
        "parked": App::is_parked(tree),
        "git": git(tree),
        "ports": tree.ports(),
        "pr": tree.ship.as_ref().map(|s| json!({
            "label": s.label,
            "stage": s.stage,
            "url": s.url,
        })),
        "active_ago": app.active_at(tree).map(|at| now.saturating_sub(at)),
    })
}

/// The tree's git state as the GIT column counts it; `unknown` where git
/// could not read a held tree, and null for a free one.
fn git(tree: &Tree) -> Value {
    match tree.work {
        Some(w) => json!({
            "unpushed": w.unpushed,
            "staged": w.staged,
            "unstaged": w.unstaged,
            "untracked": w.untracked,
        }),
        None if tree.is_held() => json!("unknown"),
        None => Value::Null,
    }
}

/// What a card is headed with: the ticket and the slug after it, read off
/// the branch's last segment, as `END-123 fix-login` for
/// `anthony/END-123-fix-login`; the segment alone when it carries no
/// ticket; None for a tree on no branch.
pub fn title(branch: Option<&str>, ticket: Option<&str>) -> Option<String> {
    let segment = branch?.rsplit('/').next()?;
    let found;
    let ticket = match ticket {
        Some(ticket) => ticket,
        None => match leading_ticket(segment) {
            Some(id) => {
                found = id;
                &found
            }
            None => return Some(segment.to_string()),
        },
    };
    let rest = segment
        .get(..ticket.len())
        .filter(|head| head.eq_ignore_ascii_case(ticket))
        .map(|_| segment[ticket.len()..].trim_start_matches(['-', '_']));
    Some(match rest {
        Some("") => ticket.to_string(),
        Some(rest) => format!("{ticket} {rest}"),
        None => format!("{ticket} {segment}"),
    })
}

/// The ticket a branch segment starts with, as Linear names branches,
/// `anb-14-treetop-mobile` giving `ANB-14`: letters, a dash, digits, then a
/// dash or the end.
fn leading_ticket(segment: &str) -> Option<String> {
    let (team, rest) = segment.split_once('-')?;
    let digits = rest.split('-').next()?;
    let is_team = !team.is_empty() && team.chars().all(|c| c.is_ascii_alphabetic());
    let is_number = !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit());
    (is_team && is_number).then(|| format!("{}-{digits}", team.to_ascii_uppercase()))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn a_card_is_headed_by_its_ticket_and_slug() {
        let cases = [
            (
                Some("anthony/END-123-fix-login"),
                Some("END-123"),
                Some("END-123 fix-login"),
            ),
            (
                Some("anthony/end-123-fix-login"),
                Some("END-123"),
                Some("END-123 fix-login"),
            ),
            (Some("anthony/END-123"), Some("END-123"), Some("END-123")),
            (Some("feature/copy-tweak"), None, Some("copy-tweak")),
            (Some("main"), None, Some("main")),
            (
                Some("anthony/fix-login"),
                Some("END-9"),
                Some("END-9 fix-login"),
            ),
            (
                Some("anthonyb8/anb-14-treetop-mobile"),
                None,
                Some("ANB-14 treetop-mobile"),
            ),
            (Some("v2-release"), None, Some("v2-release")),
            (None, Some("END-1"), None),
        ];
        for (branch, ticket, want) in cases {
            assert_eq!(
                title(branch, ticket).as_deref(),
                want,
                "{branch:?} {ticket:?}"
            );
        }
    }

    use std::collections::{BTreeSet, HashMap};
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::agents::{Agent, Source};
    use crate::pool::Work;
    use crate::reports::Report;

    pub fn tree(name: &str, branch: &str) -> Tree {
        Tree {
            name: name.into(),
            status: "leased".into(),
            branch: Some(branch.into()),
            holder: Some("me".into()),
            path: PathBuf::from(format!("/pool/{name}")),
            leased_at: None,
            processes: Vec::new(),
            agents: Vec::new(),
            work: Some(Work::default()),
            is_main: false,
            sidecar: None,
            ship: None,
        }
    }

    fn agent(name: &str, status: Status, active_at: u64) -> Agent {
        Agent {
            name: name.into(),
            pid: Some(1),
            status: Some(status),
            source: Source::Plugin,
            is_background: true,
            attach: None,
            job: Some(format!("job-{name}")),
            plugin: Some("claude".into()),
            active_at: Some(active_at),
            context: None,
            spend: None,
        }
    }

    /// A pool of four held trees and one free: 12 idle, 14 busy and more
    /// recently active, 17 whose agent asks a question, 9 with no agent.
    pub fn pool() -> App {
        let mut app = App::new(None);
        let free = Tree {
            status: "available".into(),
            branch: None,
            holder: None,
            work: None,
            ..tree("3", "")
        };
        app.set_trees(vec![
            tree("12", "me/END-131-copy-tweak"),
            tree("14", "me/END-140-rate-limits"),
            tree("17", "me/END-123-fix-login"),
            tree("9", "me/END-118-csv-export"),
            free,
        ]);
        app.apply_agents(&HashMap::from([
            (
                PathBuf::from("/pool/12"),
                vec![agent("copy", Status::Idle, 100)],
            ),
            (
                PathBuf::from("/pool/14"),
                vec![agent("limits", Status::Busy, 300)],
            ),
            (
                PathBuf::from("/pool/17"),
                vec![agent("login", Status::Busy, 200)],
            ),
        ]));
        app.apply_reports(vec![Report {
            job: "job-login".into(),
            name: "login".into(),
            state: State::Blocked,
            text: "Clerk or Supabase?".into(),
            dir: Some(PathBuf::from("/pool/17")),
            updated: UNIX_EPOCH + Duration::from_secs(200),
            steps: Vec::new(),
        }]);
        app
    }

    #[test]
    fn the_pool_lifts_what_needs_you_and_keeps_the_list_s_order() {
        let json = pool_json(&pool(), "app");
        let trees = |cards: &Value| -> Vec<String> {
            cards
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["tree"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(trees(&json["needs_you"]), ["17"]);
        assert_eq!(json["needs_you"][0]["mark"], "waiting");
        assert_eq!(json["needs_you"][0]["says"], "Clerk or Supabase?");
        let cards = &json["groups"][0]["cards"];
        assert_eq!(trees(cards), ["14", "12", "9"]);
        let marks: Vec<&str> = cards
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["mark"].as_str().unwrap())
            .collect();
        assert_eq!(marks, ["busy", "idle", "no-agent"]);
        assert_eq!(json["free"], 1);
    }

    /// The fields an interface in `web/src/api.ts` declares itself, by
    /// name; those it extends are the other interface's.
    pub fn declared(interface: &str) -> BTreeSet<String> {
        let api = include_str!("../../web/src/api.ts");
        let start = [" {", " extends "]
            .iter()
            .find_map(|after| api.find(&format!("export interface {interface}{after}")))
            .unwrap_or_else(|| panic!("api.ts has no {interface}"));
        let body = &api[start..];
        let body = &body[body.find('{').unwrap() + 1..body.find("\n}").unwrap()];
        body.lines()
            .filter_map(|line| line.trim().split_once(':'))
            .map(|(name, _)| name.trim().trim_end_matches('?').to_string())
            .filter(|name| !name.starts_with('/') && !name.starts_with('*'))
            .collect()
    }

    pub fn keys(value: &Value) -> BTreeSet<String> {
        value.as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn the_json_has_exactly_the_fields_the_app_reads() {
        let json = pool_json(&pool(), "app");
        assert_eq!(keys(&json), declared("Pool"));
        assert_eq!(keys(&json["needs_you"][0]), declared("Card"));
        assert_eq!(keys(&json["groups"][0]), declared("Group"));
        assert_eq!(keys(&json["groups"][0]["cards"][0]["git"]), declared("Git"));
    }
}
