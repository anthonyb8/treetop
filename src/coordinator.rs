//! The coordinator: one Claude Code session in the main checkout that you
//! talk to about every agent at once. It runs in the background like any
//! agent treetop starts, under a fixed name so treetop finds it again, and
//! sees and steers the others through treetop's own non-interactive commands
//! (`cli`), which it may run without asking. Those commands print only what
//! it asks for, so a question costs it one small JSON read rather than a
//! crawl through every agent's transcript.

use std::path::Path;

/// The session name the coordinator runs under, which is how treetop tells it
/// from the agents it coordinates.
pub const NAME: &str = "treetop-coordinator";

/// What the coordinator is told on top of Claude Code's own system prompt.
/// `treetop` is the command that runs this treetop, `checkout` the main
/// checkout of the pool it coordinates.
fn system_prompt(treetop: &str, checkout: &Path) -> String {
    let checkout = checkout.display();
    format!(
        "You are the coordinator for the treehouse worktree pool of {checkout}, \
opened from treetop. Other coding agents work in the pool's trees, one task each. \
You do not change code yourself: you keep track of the agents, answer questions \
about them, and pass work and messages to them.

These commands are yours, and you may run them without asking:
- `{treetop} status` prints JSON: every tree with its number, branch, uncommitted \
and unpushed counts and ports, each agent working in it with its state, and each \
background agent's latest report (the question it is waiting on, its answer once \
done, else what it is doing now).
- `{treetop} send <tree|agent> <message>` types the message into that agent's chat \
and submits it. <tree> is a tree number or `main`; an agent's name also works.
- `{treetop} dispatch <new|main|N> <task>` starts a new Claude Code agent on the \
task in a fresh tree, the main checkout, or tree N.

Run `{treetop} status` before answering anything about the agents; never answer \
from memory, since they move on between your turns. Lead with what is waiting on \
the user, then what has finished, then what is still working. Keep it short: a \
line per agent, its tree number first. Before sending or dispatching, say what you \
are about to do and wait for a yes, unless the user has already told you to go \
ahead."
    )
}

/// The command that starts the coordinator, with `task` as its first message
/// when there is one.
pub fn command(treetop: &str, checkout: &Path, task: &str) -> Vec<String> {
    let mut command: Vec<String> = ["claude", "--bg", "--allowedTools"]
        .map(String::from)
        .into();
    // The tool list is variadic, so the flag after it ends it.
    for (program, subcommand) in [treetop, "treetop"]
        .into_iter()
        .flat_map(|p| ["status", "send", "dispatch"].map(|s| (p, s)))
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

/// How the coordinator runs this treetop: the path of the running binary, so
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
        assert!(command.contains(&"Bash(treetop dispatch:*)".to_string()));
        let prompt = command
            .iter()
            .position(|a| a == "--append-system-prompt")
            .unwrap();
        assert!(command[prompt + 1].contains("pool of /repo"));
        assert!(command[prompt + 1].contains("`/bin/treetop send <tree|agent> <message>`"));
        assert_eq!(
            command[command.len() - 4..],
            ["--name", NAME, "--", "what needs me?"]
        );
        let idle = super::command("/bin/treetop", Path::new("/repo"), " ");
        assert_eq!(idle.last().map(String::as_str), Some(NAME), "no task, idle");
    }
}
