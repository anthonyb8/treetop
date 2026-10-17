//! A picker over a team's states, labels or members, filtered as you type.
//! Status and assignee take one choice; labels take a set, marked with Tab.

use std::collections::BTreeSet;

use linear::{Change, Issue, Team};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What is being picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Status,
    Labels,
    Assignee,
    Project,
}

impl Kind {
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Status => "Status",
            Kind::Labels => "Labels",
            Kind::Assignee => "Assignee",
            Kind::Project => "Project",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// None for "unassigned".
    pub id: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picker {
    pub kind: Kind,
    /// The issue's id, such as `END-123`, for the title.
    pub issue: String,
    pub choices: Vec<Choice>,
    pub query: String,
    /// The cursor, an index into the choices the query keeps.
    pub cursor: usize,
    /// The labels marked, by id.
    pub marked: BTreeSet<String>,
    /// The choice the issue has now, by id, marked in the list.
    pub current: Option<String>,
}

/// What a key did to the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Picked {
    Continue,
    Cancel,
    /// The change to make, and the issue as it looks with it made.
    Apply(Box<Change>, Box<Issue>),
}

impl Picker {
    /// A picker of `kind` for `issue`, over what `team` offers.
    pub fn new(kind: Kind, issue: &Issue, team: &Team) -> Self {
        let named = |list: &[linear::Named]| -> Vec<Choice> {
            list.iter()
                .map(|n| Choice {
                    id: Some(n.id.clone()),
                    name: n.name.clone(),
                })
                .collect()
        };
        let (choices, current) = match kind {
            Kind::Status => (
                team.states
                    .iter()
                    .map(|s| Choice {
                        id: Some(s.id.clone()),
                        name: s.name.clone(),
                    })
                    .collect(),
                issue.state_id.clone(),
            ),
            Kind::Labels => (named(&team.labels), None),
            Kind::Assignee => {
                // You first, then nobody, then everyone else by name.
                let me = team.me.as_ref();
                let mut choices: Vec<Choice> = me
                    .map(|me| Choice {
                        id: Some(me.id.clone()),
                        name: format!("{} (you)", me.name),
                    })
                    .into_iter()
                    .collect();
                choices.push(Choice {
                    id: None,
                    name: "unassigned".into(),
                });
                choices.extend(
                    named(&team.members)
                        .into_iter()
                        .filter(|c| me.is_none_or(|me| c.id.as_ref() != Some(&me.id))),
                );
                (choices, issue.assignee_id.clone())
            }
            Kind::Project => {
                let mut choices = vec![Choice {
                    id: None,
                    name: "no project".into(),
                }];
                choices.extend(named(&team.projects));
                (choices, issue.project_id.clone())
            }
        };
        let marked = match kind {
            Kind::Labels => issue.label_ids.iter().cloned().collect(),
            _ => BTreeSet::new(),
        };
        let cursor = choices
            .iter()
            .position(|c| c.id == current && kind != Kind::Labels)
            .unwrap_or(0);
        Picker {
            kind,
            issue: issue.id.clone(),
            choices,
            query: String::new(),
            cursor,
            marked,
            current,
        }
    }

    /// The choices the query keeps, by name, case ignored.
    pub fn shown(&self) -> Vec<&Choice> {
        let query = self.query.to_lowercase();
        self.choices
            .iter()
            .filter(|c| c.name.to_lowercase().contains(&query))
            .collect()
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.shown().len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// The change the picker asks for, made to `issue`, and the issue as it
    /// will look.
    fn apply(&self, issue: &Issue) -> Option<Picked> {
        let mut after = issue.clone();
        let change = match self.kind {
            Kind::Labels => {
                let ids: Vec<String> = self.marked.iter().cloned().collect();
                after.labels = self
                    .choices
                    .iter()
                    .filter(|c| c.id.as_ref().is_some_and(|id| self.marked.contains(id)))
                    .map(|c| c.name.clone())
                    .collect();
                after.label_ids = ids.clone();
                Change {
                    label_ids: Some(ids),
                    ..Change::default()
                }
            }
            Kind::Status => {
                let choice = self.shown().get(self.cursor).copied()?;
                after.state = choice.name.clone();
                after.state_id = choice.id.clone();
                Change {
                    state_id: choice.id.clone(),
                    ..Change::default()
                }
            }
            Kind::Assignee => {
                let choice = self.shown().get(self.cursor).copied()?;
                after.assignee = choice
                    .id
                    .as_ref()
                    .map(|_| choice.name.trim_end_matches(" (you)").to_string());
                after.assignee_id = choice.id.clone();
                Change {
                    assignee_id: Some(choice.id.clone()),
                    ..Change::default()
                }
            }
            Kind::Project => {
                let choice = self.shown().get(self.cursor).copied()?;
                after.project = choice.id.as_ref().map(|_| choice.name.clone());
                after.project_id = choice.id.clone();
                Change {
                    project_id: Some(choice.id.clone()),
                    ..Change::default()
                }
            }
        };
        Some(Picked::Apply(Box::new(change), Box::new(after)))
    }

    pub fn handle_key(&mut self, key: KeyEvent, issue: &Issue) -> Picked {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Picked::Cancel,
            KeyCode::Char('c') if ctrl => return Picked::Cancel,
            KeyCode::Enter => return self.apply(issue).unwrap_or(Picked::Continue),
            KeyCode::Down => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Tab if self.kind == Kind::Labels => {
                if let Some(id) = self.shown().get(self.cursor).and_then(|c| c.id.clone())
                    && !self.marked.remove(&id)
                {
                    self.marked.insert(id);
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.cursor = 0;
            }
            _ => {}
        }
        Picked::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linear::{Named, State};

    fn named(id: &str, name: &str) -> Named {
        Named {
            id: id.into(),
            name: name.into(),
        }
    }

    fn team() -> Team {
        Team {
            id: "t".into(),
            key: "END".into(),
            name: "Endo".into(),
            states: [("s1", "Todo"), ("s2", "In Progress"), ("s3", "Done")]
                .iter()
                .map(|(id, name)| State {
                    id: (*id).into(),
                    name: (*name).into(),
                    kind: "started".into(),
                    position: 0.0,
                })
                .collect(),
            labels: vec![named("l1", "Bug"), named("l2", "Feature")],
            members: vec![named("u1", "Anthony"), named("u2", "Zoe")],
            projects: Vec::new(),
            cycles: Vec::new(),
            me: Some(named("u1", "Anthony")),
        }
    }

    fn issue() -> Issue {
        Issue {
            id: "END-1".into(),
            state: "Todo".into(),
            state_id: Some("s1".into()),
            labels: vec!["Bug".into()],
            label_ids: vec!["l1".into()],
            ..Issue::default()
        }
    }

    fn typed(picker: &mut Picker, text: &str) {
        for c in text.chars() {
            picker.handle_key(KeyEvent::from(KeyCode::Char(c)), &issue());
        }
    }

    #[test]
    fn status_filters_as_you_type_and_enter_applies_it() {
        let mut picker = Picker::new(Kind::Status, &issue(), &team());
        assert_eq!(picker.cursor, 0, "on the current status");
        typed(&mut picker, "prog");
        assert_eq!(picker.shown().len(), 1);
        let Picked::Apply(change, after) =
            picker.handle_key(KeyEvent::from(KeyCode::Enter), &issue())
        else {
            panic!("Enter applies");
        };
        assert_eq!(change.state_id.as_deref(), Some("s2"));
        assert_eq!(after.state, "In Progress");
    }

    #[test]
    fn labels_are_a_set_marked_with_tab() {
        let mut picker = Picker::new(Kind::Labels, &issue(), &team());
        assert!(
            picker.marked.contains("l1"),
            "starts from the issue's labels"
        );
        picker.handle_key(KeyEvent::from(KeyCode::Tab), &issue());
        picker.handle_key(KeyEvent::from(KeyCode::Down), &issue());
        picker.handle_key(KeyEvent::from(KeyCode::Tab), &issue());
        let Picked::Apply(change, after) =
            picker.handle_key(KeyEvent::from(KeyCode::Enter), &issue())
        else {
            panic!("Enter applies");
        };
        assert_eq!(change.label_ids, Some(vec!["l2".to_string()]));
        assert_eq!(after.labels, ["Feature"]);
    }

    #[test]
    fn the_assignee_offers_you_then_nobody_then_the_rest() {
        let mine = Issue {
            assignee: Some("Anthony".into()),
            assignee_id: Some("u1".into()),
            ..issue()
        };
        let picker = Picker::new(Kind::Assignee, &mine, &team());
        let names: Vec<&str> = picker.choices.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Anthony (you)", "unassigned", "Zoe"]);
        assert_eq!(picker.cursor, 0, "on whoever has it now");
        let mut picker = picker;
        picker.handle_key(KeyEvent::from(KeyCode::Down), &issue());
        let Picked::Apply(change, after) =
            picker.handle_key(KeyEvent::from(KeyCode::Enter), &issue())
        else {
            panic!("Enter applies");
        };
        assert_eq!(change.assignee_id, Some(None), "unassigning clears it");
        assert_eq!(after.assignee, None);
        assert_eq!(
            picker.handle_key(KeyEvent::from(KeyCode::Esc), &issue()),
            Picked::Cancel
        );
    }
}
