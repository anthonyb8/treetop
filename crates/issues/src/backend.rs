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

    fn create(&self, _new: &NewIssue) -> Result<Issue, String> {
        Err(READ_ONLY.into())
    }

    fn comment(&self, _issue: &Issue, _body: &str) -> Result<Comment, String> {
        Err(READ_ONLY.into())
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
}

impl Linear {
    /// The source `name`, with its key in the environment variable `key_env`.
    pub fn new(name: &str, key_env: Option<&str>, team: Option<String>) -> Self {
        let client = match key_env {
            Some(key_env) => Client::from_env(key_env).map_err(|err| err.message),
            None => Err(format!(
                "[issues.{name}] needs key_env, the variable holding a Linear API key"
            )),
        };
        Linear {
            name: name.to_string(),
            client,
            team,
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

fn filter(filters: &Filters) -> linear::Filter {
    linear::Filter {
        mine: filters.mine,
        assignee: filters.assignee.clone(),
        label: filters.label.clone(),
        state: filters.state.clone(),
        project: filters.project.clone(),
        cycle: filters.cycle.clone(),
        query: filters.query.clone(),
    }
}

impl Backend for Linear {
    fn name(&self) -> &str {
        &self.name
    }

    fn list(&self, filters: &Filters) -> Result<Vec<Issue>, String> {
        self.client()?
            .issues(self.team.as_deref(), &filter(filters))
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

    fn create(&self, new: &NewIssue) -> Result<Issue, String> {
        self.client()?.create(new).map_err(|err| err.message)
    }

    fn comment(&self, issue: &Issue, body: &str) -> Result<Comment, String> {
        self.client()?
            .comment(&issue.uuid, body)
            .map_err(|err| err.message)
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        self.client()?.fetch(url).map_err(|err| err.message)
    }
}
