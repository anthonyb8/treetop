//! What the assistant keeps on screen: a briefing of where the agents stand,
//! and cards, each an action it proposes that one key takes. The assistant
//! is the coordinator's session; it writes both through `treetop brief` and
//! `treetop propose`, and treetop draws them and carries out a card taken.
//! Both live in one file per workspace, written through a temporary file and
//! a rename, so the screen never reads half of it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::cache;

/// How long a card stays when nobody takes or drops it.
const CARD_LIFE: u64 = 24 * 60 * 60;

/// An action a card proposes: what the key for it would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// `send <tree> <message>`: a message to the tree's agent, as `m` sends.
    Send,
    /// `dispatch <new|main|N> <task>`: a new agent on a task, as `n` starts.
    Dispatch,
    /// `pick <issue>`: the issue picked up into a new tree.
    Pick,
    /// `return <tree>`: the tree's return dialog.
    Return,
    /// `open <url>`: the link in the browser.
    Open,
}

impl Action {
    pub fn parse(word: &str) -> Option<Action> {
        match word {
            "send" => Some(Action::Send),
            "dispatch" | "start" => Some(Action::Dispatch),
            "pick" => Some(Action::Pick),
            "return" => Some(Action::Return),
            "open" => Some(Action::Open),
            _ => None,
        }
    }

    /// The arguments it takes, the last taking the rest of the words.
    fn usage(self) -> &'static str {
        match self {
            Action::Send => "send <tree> <message>",
            Action::Dispatch => "dispatch <new|main|N> <task>",
            Action::Pick => "pick <issue>",
            Action::Return => "return <tree>",
            Action::Open => "open <url>",
        }
    }

    /// How many words it takes before the last, which takes the rest.
    fn leading(self) -> usize {
        match self {
            Action::Send | Action::Dispatch => 1,
            Action::Pick | Action::Return | Action::Open => 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardState {
    #[default]
    Open,
    Taken,
    Dropped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: String,
    pub action: Action,
    /// Its arguments: the tree or place, then the message or task; or the
    /// issue, tree or link alone.
    pub args: Vec<String>,
    /// Why the assistant proposes it, in a line.
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub state: CardState,
    /// What treetop said when it carried the card out.
    #[serde(default)]
    pub outcome: Option<String>,
    /// When it was proposed, in seconds since the epoch.
    #[serde(default)]
    pub at: u64,
}

impl Card {
    /// The card as one line: what it does, to what.
    pub fn summary(&self) -> String {
        let arg = |at: usize| self.args.get(at).map(String::as_str).unwrap_or_default();
        match self.action {
            Action::Send => format!("Tell {}: \"{}\"", place(arg(0)), arg(1)),
            Action::Dispatch => format!("Start in {}: {}", place(arg(0)), arg(1)),
            Action::Pick => format!("Pick up {}", arg(0)),
            Action::Return => format!("Return {}", place(arg(0))),
            Action::Open => format!("Open {}", arg(0)),
        }
    }

    /// The card as it is typed in the assistant's box, which sends it:
    /// `@17 message`, `/start new task`, `/pick END-1`, `/return 33`, `/open url`.
    pub fn box_text(&self) -> String {
        let arg = |at: usize| self.args.get(at).map(String::as_str).unwrap_or_default();
        match self.action {
            Action::Send => format!("@{} {}", arg(0), arg(1)),
            Action::Dispatch => format!("/start {} {}", arg(0), arg(1)),
            Action::Pick => format!("/pick {}", arg(0)),
            Action::Return => format!("/return {}", arg(0)),
            Action::Open => format!("/open {}", arg(0)),
        }
    }
}

/// A tree or place as a card says it: `tree 4`, `the main checkout`, `a new
/// tree`, or an agent's name as given.
fn place(place: &str) -> String {
    match place {
        "new" => "a new tree".into(),
        "main" => "the main checkout".into(),
        n if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => format!("tree {n}"),
        name => name.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Briefing {
    /// Markdown: what needs you, what is done, what is working.
    pub markdown: String,
    /// When it was written, in seconds since the epoch.
    pub at: u64,
}

/// A line of the conversation in the pane: what you asked, or what the
/// assistant said back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Said {
    /// Markdown for the assistant's, plain text for yours.
    pub text: String,
    /// When it was said, in seconds since the epoch.
    pub at: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    pub briefing: Option<Briefing>,
    /// What you last asked through the pane's box.
    #[serde(default)]
    pub asked: Option<Said>,
    /// What the assistant last said back, through `treetop say`.
    #[serde(default)]
    pub reply: Option<Said>,
    /// Newest first.
    #[serde(default)]
    pub cards: Vec<Card>,
    /// The number the next card's id takes, so an id is never reused.
    #[serde(default)]
    pub next: u64,
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// `$XDG_STATE_HOME/treetop`, else `~/.local/state/treetop`.
fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".local/state")))?;
    Some(base.join("treetop"))
}

/// The state file of the workspace checked out at `checkout`.
pub fn path(checkout: &Path) -> Option<PathBuf> {
    Some(dir()?.join(format!("assistant-{}.json", cache::key(checkout))))
}

/// The state at `path`, or none yet; an unreadable file reads as none.
pub fn read_from(path: &Path) -> State {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_to(path: &Path, state: &State) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("json.partial");
    fs::write(&partial, serde_json::to_string_pretty(state)?)?;
    fs::rename(partial, path)
}

/// Reads the state at `path`, changes it with `change`, and writes it back.
pub fn update<T>(path: &Path, change: impl FnOnce(&mut State) -> T) -> io::Result<T> {
    let mut state = read_from(path);
    let out = change(&mut state);
    write_to(path, &state)?;
    Ok(out)
}

impl State {
    /// Puts `markdown` in as the briefing, written `at`. The cards it has
    /// overtaken go: taken, dropped and failed ones, and any a day old.
    pub fn brief(&mut self, markdown: &str, at: u64) {
        self.briefing = Some(Briefing {
            markdown: markdown.trim().to_string(),
            at,
        });
        self.cards
            .retain(|card| card.state == CardState::Open && at.saturating_sub(card.at) < CARD_LIFE);
    }

    /// Records what you asked, which the reply then answers.
    pub fn ask(&mut self, text: &str, at: u64) {
        self.asked = Some(Said {
            text: text.trim().to_string(),
            at,
        });
    }

    /// Puts `markdown` up as the assistant's answer.
    pub fn say(&mut self, markdown: &str, at: u64) {
        self.reply = Some(Said {
            text: markdown.trim().to_string(),
            at,
        });
    }

    /// Whether the assistant has yet to answer what was last asked.
    pub fn is_thinking(&self) -> bool {
        self.asked
            .as_ref()
            .is_some_and(|asked| self.reply.as_ref().is_none_or(|r| r.at < asked.at))
    }

    /// Adds a card for `action` with `words`, its arguments as typed, the
    /// last taking the rest; returns its id, or how to call it.
    pub fn propose(
        &mut self,
        action: Action,
        words: &[String],
        why: &str,
        at: u64,
    ) -> Result<String, String> {
        let leading = action.leading();
        if words.len() <= leading || words.iter().all(|w| w.trim().is_empty()) {
            return Err(format!(
                "usage: treetop propose {} --why <text>",
                action.usage()
            ));
        }
        let mut args: Vec<String> = words[..leading].to_vec();
        args.push(words[leading..].join(" "));
        // The same proposal twice is one card.
        if let Some(card) = self
            .cards
            .iter()
            .find(|c| c.state == CardState::Open && c.action == action && c.args == args)
        {
            return Ok(card.id.clone());
        }
        self.next += 1;
        let id = format!("c{}", self.next);
        self.cards.insert(
            0,
            Card {
                id: id.clone(),
                action,
                args,
                why: why.trim().to_string(),
                state: CardState::Open,
                outcome: None,
                at,
            },
        );
        Ok(id)
    }

    /// The cards still waiting on you, newest first.
    pub fn open_cards(&self) -> Vec<&Card> {
        self.cards
            .iter()
            .filter(|c| c.state == CardState::Open)
            .collect()
    }

    /// Records what became of card `id`.
    pub fn settle(&mut self, id: &str, state: CardState, outcome: Option<String>) {
        if let Some(card) = self.cards.iter_mut().find(|c| c.id == id) {
            card.state = state;
            card.outcome = outcome;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_proposal_takes_its_words_the_last_taking_the_rest() {
        let mut state = State::default();
        let id = state
            .propose(
                Action::Send,
                &words("17 Put it in branding/."),
                "17 asks",
                100,
            )
            .unwrap();
        assert_eq!(id, "c1");
        let card = &state.cards[0];
        assert_eq!(card.args, ["17", "Put it in branding/."]);
        assert_eq!(card.summary(), "Tell tree 17: \"Put it in branding/.\"");
        assert_eq!(card.box_text(), "@17 Put it in branding/.");
        assert_eq!(
            state.propose(
                Action::Send,
                &words("17 Put it in branding/."),
                "again",
                101
            ),
            Ok("c1".into()),
            "the same proposal is one card"
        );
        state
            .propose(Action::Return, &words("33"), "merged", 102)
            .unwrap();
        assert_eq!(state.cards[0].summary(), "Return tree 33");
        assert!(
            state
                .propose(Action::Dispatch, &words("new"), "", 103)
                .unwrap_err()
                .contains("dispatch <new|main|N> <task>")
        );
    }

    #[test]
    fn a_briefing_clears_the_cards_it_overtakes() {
        let mut state = State::default();
        for (at, tree) in [(0, "1"), (CARD_LIFE, "2"), (CARD_LIFE, "3")] {
            state.propose(Action::Return, &words(tree), "", at).unwrap();
        }
        state.settle("c2", CardState::Taken, Some("returned".into()));
        state.brief("## Needs you\n\n- nothing\n", CARD_LIFE + 10);
        let left: Vec<&str> = state.cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(left, ["c3"], "taken and day-old cards go");
        assert_eq!(
            state.briefing.unwrap().markdown,
            "## Needs you\n\n- nothing"
        );
        assert_eq!(state.next, 3, "ids are not reused");
    }

    #[test]
    fn the_state_survives_a_write_and_a_read() {
        let dir = std::env::temp_dir().join(format!("treetop-assistant-{}", std::process::id()));
        let path = dir.join("assistant.json");
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(read_from(&path), State::default(), "none yet");
        let id = update(&path, |s| {
            s.brief("hi", 5);
            s.propose(Action::Open, &words("https://x.dev/pr/1"), "CI", 5)
        })
        .unwrap()
        .unwrap();
        let state = read_from(&path);
        assert_eq!(state.cards[0].id, id);
        assert_eq!(state.briefing.unwrap().markdown, "hi");
        let _ = fs::remove_dir_all(&dir);
    }
}
