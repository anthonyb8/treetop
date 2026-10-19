//! What narrows a list of issues, as Linear's `IssueFilter` takes it.

use serde_json::{Map, Value, json};

/// The assignee that asks for issues assigned to nobody.
pub const UNASSIGNED: &str = "unassigned";

/// The state that asks for issues in any state, done and canceled too.
pub const ANY_STATE: &str = "any";

/// Each field is optional; none lists the team's open issues.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    /// Only issues assigned to the key's user.
    pub mine: bool,
    /// A person's name, or UNASSIGNED.
    pub assignee: Option<String>,
    pub label: Option<String>,
    /// A state's name, or ANY_STATE; without one, only open issues are
    /// listed.
    pub state: Option<String>,
    /// Part of a project's name.
    pub project: Option<String>,
    /// `current` for the active cycle, else a cycle number.
    pub cycle: Option<String>,
    /// Words to search for: each in the title, or an issue's number, as
    /// `END-123` or `123`.
    pub query: Option<String>,
}

/// The number in `word`, an issue's id such as `END-123` or a bare `123`.
fn number(word: &str) -> Option<u32> {
    let digits = word.rsplit_once('-').map_or(word, |(_, n)| n);
    digits.parse().ok()
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
        } else if let Some(assignee) = &self.assignee {
            let filter = if assignee.eq_ignore_ascii_case(UNASSIGNED) {
                json!({ "null": true })
            } else {
                json!({ "name": { "eqIgnoreCase": assignee } })
            };
            out.insert("assignee".into(), filter);
        }
        if let Some(label) = &self.label {
            out.insert(
                "labels".into(),
                json!({ "some": { "name": { "eqIgnoreCase": label } } }),
            );
        }
        match &self.state {
            Some(state) if state.eq_ignore_ascii_case(ANY_STATE) => None,
            Some(state) => out.insert("state".into(), json!({ "name": { "eqIgnoreCase": state } })),
            None => out.insert(
                "state".into(),
                json!({ "type": { "nin": ["completed", "canceled", "duplicate"] } }),
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
        // Every word must match: in the title, or as the issue's number.
        let words: Vec<Value> = self
            .query
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(|word| {
                let title = json!({ "title": { "containsIgnoreCase": word } });
                match number(word) {
                    Some(n) => json!({ "or": [title, { "number": { "eq": n } }] }),
                    None => title,
                }
            })
            .collect();
        if !words.is_empty() {
            out.insert("and".into(), Value::Array(words));
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
                "state": { "type": { "nin": ["completed", "canceled", "duplicate"] } },
            })
        );
        let all = Filter {
            mine: true,
            assignee: None,
            label: Some("Bug".into()),
            state: Some("In Review".into()),
            project: Some("checkout".into()),
            cycle: Some("current".into()),
            query: Some(" login END-12 ".into()),
        }
        .to_value(None);
        assert_eq!(all["assignee"], json!({ "isMe": { "eq": true } }));
        assert_eq!(all["labels"]["some"]["name"]["eqIgnoreCase"], "Bug");
        assert_eq!(
            all["state"],
            json!({ "name": { "eqIgnoreCase": "In Review" } })
        );
        assert_eq!(all["cycle"], json!({ "isActive": { "eq": true } }));
        assert_eq!(
            all["and"],
            json!([
                { "title": { "containsIgnoreCase": "login" } },
                { "or": [
                    { "title": { "containsIgnoreCase": "END-12" } },
                    { "number": { "eq": 12 } },
                ] },
            ])
        );
        let anyone = |assignee: &str, state: &str| {
            Filter {
                assignee: Some(assignee.into()),
                state: Some(state.into()),
                ..Filter::default()
            }
            .to_value(None)
        };
        let nobodys = anyone("Unassigned", "any");
        assert_eq!(nobodys["assignee"], json!({ "null": true }));
        assert!(nobodys.get("state").is_none(), "done and canceled too");
        assert_eq!(
            anyone("Zoe", "Todo")["assignee"],
            json!({ "name": { "eqIgnoreCase": "Zoe" } })
        );
        let numbered = Filter {
            cycle: Some("12".into()),
            ..Filter::default()
        }
        .to_value(None);
        assert_eq!(numbered["cycle"], json!({ "number": { "eq": 12 } }));
    }
}
