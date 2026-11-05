//! The inbox down the right of the list: each tree whose agent needs you,
//! died or finished, and each whose PR failed its checks or merged, with
//! the agents of other workspaces whose reports place them in one of their
//! trees. It is a view of how things stand now, not a log of what happened,
//! so an agent already waiting when treetop opens is in it.
//!
//! What counts is decided here for the phone's pushes too, so the inbox and
//! the phone never disagree. Which entries you have opened or dismissed is
//! kept under treetop's state directory, so a restart or a switch keeps them.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::agents::Status;
use crate::app::App;
use crate::assistant;
use crate::pool::Tree;
use crate::reports::{Report, State};
use crate::ship::Stage;

/// An agent's state as far as the inbox and pushes go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Said {
    NeedsYou,
    Done,
    Died,
    Other,
}

/// What the tree's agent last came to: waiting on you, at a prompt or with
/// a question; finished; its process dead; else nothing worth saying. None
/// when no report places an agent in it.
pub fn said(app: &App, tree: &Tree) -> Option<Said> {
    let waiting = tree
        .agents
        .iter()
        .any(|a| a.status == Some(Status::Waiting) && !assistant::is_assistant(&a.name));
    match app.says(tree).map(|i| i.state) {
        Some(State::Blocked) => Some(Said::NeedsYou),
        _ if waiting => Some(Said::NeedsYou),
        Some(State::Done) => Some(Said::Done),
        Some(State::Failed) => Some(Said::Died),
        Some(_) => Some(Said::Other),
        None => None,
    }
}

/// The PR stages worth telling you about: its checks failed, or it merged.
pub fn pr_kind(stage: Stage) -> Option<Kind> {
    match stage {
        Stage::CiFailed | Stage::Failed => Some(Kind::Failed),
        Stage::Merged => Some(Kind::Merged),
        _ => None,
    }
}

/// What an entry is about, in the order the inbox groups them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    NeedsYou,
    /// Its PR's checks failed.
    Failed,
    /// Its agent's process died.
    Died,
    Done,
    /// Its PR merged.
    Merged,
}

impl Kind {
    /// The heading the entry goes under while unseen.
    pub fn heading(self) -> &'static str {
        match self {
            Kind::NeedsYou => "NEEDS YOU",
            Kind::Failed | Kind::Died => "FAILED",
            Kind::Done | Kind::Merged => "DONE",
        }
    }

    fn rank(self) -> u8 {
        match self {
            Kind::NeedsYou => 0,
            Kind::Failed | Kind::Died => 1,
            Kind::Done | Kind::Merged => 2,
        }
    }
}

/// Another workspace's entry: where it lives, so opening it switches there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Away {
    pub name: String,
    pub checkout: PathBuf,
}

/// One thing in the inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What seen and dismissed remember it by: the same until it changes,
    /// so an agent that asks again comes back.
    pub key: String,
    pub kind: Kind,
    /// None for the workspace on screen.
    pub away: Option<Away>,
    /// The tree's name, its number in the pool.
    pub tree: String,
    pub path: PathBuf,
    /// The ticket and branch, as `END-123 fix-login`.
    pub title: Option<String>,
    /// What it is about, on one line: the question, the answer, the PR.
    pub what: String,
    /// When it happened, where that is known; a PR's stage carries no time.
    pub at: Option<SystemTime>,
    /// The chat command of the agent asking, so an ask its chat already
    /// showed counts as seen.
    pub asking: Option<String>,
}

/// Another workspace's trees as its cache last listed them, which is where
/// its agents' reports are placed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pool {
    pub away: Away,
    pub trees: Vec<Tree>,
    /// treetop's record of each tree it leased there, by path: a tree whose
    /// record names a job is that job's.
    pub sidecars: std::collections::HashMap<PathBuf, crate::sidecar::Sidecar>,
}

/// The other workspaces' pools, from their caches.
pub fn other_pools(here: &Path) -> Vec<Pool> {
    crate::workspaces::list()
        .into_iter()
        .filter(|w| w.checkout != here)
        .filter_map(|w| {
            let trees = crate::cache::read(&w.checkout)?;
            Some(Pool {
                sidecars: crate::sidecar::read_all(&trees),
                trees,
                away: Away {
                    name: w.name(),
                    checkout: w.checkout,
                },
            })
        })
        .collect()
}

fn secs(at: SystemTime) -> u64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The first line of `text`, for an entry's one line.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// The tree's ticket and branch, as `END-123 fix-login`, the ticket from
/// `sidecar`, treetop's record of the tree, where it has one.
fn title_of(tree: &Tree, sidecar: Option<&crate::sidecar::Sidecar>) -> Option<String> {
    let ticket = sidecar.and_then(|s| s.ticket.as_deref());
    crate::serve::title(tree.branch.as_deref(), ticket)
}

/// What a died agent's entry says, wherever it lives.
const DIED: &str = "its agent died; R resumes it";

/// Every entry, unsorted: the trees on screen, then the other workspaces'
/// agents placed in their trees.
pub fn entries(app: &App) -> Vec<Entry> {
    let mut entries = Vec::new();
    for tree in app.trees.iter().filter(|t| t.is_held() && !t.is_main) {
        let item = app.says(tree);
        let agent = App::agents_of(tree).into_iter().next();
        let kind = match said(app, tree) {
            Some(Said::NeedsYou) => Some(Kind::NeedsYou),
            Some(Said::Done) => Some(Kind::Done),
            Some(Said::Died) => Some(Kind::Died),
            _ => None,
        };
        if let Some(kind) = kind {
            let what = match kind {
                Kind::NeedsYou => item
                    .as_ref()
                    .filter(|i| i.state == State::Blocked)
                    .map_or_else(
                        || "waiting at a prompt in its chat".into(),
                        |i| first_line(i.text()),
                    ),
                Kind::Died => DIED.into(),
                _ => item
                    .as_ref()
                    .map_or_else(String::new, |i| first_line(i.text())),
            };
            let at = item.as_ref().map(|i| i.report.updated);
            let stamp = item.as_ref().map_or_else(
                || agent.map_or_else(String::new, |a| a.name.clone()),
                |i| format!("{}@{}", i.report.job, secs(i.report.updated)),
            );
            entries.push(Entry {
                key: format!("{kind:?}:{}:{stamp}", tree.path.display()),
                kind,
                away: None,
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, tree.sidecar.as_deref()),
                what,
                at,
                asking: (kind == Kind::NeedsYou)
                    .then(|| agent.and_then(|a| a.attach.as_ref()).map(|c| c.join(" ")))
                    .flatten(),
            });
        }
        if let Some(ship) = &tree.ship
            && let Some(kind) = pr_kind(ship.stage)
        {
            entries.push(Entry {
                key: format!("{kind:?}:{}:{}", tree.path.display(), ship.label),
                kind,
                away: None,
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, tree.sidecar.as_deref()),
                what: ship.label.clone(),
                at: None,
                asking: None,
            });
        }
    }
    entries.extend(away_entries(app));
    entries
}

/// The other workspaces' agents that need you, died or finished, each
/// placed in the tree its report says it works in.
fn away_entries(app: &App) -> Vec<Entry> {
    app.reports
        .iter()
        .filter(|r| !assistant::is_assistant(&r.name))
        .filter_map(|report| {
            let kind = match report.state {
                State::Blocked => Kind::NeedsYou,
                State::Done => Kind::Done,
                State::Failed => Kind::Died,
                State::Working | State::Stopped => return None,
            };
            let (pool, tree) = place(report, &app.pools)?;
            Some(Entry {
                key: format!(
                    "{kind:?}:{}:{}@{}",
                    tree.path.display(),
                    report.job,
                    secs(report.updated)
                ),
                kind,
                away: Some(pool.away.clone()),
                tree: tree.name.clone(),
                path: tree.path.clone(),
                title: title_of(tree, pool.sidecars.get(&tree.path)),
                what: if kind == Kind::Died {
                    DIED.into()
                } else {
                    first_line(&report.text)
                },
                at: Some(report.updated),
                asking: None,
            })
        })
        .collect()
}

/// The other workspace's held tree `report`'s agent works in: the one its
/// directory is in, as agents are placed: one whose sidecar names a job only
/// for that job, else only if it wrote since that tree's lease began, since
/// a tree returned and handed out again is not still the old agent's.
fn place<'a>(report: &Report, pools: &'a [Pool]) -> Option<(&'a Pool, &'a Tree)> {
    let dir = report.dir.as_deref()?;
    let wrote = secs(report.updated);
    pools.iter().find_map(|pool| {
        let tree = pool.trees.iter().find(|t| {
            if !t.is_held() || !dir.starts_with(&t.path) {
                return false;
            }
            // A tree whose sidecar names a job is that job's alone.
            match pool.sidecars.get(&t.path).and_then(|s| s.job.as_deref()) {
                Some(job) => job == report.job,
                None => t
                    .leased_at
                    .as_deref()
                    .and_then(crate::pool::seconds)
                    .is_some_and(|began| wrote >= began),
            }
        })?;
        Some((pool, tree))
    })
}

/// The entries in the order the inbox shows them, with whether each has
/// been seen: unseen first, what needs you, then what failed, then what is
/// done, newest first in each; seen ones after, the same way. Dismissed
/// ones are left out.
pub fn shown(app: &App) -> Vec<(Entry, bool)> {
    let mut shown: Vec<(Entry, bool)> = entries(app)
        .into_iter()
        .filter(|e| !app.inbox.dismissed.contains(&e.key))
        .map(|e| {
            let seen = app.inbox.seen.contains(&e.key)
                || e.asking.as_ref().is_some_and(|c| app.has_asked(c));
            (e, seen)
        })
        .collect();
    shown.sort_by(|(a, a_seen), (b, b_seen)| {
        a_seen
            .cmp(b_seen)
            .then(a.kind.rank().cmp(&b.kind.rank()))
            .then(b.at.cmp(&a.at))
            .then(a.away.is_some().cmp(&b.away.is_some()))
            .then(a.tree.cmp(&b.tree))
    });
    shown
}

/// What you have done with the inbox, kept across runs: the entries opened
/// and dismissed, and whether it is hidden.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Marks {
    #[serde(default)]
    pub seen: BTreeSet<String>,
    #[serde(default)]
    pub dismissed: BTreeSet<String>,
    #[serde(default)]
    pub is_hidden: bool,
}

/// How many keys each set keeps. An entry's key changes when it does, so
/// old keys pile up; past this many the set starts again, which at worst
/// shows a few old entries as new once.
const KEPT: usize = 1_000;

impl Marks {
    pub fn write(&mut self, file: &Path) {
        for set in [&mut self.seen, &mut self.dismissed] {
            if set.len() > KEPT {
                set.clear();
            }
        }
        crate::state::write(file, self);
    }
}

/// The file the marks are kept in. One for every workspace, since an
/// entry opened in one is seen from all.
pub fn file() -> Option<PathBuf> {
    Some(crate::state::dir()?.join("inbox.json"))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::agents::{Agent, Source};
    use crate::app::Outcome;
    use crate::ship::Ship;
    use crate::sidecar::Sidecar;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use std::fs;
    use std::time::Duration;

    fn agent(name: &str, job: &str, status: Status) -> Agent {
        Agent {
            name: name.into(),
            pid: Some(1),
            status: Some(status),
            source: Source::Plugin,
            is_background: true,
            attach: Some(vec!["claude".into(), "attach".into(), job.into()]),
            job: Some(job.into()),
            plugin: None,
            active_at: None,
            context: None,
            label: None,
            spend: None,
        }
    }

    pub fn tree(path: &str, name: &str, branch: &str, agents: Vec<Agent>) -> Tree {
        Tree {
            name: name.into(),
            status: "leased".into(),
            branch: Some(branch.into()),
            holder: Some("claude:x".into()),
            path: PathBuf::from(path),
            leased_at: None,
            processes: Vec::new(),
            agents,
            work: None,
            is_main: false,
            sidecar: None,
            ship: None,
        }
    }

    pub fn report(job: &str, dir: &str, state: State, text: &str, ago: u64) -> Report {
        Report {
            job: job.into(),
            name: job.into(),
            state,
            text: text.into(),
            dir: Some(PathBuf::from(dir)),
            updated: SystemTime::now() - Duration::from_secs(ago),
            steps: Vec::new(),
        }
    }

    /// A pool with one tree of each kind, and another workspace whose
    /// agent waits on you in its tree 12.
    pub fn app() -> App {
        let mut app = App::new(None);
        let mut asking = tree(
            "/pool/4/app",
            "4",
            "me/anb-41-inbox-rail",
            vec![agent("inbox-rail", "j4", Status::Idle)],
        );
        asking.sidecar = Some(Box::new(Sidecar {
            ticket: Some("ANB-41".into()),
            ..Sidecar::leased_now()
        }));
        let mut failing = tree(
            "/pool/7/app",
            "7",
            "me/fix-login",
            vec![agent("fix-login", "j7", Status::Busy)],
        );
        failing.ship = Some(Box::new(Ship {
            branch: "me/fix-login".into(),
            stage: Stage::CiFailed,
            label: "#212 ✗ e2e".into(),
            url: None,
            head: None,
            detail: Vec::new(),
            plugin: "github".into(),
        }));
        app.trees = vec![
            asking,
            tree(
                "/pool/2/app",
                "2",
                "me/help-columns",
                vec![agent("help-columns", "j2", Status::Idle)],
            ),
            failing,
            tree(
                "/pool/9/app",
                "9",
                "me/port-menu",
                vec![agent("port-menu", "j9", Status::Cold)],
            ),
        ];
        app.is_loaded = true;
        app.is_agents_read = true;
        app.pools = vec![Pool {
            sidecars: Default::default(),
            away: Away {
                name: "endodeals-web".into(),
                checkout: "/code/endodeals-web".into(),
            },
            trees: vec![Tree {
                leased_at: Some("2026-01-01T09:00:00-05:00".into()),
                ..tree("/other/12/web", "12", "me/END-88-checkout-tax", Vec::new())
            }],
        }];
        app.apply_reports(vec![
            report(
                "j4",
                "/pool/4/app",
                State::Blocked,
                "Where should the inbox sit?",
                120,
            ),
            report(
                "j2",
                "/pool/2/app",
                State::Done,
                "Done: opened PR #31",
                1_100,
            ),
            report("j9", "/pool/9/app", State::Failed, "", 1_900),
            report(
                "j12",
                "/other/12/web/src",
                State::Blocked,
                "Reseed the rates?",
                360,
            ),
        ]);
        app
    }

    fn order(app: &App) -> Vec<(Kind, String, bool)> {
        shown(app)
            .into_iter()
            .map(|(e, seen)| (e.kind, e.tree, seen))
            .collect()
    }

    fn press(app: &mut App, code: KeyCode) -> Outcome {
        app.handle_key(KeyEvent::from(code))
    }

    #[test]
    fn what_needs_you_comes_first_then_what_failed_then_what_is_done() {
        let app = app();
        assert_eq!(
            order(&app),
            [
                (Kind::NeedsYou, "4".into(), false),
                (Kind::NeedsYou, "12".into(), false),
                (Kind::Died, "9".into(), false),
                (Kind::Failed, "7".into(), false),
                (Kind::Done, "2".into(), false),
            ]
        );
        let away = &shown(&app)[1].0;
        assert_eq!(
            away.away.as_ref().map(|a| a.name.as_str()),
            Some("endodeals-web")
        );
        assert_eq!(away.title.as_deref(), Some("END-88 checkout-tax"));
    }

    #[test]
    fn enter_opens_the_agents_chat_and_the_entry_sinks_under_seen() {
        let mut app = app();
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(app.rail, Some(0));
        let opened = press(&mut app, KeyCode::Enter);
        assert_eq!(
            opened,
            Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "j4".into()],
                agent: "inbox-rail".into(),
                tree: "4".into(),
            }
        );
        assert_eq!(app.rail, None, "the list has the keys back");
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("4"));
        assert_eq!(
            order(&app).last(),
            Some(&(Kind::NeedsYou, "4".into(), true))
        );
    }

    #[test]
    fn an_ask_whose_chat_opened_for_you_counts_as_seen() {
        let mut app = app();
        app.saw_chat(&["claude".into(), "attach".into(), "j4".into()]);
        assert_eq!(
            order(&app).last(),
            Some(&(Kind::NeedsYou, "4".into(), true))
        );
    }

    #[test]
    fn a_failed_pr_puts_the_cursor_on_its_tree_and_says_f_sends_it() {
        let mut app = app();
        app.rail = Some(3);
        assert_eq!(press(&mut app, KeyCode::Enter), Outcome::Continue);
        assert_eq!(app.current().map(|t| t.name.as_str()), Some("7"));
        assert!(
            app.message.as_deref().unwrap().contains("f sends"),
            "{:?}",
            app.message
        );
    }

    #[test]
    fn a_dismissed_entry_stays_gone_across_runs_until_its_agent_asks_again() {
        let file = std::env::temp_dir().join(format!("treetop-inbox-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let mut app = app();
        app.keep_inbox_in(file.clone());
        app.rail = Some(0);
        press(&mut app, KeyCode::Char('x'));
        assert!(!order(&app).iter().any(|(_, tree, _)| tree == "4"));

        let mut again = self::app();
        again.keep_inbox_in(file.clone());
        assert!(!order(&again).iter().any(|(_, tree, _)| tree == "4"));

        let mut reports = again.reports.clone();
        reports[0] = report("j4", "/pool/4/app", State::Blocked, "And the strip?", 5);
        again.apply_reports(reports);
        assert_eq!(order(&again)[0], (Kind::NeedsYou, "4".into(), false));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn another_workspaces_entry_switches_there_and_opens_its_agent() {
        let mut app = app();
        app.rail = Some(1);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            Outcome::SwitchTo {
                checkout: "/code/endodeals-web".into(),
                tree: "/other/12/web".into(),
            }
        );

        // The pool of endodeals-web, as the switch opens it.
        let mut there = App::new(None);
        there.arriving = Some("/other/12/web".into());
        there.trees = vec![tree(
            "/other/12/web",
            "12",
            "me/END-88-checkout-tax",
            vec![agent("checkout-tax", "j12", Status::Waiting)],
        )];
        assert_eq!(
            there.arrival(),
            None,
            "nothing opens before the agents are read"
        );
        there.is_agents_read = true;
        assert_eq!(
            there.arrival(),
            Some(Outcome::Chat {
                command: vec!["claude".into(), "attach".into(), "j12".into()],
                agent: "checkout-tax".into(),
                tree: "12".into(),
            })
        );
        assert_eq!(there.current().map(|t| t.name.as_str()), Some("12"));
        assert_eq!(there.arrival(), None, "it opens once");
    }

    #[test]
    fn a_report_from_before_its_trees_lease_is_not_that_trees() {
        let mut app = app();
        app.pools[0].trees[0].leased_at = Some("2099-01-01T09:00:00-05:00".into());
        assert!(!order(&app).iter().any(|(_, tree, _)| tree == "12"));
    }

    #[test]
    fn a_tree_whose_sidecar_names_another_job_is_not_that_reports() {
        let mut app = app();
        let named = |job: &str| Sidecar {
            job: Some(job.into()),
            ..Sidecar::leased_now()
        };
        app.pools[0].sidecars = [("/other/12/web".into(), named("someone-else"))].into();
        assert!(!order(&app).iter().any(|(_, tree, _)| tree == "12"));
        app.pools[0].sidecars = [("/other/12/web".into(), named("j12"))].into();
        assert!(order(&app).iter().any(|(_, tree, _)| tree == "12"));
    }

    #[test]
    fn another_workspaces_entry_names_the_ticket_its_sidecar_records() {
        let mut app = app();
        app.pools[0].trees[0].branch = Some("me/checkout-tax".into());
        app.pools[0].sidecars = [(
            "/other/12/web".into(),
            Sidecar {
                job: Some("j12".into()),
                ticket: Some("END-88".into()),
                ..Sidecar::leased_now()
            },
        )]
        .into();
        let away = shown(&app)
            .into_iter()
            .find(|(e, _)| e.tree == "12")
            .unwrap()
            .0;
        assert_eq!(away.title.as_deref(), Some("END-88 checkout-tax"));
    }

    #[test]
    fn an_ask_seen_in_one_workspace_is_seen_from_every_other() {
        let file =
            std::env::temp_dir().join(format!("treetop-inbox-asked-{}.json", std::process::id()));
        let _ = fs::remove_file(&file);
        let mut here = app();
        here.keep_inbox_in(file.clone());
        here.saw_chat(&["claude".into(), "attach".into(), "j4".into()]);

        // Another workspace's treetop: its own asked file, the same inbox.
        let mut there = app();
        there.keep_inbox_in(file.clone());
        assert!(!there.has_asked("claude attach j4"));
        assert_eq!(
            order(&there).last(),
            Some(&(Kind::NeedsYou, "4".into(), true))
        );
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn hiding_the_inbox_gives_the_mouse_back_to_the_terminal() {
        let mut app = app();
        assert!(app.wants_mouse());
        press(&mut app, KeyCode::Char('E'));
        assert!(!app.wants_mouse());
        assert!(app.inbox.is_hidden);
    }

    #[test]
    fn asks_wait_while_the_inbox_has_the_keys_and_esc_gives_them_back() {
        let mut app = app();
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.rail, Some(1));
        assert_eq!(app.next_ask(), None, "no chat opens over the inbox");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.rail, None);
        assert!(matches!(app.next_ask(), Some(Outcome::Chat { tree, .. }) if tree == "4"));
    }
}
