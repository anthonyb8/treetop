//! Where the Issues screen's issues come from. A Linear source does
//! everything; any other ticket system answers what its plugin can, through
//! a backend the program around the screen provides, and the edits it cannot
//! make say so.

use std::sync::Arc;

use linear::{Change, Client, Comment, Detail, Issue, NewIssue, Team};

use crate::Filters;

/// What an edit says when its source cannot make it.
pub const READ_ONLY: &str = "this source's plugin can list and read issues, not change them";

pub trait Backend: Send + Sync {
    /// The source's name, as the config gives it.
    fn name(&self) -> &str;

    fn list(&self, filters: &Filters) -> Result<Vec<Issue>, String>;

    /// One issue with its description, comments and links.
    fn detail(&self, id: &str) -> Result<Detail, String>;

    /// What its issues can be set to; None for a source that cannot change
    /// them.
    fn team(&self) -> Result<Option<Arc<Team>>, String> {
        Ok(None)
    }

    /// Reads the team again on the next `team`.
    fn refresh(&self) {}

    fn update(&self, _issue: &Issue, _change: &Change) -> Result<Issue, String> {
        Err(READ_ONLY.into())
    }

    /// Makes `change` to every one of `issues`, and returns them as they
    /// now are; when it stops short, the error carries those it changed.
    fn update_many(
        &self,
        issues: &[Issue],
        change: &Change,
    ) -> Result<Vec<Issue>, (Vec<Issue>, String)> {
        let mut done = Vec::new();
        for issue in issues {
            match self.update(issue, change) {
                Ok(issue) => done.push(issue),
                Err(err) => return Err((done, err)),
            }
        }
        Ok(done)
    }

    fn create(&self, _new: &NewIssue) -> Result<Issue, String> {
        Err(READ_ONLY.into())
    }

    fn comment(&self, _issue: &Issue, _body: &str) -> Result<Comment, String> {
        Err(READ_ONLY.into())
    }

    /// Marks issue `id` started and assigned to the key's user.
    fn start(&self, _id: &str) -> Result<(), String> {
        Err(READ_ONLY.into())
    }

    /// Marks issue `id` done.
    fn finish(&self, _id: &str) -> Result<(), String> {
        Err(READ_ONLY.into())
    }

    /// The source says `finish = true`: an issue is marked done once its
    /// tree's PR merges.
    fn finishes(&self) -> bool {
        false
    }

    /// The issues `id` blocks that are ready now that it is done: not done
    /// themselves, and with every other issue blocking them done.
    fn ready_after(&self, id: &str) -> Result<Vec<String>, String> {
        let done = self.detail(id)?;
        let mut ready = Vec::new();
        for blocked in done
            .links
            .iter()
            .filter(|l| l.kind == "blocks" && !l.is_done)
        {
            let detail = self.detail(&blocked.id)?;
            let is_free = detail
                .links
                .iter()
                .filter(|l| l.kind == "blocked by" && l.id != id)
                .all(|l| l.is_done);
            if is_free {
                ready.push(detail.issue.id);
            }
        }
        Ok(ready)
    }

    /// The file at `url`, such as an image in a description.
    fn fetch(&self, _url: &str) -> Result<Vec<u8>, String> {
        Err("this source's plugin cannot fetch images".into())
    }
}

/// A Linear source: an API key and the team its issues come from.
pub struct Linear {
    name: String,
    /// The client, or why there is none, such as a key that is not set.
    client: Result<Client, String>,
    team: Option<String>,
    is_finishing: bool,
}

impl Linear {
    /// The source `name`, with its API key or why there is none, marking
    /// merged issues done when `is_finishing`.
    pub fn new(
        name: &str,
        key: Result<String, String>,
        team: Option<String>,
        is_finishing: bool,
    ) -> Self {
        Linear {
            name: name.to_string(),
            client: key.map(Client::new),
            team,
            is_finishing,
        }
    }

    fn client(&self) -> Result<&Client, String> {
        self.client.as_ref().map_err(String::clone)
    }

    fn team_key(&self) -> Result<&str, String> {
        self.team.as_deref().ok_or_else(|| {
            format!(
                "{} names no team: add team = \"KEY\" to [issues.{}]",
                self.name, self.name
            )
        })
    }
}

/// The Linear filter `filters` ask for, or why a priority is not one.
fn filter(filters: &Filters) -> Result<linear::Filter, String> {
    let priority = match filters.priority.as_deref() {
        Some(word) => Some(
            linear::priority(word)
                .ok_or_else(|| format!("no priority {word}: urgent, high, medium, low or none"))?,
        ),
        None => None,
    };
    Ok(linear::Filter {
        mine: filters.mine,
        assignee: filters.assignee.clone(),
        creator: filters.creator.clone(),
        priority,
        label: filters.label.clone(),
        state: filters.state.clone(),
        project: filters.project.clone(),
        cycle: filters.cycle.clone(),
        query: filters.query.clone(),
    })
}

impl Backend for Linear {
    fn name(&self) -> &str {
        &self.name
    }

    fn list(&self, filters: &Filters) -> Result<Vec<Issue>, String> {
        self.client()?
            .issues(self.team.as_deref(), &filter(filters)?)
            .map_err(|err| err.message)
    }

    fn detail(&self, id: &str) -> Result<Detail, String> {
        self.client()?.detail(id).map_err(|err| err.message)
    }

    fn team(&self) -> Result<Option<Arc<Team>>, String> {
        let key = self.team_key()?;
        self.client()?
            .team(key)
            .map(Some)
            .map_err(|err| err.message)
    }

    fn refresh(&self) {
        if let Ok(client) = &self.client {
            client.refresh();
        }
    }

    fn update(&self, issue: &Issue, change: &Change) -> Result<Issue, String> {
        self.client()?
            .update(&issue.uuid, change)
            .map_err(|err| err.message)
    }

    fn update_many(
        &self,
        issues: &[Issue],
        change: &Change,
    ) -> Result<Vec<Issue>, (Vec<Issue>, String)> {
        let client = self.client().map_err(|err| (Vec::new(), err))?;
        let uuids: Vec<String> = issues.iter().map(|i| i.uuid.clone()).collect();
        client
            .update_many(&uuids, change)
            .map_err(|(done, err)| (done, err.message))
    }

    fn create(&self, new: &NewIssue) -> Result<Issue, String> {
        self.client()?.create(new).map_err(|err| err.message)
    }

    fn comment(&self, issue: &Issue, body: &str) -> Result<Comment, String> {
        self.client()?
            .comment(&issue.uuid, body)
            .map_err(|err| err.message)
    }

    fn start(&self, id: &str) -> Result<(), String> {
        self.client()?.start(id).map_err(|err| err.message)
    }

    fn finish(&self, id: &str) -> Result<(), String> {
        self.client()?.finish(id).map_err(|err| err.message)
    }

    fn finishes(&self) -> bool {
        self.is_finishing
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        self.client()?.fetch(url).map_err(|err| err.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_priority_linear_has_no_name_for_is_refused_not_dropped() {
        let source = Linear::new("work", Ok("lin_api_x".into()), Some("END".into()), false);
        let filters = Filters {
            priority: Some("hgih".into()),
            ..Filters::default()
        };
        let err = source.list(&filters).unwrap_err();
        assert_eq!(err, "no priority hgih: urgent, high, medium, low or none");
    }
}
