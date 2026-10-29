//! The compose box where you write to agents: a word with the assistant
//! (`:`), a task for a new agent in a fresh tree (`n`) or in the tree
//! under the cursor (`N`), a line for agents already working (`m`), a
//! branch's new name (`b`), or the handoff an agent wrote for the one that
//! takes over from it (`h`). Kept free of terminal I/O, like `app`.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::pool::Tree;

/// Where the text goes: to the assistant, or to a new agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// One line for the assistant about what is on screen, or for the
    /// agents its leading `@` words name.
    Ask,
    /// A new agent in a tree the pool hands out when the job runs, so two
    /// quick dispatches never share one.
    NewTree,
}

/// An agent a line is typed into, through the chat command it offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub agent: String,
    pub tree: String,
    pub command: Vec<String>,
    /// The agent plugin that reaches it, and its id there.
    pub plugin: Option<String>,
    pub job: Option<String>,
    /// Not running: a message starts it again through its plugin rather
    /// than being typed into its chat.
    pub is_cold: bool,
}

impl Recipient {
    /// `agent` in tree `tree`, if it has a chat to type into or a plugin to
    /// send through.
    pub fn of(agent: &crate::agents::Agent, tree: &str) -> Option<Self> {
        let is_cold = agent.status == Some(crate::agents::Status::Cold);
        if is_cold && (agent.plugin.is_none() || agent.job.is_none()) {
            return None;
        }
        Some(Recipient {
            agent: agent.name.clone(),
            tree: tree.to_string(),
            command: agent.attach.clone()?,
            plugin: agent.plugin.clone(),
            job: agent.job.clone(),
            is_cold,
        })
    }

    /// The Claude Code background job `job`, named `agent`, from its report:
    /// cold unless `is_running`.
    pub fn of_job(agent: &str, tree: String, job: &str, is_running: bool) -> Self {
        Recipient {
            agent: agent.to_string(),
            tree,
            command: vec!["claude".into(), "attach".into(), job.to_string()],
            plugin: Some("claude".into()),
            job: Some(job.to_string()),
            is_cold: !is_running,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// Send the text to `target`.
    Dispatch { target: Target },
    /// Type the text into each of these agents.
    Message { to: Vec<Recipient> },
    /// The text is the new name of this tree's branch.
    Rename { tree: Box<Tree> },
    /// The text is the task of a new agent in this tree, leased first if
    /// it is free.
    Start { tree: Box<Tree> },
    /// The text is the handoff `from` wrote, the task of the new agent that
    /// takes over its work in this tree.
    Handoff { tree: Box<Tree>, from: Recipient },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub purpose: Purpose,
    pub text: String,
    /// The branch a new agent will work on, once it has been edited; until
    /// then it follows the task.
    pub branch: Option<String>,
    /// Keys go to the branch rather than the task.
    pub is_editing_branch: bool,
    /// Which of the branches matching `base_typed` a fresh tree is cut
    /// from: the first, the pool's base with nothing typed, until `←`/`→`
    /// picks another.
    pub base_at: usize,
    /// What is typed on the base line, which narrows the branches it steps
    /// through to those that start with it, then those that contain it.
    pub base_typed: String,
    /// Keys go to the base rather than the task.
    pub is_editing_base: bool,
    /// The pool's base, which a fresh tree is cut from unless another is
    /// picked.
    pub default_base: Option<String>,
    /// The branches a fresh tree can be cut from, the pool's base first,
    /// so the base line only ever names a real one.
    pub bases: Vec<String>,
    /// Whose branches these are, for the name a task gives.
    pub user: String,
    /// The text shows from its start, as a handoff does until it is typed
    /// in, since it is read from the top; otherwise its end stays in view,
    /// where it is typed.
    pub is_from_top: bool,
}

/// What a key did to the box.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    Continue,
    Submit,
    Cancel,
    /// Open the text in the editor, for a handoff too long to read over in
    /// the box.
    Editor,
}

impl Prompt {
    fn new(purpose: Purpose) -> Self {
        Prompt {
            purpose,
            text: String::new(),
            branch: None,
            is_editing_branch: false,
            base_at: 0,
            base_typed: String::new(),
            is_editing_base: false,
            default_base: None,
            bases: Vec::new(),
            user: String::new(),
            is_from_top: false,
        }
    }

    pub fn dispatch(target: Target, user: String) -> Self {
        Prompt {
            user,
            ..Prompt::new(Purpose::Dispatch { target })
        }
    }

    pub fn message(to: Vec<Recipient>) -> Self {
        Prompt::new(Purpose::Message { to })
    }

    /// Renaming `tree`'s branch, starting from its current name.
    pub fn rename(tree: Tree) -> Self {
        Prompt {
            text: tree.branch.clone().unwrap_or_default(),
            ..Prompt::new(Purpose::Rename {
                tree: Box::new(tree),
            })
        }
    }

    /// A new agent in `tree`, which is leased for it if it is free.
    pub fn start(tree: Tree, user: String) -> Self {
        Prompt {
            user,
            ..Prompt::new(Purpose::Start {
                tree: Box::new(tree),
            })
        }
    }

    /// The handoff `from` wrote in `tree`, to read over before a new
    /// agent starts on it.
    pub fn handoff(tree: Tree, from: Recipient, text: String) -> Self {
        Prompt {
            text,
            is_from_top: true,
            ..Prompt::new(Purpose::Handoff {
                tree: Box::new(tree),
                from,
            })
        }
    }

    pub fn target(&self) -> Option<Target> {
        match &self.purpose {
            Purpose::Dispatch { target } => Some(*target),
            Purpose::Message { .. }
            | Purpose::Rename { .. }
            | Purpose::Start { .. }
            | Purpose::Handoff { .. } => None,
        }
    }

    pub fn is_handoff(&self) -> bool {
        matches!(self.purpose, Purpose::Handoff { .. })
    }

    /// The target gets a branch named for it: a new agent's tree does, and
    /// a free tree leased for one; the assistant's main checkout and a tree
    /// someone holds keep their own.
    pub fn names_branch(&self) -> bool {
        match &self.purpose {
            Purpose::Start { tree } => !tree.is_held() && !tree.is_main,
            _ => self.target() == Some(Target::NewTree),
        }
    }

    /// The branch the agent will start on, as the box shows it: the edited
    /// name, else the task's. None when the target keeps its own branch, or
    /// the task has no words yet.
    pub fn branch_name(&self) -> Option<String> {
        if !self.names_branch() {
            return None;
        }
        self.branch
            .clone()
            .or_else(|| crate::branch::for_task(&self.user, &self.text))
    }

    /// The target is a fresh tree, which can be cut from a branch other
    /// than the pool's base.
    pub fn names_base(&self) -> bool {
        self.target() == Some(Target::NewTree)
    }

    /// The branches the base line steps through: all of them with nothing
    /// typed, else those that start with what is typed, then those that
    /// contain it, ignoring case.
    pub fn base_matches(&self) -> Vec<&str> {
        let typed = self.base_typed.trim().to_lowercase();
        let (mut starts, contains): (Vec<&str>, Vec<&str>) = self
            .bases
            .iter()
            .map(String::as_str)
            .filter(|b| b.to_lowercase().contains(&typed))
            .partition(|b| b.to_lowercase().starts_with(&typed));
        starts.extend(contains);
        starts
    }

    /// Something is typed on the base line that names no branch, so there
    /// is nothing to cut a tree from.
    pub fn is_base_unmatched(&self) -> bool {
        self.names_base() && !self.base_typed.trim().is_empty() && self.base_matches().is_empty()
    }

    /// The branch the base line shows: the one picked among the matches,
    /// else, with nothing typed, the pool's.
    pub fn base_shown(&self) -> Option<&str> {
        match self.base_matches().get(self.base_at) {
            Some(base) => Some(base),
            None if self.base_typed.trim().is_empty() => self.default_base.as_deref(),
            None => None,
        }
    }

    /// The base a fresh tree is cut from when it is not the pool's: the one
    /// picked. None cuts it from the pool's base.
    pub fn base_name(&self) -> Option<String> {
        if !self.names_base() {
            return None;
        }
        self.base_matches()
            .get(self.base_at)
            .filter(|b| Some(**b) != self.default_base.as_deref())
            .map(|b| b.to_string())
    }

    /// Narrows the base line to what is now typed, back on its first match.
    fn retype_base(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.base_typed);
        self.base_at = 0;
    }

    /// Steps the base to the next matching branch, or the one before, round
    /// the list.
    fn step_base(&mut self, forward: bool) {
        let count = self.base_matches().len();
        if count == 0 {
            return;
        }
        self.base_at = if forward {
            (self.base_at + 1) % count
        } else {
            (self.base_at + count - 1) % count
        };
    }

    /// A one-line field has the focus: the branch or the base.
    fn is_editing_field(&self) -> bool {
        self.is_editing_branch || self.is_editing_base
    }

    /// The line keys edit: the branch while it has the focus, else the
    /// text. The base has keys of its own, since what is typed there only
    /// narrows the branches.
    fn line(&mut self) -> &mut String {
        if self.is_editing_branch {
            self.branch.get_or_insert_with(String::new)
        } else {
            &mut self.text
        }
    }

    /// Pasted text is kept whole, line breaks and all: the agent reads it as
    /// one message. A branch or base takes it as one line.
    pub fn paste(&mut self, text: &str) {
        self.is_from_top = false;
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.is_editing_base {
            let line = text.replace('\n', "");
            self.retype_base(|typed| typed.push_str(line.trim()));
            return;
        }
        if self.is_editing_field() {
            self.line().push_str(&text.replace('\n', ""));
        } else {
            self.text.push_str(&text);
        }
    }

    /// Moves the focus round the task, the branch and, for a fresh tree,
    /// the base, `forward` going down. The branch starts from the name the
    /// task gave it, so it is edited rather than retyped.
    fn move_focus(&mut self, forward: bool) {
        if !self.names_branch() {
            return;
        }
        // 0 is the task, 1 the branch, 2 the base.
        let lines = if self.names_base() { 3 } else { 2 };
        let at = if self.is_editing_branch {
            1
        } else if self.is_editing_base {
            2
        } else {
            0
        };
        let next = if forward {
            (at + 1) % lines
        } else {
            (at + lines - 1) % lines
        };
        if next == 1 && self.branch.is_none() {
            self.branch = self.branch_name();
        }
        self.is_editing_branch = next == 1;
        self.is_editing_base = next == 2;
    }

    pub fn edit(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Typing happens at the end, so the end comes into view.
        if matches!(
            key.code,
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter
        ) && !(ctrl && key.code == KeyCode::Char('e'))
        {
            self.is_from_top = false;
        }
        match key.code {
            KeyCode::Esc => return Edit::Cancel,
            KeyCode::Char('c') if ctrl => return Edit::Cancel,
            KeyCode::Char('e') if ctrl && self.is_handoff() => return Edit::Editor,
            // A terminal reports Shift+Enter as Enter unless it is asked for
            // extended keys, so Alt+Enter is the break that always arrives.
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                if !self.is_editing_field() {
                    self.text.push('\n');
                }
            }
            KeyCode::Enter => return Edit::Submit,
            KeyCode::Down => self.move_focus(true),
            KeyCode::Up => self.move_focus(false),
            KeyCode::Right if self.is_editing_base => self.step_base(true),
            KeyCode::Left if self.is_editing_base => self.step_base(false),
            // Typing on the base line narrows the branches; it never names
            // one that does not exist.
            KeyCode::Char('u' | 'w') if ctrl && self.is_editing_base => {
                self.retype_base(String::clear);
            }
            KeyCode::Char(c) if !ctrl && self.is_editing_base => {
                self.retype_base(|typed| typed.push(c));
            }
            KeyCode::Backspace if self.is_editing_base => {
                self.retype_base(|typed| {
                    typed.pop();
                });
            }
            KeyCode::Char('u') if ctrl => self.line().clear(),
            KeyCode::Char('w') if ctrl => {
                // A branch's words are split by / and - rather than spaces.
                let is_branch = self.is_editing_field();
                let line = self.line();
                let kept = line.trim_end().len();
                let cut = line[..kept]
                    .rfind(|c: char| c.is_whitespace() || (is_branch && "/-".contains(c)))
                    .map_or(0, |at| at + 1);
                line.truncate(cut);
            }
            KeyCode::Char(c) if !ctrl => self.line().push(c),
            KeyCode::Backspace => {
                self.line().pop();
            }
            _ => {}
        }
        Edit::Continue
    }
}

/// The session name for a task: its first few words, so `claude agents` and
/// treetop's AGENT column say what it is doing. A leading issue id such as
/// `END-123` stays at the front, as session names put it.
pub fn session_name(task: &str) -> Option<String> {
    const WORDS: usize = 6;
    const LONGEST: usize = 48;
    let words: Vec<String> = task
        .split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric() || *c == '-')
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .take(WORDS)
        .collect();
    let mut name = String::new();
    for word in words {
        if name.len() + word.len() + 1 > LONGEST {
            break;
        }
        if !name.is_empty() {
            name.push(' ');
        }
        name.push_str(&word);
    }
    (!name.is_empty()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn type_in(prompt: &mut Prompt, text: &str) {
        for c in text.chars() {
            prompt.edit(KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn edits_like_a_shell_line() {
        let mut prompt = Prompt::message(Vec::new());
        type_in(&mut prompt, "fix the  login");
        prompt.edit(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(prompt.text, "fix the  ");
        prompt.edit(KeyEvent::from(KeyCode::Backspace));
        type_in(&mut prompt, "x");
        assert_eq!(prompt.text, "fix the x");
        prompt.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(prompt.text, "");
        prompt.edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(prompt.text, "\n", "Alt+Enter breaks the line");
        assert_eq!(prompt.edit(KeyEvent::from(KeyCode::Enter)), Edit::Submit);
        assert_eq!(prompt.edit(KeyEvent::from(KeyCode::Esc)), Edit::Cancel);
    }

    #[test]
    fn a_new_tree_gets_a_branch_that_follows_the_task_until_edited() {
        let mut prompt = Prompt::dispatch(Target::NewTree, "anthony".into());
        assert_eq!(prompt.branch_name(), None, "no words yet");
        type_in(&mut prompt, "Fix login");
        assert_eq!(prompt.branch_name().as_deref(), Some("anthony/fix-login"));
        prompt.edit(KeyEvent::from(KeyCode::Down));
        prompt.edit(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        type_in(&mut prompt, "auth");
        assert_eq!(prompt.branch_name().as_deref(), Some("anthony/fix-auth"));
        prompt.edit(KeyEvent::from(KeyCode::Up));
        type_in(&mut prompt, " now");
        assert_eq!(prompt.text, "Fix login now");
        assert_eq!(
            prompt.branch_name().as_deref(),
            Some("anthony/fix-auth"),
            "an edited branch no longer follows the task"
        );
    }

    #[test]
    fn a_fresh_tree_is_cut_from_a_branch_picked_by_scrolling_else_the_pools() {
        let mut prompt = Prompt::dispatch(Target::NewTree, "anthony".into());
        prompt.default_base = Some("development".into());
        prompt.bases = ["development", "main", "release/2026-10"]
            .map(String::from)
            .into();
        assert_eq!(prompt.base_name(), None, "the pool's base");
        assert_eq!(prompt.base_shown(), Some("development"));
        prompt.edit(KeyEvent::from(KeyCode::Down));
        assert!(prompt.is_editing_branch);
        prompt.edit(KeyEvent::from(KeyCode::Down));
        assert!(prompt.is_editing_base && !prompt.is_editing_branch);
        prompt.edit(KeyEvent::from(KeyCode::Right));
        assert_eq!(prompt.base_name().as_deref(), Some("main"));
        prompt.edit(KeyEvent::from(KeyCode::Left));
        prompt.edit(KeyEvent::from(KeyCode::Left));
        assert_eq!(
            prompt.base_name().as_deref(),
            Some("release/2026-10"),
            "round the list"
        );
        prompt.edit(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(prompt.text, "", "a base is one line");
        prompt.edit(KeyEvent::from(KeyCode::Down));
        assert!(!prompt.is_editing_base, "round to the task");
        type_in(&mut prompt, "x");
        assert_eq!(prompt.text, "x");
        prompt.edit(KeyEvent::from(KeyCode::Up));
        assert!(prompt.is_editing_base, "up goes back to the base");
        prompt.edit(KeyEvent::from(KeyCode::Right));
        assert_eq!(prompt.base_name(), None, "back on the pool's base");
    }

    #[test]
    fn typing_on_the_base_line_narrows_it_to_real_branches() {
        let mut prompt = Prompt::dispatch(Target::NewTree, "anthony".into());
        prompt.default_base = Some("development".into());
        prompt.bases = [
            "development",
            "main",
            "anthony/release-notes",
            "release/2026-10",
        ]
        .map(String::from)
        .into();
        prompt.edit(KeyEvent::from(KeyCode::Down));
        prompt.edit(KeyEvent::from(KeyCode::Down));
        type_in(&mut prompt, "rel");
        assert_eq!(
            prompt.base_matches(),
            ["release/2026-10", "anthony/release-notes"],
            "those it starts first"
        );
        assert_eq!(prompt.base_name().as_deref(), Some("release/2026-10"));
        prompt.edit(KeyEvent::from(KeyCode::Right));
        assert_eq!(prompt.base_name().as_deref(), Some("anthony/release-notes"));
        assert_eq!(prompt.text, "", "the task is untouched");
        type_in(&mut prompt, "zz");
        assert!(prompt.is_base_unmatched());
        assert_eq!(prompt.base_shown(), None, "no branch, never the pool's");
        assert_eq!(prompt.base_name(), None);
        prompt.edit(KeyEvent::from(KeyCode::Backspace));
        prompt.edit(KeyEvent::from(KeyCode::Backspace));
        assert!(!prompt.is_base_unmatched());
        prompt.edit(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(
            prompt.base_shown(),
            Some("development"),
            "all of them again"
        );
        prompt.paste("MAI\n");
        assert_eq!(
            prompt.base_name().as_deref(),
            Some("main"),
            "case is ignored"
        );
    }

    #[test]
    fn a_held_trees_box_has_no_base_line() {
        let mut prompt = Prompt::message(Vec::new());
        assert!(!prompt.names_base());
        prompt.edit(KeyEvent::from(KeyCode::Down));
        assert!(!prompt.is_editing_base);
    }

    #[test]
    fn a_free_tree_gets_a_branch_and_a_held_one_keeps_its_own() {
        let free = crate::pool::stand_in(std::path::PathBuf::from("/p/5/app"));
        assert!(!free.is_held());
        let mut prompt = Prompt::start(free.clone(), "anthony".into());
        type_in(&mut prompt, "Fix login");
        assert_eq!(prompt.branch_name().as_deref(), Some("anthony/fix-login"));
        let held = Tree {
            status: "leased".into(),
            ..free
        };
        let mut prompt = Prompt::start(held, "anthony".into());
        type_in(&mut prompt, "Fix login");
        assert_eq!(prompt.branch_name(), None);
    }

    #[test]
    fn names_a_session_from_the_task() {
        assert_eq!(
            session_name("END-123: fix the login redirect, then add a test for it").as_deref(),
            Some("END-123 fix the login redirect then")
        );
        assert_eq!(session_name("  ...  "), None);
    }
}
