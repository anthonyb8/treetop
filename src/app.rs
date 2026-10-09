//! What the screen shows and what each key does, kept free of terminal I/O so
//! the rules about what may be returned or destroyed are testable.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::pool::{Process, Tree, Work};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Return,
    Destroy,
}

impl Action {
    pub fn verb(self) -> &'static str {
        match self {
            Action::Return => "Return",
            Action::Destroy => "Destroy",
        }
    }
}

/// An action waiting on y/n in the confirm dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub action: Action,
    pub trees: Vec<Tree>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    /// Open this tree: a tmux window inside tmux, else a shell that comes
    /// back to treetop when it exits.
    Enter(Tree),
    Run(Pending),
    /// List the pool again now rather than at the next interval.
    Refresh,
}

#[derive(Default)]
pub struct App {
    pub trees: Vec<Tree>,
    pub error: Option<String>,
    /// A first listing has arrived; until then an empty table means nothing.
    pub is_loaded: bool,
    /// The trees on screen come from the cache, not from treehouse yet.
    pub is_cached: bool,
    /// A `treehouse status` is running.
    pub is_listing: bool,
    /// When the last live listing landed, for the summary line.
    pub listed_at: Option<Instant>,
    pub filter: String,
    pub is_filtering: bool,
    pub marked: BTreeSet<PathBuf>,
    pub selected: usize,
    pub pending: Option<Pending>,
    pub message: Option<String>,
    /// The tree treetop was started in. Returning or destroying it would kill
    /// the shell standing in it, so it is never a target.
    pub here: Option<PathBuf>,
}

impl App {
    pub fn new(here: Option<PathBuf>) -> Self {
        App {
            here,
            ..App::default()
        }
    }

    pub fn is_here(&self, tree: &Tree) -> bool {
        self.here.as_deref().is_some_and(|h| tree.contains(h))
    }

    pub fn visible(&self) -> Vec<&Tree> {
        let needle = self.filter.to_lowercase();
        self.trees
            .iter()
            .filter(|t| {
                needle.is_empty()
                    || [Some(&t.name), t.branch.as_ref(), t.holder.as_ref()]
                        .into_iter()
                        .flatten()
                        .any(|field| field.to_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn current(&self) -> Option<&Tree> {
        self.visible().get(self.selected).copied()
    }

    /// Swaps in a live listing from treehouse.
    pub fn set_trees(&mut self, trees: Vec<Tree>) {
        self.replace_trees(trees);
        self.is_cached = false;
        self.listed_at = Some(Instant::now());
    }

    /// Shows the cached listing until a live one lands.
    pub fn set_cached(&mut self, trees: Vec<Tree>) {
        self.replace_trees(trees);
        self.is_cached = true;
    }

    /// Takes the processes read from /proc, which are fresher than any
    /// listing. A tree absent from the map has none running.
    pub fn apply_processes(&mut self, processes: &HashMap<PathBuf, Vec<Process>>) {
        for tree in &mut self.trees {
            tree.processes = processes.get(&tree.path).cloned().unwrap_or_default();
        }
    }

    /// Takes the git state of the held trees in the map, leaving the rest as
    /// they were.
    pub fn apply_work(&mut self, work: &HashMap<PathBuf, Option<Work>>) {
        for tree in self.trees.iter_mut().filter(|t| t.is_held()) {
            if let Some(entry) = work.get(&tree.path) {
                tree.work = *entry;
            }
        }
    }

    /// Keeps the cursor on the same tree, keeps each surviving tree's probed
    /// processes and git state until the next probe replaces them, and drops
    /// marks on trees that have left the pool.
    fn replace_trees(&mut self, mut trees: Vec<Tree>) {
        let cursor = self.current().map(|t| t.path.clone());
        for tree in &mut trees {
            if let Some(old) = self.trees.iter().find(|old| old.path == tree.path) {
                tree.processes = old.processes.clone();
                tree.work = old.work.filter(|_| tree.is_held());
            }
        }
        self.trees = trees;
        self.error = None;
        self.is_loaded = true;
        let paths: BTreeSet<PathBuf> = self.trees.iter().map(|t| t.path.clone()).collect();
        self.marked.retain(|p| paths.contains(p));
        let visible = self.visible();
        self.selected = cursor
            .and_then(|c| visible.iter().position(|t| t.path == c))
            .unwrap_or(self.selected)
            .min(visible.len().saturating_sub(1));
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.visible().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// Marked trees, else the one under the cursor, minus any the action
    /// cannot take. The reason for the first exclusion goes to the status line.
    fn targets(&mut self, action: Action) -> Vec<Tree> {
        let chosen: Vec<Tree> = if self.marked.is_empty() {
            self.current().cloned().into_iter().collect()
        } else {
            self.trees
                .iter()
                .filter(|t| self.marked.contains(&t.path))
                .cloned()
                .collect()
        };
        let mut skipped = None;
        let targets = chosen
            .into_iter()
            .filter(|t| {
                let reason = if self.is_here(t) {
                    Some(format!(
                        "tree {} is where you started treetop; cd out first",
                        t.name
                    ))
                } else if action == Action::Return && !t.is_held() {
                    Some(format!("tree {} is already available", t.name))
                } else {
                    None
                };
                if reason.is_some() && skipped.is_none() {
                    skipped = reason.clone();
                }
                reason.is_none()
            })
            .collect();
        self.message = skipped;
        targets
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Outcome {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Outcome::Quit,
                KeyCode::Char('r') => Outcome::Refresh,
                _ => Outcome::Continue,
            };
        }
        if let Some(pending) = self.pending.take() {
            return match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.marked.clear();
                    Outcome::Run(pending)
                }
                _ => Outcome::Continue,
            };
        }
        if self.is_filtering {
            match key.code {
                KeyCode::Enter => self.is_filtering = false,
                KeyCode::Esc => {
                    self.is_filtering = false;
                    self.filter.clear();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            self.selected = 0;
            return Outcome::Continue;
        }
        self.message = None;
        match key.code {
            KeyCode::Char('q') => return Outcome::Quit,
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc => return Outcome::Quit,
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.move_by(isize::MAX),
            KeyCode::Char('/') => self.is_filtering = true,
            KeyCode::Char(' ') => {
                if let Some(path) = self.current().map(|t| t.path.clone()) {
                    if !self.marked.remove(&path) {
                        self.marked.insert(path);
                    }
                    self.move_by(1);
                }
            }
            KeyCode::Char('u') => self.marked.clear(),
            KeyCode::Enter => {
                if let Some(tree) = self.current() {
                    return Outcome::Enter(tree.clone());
                }
            }
            KeyCode::Char('r') => self.ask(Action::Return),
            KeyCode::Char('D') => self.ask(Action::Destroy),
            _ => {}
        }
        Outcome::Continue
    }

    fn ask(&mut self, action: Action) {
        let trees = self.targets(action);
        if !trees.is_empty() {
            self.pending = Some(Pending { action, trees });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::parse;
    use ratatui::crossterm::event::KeyEvent;

    fn app() -> App {
        let trees = parse(
            r#"[
          {"name":"4","status":"leased","branch":"feature/end-1-a","lease_holder":"anthony","path":"/p/4/app","leased_at":null,"processes":[]},
          {"name":"7","status":"available","branch":"","lease_holder":"","path":"/p/7/app","leased_at":null,"processes":[]},
          {"name":"8","status":"leased","branch":"feature/end-2-b","lease_holder":"claude:x","path":"/p/8/app","leased_at":null,"processes":[]}
        ]"#,
        )
        .unwrap();
        let mut app = App::new(None);
        app.set_trees(trees);
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Outcome {
        app.handle_key(KeyEvent::from(code))
    }

    fn names(trees: &[Tree]) -> Vec<&str> {
        trees.iter().map(|t| t.name.as_str()).collect()
    }

    #[test]
    fn return_takes_marked_trees_and_skips_available_ones() {
        let mut app = app();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('r'));
        let pending = app.pending.clone().unwrap();
        assert_eq!(names(&pending.trees), ["4", "8"]);
        assert_eq!(app.message.as_deref(), Some("tree 7 is already available"));
    }

    #[test]
    fn destroy_takes_the_cursor_tree_when_nothing_is_marked() {
        let mut app = app();
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('D'));
        let pending = app.pending.clone().unwrap();
        assert_eq!(
            (pending.action, names(&pending.trees)),
            (Action::Destroy, vec!["7"])
        );
    }

    #[test]
    fn only_y_confirms_and_confirming_clears_marks() {
        let mut app = app();
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(press(&mut app, KeyCode::Char('n')), Outcome::Continue);
        assert!(app.pending.is_none());
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(
            press(&mut app, KeyCode::Char('y')),
            Outcome::Run(_)
        ));
        assert!(app.marked.is_empty());
    }

    #[test]
    fn never_targets_the_tree_treetop_started_in() {
        let mut app = app();
        app.here = Some(PathBuf::from("/p/4/app/ui"));
        press(&mut app, KeyCode::Char('D'));
        assert!(app.pending.is_none());
        assert!(app.message.unwrap().contains("cd out first"));
    }

    #[test]
    fn filter_matches_branch_and_holder() {
        let mut app = app();
        press(&mut app, KeyCode::Char('/'));
        for c in "claude".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(
            app.visible()
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>(),
            ["8"]
        );
        assert!(matches!(press(&mut app, KeyCode::Enter), Outcome::Enter(t) if t.name == "8"));
    }

    #[test]
    fn ctrl_r_refreshes_and_never_returns() {
        let mut app = app();
        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_key(ctrl_r), Outcome::Refresh);
        assert!(app.pending.is_none());
    }

    #[test]
    fn a_new_listing_keeps_probed_state_until_the_next_probe() {
        let mut app = app();
        let path = PathBuf::from("/p/8/app");
        let node = Process {
            pid: 1,
            name: "node".into(),
        };
        app.apply_processes(&HashMap::from([(path.clone(), vec![node])]));
        app.apply_work(&HashMap::from([(
            path,
            Some(Work {
                changed: 2,
                unpushed: 1,
            }),
        )]));
        let listing = app
            .trees
            .iter()
            .cloned()
            .map(|t| Tree {
                processes: vec![],
                work: None,
                ..t
            })
            .collect();
        app.set_trees(listing);
        let tree = app.trees.iter().find(|t| t.name == "8").unwrap();
        assert_eq!(
            (tree.processes.len(), tree.work.map(|w| w.changed)),
            (1, Some(2))
        );
        assert!(!app.is_cached && app.listed_at.is_some());
    }

    #[test]
    fn refresh_keeps_the_cursor_on_the_same_tree() {
        let mut app = app();
        press(&mut app, KeyCode::End);
        let mut trees = app.trees.clone();
        trees.remove(1);
        app.set_trees(trees);
        assert_eq!(app.current().unwrap().name, "8");
    }
}
