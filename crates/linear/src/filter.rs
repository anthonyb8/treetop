//! What narrows a list of issues, as Linear's `IssueFilter` takes it.

use serde_json::{Map, Value, json};

/// Each field is optional; none lists the team's open issues.
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

impl Filter {
    /// The `IssueFilter` this asks for, in team `team`.
    pub(crate) fn to_value(&self, team: Option<&str>) -> Value {
        let mut out = Map::new();
        if let Some(team) = team {
            out.insert("team".into(), json!({ "key": { "eqIgnoreCase": team } }));
        }
        if self.mine {
            out.insert("assignee".into(), json!({ "isMe": { "eq": true } }));
        }
        if let Some(label) = &self.label {
            out.insert(
                "labels".into(),
                json!({ "some": { "name": { "eqIgnoreCase": label } } }),
            );
        }
        match &self.state {
            Some(state) => out.insert("state".into(), json!({ "name": { "eqIgnoreCase": state } })),
            None => out.insert(
                "state".into(),
                json!({ "type": { "nin": ["completed", "canceled"] } }),
            ),
        };
        if let Some(project) = &self.project {
            out.insert(
                "project".into(),
                json!({ "name": { "containsIgnoreCase": project } }),
            );
        }
        match self.cycle.as_deref() {
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
        if let Some(query) = self.query.as_deref().filter(|q| !q.trim().is_empty()) {
            out.insert(
                "title".into(),
                json!({ "containsIgnoreCase": query.trim() }),
            );
        }
        Value::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn becomes_an_issue_filter_open_issues_by_default() {
        let none = Filter::default().to_value(Some("END"));
        assert_eq!(
            none,
            json!({
                "team": { "key": { "eqIgnoreCase": "END" } },
                "state": { "type": { "nin": ["completed", "canceled"] } },
            })
        );
        let all = Filter {
            mine: true,
            label: Some("Bug".into()),
            state: Some("In Review".into()),
            project: Some("checkout".into()),
            cycle: Some("current".into()),
            query: Some(" login ".into()),
        }
        .to_value(None);
        assert_eq!(all["assignee"], json!({ "isMe": { "eq": true } }));
        assert_eq!(all["labels"]["some"]["name"]["eqIgnoreCase"], "Bug");
        assert_eq!(
            all["state"],
            json!({ "name": { "eqIgnoreCase": "In Review" } })
        );
        assert_eq!(all["cycle"], json!({ "isActive": { "eq": true } }));
        assert_eq!(all["title"], json!({ "containsIgnoreCase": "login" }));
        let numbered = Filter {
            cycle: Some("12".into()),
            ..Filter::default()
        }
        .to_value(None);
        assert_eq!(numbered["cycle"], json!({ "number": { "eq": 12 } }));
    }
}
