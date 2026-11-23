//! The issues the open workspace's trees work on, for the Linear panel and
//! the window's issue tab: which each tree is on, what its source last said
//! of it and of its project, and the changes made to it here.
//!
//! Every tree's issue is read at once, off the screen's thread, so moving
//! from tree to tree shows each as it was last read. A change shows at once
//! and is put back if the source refuses it. Changes go one at a time, in
//! the order they were made, whatever workspace is open by then.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use issues::Backend;
use linear::{Change, Comment, Detail, Issue, Project, Standing, Team};

use crate::planned::{self, Stage};

/// How often the issues are read again, and how soon after a read that
/// failed.
const EVERY: Duration = Duration::from_secs(300);
const RETRY: Duration = Duration::from_secs(60);

/// An issue, by its source, None for the repository's own, and its id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub source: Option<String>,
    pub id: String,
}

/// The repository's own issue source: its name, and the key of the team its
/// issues are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Own {
    pub name: String,
    pub team: Option<String>,
}

/// The first issue of `team` that `text` names, such as ANB-41 in
/// `me/anb-41-inbox-rail`: the team's key, a dash and a number, with no
/// letter or digit against either end.
pub fn id_in(text: &str, team: &str) -> Option<String> {
    let upper = text.to_ascii_uppercase();
    let head = format!("{}-", team.to_ascii_uppercase());
    upper.match_indices(&head).find_map(|(at, _)| {
        let is_word = |c: char| c.is_alphanumeric();
        if upper[..at].chars().next_back().is_some_and(is_word) {
            return None;
        }
        let rest = &upper[at + head.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let number: u64 = rest[..end].parse().ok()?;
        let is_ended = !rest[end..].chars().next().is_some_and(is_word);
        (number > 0 && is_ended).then(|| format!("{head}{number}"))
    })
}

/// `id` as its source writes it: a team's key and a number, such as
/// `anb-41`, in capitals, and any other as it is.
fn tidy(id: &str) -> String {
    let is_numbered = id.split_once('-').is_some_and(|(team, number)| {
        team.chars().all(char::is_alphanumeric)
            && !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit())
    });
    if is_numbered {
        id.to_ascii_uppercase()
    } else {
        id.to_string()
    }
}

/// What one source said of the issues asked of it.
pub struct SourceRead {
    pub source: Option<String>,
    pub ids: Vec<String>,
    pub issues: Result<Vec<Issue>, String>,
    /// What its issues can be set to; None where they cannot be changed.
    pub team: Result<Option<Arc<Team>>, String>,
    pub projects: HashMap<String, Result<Project, String>>,
}

/// A read of the trees' issues, and when it was asked for.
pub struct Read {
    pub asked: Instant,
    pub sources: Vec<SourceRead>,
}

/// Reads the issues of `asked`, with their projects, from the sources `of`
/// names.
pub fn read(
    asked: Vec<(Option<String>, Vec<String>)>,
    at: Instant,
    of: impl Fn(Option<&str>) -> Result<Arc<dyn Backend>, String>,
) -> Read {
    let sources = asked
        .into_iter()
        .map(|(source, ids)| match of(source.as_deref()) {
            Ok(backend) => read_source(source, ids, &*backend),
            Err(why) => SourceRead {
                source,
                ids,
                issues: Err(why.clone()),
                team: Err(why),
                projects: HashMap::new(),
            },
        })
        .collect();
    Read { asked: at, sources }
}

fn read_source(source: Option<String>, ids: Vec<String>, backend: &dyn Backend) -> SourceRead {
    let issues = backend.issues(&ids);
    let projects: HashSet<String> = issues
        .iter()
        .flatten()
        .filter_map(|i| i.project_id.clone())
        .collect();
    SourceRead {
        source,
        ids,
        team: backend.team(),
        projects: projects
            .into_iter()
            .map(|id| {
                let read = backend.project(&id);
                (id, read)
            })
            .collect(),
        issues,
    }
}

/// A change to an issue, or a comment on it, for its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Change(Box<Change>),
    Comment(String),
}

/// `what` done to `issue`, which is `key`'s as it stood before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asked {
    pub key: Key,
    pub issue: Box<Issue>,
    pub what: Edit,
}

/// What an edit came to.
#[derive(Debug, Clone)]
pub enum Said {
    Changed(Result<Box<Issue>, String>),
    Commented(Result<Comment, String>),
}

/// An edit the source has answered, and the workspace it was made in.
#[derive(Debug, Clone)]
pub struct Answered {
    pub checkout: PathBuf,
    pub asked: Asked,
    pub said: Said,
}

impl Answered {
    /// What did not go through, with the workspace it was made in; None
    /// for what did.
    pub fn failed(&self) -> Option<String> {
        let at = crate::workspaces::name(&self.checkout);
        let id = &self.asked.key.id;
        match &self.said {
            Said::Changed(Err(why)) => Some(format!("{at}: status of {id} not changed: {why}")),
            Said::Commented(Err(why)) => Some(format!("{at}: not commented on {id}: {why}")),
            _ => None,
        }
    }

    /// The comment the source would not take.
    pub fn unposted(&self) -> Option<&str> {
        match (&self.said, &self.asked.what) {
            (Said::Commented(Err(_)), Edit::Comment(text)) => Some(text),
            _ => None,
        }
    }
}

/// Sends the edits made here to their sources, one at a time and in order,
/// on a thread of its own, for as long as treetop runs.
pub struct Editor {
    asks: Option<Sender<(PathBuf, Asked)>>,
    answers: Receiver<Answered>,
    thread: Option<JoinHandle<()>>,
    pending: usize,
    /// The comments refused, until each is back in its box.
    unposted: Vec<Answered>,
}

impl Editor {
    /// An editor of the sources `of` names for a checkout.
    pub fn spawn(
        of: impl Fn(Option<&str>, &Path) -> Result<Arc<dyn Backend>, String> + Send + 'static,
    ) -> Self {
        let (asks, asked) = mpsc::channel::<(PathBuf, Asked)>();
        let (answer, answers) = mpsc::channel();
        let thread = thread::spawn(move || {
            for (checkout, asked) in asked {
                let backend = of(asked.key.source.as_deref(), &checkout);
                let said = match &asked.what {
                    Edit::Change(change) => Said::Changed(
                        backend
                            .and_then(|b| b.update(&asked.issue, change))
                            .map(Box::new),
                    ),
                    Edit::Comment(body) => {
                        Said::Commented(backend.and_then(|b| b.comment(&asked.issue, body)))
                    }
                };
                let answered = Answered {
                    checkout,
                    asked,
                    said,
                };
                if answer.send(answered).is_err() {
                    return;
                }
            }
        });
        Editor {
            asks: Some(asks),
            answers,
            thread: Some(thread),
            pending: 0,
            unposted: Vec::new(),
        }
    }

    pub fn send(&mut self, checkout: &Path, asked: Asked) {
        if let Some(asks) = &self.asks
            && asks.send((checkout.to_path_buf(), asked)).is_ok()
        {
            self.pending += 1;
        }
    }

    /// The next edit answered, if one has been.
    pub fn answered(&mut self) -> Option<Answered> {
        let answered = self.answers.try_recv().ok()?;
        self.pending -= 1;
        if answered.unposted().is_some() {
            self.unposted.push(answered.clone());
        }
        Some(answered)
    }

    /// A comment made in `checkout` that was refused, to be posted again.
    pub fn take_unposted(&mut self, checkout: &Path) -> Option<Asked> {
        let at = self.unposted.iter().position(|a| a.checkout == checkout)?;
        Some(self.unposted.remove(at).asked)
    }

    /// Waits for what is still on its way, which quitting would lose, and
    /// returns it after the comments refused before and not posted since.
    pub fn finish(mut self) -> Vec<Answered> {
        self.asks = None;
        if self.pending > 0
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
        self.unposted
            .into_iter()
            .chain(self.answers.try_iter())
            .collect()
    }
}

/// The changes to an issue still on their way, and the issue as its source
/// last had it, which it goes back to where they are refused.
struct Pending {
    left: usize,
    source_has: Issue,
}

/// The trees' issues as last read, with the changes made here since.
#[derive(Default)]
pub struct TreeIssues {
    /// The repository's own source; None without one.
    pub own: Option<Own>,
    issues: HashMap<Key, Issue>,
    /// Issues asked for that their source does not have.
    missing: HashSet<Key>,
    /// Why a source's last read failed.
    errors: HashMap<Option<String>, String>,
    /// What each source's issues can be set to; None where they cannot be.
    teams: HashMap<Option<String>, Option<Arc<Team>>>,
    /// Why a source's statuses could not be read.
    team_errors: HashMap<Option<String>, String>,
    projects: HashMap<String, Project>,
    project_errors: HashMap<String, String>,
    /// Each issue's text and comments, or why not.
    details: HashMap<Key, Result<Box<Detail>, String>>,
    detail_asked: HashSet<(Key, String)>,
    /// When an issue's text last could not be read.
    detail_failed: HashMap<Key, Instant>,
    /// When each issue was last changed here: a read asked for before it
    /// says nothing of what it is now.
    changed: HashMap<Key, Instant>,
    pending: HashMap<Key, Pending>,
    asked: HashSet<Key>,
    /// When the last read was asked for, and whether it is on its way.
    asked_at: Option<Instant>,
    is_reading: bool,
}

impl TreeIssues {
    /// The issue the tree works on whose record says `ticket` of `source`,
    /// whose keel run's goal is `goal`, and on `branch`: the one recorded,
    /// else the first the goal or the branch names of the own source's team,
    /// unless the source has never had it.
    pub fn on(
        &self,
        ticket: Option<&str>,
        source: Option<&str>,
        goal: Option<&str>,
        branch: Option<&str>,
    ) -> Option<Key> {
        if let Some(id) = ticket {
            let source = source
                .filter(|s| self.own.as_ref().is_none_or(|own| own.name != **s))
                .map(str::to_string);
            let id = tidy(id);
            return Some(Key { source, id });
        }
        let team = self.own.as_ref()?.team.as_deref()?;
        [goal, branch]
            .into_iter()
            .flatten()
            .filter_map(|text| id_in(text, team))
            .map(|id| Key { source: None, id })
            .find(|key| !self.missing.contains(key) || self.issues.contains_key(key))
    }

    pub fn issue(&self, key: &Key) -> Option<&Issue> {
        self.issues.get(key)
    }

    /// Why what is shown of the issue is old, or not there: its source could
    /// not be read, or was and does not have it.
    pub fn error(&self, key: &Key) -> Option<&str> {
        let missing = self.missing.contains(key);
        self.errors
            .get(&key.source)
            .map(String::as_str)
            .or(missing.then_some("the issue source does not have it"))
    }

    /// What the issues of `source` can be set to: None until read, and
    /// Some(None) where they cannot be changed.
    pub fn team(&self, source: &Option<String>) -> Option<Option<&Arc<Team>>> {
        self.teams.get(source).map(Option::as_ref)
    }

    /// Why they could not be read.
    pub fn team_error(&self, source: &Option<String>) -> Option<&str> {
        self.team_errors.get(source).map(String::as_str)
    }

    pub fn project(&self, issue: &Issue) -> Option<&Project> {
        self.project_by_id(issue.project_id.as_ref()?)
    }

    /// The project `id`, where a tree's issue is in it.
    pub fn project_by_id(&self, id: &str) -> Option<&Project> {
        self.projects.get(id)
    }

    pub fn project_error(&self, issue: &Issue) -> Option<&str> {
        self.project_errors
            .get(issue.project_id.as_ref()?)
            .map(String::as_str)
    }

    /// The issue's text and comments as last read, or why they could not be.
    pub fn detail(&self, key: &Key) -> Option<&Result<Box<Detail>, String>> {
        self.details.get(key)
    }

    /// Where `issue` of `source` stands, by its status's type in its team.
    pub fn stage(&self, source: &Option<String>, issue: &Issue) -> Stage {
        let kind = self.kind_of(source, issue);
        planned::stage(kind.as_deref(), &issue.state)
    }

    fn kind_of(&self, source: &Option<String>, issue: &Issue) -> Option<String> {
        let team = self.teams.get(source)?.as_ref()?;
        team.states
            .iter()
            .find(|s| Some(&s.id) == issue.state_id.as_ref())
            .map(|s| s.kind.clone())
    }

    /// The read due for the trees' issues, `keys`, by source: when one has
    /// not been asked for, or the last is old or failed a while ago. An old
    /// one takes in those a source did not have, which it may by now.
    pub fn wanted(&mut self, keys: &HashSet<Key>) -> Option<Vec<(Option<String>, Vec<String>)>> {
        if self.is_reading || keys.is_empty() && self.missing.is_empty() {
            return None;
        }
        let wait = if self.errors.is_empty() { EVERY } else { RETRY };
        let is_due = self.asked_at.is_none_or(|at| at.elapsed() >= wait);
        if !is_due && keys.is_subset(&self.asked) {
            return None;
        }
        self.is_reading = true;
        self.asked_at = Some(Instant::now());
        self.asked = keys.clone();
        if is_due {
            self.asked.extend(self.missing.iter().cloned());
        }
        // One no tree needs any more is not waited on.
        let asked = &self.asked;
        self.errors
            .retain(|source, _| asked.iter().any(|key| key.source == *source));
        let mut by_source: HashMap<Option<String>, Vec<String>> = HashMap::new();
        for key in &self.asked {
            let ids = by_source.entry(key.source.clone()).or_default();
            ids.push(key.id.clone());
        }
        Some(by_source.into_iter().collect())
    }

    /// Reads them all again at the next look.
    pub fn read_again(&mut self) {
        self.asked_at = None;
    }

    /// Takes a read. An issue changed here since it was asked for keeps the
    /// change.
    pub fn apply(&mut self, read: Read) {
        self.is_reading = false;
        for source in read.sources {
            let key = |id: &str| Key {
                source: source.source.clone(),
                id: tidy(id),
            };
            match source.issues {
                Ok(found) => {
                    self.errors.remove(&source.source);
                    for id in &source.ids {
                        self.missing.insert(key(id));
                    }
                    for issue in found {
                        let key = key(&issue.id);
                        self.missing.remove(&key);
                        let is_changed = self.pending.contains_key(&key)
                            || self.changed.get(&key).is_some_and(|at| *at >= read.asked);
                        if !is_changed || !self.issues.contains_key(&key) {
                            self.issues.insert(key, issue);
                        }
                    }
                }
                Err(why) => {
                    self.errors.insert(source.source.clone(), why);
                }
            }
            match source.team {
                Ok(team) => {
                    self.team_errors.remove(&source.source);
                    self.teams.insert(source.source, team);
                }
                Err(why) => {
                    self.team_errors.insert(source.source, why);
                }
            }
            for (id, project) in source.projects {
                match project {
                    Ok(project) => {
                        self.project_errors.remove(&id);
                        self.projects.insert(id, project);
                    }
                    Err(why) => {
                        self.project_errors.insert(id, why);
                    }
                }
            }
        }
        // Only what was kept here is newer than the projects just read.
        let kept: Vec<Key> = self
            .changed
            .iter()
            .filter(|(key, at)| self.pending.contains_key(key) || **at >= read.asked)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &kept {
            self.show_in_project(key);
        }
    }

    /// The issue's project counts it as it stands here.
    fn show_in_project(&mut self, key: &Key) {
        let Some(issue) = self.issues.get(key) else {
            return;
        };
        let kind = self.kind_of(&key.source, issue);
        let Some(project) = issue
            .project_id
            .as_ref()
            .and_then(|id| self.projects.get_mut(id))
        else {
            return;
        };
        if let Some(row) = project.issues.iter_mut().find(|i| i.id == issue.id) {
            row.state = issue.state.clone();
            if kind.is_some() {
                row.kind = kind;
            }
        }
    }

    /// `key`'s issue reads `after` from now on, the change to it on its way;
    /// returns it as it was.
    pub fn change(&mut self, key: &Key, after: Issue) -> Option<Issue> {
        let before = self.issues.insert(key.clone(), after)?;
        self.changed.insert(key.clone(), Instant::now());
        let pending = self.pending.entry(key.clone()).or_insert_with(|| Pending {
            left: 0,
            source_has: before.clone(),
        });
        pending.left += 1;
        self.show_in_project(key);
        Some(before)
    }

    /// A change to `key`'s issue has been answered: it is as the source last
    /// had it, unless another is on its way. One made before the workspace
    /// was left and opened again has only its answer to go by.
    pub fn changed(&mut self, key: &Key, said: &Result<Box<Issue>, String>) {
        self.read_again();
        let has = match (self.pending.get_mut(key), said) {
            (Some(pending), said) => {
                pending.left -= 1;
                if let Ok(issue) = said {
                    pending.source_has = (**issue).clone();
                }
                (pending.left == 0).then(|| pending.source_has.clone())
            }
            (None, Ok(issue)) => Some((**issue).clone()),
            (None, Err(_)) => None,
        };
        let Some(issue) = has else {
            return;
        };
        self.pending.remove(key);
        self.changed.insert(key.clone(), Instant::now());
        self.issues.insert(key.clone(), issue);
        self.show_in_project(key);
    }

    /// The issue whose text the issue tab wants read, once for each version
    /// of it, and again a while after a read that failed.
    pub fn wanted_detail(&mut self, key: &Key) -> Option<Key> {
        let version = self.issues.get(key)?.updated_at.clone();
        let is_new = self.detail_asked.insert((key.clone(), version));
        let is_failed = |at: &Instant| at.elapsed() >= RETRY;
        let is_again = self.detail_failed.get(key).is_some_and(is_failed);
        (is_new || is_again).then(|| {
            self.detail_failed.remove(key);
            key.clone()
        })
    }

    /// Takes an issue's text and comments. A read that failed keeps what was
    /// there.
    pub fn apply_detail(&mut self, key: Key, read: Result<Box<Detail>, String>) {
        if read.is_err() {
            self.detail_failed.insert(key.clone(), Instant::now());
        }
        let has_one = self.details.get(&key).is_some_and(Result::is_ok);
        if read.is_ok() || !has_one {
            self.details.insert(key, read);
        }
    }

    /// A comment on `key`'s issue was posted; a read of it on its way may
    /// have it already, or not.
    pub fn commented(&mut self, key: &Key, comment: Comment) {
        if let Some(Ok(detail)) = self.details.get_mut(key)
            && !detail.comments.contains(&comment)
        {
            detail.comments.push(comment);
        }
        self.read_again();
    }

    /// The project's issues done, under way, and all but those canceled.
    pub fn counts(project: &Project) -> (usize, usize, usize) {
        let stage = |i: &Standing| planned::stage(i.kind.as_deref(), &i.state);
        let of = |wanted: Stage| project.issues.iter().filter(|i| stage(i) == wanted).count();
        let all = project.issues.len() - of(Stage::Canceled);
        (of(Stage::Done), of(Stage::Going), all)
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use linear::{Named, State};
    use std::sync::Mutex;

    pub fn key(id: &str) -> Key {
        Key {
            source: None,
            id: id.into(),
        }
    }

    pub fn issue(id: &str, state: &str) -> Issue {
        Issue {
            uuid: format!("u-{id}"),
            id: id.into(),
            title: format!("{id} title"),
            state: state.into(),
            state_id: Some(format!("s-{state}")),
            team_id: Some("t".into()),
            project: Some("Inbox".into()),
            project_id: Some("p".into()),
            updated_at: "1".into(),
            ..Issue::default()
        }
    }

    pub fn team() -> Arc<Team> {
        let state = |name: &str, kind: &str| State {
            id: format!("s-{name}"),
            name: name.into(),
            kind: kind.into(),
            position: 0.0,
        };
        Arc::new(Team {
            id: "t".into(),
            key: "ANB".into(),
            name: "Anb".into(),
            states: vec![
                state("Todo", "unstarted"),
                state("In Progress", "started"),
                state("In Review", "started"),
                state("Done", "completed"),
            ],
            labels: Vec::new(),
            members: Vec::new(),
            projects: Vec::new(),
            cycles: Vec::new(),
            me: Some(Named {
                id: "me".into(),
                name: "Anthony".into(),
            }),
        })
    }

    /// A project of three issues: ANB-40 done, ANB-41 under way, ANB-42 to do.
    pub fn project() -> Project {
        let row = |id: &str, state: &str, kind: &str| Standing {
            id: id.into(),
            title: format!("{id} title"),
            state: state.into(),
            kind: Some(kind.into()),
            ..Standing::default()
        };
        Project {
            id: "p".into(),
            name: "Inbox".into(),
            summary: Some("Answer every agent".into()),
            content: Some("## Goal".into()),
            issues: vec![
                row("ANB-40", "Done", "completed"),
                row("ANB-41", "In Progress", "started"),
                row("ANB-42", "Todo", "unstarted"),
            ],
        }
    }

    /// A read asked for at `asked` that found `issues` of `ids`.
    pub fn found(ids: &[&str], issues: Vec<Issue>, asked: Instant) -> Read {
        Read {
            asked,
            sources: vec![SourceRead {
                source: None,
                ids: ids.iter().map(|id| id.to_string()).collect(),
                issues: Ok(issues),
                team: Ok(Some(team())),
                projects: [("p".to_string(), Ok(project()))].into(),
            }],
        }
    }

    /// ANB-41, In Progress, read from a source of team ANB.
    pub fn one_read() -> TreeIssues {
        let mut all = TreeIssues {
            own: Some(Own {
                name: "anb".into(),
                team: Some("ANB".into()),
            }),
            ..TreeIssues::default()
        };
        let issues = vec![issue("ANB-41", "In Progress")];
        all.apply(found(&["ANB-41"], issues, Instant::now()));
        all
    }

    fn state(all: &TreeIssues, id: &str) -> String {
        all.issue(&key(id)).unwrap().state.clone()
    }

    #[test]
    fn an_issue_is_named_by_its_teams_key_alone_with_nothing_against_it() {
        for (text, id) in [
            ("me/anb-109-one-line", Some("ANB-109")),
            ("ANB-41 Show the inbox", Some("ANB-41")),
            ("anb-7", Some("ANB-7")),
            ("xanb-7 then anb-8", Some("ANB-8")),
            ("anb-7x", None),
            ("release-0.1.87", None),
            ("utf-8-fix", None),
            ("anb-0", None),
            ("anb-", None),
            ("é-anb-5", Some("ANB-5")),
        ] {
            assert_eq!(id_in(text, "anb").as_deref(), id, "{text}");
        }
    }

    #[test]
    fn a_tree_is_on_its_recorded_issue_else_its_goals_else_its_branchs() {
        let mut all = one_read();
        let on = |all: &TreeIssues, ticket, source, goal, branch| {
            all.on(ticket, source, goal, branch)
                .map(|key| (key.source, key.id))
        };
        let (goal, branch) = (Some("ANB-7 Help"), Some("me/anb-8-x"));
        assert_eq!(
            on(&all, Some("anb-41"), Some("anb"), goal, branch),
            Some((None, "ANB-41".into())),
            "the own source by name is the own source"
        );
        assert_eq!(
            on(&all, Some("END-1"), Some("work"), None, None),
            Some((Some("work".into()), "END-1".into()))
        );
        assert_eq!(
            on(&all, Some("fix-login"), Some("files"), None, None),
            Some((Some("files".into()), "fix-login".into())),
            "an id that is no team's number is asked for as it was recorded"
        );
        assert_eq!(
            on(&all, None, None, goal, branch),
            Some((None, "ANB-7".into()))
        );
        // Linear has none so named, so the branch's is tried.
        all.apply(found(&["ANB-7"], Vec::new(), Instant::now()));
        assert_eq!(
            on(&all, None, None, goal, branch),
            Some((None, "ANB-8".into()))
        );
        assert_eq!(on(&all, None, None, None, Some("main")), None);
        // Without a team to tell an issue by, only a record names one.
        all.own = None;
        assert_eq!(on(&all, None, None, goal, branch), None);
    }

    #[test]
    fn a_change_shows_at_once_counts_in_its_project_and_outlasts_an_older_read() {
        let mut all = one_read();
        let asked = Instant::now();
        all.change(&key("ANB-41"), issue("ANB-41", "Done")).unwrap();
        assert_eq!(state(&all, "ANB-41"), "Done");
        assert_eq!(TreeIssues::counts(&all.projects["p"]), (2, 0, 3));
        // A read asked for before the change, on its way and after it.
        let old = || vec![issue("ANB-41", "In Progress")];
        all.apply(found(&["ANB-41"], old(), asked));
        assert_eq!(state(&all, "ANB-41"), "Done");
        assert_eq!(TreeIssues::counts(&all.projects["p"]), (2, 0, 3));
        all.changed(&key("ANB-41"), &Ok(Box::new(issue("ANB-41", "Done"))));
        all.apply(found(&["ANB-41"], old(), asked));
        assert_eq!(state(&all, "ANB-41"), "Done");
        // One asked for since says what it is.
        all.apply(found(
            &["ANB-41"],
            vec![issue("ANB-41", "Todo")],
            Instant::now(),
        ));
        assert_eq!(state(&all, "ANB-41"), "Todo");
    }

    #[test]
    fn a_project_counts_an_issue_no_tree_is_on_as_it_was_read_itself() {
        let mut all = one_read();
        let mut done = project();
        done.issues[1].state = "Done".into();
        done.issues[1].kind = Some("completed".into());
        // ANB-41's tree is returned, so only ANB-42 is asked for.
        let mut read = found(&["ANB-42"], vec![issue("ANB-42", "Todo")], Instant::now());
        read.sources[0].projects = [("p".to_string(), Ok(done))].into();
        all.apply(read);
        assert_eq!(TreeIssues::counts(&all.projects["p"]), (2, 0, 3));
    }

    #[test]
    fn a_change_refused_goes_back_to_what_the_source_has_unless_another_is_on_its_way() {
        let on = key("ANB-41");
        let ok = |to: &str| Ok(Box::new(issue("ANB-41", to)));
        let no = || Err("no".to_string());
        // To In Review and then Done at once, each answered in turn.
        for (first, second, has) in [
            (no(), ok("Done"), "Done"),
            (no(), no(), "In Progress"),
            (ok("In Review"), no(), "In Review"),
        ] {
            let mut all = one_read();
            all.change(&on, issue("ANB-41", "In Review")).unwrap();
            all.change(&on, issue("ANB-41", "Done")).unwrap();
            all.changed(&on, &first);
            assert_eq!(state(&all, "ANB-41"), "Done", "the second is on its way");
            all.changed(&on, &second);
            assert_eq!(state(&all, "ANB-41"), has);
            let rows = &all.projects["p"].issues;
            assert_eq!(rows[1].state, has);
        }
        // Refused once the workspace was left and opened again, it is as
        // that has read it.
        let mut again = one_read();
        again.changed(&on, &no());
        assert_eq!(state(&again, "ANB-41"), "In Progress");
    }

    #[test]
    fn a_read_is_due_for_a_new_issue_after_a_change_and_when_old() {
        let mut all = one_read();
        let one: HashSet<Key> = [key("ANB-41")].into();
        let two: HashSet<Key> = [key("ANB-41"), key("ANB-42")].into();
        assert!(all.wanted(&HashSet::new()).is_none());
        assert!(all.wanted(&one).is_some());
        assert!(all.wanted(&two).is_none(), "one at a time");
        let read = || found(&["ANB-41"], vec![issue("ANB-41", "Todo")], Instant::now());
        all.apply(read());
        assert!(all.wanted(&one).is_none());
        let (source, mut ids) = all.wanted(&two).unwrap().remove(0);
        ids.sort();
        assert_eq!(
            (source, ids),
            (None, vec!["ANB-41".into(), "ANB-42".into()])
        );
        all.apply(read());
        assert!(all.wanted(&two).is_none());
        all.read_again();
        assert!(all.wanted(&two).is_some());
        // A failed one is tried again sooner, and what was read stays.
        all.apply(Read {
            asked: Instant::now(),
            sources: vec![SourceRead {
                source: None,
                ids: vec!["ANB-41".into()],
                issues: Err("Linear: unreachable".into()),
                team: Err("Linear: unreachable".into()),
                projects: HashMap::new(),
            }],
        });
        assert_eq!(state(&all, "ANB-41"), "Todo");
        assert_eq!(all.error(&key("ANB-41")), Some("Linear: unreachable"));
        assert!(all.wanted(&two).is_none());
        all.asked_at = Instant::now().checked_sub(RETRY);
        assert!(all.wanted(&two).is_some());
    }

    #[test]
    fn an_issue_its_source_did_not_have_is_asked_for_again_when_old() {
        let mut all = one_read();
        all.apply(found(&["ANB-41", "ANB-9"], Vec::new(), Instant::now()));
        let on = |all: &TreeIssues, branch| all.on(None, None, None, Some(branch));
        assert_eq!(on(&all, "me/anb-9"), None, "one it never had is no issue");
        // One read before may be back, and shows as it was, and why.
        assert_eq!(on(&all, "me/anb-41"), Some(key("ANB-41")));
        assert!(all.error(&key("ANB-41")).is_some());
        let none = HashSet::new();
        assert!(all.wanted(&none).is_some());
        all.apply(found(&["ANB-41", "ANB-9"], Vec::new(), Instant::now()));
        assert!(all.wanted(&none).is_none());
        all.asked_at = Instant::now().checked_sub(EVERY);
        let (_, mut ids) = all.wanted(&none).unwrap().remove(0);
        ids.sort();
        assert_eq!(ids, ["ANB-41", "ANB-9"]);
        all.apply(found(
            &["ANB-9"],
            vec![issue("ANB-9", "Todo")],
            Instant::now(),
        ));
        assert_eq!(on(&all, "me/anb-9"), Some(key("ANB-9")));
    }

    #[test]
    fn an_issues_text_is_read_once_for_each_version_and_a_failed_read_keeps_the_last() {
        let mut all = one_read();
        let on = key("ANB-41");
        assert!(all.wanted_detail(&on).is_some());
        assert!(all.wanted_detail(&on).is_none());
        all.apply_detail(on.clone(), Ok(Box::default()));
        let mut newer = issue("ANB-41", "Todo");
        newer.updated_at = "2".into();
        all.apply(found(&["ANB-41"], vec![newer], Instant::now()));
        assert!(all.wanted_detail(&on).is_some());
        all.apply_detail(on.clone(), Err("unreachable".into()));
        assert!(all.detail(&on).unwrap().is_ok());
        assert!(all.wanted_detail(&on).is_none(), "not at once");
        all.detail_failed
            .insert(on.clone(), Instant::now().checked_sub(RETRY).unwrap());
        assert!(all.wanted_detail(&on).is_some());
        assert!(all.wanted_detail(&on).is_none());
        assert!(all.wanted_detail(&key("ANB-9")).is_none(), "not read yet");
    }

    /// A source that writes down the edits it gets, and refuses comments.
    struct Writes(Arc<Mutex<Vec<String>>>);

    impl Backend for Writes {
        fn name(&self) -> &str {
            "anb"
        }

        fn list(&self, _: &issues::Filters) -> Result<Vec<Issue>, String> {
            Ok(Vec::new())
        }

        fn detail(&self, id: &str) -> Result<Detail, String> {
            match id {
                "ANB-41" => Ok(Detail {
                    issue: issue(id, "Todo"),
                    ..Detail::default()
                }),
                _ => Err(format!("no {id}")),
            }
        }

        fn update(&self, on: &Issue, change: &Change) -> Result<Issue, String> {
            thread::sleep(Duration::from_millis(20));
            let to = change.state_id.clone().unwrap_or_default();
            self.0.lock().unwrap().push(format!("{} {to}", on.id));
            Ok(issue(&on.id, to.trim_start_matches("s-")))
        }
    }

    #[test]
    fn edits_reach_their_source_in_order_and_quitting_waits_for_them() {
        let wrote = Arc::new(Mutex::new(Vec::new()));
        let backend = Arc::clone(&wrote);
        let mut editor = Editor::spawn(move |source, here| {
            assert_eq!((source, here), (None, Path::new("/code/app")));
            Ok(Arc::new(Writes(Arc::clone(&backend))))
        });
        let ask = |what: Edit| Asked {
            key: key("ANB-41"),
            issue: Box::new(issue("ANB-41", "Todo")),
            what,
        };
        for to in ["s-In Review", "s-Done", "s-Todo"] {
            let change = Change {
                state_id: Some(to.into()),
                ..Change::default()
            };
            editor.send(Path::new("/code/app"), ask(Edit::Change(Box::new(change))));
        }
        editor.send(Path::new("/code/app"), ask(Edit::Comment("hello".into())));
        let answered = editor.finish();
        assert_eq!(
            *wrote.lock().unwrap(),
            ["ANB-41 s-In Review", "ANB-41 s-Done", "ANB-41 s-Todo"]
        );
        let failed: Vec<String> = answered.iter().filter_map(Answered::failed).collect();
        assert_eq!(
            failed,
            [format!(
                "app: not commented on ANB-41: {}",
                issues::READ_ONLY
            )]
        );
    }

    #[test]
    fn a_read_says_which_issues_a_source_has_and_why_one_it_cannot_reach_has_none() {
        let asked = vec![
            (None, vec!["ANB-41".to_string(), "ANB-9".to_string()]),
            (Some("gone".to_string()), vec!["END-1".to_string()]),
        ];
        let read = read(asked, Instant::now(), |name| match name {
            None => Ok(Arc::new(Writes(Arc::default()))),
            Some(name) => Err(format!("no issue source {name}")),
        });
        let mut all = TreeIssues::default();
        all.apply(read);
        assert_eq!(state(&all, "ANB-41"), "Todo");
        assert!(all.missing.contains(&key("ANB-9")));
        assert_eq!(all.team(&None), Some(None), "it cannot be changed");
        let gone = Key {
            source: Some("gone".into()),
            id: "END-1".into(),
        };
        assert_eq!(all.error(&gone), Some("no issue source gone"));
    }

    #[test]
    fn a_comment_refused_is_kept_for_its_own_workspace_and_for_quitting() {
        let mut editor =
            Editor::spawn(|_, _| Ok(Arc::new(Writes(Arc::default())) as Arc<dyn Backend>));
        let ask = |text: &str| Asked {
            key: key("ANB-41"),
            issue: Box::new(issue("ANB-41", "Todo")),
            what: Edit::Comment(text.into()),
        };
        editor.send(Path::new("/code/app"), ask("one"));
        editor.send(Path::new("/code/web"), ask("two"));
        for _ in 0..2 {
            while editor.answered().is_none() {
                thread::yield_now();
            }
        }
        assert_eq!(
            editor.take_unposted(Path::new("/code/app")),
            Some(ask("one"))
        );
        assert_eq!(editor.take_unposted(Path::new("/code/app")), None);
        let left = editor.finish();
        let texts: Vec<_> = left.iter().map(Answered::unposted).collect();
        assert_eq!(texts, [Some("two")]);
    }
}
