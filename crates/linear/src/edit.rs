//! Changes to issues, as Linear's `IssueUpdateInput` and `IssueCreateInput`
//! take them: only the fields a change names are sent, so nothing else on
//! the issue is touched.

use serde_json::{Map, Value, json};

/// A change to an existing issue. A field left `None` is left alone; an
/// assignee or project of `Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Change {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state_id: Option<String>,
    pub assignee_id: Option<Option<String>>,
    /// The whole set of labels the issue ends with.
    pub label_ids: Option<Vec<String>>,
    pub project_id: Option<Option<String>>,
    pub priority: Option<u8>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        *self == Change::default()
    }

    pub(crate) fn input(&self) -> Value {
        let mut input = Map::new();
        let mut set = |key: &str, value: Value| {
            input.insert(key.to_string(), value);
        };
        if let Some(title) = &self.title {
            set("title", json!(title));
        }
        if let Some(description) = &self.description {
            set("description", json!(description));
        }
        if let Some(state) = &self.state_id {
            set("stateId", json!(state));
        }
        if let Some(assignee) = &self.assignee_id {
            set("assigneeId", json!(assignee));
        }
        if let Some(labels) = &self.label_ids {
            set("labelIds", json!(labels));
        }
        if let Some(project) = &self.project_id {
            set("projectId", json!(project));
        }
        if let Some(priority) = self.priority {
            set("priority", json!(priority));
        }
        Value::Object(input)
    }
}

/// A new issue. Only the team and title are needed; Linear fills the rest
/// with the team's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewIssue {
    pub team_id: String,
    pub title: String,
    pub description: Option<String>,
    pub state_id: Option<String>,
    pub assignee_id: Option<String>,
    pub label_ids: Vec<String>,
    pub project_id: Option<String>,
    pub priority: Option<u8>,
}

impl NewIssue {
    pub(crate) fn input(&self) -> Value {
        let mut input = Map::new();
        input.insert("teamId".into(), json!(self.team_id));
        input.insert("title".into(), json!(self.title));
        let optional = [
            ("description", self.description.as_ref()),
            ("stateId", self.state_id.as_ref()),
            ("assigneeId", self.assignee_id.as_ref()),
            ("projectId", self.project_id.as_ref()),
        ];
        for (key, value) in optional {
            if let Some(value) = value {
                input.insert(key.into(), json!(value));
            }
        }
        if !self.label_ids.is_empty() {
            input.insert("labelIds".into(), json!(self.label_ids));
        }
        if let Some(priority) = self.priority {
            input.insert("priority".into(), json!(priority));
        }
        Value::Object(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_sends_only_what_it_names_and_null_to_clear() {
        let change = Change {
            state_id: Some("s1".into()),
            assignee_id: Some(None),
            label_ids: Some(vec!["l1".into(), "l2".into()]),
            ..Change::default()
        };
        assert_eq!(
            change.input(),
            json!({ "stateId": "s1", "assigneeId": null, "labelIds": ["l1", "l2"] })
        );
        assert!(Change::default().is_empty());
        assert_eq!(Change::default().input(), json!({}));
    }

    #[test]
    fn a_new_issue_sends_its_team_title_and_what_it_was_given() {
        let new = NewIssue {
            team_id: "t1".into(),
            title: "Fix login".into(),
            assignee_id: Some("u1".into()),
            ..NewIssue::default()
        };
        assert_eq!(
            new.input(),
            json!({ "teamId": "t1", "title": "Fix login", "assigneeId": "u1" })
        );
    }
}
