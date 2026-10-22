//! The assistant: one Claude Code session in the main checkout that you
//! talk to about every agent at once. It runs in the background like any
//! agent treetop starts, under a fixed name so treetop finds it again. It sees the others through `treetop status` and
//! `tickets`, which it may run without asking, and answers in its chat. It
//! does not send, start or return anything itself; it says what it would do,
//! and you do it with treetop's keys.

use std::path::Path;

/// The session name the assistant runs under, which is how treetop tells it
/// from the agents it coordinates.
pub const NAME: &str = "treetop-assistant";

/// The name the assistant ran under when it was the coordinator, which
/// acted itself. One still running is never taken as the assistant, but it
/// is left off the rows like one.
const LEGACY: &str = "treetop-coordinator";

/// Whether an agent of this name is an assistant, this one or an old
/// coordinator, rather than an agent it keeps track of.
pub fn is_assistant(name: &str) -> bool {
    name == NAME || name == LEGACY
}

/// The commands the assistant may run without asking.
const ALLOWED: [&str; 2] = ["status", "tickets"];

/// What the assistant is told on top of Claude Code's own system prompt.
/// `treetop` is the command that runs this treetop, `checkout` the main
/// checkout of the pool it coordinates.
fn system_prompt(treetop: &str, checkout: &Path) -> String {
    let checkout = checkout.display();
    format!(
        "You are the assistant for the treehouse worktree pool of {checkout}, \
opened from treetop. Other coding agents work in the pool's trees, one task each. \
You do not change code yourself, and you do not message, start or return agents \
yourself: you keep track of them, answer questions about them, and say what you \
would do next, which the user does from treetop.

These commands are yours, and you may run them without asking:
- `{treetop} status` prints JSON: every tree with its number, branch, uncommitted \
and unpushed counts, ports and PR, each agent working in it with its state, and each \
background agent's latest report (the question it is waiting on, its answer once \
done, else what it is doing now).
- `{treetop} tickets [--mine] [words]` prints the repository's open issues.

Run `{treetop} status` before answering anything about the agents; never answer \
from memory, since they move on between your turns. Lead with what is waiting on \
the user, then what has finished, then what is still working. Keep it short: a \
line per agent, its tree number first, saying what it means for the user, not just \
what the agent said. End with the next steps you would take, a line each, such as \
answering an agent's question, returning a tree whose PR merged with nothing \
unpushed, or starting the next issue from `{treetop} tickets`."
    )
}

/// The command that starts the assistant, with `task` as its first message
/// when there is one.
pub fn command(treetop: &str, checkout: &Path, task: &str) -> Vec<String> {
    let mut command: Vec<String> = ["claude", "--bg", "--allowedTools"]
        .map(String::from)
        .into();
    // The tool list is variadic, so the flag after it ends it.
    for (program, subcommand) in [treetop, "treetop"]
        .into_iter()
        .flat_map(|p| ALLOWED.map(|s| (p, s)))
    {
        command.push(format!("Bash({program} {subcommand}:*)"));
    }
    command.extend([
        "--append-system-prompt".into(),
        system_prompt(treetop, checkout),
        "--name".into(),
        NAME.into(),
    ]);
    if !task.trim().is_empty() {
        command.extend(["--".into(), task.into()]);
    }
    command
}

/// How the assistant runs this treetop: the path of the running binary, so
/// it works whether or not treetop is on its PATH.
pub fn treetop() -> String {
    std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "treetop".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_claude_named_with_its_commands_allowed_and_the_task_last() {
        let command = command("/bin/treetop", Path::new("/repo"), "what needs me?");
        assert_eq!(command[..3], ["claude", "--bg", "--allowedTools"]);
        assert!(command.contains(&"Bash(/bin/treetop status:*)".to_string()));
        assert!(command.contains(&"Bash(treetop tickets:*)".to_string()));
        assert!(
            !command
                .iter()
                .any(|a| a.contains("send:") || a.contains("dispatch:")),
            "it advises; it does not act"
        );
        let prompt = command
            .iter()
            .position(|a| a == "--append-system-prompt")
            .unwrap();
        assert!(command[prompt + 1].contains("pool of /repo"));
        assert!(command[prompt + 1].contains("`/bin/treetop status`"));
        assert_eq!(
            command[command.len() - 4..],
            ["--name", NAME, "--", "what needs me?"]
        );
        let idle = super::command("/bin/treetop", Path::new("/repo"), " ");
        assert_eq!(idle.last().map(String::as_str), Some(NAME), "no task, idle");
    }
}
