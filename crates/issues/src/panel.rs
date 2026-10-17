//! The filter panel: a row per thing the list can be narrowed by, each set
//! from a picker of what the team has, and the filters said in words for the
//! bar above the list.

use linear::{ANY_STATE, Team, UNASSIGNED};

use crate::Filters;
use crate::picker::Choice;
use crate::views::Group;

/// What `mine` is picked as among the assignees.
const ME: &str = "@me";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Assignee,
    Status,
    Label,
    Project,
    Cycle,
    Group,
}

pub const ROWS: [Row; 6] = [
    Row::Assignee,
    Row::Status,
    Row::Label,
    Row::Project,
    Row::Cycle,
    Row::Group,
];

impl Row {
    pub fn label(self) -> &'static str {
        match self {
            Row::Assignee => "Assignee",
            Row::Status => "Status",
            Row::Label => "Label",
            Row::Project => "Project",
            Row::Cycle => "Cycle",
            Row::Group => "Group by",
        }
    }
}

fn choice(id: Option<&str>, name: &str) -> Choice {
    Choice {
        id: id.map(str::to_string),
        name: name.to_string(),
    }
}

/// What row `row` can be set to, the first clearing it. Without the team,
/// only what needs no names of its own.
pub fn choices(row: Row, team: Option<&Team>) -> Vec<Choice> {
    fn named(names: Vec<&str>) -> Vec<Choice> {
        names.into_iter().map(|n| choice(Some(n), n)).collect()
    }
    let mut choices = match row {
        Row::Assignee => vec![
            choice(None, "anyone"),
            choice(Some(ME), "me"),
            choice(Some(UNASSIGNED), "unassigned"),
        ],
        Row::Status => vec![
            choice(None, "open issues"),
            choice(Some(ANY_STATE), "all, done and canceled too"),
        ],
        Row::Label => vec![choice(None, "any label")],
        Row::Project => vec![choice(None, "any project")],
        Row::Cycle => vec![
            choice(None, "any cycle"),
            choice(Some("current"), "the current cycle"),
        ],
        Row::Group => vec![
            choice(None, "nothing"),
            choice(Some("project"), "project"),
            choice(Some("status"), "status"),
        ],
    };
    let Some(team) = team else {
        return choices;
    };
    match row {
        Row::Assignee => choices.extend(named(
            team.members
                .iter()
                .filter(|m| team.me.as_ref().is_none_or(|me| me.id != m.id))
                .map(|m| m.name.as_str())
                .collect(),
        )),
        Row::Status => choices.extend(named(team.states.iter().map(|s| s.name.as_str()).collect())),
        Row::Label => choices.extend(named(team.labels.iter().map(|l| l.name.as_str()).collect())),
        Row::Project => {
            choices.extend(named(
                team.projects.iter().map(|p| p.name.as_str()).collect(),
            ));
        }
        Row::Cycle => choices.extend(team.cycles.iter().rev().map(|c| {
            let name = match &c.name {
                Some(name) => format!("{name} (cycle {})", c.number),
                None => format!("cycle {}", c.number),
            };
            choice(Some(&c.number.to_string()), &name)
        })),
        Row::Group => {}
    }
    choices
}

/// What row `row` is set to now, by the id its choice carries.
pub fn current(row: Row, filters: &Filters, group: Group) -> Option<String> {
    match row {
        Row::Assignee if filters.mine => Some(ME.to_string()),
        Row::Assignee => filters.assignee.clone(),
        Row::Status => filters.state.clone(),
        Row::Label => filters.label.clone(),
        Row::Project => filters.project.clone(),
        Row::Cycle => filters.cycle.clone(),
        Row::Group => match group {
            Group::None => None,
            Group::Project => Some("project".into()),
            Group::Status => Some("status".into()),
        },
    }
}

/// Sets row `row` to the choice with id `id`; None clears it.
pub fn set(row: Row, id: Option<String>, filters: &mut Filters, group: &mut Group) {
    match row {
        Row::Assignee => {
            filters.mine = id.as_deref() == Some(ME);
            filters.assignee = id.filter(|id| id != ME);
        }
        Row::Status => filters.state = id,
        Row::Label => filters.label = id,
        Row::Project => filters.project = id,
        Row::Cycle => filters.cycle = id,
        Row::Group => {
            *group = match id.as_deref() {
                Some("project") => Group::Project,
                Some("status") => Group::Status,
                _ => Group::None,
            }
        }
    }
}

/// Row `row`'s value in words, as the panel shows it.
pub fn value(row: Row, filters: &Filters, group: Group) -> String {
    let id = current(row, filters, group);
    choices(row, None)
        .into_iter()
        .find(|c| c.id == id)
        .map(|c| c.name)
        .or(id)
        .unwrap_or_default()
}

/// The filters in words, a piece each, for the bar above the list: whose
/// issues, which, then whatever else narrows them.
pub fn describe(filters: &Filters, group: Group) -> Vec<String> {
    let whose = if filters.mine {
        "my issues".to_string()
    } else {
        match filters.assignee.as_deref() {
            Some(name) if name.eq_ignore_ascii_case(UNASSIGNED) => "unassigned issues".to_string(),
            Some(name) => format!("{name}'s issues"),
            None => "everyone's issues".to_string(),
        }
    };
    let which = match filters.state.as_deref() {
        None => "open".to_string(),
        Some(state) if state.eq_ignore_ascii_case(ANY_STATE) => "all states".to_string(),
        Some(state) => state.to_string(),
    };
    let mut pieces = vec![whose, which];
    pieces.extend(filters.label.as_ref().map(|l| format!("label {l}")));
    pieces.extend(filters.project.as_ref().map(|p| format!("project {p}")));
    pieces.extend(filters.cycle.as_ref().map(|c| match c.as_str() {
        "current" => "this cycle".to_string(),
        n => format!("cycle {n}"),
    }));
    match group {
        Group::None => {}
        Group::Project => pieces.push("by project".into()),
        Group::Status => pieces.push("by status".into()),
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_row_sets_its_filter_and_says_it() {
        let (mut filters, mut group) = (Filters::default(), Group::None);
        set(Row::Assignee, Some(ME.into()), &mut filters, &mut group);
        assert!(filters.mine);
        assert_eq!(value(Row::Assignee, &filters, group), "me");
        set(Row::Assignee, Some("Zoe".into()), &mut filters, &mut group);
        assert_eq!(
            (filters.mine, filters.assignee.as_deref()),
            (false, Some("Zoe"))
        );
        set(
            Row::Status,
            Some(ANY_STATE.into()),
            &mut filters,
            &mut group,
        );
        set(Row::Label, Some("Bug".into()), &mut filters, &mut group);
        set(Row::Cycle, Some("current".into()), &mut filters, &mut group);
        set(Row::Group, Some("status".into()), &mut filters, &mut group);
        assert_eq!(
            describe(&filters, group),
            [
                "Zoe's issues",
                "all states",
                "label Bug",
                "this cycle",
                "by status"
            ]
        );
        set(Row::Assignee, None, &mut filters, &mut group);
        assert_eq!(describe(&filters, group)[0], "everyone's issues");
        assert_eq!(value(Row::Label, &filters, group), "Bug");
        assert_eq!(value(Row::Project, &filters, group), "any project");
    }
}
