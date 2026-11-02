//! Views: a named filter line and how the list is grouped, one number key
//! each. Five are built in; the config adds its own after them.

use linear::{Issue, Team};

use crate::{Filters, parse};

/// How the list is cut into groups, each under a heading.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Group {
    #[default]
    None,
    /// By project, by name, issues in none last.
    Project,
    /// By status, in the team's workflow order.
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub name: String,
    pub filters: Filters,
    pub group: Group,
}

impl View {
    /// A view named `name` from a filter line, which may also say
    /// `group:project` or `group:status`.
    pub fn parse(name: &str, text: &str) -> View {
        let mut group = Group::None;
        let rest: Vec<&str> = text
            .split_whitespace()
            .filter(|word| match *word {
                "group:project" => {
                    group = Group::Project;
                    false
                }
                "group:status" => {
                    group = Group::Status;
                    false
                }
                _ => true,
            })
            .collect();
        View {
            name: name.to_string(),
            filters: parse(&rest.join(" ")),
            group,
        }
    }
}

/// Mine and all, grouped by status as Linear shows them, by project, by
/// status and the current cycle, on keys 1 to 5.
pub fn builtin() -> Vec<View> {
    [
        ("mine", "mine group:status"),
        ("all", "group:status"),
        ("by project", "group:project"),
        ("by status", "group:status"),
        ("this cycle", "cycle:current"),
    ]
    .iter()
    .map(|(name, text)| View::parse(name, text))
    .collect()
}

/// Where an issue sorts under `group`: its group's place, then the group's
/// heading. Issues keep their own order within a group.
pub fn place(issue: &Issue, group: Group, team: Option<&Team>) -> (usize, String) {
    match group {
        Group::None => (0, String::new()),
        Group::Project => match &issue.project {
            Some(project) => (0, project.clone()),
            None => (1, "No project".into()),
        },
        Group::Status => {
            let at = team
                .and_then(|team| {
                    status_order(team)
                        .iter()
                        .position(|s| Some(&s.id) == issue.state_id.as_ref())
                })
                .unwrap_or(usize::MAX);
            (at, issue.state.clone())
        }
    }
}

/// One group of a list, under its heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    /// Empty for a list with no groups.
    pub heading: String,
    /// Its issues' work is over, so it comes folded until opened by hand.
    pub is_closed: bool,
    pub issues: Vec<Issue>,
}

/// `issues` cut into the groups of `group`, in the order the groups stack,
/// each keeping Linear's order, for a program that draws the list itself.
pub fn sections(mut issues: Vec<Issue>, group: Group, team: Option<&Team>) -> Vec<Section> {
    issues.sort_by_cached_key(|issue| place(issue, group, team));
    let is_closed_state = |name: &str| {
        group == Group::Status
            && team.is_some_and(|t| t.states.iter().any(|s| s.name == name && is_closed(s)))
    };
    let mut sections: Vec<Section> = Vec::new();
    for issue in issues {
        let (_, heading) = place(&issue, group, team);
        match sections.last_mut() {
            Some(last) if last.heading == heading => last.issues.push(issue),
            _ => sections.push(Section {
                is_closed: is_closed_state(&heading),
                heading,
                issues: vec![issue],
            }),
        }
    }
    sections
}

/// A state's work is over: done, cancelled, or a duplicate, which Linear
/// gives a type of its own.
pub fn is_closed(state: &linear::State) -> bool {
    matches!(state.kind.as_str(), "completed" | "canceled" | "duplicate")
}

/// The team's states as the status groups stack: what is still open, the
/// state closest to done first, then the closed ones in workflow order.
fn status_order(team: &Team) -> Vec<&linear::State> {
    let (closed, open): (Vec<&linear::State>, Vec<&linear::State>) =
        team.states.iter().partition(|s| is_closed(s));
    open.into_iter().rev().chain(closed).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_stack_as_the_status_groups_do_and_closed_ones_say_so() {
        let state = |id: &str, kind: &str| linear::State {
            id: id.into(),
            name: id.into(),
            kind: kind.into(),
            position: 0.0,
        };
        let team = Team {
            id: "t".into(),
            key: "END".into(),
            name: "End".into(),
            states: vec![
                state("Todo", "unstarted"),
                state("Doing", "started"),
                state("Done", "completed"),
            ],
            labels: Vec::new(),
            members: Vec::new(),
            projects: Vec::new(),
            cycles: Vec::new(),
            me: None,
        };
        let issue = |id: &str, state: &str, project: Option<&str>| Issue {
            id: id.into(),
            state: state.into(),
            state_id: Some(state.into()),
            project: project.map(str::to_string),
            ..Issue::default()
        };
        let issues = vec![
            issue("A", "Done", None),
            issue("B", "Todo", Some("Web")),
            issue("C", "Doing", Some("Web")),
            issue("D", "Todo", None),
        ];
        let said = |group| -> Vec<(String, bool, Vec<String>)> {
            sections(issues.clone(), group, Some(&team))
                .into_iter()
                .map(|s| {
                    let ids = s.issues.into_iter().map(|i| i.id).collect();
                    (s.heading, s.is_closed, ids)
                })
                .collect()
        };
        assert_eq!(
            said(Group::Status),
            [
                ("Doing".to_string(), false, vec!["C".to_string()]),
                (
                    "Todo".to_string(),
                    false,
                    vec!["B".to_string(), "D".to_string()]
                ),
                ("Done".to_string(), true, vec!["A".to_string()]),
            ]
        );
        let by_project = said(Group::Project);
        assert_eq!(by_project[0].0, "Web");
        assert_eq!(by_project[1].0, "No project", "none last");
        assert_eq!(said(Group::None)[0].2.len(), 4, "one section, whole");
    }

    #[test]
    fn a_views_line_may_say_how_to_group() {
        let view = View::parse("bugs", "mine label:Bug group:status");
        assert_eq!(view.group, Group::Status);
        assert!(view.filters.mine);
        assert_eq!(view.filters.label.as_deref(), Some("Bug"));
        assert_eq!(view.filters.query, None, "group is no search word");
        let names: Vec<String> = builtin().into_iter().map(|v| v.name).collect();
        assert_eq!(
            names,
            ["mine", "all", "by project", "by status", "this cycle"]
        );
    }

    #[test]
    fn projects_sort_by_name_with_none_last() {
        let auth = Issue {
            project: Some("Auth".into()),
            ..Issue::default()
        };
        let none = Issue::default();
        assert!(place(&auth, Group::Project, None) < place(&none, Group::Project, None));
    }
}
