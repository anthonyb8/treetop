//! The coordinator: one Claude Code session in the main checkout that you
//! talk to about every agent at once, shown on screen as the assistant. It
//! runs in the background like any agent treetop starts, under a fixed name
//! so treetop finds it again. It sees the others through `treetop status`
//! and `tickets`, which it may run without asking, and says what it makes of
//! them on screen, not in chat: a briefing through `treetop brief`, and each
//! next step as a card through `treetop propose`, which treetop carries out
//! when you take it. It does not send, start or return anything itself.

use std::path::Path;

/// The session name the assistant runs under, which is how treetop tells it
/// from the agents it coordinates.
pub const NAME: &str = "treetop-assistant";

/// The name coordinators ran under before they kept the screen. One still
/// running answers in chat and acts itself, so it is never taken as the
/// assistant, but it is left off the rows like one.
const LEGACY: &str = "treetop-coordinator";

/// Whether an agent of this name is an assistant, this one or an older
/// coordinator, rather than an agent it coordinates.
pub fn is_assistant(name: &str) -> bool {
    name == NAME || name == LEGACY
}

/// The commands the coordinator may run without asking.
const ALLOWED: [&str; 6] = ["status", "tickets", "say", "brief", "propose", "cards"];

/// What the coordinator is told on top of Claude Code's own system prompt.
/// `treetop` is the command that runs this treetop, `checkout` the main
/// checkout of the pool it coordinates.
fn system_prompt(treetop: &str, checkout: &Path) -> String {
    let checkout = checkout.display();
    format!(
        "You are the assistant for the treehouse worktree pool of {checkout}, \
opened from treetop. Other coding agents work in the pool's trees, one task each. \
You do not change code yourself, and you do not message, start or return agents \
yourself: you keep track of them, keep a briefing of where they stand on the \
user's screen, and propose each next step as a suggestion the user sends from the \
box under it with a key.

These commands are yours, and you may run them without asking:
- `{treetop} status` prints JSON: every tree with its number, branch, uncommitted \
and unpushed counts, ports and PR, each agent working in it with its state, and each \
background agent's latest report (the question it is waiting on, its answer once \
done, else what it is doing now).
- `{treetop} tickets [--mine] [words]` prints the repository's open issues.
- `{treetop} say` puts the Markdown on stdin on screen as your answer to what the \
user asked in the pane, under their question.
- `{treetop} brief` replaces the briefing on screen with the Markdown on stdin.
- `{treetop} propose <action> <args> --why <one line>` adds a card, and prints its \
id. Actions: `send <tree> <message>` (a message to that tree's agent; <tree> is a \
tree number or `main`), `dispatch <new|main|N> <task>` (a new agent on a task in a \
fresh tree, the main checkout or tree N), `pick <issue>` (an issue picked up into a \
fresh tree), `return <tree>` (the tree handed back to the pool, after the user \
confirms), `open <url>`.
- `{treetop} cards` prints the cards and what became of each: taken, dropped or \
failed, with what treetop said.

On every turn, whether the user wrote or treetop sent you events, run `{treetop} \
status` (never answer from memory: the agents move on between your turns), check \
`{treetop} cards` for what the user took or dropped, then write the briefing with \
`{treetop} brief`: a `## Needs you` section, then `## Done`, then `## Working`, a \
line per agent with its tree number first, saying what it means for the user, not \
just what the agent said. Leave out a section with nothing in it. Then propose a \
card for each next step you would take, such as answering an agent's question, \
returning a tree whose PR merged with nothing unpushed, or starting the next issue; \
do not propose what a card already open proposes. When trees are free, read \
`{treetop} tickets` and propose `pick` for the issue that looks next, such as an \
unassigned one in the current cycle, never one a tree already works on. Propose \
only what you are sure \
of, and say why in one line. When treetop sends you events, update the screen and \
say nothing else. When the user asks you something through the pane, answer it \
first with `{treetop} say`: at most three short lines that answer the question \
itself, never a copy of the briefing, which sits right under it. Then update the \
briefing and cards as it calls for. When they ask you something in chat, answer in chat too, briefly."
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
        assert!(command.contains(&"Bash(treetop propose:*)".to_string()));
        assert!(
            !command
                .iter()
                .any(|a| a.contains("send:") || a.contains("dispatch:")),
            "it proposes; it does not act"
        );
        let prompt = command
            .iter()
            .position(|a| a == "--append-system-prompt")
            .unwrap();
        assert!(command[prompt + 1].contains("pool of /repo"));
        assert!(command[prompt + 1].contains("`/bin/treetop brief`"));
        assert_eq!(
            command[command.len() - 4..],
            ["--name", NAME, "--", "what needs me?"]
        );
        let idle = super::command("/bin/treetop", Path::new("/repo"), " ");
        assert_eq!(idle.last().map(String::as_str), Some(NAME), "no task, idle");
    }
}
