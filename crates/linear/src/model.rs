//! What Linear's API returns, read from its GraphQL shape into the types the
//! rest of treetop uses. Every field is read by name and missing ones are
//! tolerated, so a field Linear adds or drops never fails a whole list.

use serde::Deserialize;

/// An issue, as the list and the detail show it. Names are for showing and
/// ids for changing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Issue {
    /// Linear's own id, which mutations take.
    pub uuid: String,
    /// The id people use, such as `END-123`.
    pub id: String,
    pub title: String,
    pub state: String,
    pub state_id: Option<String>,
    /// 0 none, 1 urgent, 2 high, 3 medium, 4 low.
    pub priority: u8,
    pub labels: Vec<String>,
    pub label_ids: Vec<String>,
    pub assignee: Option<String>,
    pub assignee_id: Option<String>,
    pub project: Option<String>,
    pub project_id: Option<String>,
    pub cycle: Option<String>,
    pub team_id: Option<String>,
    pub url: Option<String>,
    /// Its Markdown text; only a single issue's read carries it.
    pub description: Option<String>,
    /// The branch name Linear gives it.
    pub branch: Option<String>,
    /// When it last changed, which keys its cached detail.
    pub updated_at: String,
}

impl Issue {
    /// Its priority as Linear names it.
    pub fn priority_label(&self) -> &'static str {
        match self.priority {
            1 => "urgent",
            2 => "high",
            3 => "medium",
            4 => "low",
            _ => "none",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    /// Markdown.
    pub body: String,
    pub created_at: String,
}

/// Another issue an issue is linked to, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// How it is linked, from the issue's side: `parent`, `sub-issue`,
    /// `blocked by`, `blocks`, `related`, `duplicate of` or `duplicated by`.
    pub kind: String,
    /// The id people use, such as `END-123`.
    pub id: String,
    pub title: String,
    pub state: String,
    /// Its status is done or cancelled, so it holds nothing up.
    pub is_done: bool,
}

/// An issue with everything the detail pane shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Detail {
    pub issue: Issue,
    /// Oldest first.
    pub comments: Vec<Comment>,
    /// The parent first, then sub-issues, then the other links.
    pub links: Vec<Link>,
}

/// A workflow state. `kind` is Linear's type: `triage`, `backlog`,
/// `unstarted`, `started`, `completed` or `canceled`.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub position: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Named {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    pub id: String,
    pub number: u32,
    pub name: Option<String>,
    pub is_active: bool,
}

/// What a team's issues can be set to, read once and kept.
#[derive(Debug, Clone, PartialEq)]
pub struct Team {
    pub id: String,
    pub key: String,
    pub name: String,
    /// In workflow order.
    pub states: Vec<State>,
    /// The team's labels and the workspace's, by name.
    pub labels: Vec<Named>,
    pub members: Vec<Named>,
    pub projects: Vec<Named>,
    pub cycles: Vec<Cycle>,
    /// The key's user.
    pub me: Option<Named>,
}

/// The issue fields every read asks for.
pub const ISSUE_FIELDS: &str = "id identifier title url branchName priority updatedAt \
    team { id } state { id name } assignee { id name } labels(first: 20) { nodes { id name } } \
    project { id name } cycle { number name }";

#[derive(Deserialize)]
struct Nodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct RawNamed {
    id: String,
    #[serde(default)]
    name: String,
}

impl From<RawNamed> for Named {
    fn from(raw: RawNamed) -> Self {
        Named {
            id: raw.id,
            name: raw.name,
        }
    }
}

#[derive(Deserialize)]
struct RawId {
    id: String,
}

#[derive(Deserialize)]
struct RawCycleRef {
    number: Option<f64>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RawIssue {
    id: String,
    identifier: String,
    #[serde(default)]
    title: String,
    url: Option<String>,
    branch_name: Option<String>,
    #[serde(default)]
    priority: f64,
    #[serde(default)]
    updated_at: String,
    team: Option<RawId>,
    state: Option<RawNamed>,
    assignee: Option<RawNamed>,
    labels: Option<Nodes<RawNamed>>,
    project: Option<RawNamed>,
    cycle: Option<RawCycleRef>,
    description: Option<String>,
    comments: Option<Nodes<RawComment>>,
    parent: Option<RawLinked>,
    children: Option<Nodes<RawLinked>>,
    relations: Option<Nodes<RawRelation>>,
    inverse_relations: Option<Nodes<RawRelation>>,
}

/// Another issue, as a detail names it.
#[derive(Deserialize)]
struct RawLinked {
    #[serde(default)]
    identifier: String,
    #[serde(default)]
    title: String,
    state: Option<RawName>,
}

/// A name with no id, as a linked issue's status is asked for, with the
/// status's type.
#[derive(Deserialize)]
struct RawName {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
}

impl RawLinked {
    fn into_link(self, kind: &str) -> Link {
        Link {
            kind: kind.to_string(),
            id: self.identifier,
            title: self.title,
            is_done: self
                .state
                .as_ref()
                .is_some_and(|s| matches!(s.kind.as_str(), "completed" | "canceled" | "duplicate")),
            state: self.state.map(|s| s.name).unwrap_or_default(),
        }
    }
}

/// A relation, holding the issue at its other end: `relatedIssue` on the
/// issue's own relations, `issue` on the inverse ones.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRelation {
    #[serde(default, rename = "type")]
    kind: String,
    related_issue: Option<RawLinked>,
    issue: Option<RawLinked>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    #[serde(default)]
    body: String,
    #[serde(default)]
    created_at: String,
    user: Option<RawNamed>,
}

impl From<RawComment> for Comment {
    fn from(raw: RawComment) -> Self {
        Comment {
            author: raw.user.map_or_else(|| "someone".into(), |u| u.name),
            body: raw.body,
            created_at: raw.created_at,
        }
    }
}

impl RawIssue {
    pub(crate) fn into_detail(mut self) -> Detail {
        let mut comments: Vec<Comment> = self
            .comments
            .take()
            .map(|c| c.nodes)
            .unwrap_or_default()
            .into_iter()
            .map(Comment::from)
            .collect();
        comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let mut links: Vec<Link> = self
            .parent
            .take()
            .map(|p| p.into_link("parent"))
            .into_iter()
            .collect();
        links.extend(
            self.children
                .take()
                .map(|c| c.nodes)
                .unwrap_or_default()
                .into_iter()
                .map(|c| c.into_link("sub-issue")),
        );
        // A relation reads one way from each end: what blocks from this
        // side is blocked by from the other.
        let mut related: Vec<Link> = Vec::new();
        for relation in self.relations.take().map(|r| r.nodes).unwrap_or_default() {
            let kind = match relation.kind.as_str() {
                "blocks" => "blocks",
                "duplicate" => "duplicate of",
                _ => "related",
            };
            related.extend(relation.related_issue.map(|i| i.into_link(kind)));
        }
        for relation in self
            .inverse_relations
            .take()
            .map(|r| r.nodes)
            .unwrap_or_default()
        {
            let kind = match relation.kind.as_str() {
                "blocks" => "blocked by",
                "duplicate" => "duplicated by",
                _ => "related",
            };
            related.extend(relation.issue.map(|i| i.into_link(kind)));
        }
        let rank = |kind: &str| match kind {
            "blocked by" => 0,
            "blocks" => 1,
            "related" => 2,
            _ => 3,
        };
        related.sort_by_key(|l| rank(&l.kind));
        links.extend(related);
        links.retain(|l| !l.id.is_empty());
        Detail {
            issue: Issue::from(self),
            comments,
            links,
        }
    }
}

impl From<RawIssue> for Issue {
    fn from(raw: RawIssue) -> Self {
        let labels = raw.labels.map(|l| l.nodes).unwrap_or_default();
        let cycle = raw.cycle.map(|c| match c.name {
            Some(name) if !name.is_empty() => name,
            _ => format!("cycle {}", c.number.unwrap_or(0.0)),
        });
        // Linear sends priority as a float; it is always a whole 0 to 4.
        let priority = raw.priority.clamp(0.0, 4.0) as u8;
        Issue {
            uuid: raw.id,
            id: raw.identifier,
            title: raw.title,
            state: raw
                .state
                .as_ref()
                .map(|s| s.name.clone())
                .unwrap_or_default(),
            state_id: raw.state.map(|s| s.id),
            priority,
            label_ids: labels.iter().map(|l| l.id.clone()).collect(),
            labels: labels.into_iter().map(|l| l.name).collect(),
            assignee: raw.assignee.as_ref().map(|a| a.name.clone()),
            assignee_id: raw.assignee.map(|a| a.id),
            project: raw.project.as_ref().map(|p| p.name.clone()),
            project_id: raw.project.map(|p| p.id),
            cycle,
            team_id: raw.team.map(|t| t.id),
            url: raw.url,
            description: raw.description,
            branch: raw.branch_name,
            updated_at: raw.updated_at,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawState {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    position: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCycle {
    id: String,
    #[serde(default)]
    number: f64,
    name: Option<String>,
    #[serde(default)]
    is_active: bool,
}

#[derive(Deserialize)]
pub(crate) struct RawTeam {
    id: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    name: String,
    states: Option<Nodes<RawState>>,
    labels: Option<Nodes<RawNamed>>,
    members: Option<Nodes<RawNamed>>,
    projects: Option<Nodes<RawNamed>>,
    cycles: Option<Nodes<RawCycle>>,
}

impl RawTeam {
    /// The team, with the workspace's own labels after its own and `me`.
    pub(crate) fn into_team(self, workspace_labels: Vec<Named>, me: Option<Named>) -> Team {
        let mut states: Vec<State> = self
            .states
            .map(|s| s.nodes)
            .unwrap_or_default()
            .into_iter()
            .map(|s| State {
                id: s.id,
                name: s.name,
                kind: s.kind,
                position: s.position,
            })
            .collect();
        states.sort_by(|a, b| {
            kind_rank(&a.kind)
                .cmp(&kind_rank(&b.kind))
                .then(a.position.total_cmp(&b.position))
        });
        let mut labels: Vec<Named> = named(self.labels);
        labels.extend(workspace_labels);
        labels.sort_by_key(|l| l.name.to_lowercase());
        labels.dedup_by(|a, b| a.id == b.id);
        let mut members = named(self.members);
        members.sort_by_key(|m| m.name.to_lowercase());
        let mut projects = named(self.projects);
        projects.sort_by_key(|p| p.name.to_lowercase());
        let mut cycles: Vec<Cycle> = self
            .cycles
            .map(|c| c.nodes)
            .unwrap_or_default()
            .into_iter()
            .map(|c| Cycle {
                id: c.id,
                number: c.number.max(0.0) as u32,
                name: c.name.filter(|n| !n.is_empty()),
                is_active: c.is_active,
            })
            .collect();
        cycles.sort_by_key(|c| c.number);
        Team {
            id: self.id,
            key: self.key,
            name: self.name,
            states,
            labels,
            members,
            projects,
            cycles,
            me,
        }
    }
}

fn named(nodes: Option<Nodes<RawNamed>>) -> Vec<Named> {
    nodes
        .map(|n| n.nodes)
        .unwrap_or_default()
        .into_iter()
        .map(Named::from)
        .collect()
}

pub(crate) fn named_list(nodes: Option<serde_json::Value>) -> Vec<Named> {
    nodes
        .and_then(|v| serde_json::from_value::<Nodes<RawNamed>>(v).ok())
        .map(|n| n.nodes.into_iter().map(Named::from).collect())
        .unwrap_or_default()
}

pub(crate) fn one_named(value: Option<serde_json::Value>) -> Option<Named> {
    value
        .and_then(|v| serde_json::from_value::<RawNamed>(v).ok())
        .map(Named::from)
}

/// Where a state's type sits in a workflow, so states sort as Linear shows
/// them: triage, backlog, todo, in progress, done, canceled.
fn kind_rank(kind: &str) -> u8 {
    match kind {
        "triage" => 0,
        "backlog" => 1,
        "unstarted" => 2,
        "started" => 3,
        "completed" => 4,
        "canceled" => 5,
        _ => 6,
    }
}
