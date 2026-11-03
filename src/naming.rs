//! A short title and branch for a task, written by a small, fast model, so a
//! quickly typed task such as "can you scan he project and cmpiel a list"
//! shows as "Scan project for useful tools" on `scan-project-tools` rather
//! than as its first words. It runs `claude -p` on Haiku with no tools, no
//! saved session and no user settings, beside the tree's lease so it costs
//! the start little. Anything that goes wrong, from no `claude` to an answer
//! that is not a name, gives None, and the task's own words name it as
//! before.

use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::child::{self, End};

/// How long the model gets to answer before the task's words are used.
const LIMIT: Duration = Duration::from_secs(10);

/// The most of a task the model is shown; its start says what it is.
const SHOWN: usize = 2000;

/// The longest title kept, as session names are cut.
const LONGEST_TITLE: usize = 48;

const ASK: &str = "Name this coding task for a list of agents working in parallel. \
Reply with only JSON, {\"title\": \"...\", \"branch\": \"...\"}: the title 3 to 5 words \
in sentence case, the branch 2 to 5 lowercase words joined by hyphens. Correct any typos \
and say what the work is, not how it was asked. Keep an issue id such as END-123 at the \
front of both. The task follows.\n\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Name {
    /// The session's name, as `claude agents` and the AGENT column show it.
    pub title: String,
    /// The branch's slug, without the user it is filed under.
    pub slug: String,
}

#[derive(Deserialize)]
struct Answer {
    title: String,
    branch: String,
}

/// A title and branch slug for `task`, or None to name it by its words.
pub fn suggest(task: &str) -> Option<Name> {
    let task: String = task.trim().chars().take(SHOWN).collect();
    if task.is_empty() {
        return None;
    }
    let mut command = Command::new("claude");
    command
        .args([
            "-p",
            "--model",
            "haiku",
            "--no-session-persistence",
            "--tools",
            "",
            "--strict-mcp-config",
            "--setting-sources",
            "local",
        ])
        // Outside every repository, so it lands on no tree.
        .current_dir(std::env::temp_dir());
    let ran = child::run(
        &mut command,
        Some(format!("{ASK}{task}").into_bytes()),
        LIMIT,
    )
    .ok()?;
    if !matches!(ran.end, End::Exited(status) if status.success()) {
        return None;
    }
    parse(&String::from_utf8_lossy(&ran.stdout))
}

/// The name in the model's answer, which may wrap its JSON in prose or a
/// code fence.
fn parse(answer: &str) -> Option<Name> {
    let json = &answer[answer.find('{')?..=answer.rfind('}')?];
    let answer: Answer = serde_json::from_str(json).ok()?;
    let title = answer
        .title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title = match title.char_indices().nth(LONGEST_TITLE) {
        Some((cut, _)) => title[..cut].trim_end().to_string(),
        None => title,
    };
    let slug = crate::branch::slug(&answer.branch);
    (!title.is_empty() && !slug.is_empty()).then_some(Name { title, slug })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_read_from_the_answer_and_made_safe() {
        let name = parse(
            "```json\n{\"title\": \"Scan project  for useful tools\", \
             \"branch\": \"Scan Project_Tools\"}\n```",
        )
        .unwrap();
        assert_eq!(name.title, "Scan project for useful tools");
        assert_eq!(name.slug, "scan-project-tools");
    }

    #[test]
    fn an_answer_that_is_not_a_name_names_nothing() {
        assert_eq!(parse("I can't help with that."), None);
        assert_eq!(parse("{\"title\": \"\", \"branch\": \"x\"}"), None);
        assert_eq!(parse("{\"title\": \"Fix it\", \"branch\": \"!!!\"}"), None);
    }
}
