//! `treetop hook pre-tool-use`: Claude Code's PreToolUse hook, which holds an
//! agent's shell commands to the guard. Agents give trees back with
//! `treehouse return` and `destroy` as often as through treetop, and
//! treehouse's `--if-lease-holder` takes any name it is handed, so the guard
//! has to see those too. A Bash command is split into the commands it runs,
//! following `cd`s and `sh -c`, and one that would act on a tree is judged:
//!
//! - `treehouse return` or `destroy` of one tree, as `treetop return` is;
//! - `treehouse return --all` or `destroy --all`, never, since they take
//!   every tree in the pool;
//! - a command the config's `[guard] in_tree` names, which acts on the tree
//!   it runs in, only in a tree the agent holds.
//!
//! A refused command is denied with the guard's reason, which the agent
//! reads. Anything else gets no answer, so Claude Code's own permissions
//! decide as before: the hook never allows what they would not. It reads
//! words, not what a shell would run, so it is a guard rail against an
//! agent's mistake, not a sandbox against one bent on getting round it.

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::cli;
use crate::config;
use crate::guard::{self, Caller};
use crate::pool;

/// The PreToolUse event, as far as the guard reads it.
#[derive(Deserialize)]
struct Event {
    tool_name: String,
    #[serde(default)]
    tool_input: Value,
    /// Where the session stands.
    #[serde(default)]
    cwd: Option<PathBuf>,
}

/// Something a command would do to a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    /// `treehouse <verb>` of the tree `target` names, else of the tree it
    /// runs in, `dir`.
    GiveBack {
        verb: String,
        target: Option<String>,
        dir: PathBuf,
    },
    /// A command that takes every tree in the pool.
    PoolWide(String),
    /// A command from `in_tree`, run in `dir`.
    InTree { command: String, dir: PathBuf },
}

/// Answers Claude Code's hook `event`, read from stdin.
pub fn run(event: &str) -> Result<()> {
    if event != "pre-tool-use" {
        bail!("treetop hook answers pre-tool-use, not {event}");
    }
    let mut text = String::new();
    io::stdin().read_to_string(&mut text)?;
    let Ok(event) = serde_json::from_str::<Event>(&text) else {
        return Ok(());
    };
    if let Some(why) = refusal(&event) {
        let deny = json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": format!("treetop's guard: {why}"),
            }
        });
        println!("{deny}");
    }
    Ok(())
}

/// Why the guard refuses the event's command, None when it does not.
fn refusal(event: &Event) -> Option<String> {
    if event.tool_name != "Bash" {
        return None;
    }
    let command = event.tool_input.get("command")?.as_str()?;
    let cwd = event.cwd.clone().or_else(|| std::env::current_dir().ok())?;
    let in_tree = config::load().map(|c| c.guard.in_tree).unwrap_or_default();
    let acts = acts(command, &cwd, &in_tree);
    if acts.is_empty() {
        return None;
    }
    let caller = Caller::this(&cwd);
    acts.iter().find_map(|act| judge(act, &caller).err())
}

/// Whether the caller may do `act`.
fn judge(act: &Act, caller: &Caller) -> Result<(), String> {
    match act {
        Act::PoolWide(command) => Err(format!(
            "`{command}` acts on every tree in the pool; only the user can run it"
        )),
        Act::GiveBack { verb, target, dir } => {
            // A path names its tree wherever it is; a name, in the pool of
            // the repository the command runs in.
            let path = target.as_deref().map(|t| absolute(t, dir));
            let at = path.as_deref().filter(|p| p.exists()).unwrap_or(dir);
            let Ok(checkout) = pool::main_checkout(at) else {
                return Ok(());
            };
            let trees = cli::full_listing(&checkout).map_err(|err| {
                format!("it cannot read the pool ({err}), so it cannot tell whose tree this is")
            })?;
            let tree = match (&path, target) {
                (Some(path), _) if path.exists() => pool::holding(&trees, &resolved(path)),
                (_, Some(name)) => trees.iter().find(|t| &t.name == name),
                (_, None) => pool::holding(&trees, &resolved(dir)),
            };
            let Some(tree) = tree else {
                return Ok(());
            };
            if !tree.is_main
                && !tree.is_held()
                && guard::live_pids(tree, &caller.ancestry).is_empty()
            {
                return Ok(());
            }
            guard::may_return(&trees, tree, caller).map_err(|why| format!("no {verb}: {why}"))
        }
        Act::InTree { command, dir } => {
            let Ok(checkout) = pool::main_checkout(dir) else {
                return Ok(());
            };
            let trees = cli::agent_listing(&checkout).map_err(|err| {
                format!("it cannot read the pool ({err}), so it cannot tell whose tree this is")
            })?;
            let Some(tree) = pool::holding(&trees, &resolved(dir)) else {
                return Ok(());
            };
            guard::may_act_in(&trees, tree, caller).map_err(|why| format!("no `{command}`: {why}"))
        }
    }
}

/// `path` resolved against `dir`, with a leading `~` as the home directory.
fn absolute(path: &str, dir: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (path, path.strip_prefix("~/"), home) {
        ("~", _, Some(home)) => home,
        (_, Some(rest), Some(home)) => home.join(rest),
        _ => dir.join(path),
    }
}

/// `path` with its symlinks and `..` resolved, as /proc and treehouse give
/// paths; as it is when it does not exist.
fn resolved(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// The commands `command` runs, each as its words, split where a shell
/// would end one: at `;`, `&`, `|`, a newline or a parenthesis, outside
/// quotes.
fn commands(command: &str) -> Vec<Vec<String>> {
    let mut commands = vec![Vec::new()];
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                let next = chars.next().unwrap_or('\\');
                word.get_or_insert_default().push(next);
            }
            (Some(_), c) => word.get_or_insert_default().push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            (None, '\\') => {
                if let Some(next) = chars.next().filter(|n| *n != '\n') {
                    word.get_or_insert_default().push(next);
                }
            }
            (None, ';' | '&' | '|' | '\n' | '(' | ')') => {
                let last = commands.last_mut().expect("starts with one");
                last.extend(word.take());
                commands.push(Vec::new());
            }
            (None, c) if c.is_whitespace() => {
                let last = commands.last_mut().expect("starts with one");
                last.extend(word.take());
            }
            (None, c) => word.get_or_insert_default().push(c),
        }
    }
    commands.last_mut().expect("starts with one").extend(word);
    commands.retain(|words| !words.is_empty());
    commands
}

/// A variable assignment before a command, such as `FOO=bar`.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// The file name of a program, as it is run by path or by name.
fn program(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

/// What `command`, run in `cwd`, would do to trees, with `in_tree` the
/// commands that act on the tree they run in.
fn acts(command: &str, cwd: &Path, in_tree: &[String]) -> Vec<Act> {
    let mut dir = cwd.to_path_buf();
    let mut acts = Vec::new();
    for words in commands(command) {
        act_of(&words, &mut dir, in_tree, &mut acts);
    }
    acts
}

/// Wrappers that run the command after them, and what each skips first.
const WRAPPERS: [&str; 6] = ["command", "exec", "env", "nohup", "time", "sudo"];

/// Treehouse flags that take a value.
const VALUED: [&str; 6] = [
    "--root",
    "--if-lease-holder",
    "--if-lease-id",
    "--lease-holder",
    "--base",
    "--branch",
];

fn act_of(words: &[String], dir: &mut PathBuf, in_tree: &[String], acts: &mut Vec<Act>) {
    let words: Vec<&str> = words
        .iter()
        .map(String::as_str)
        .skip_while(|w| {
            matches!(*w, "{" | "}" | "!") || is_assignment(w) || WRAPPERS.contains(&program(w))
        })
        .collect();
    let Some((first, args)) = words.split_first() else {
        return;
    };
    match program(first) {
        "cd" | "pushd" => match args.first() {
            Some(&"-") => {}
            Some(to) => *dir = absolute(to, dir),
            None => *dir = absolute("~", dir),
        },
        "sh" | "bash" | "zsh" | "dash" => {
            let script = args
                .iter()
                .position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
                .and_then(|at| args.get(at + 1));
            if let Some(script) = script {
                let mut inner = dir.clone();
                for words in commands(script) {
                    act_of(&words, &mut inner, in_tree, acts);
                }
            }
        }
        "treehouse" => acts.extend(treehouse(args, dir)),
        _ => {}
    }
    let named = in_tree.iter().find(|line| {
        let mut want = line.split_whitespace();
        want.next() == Some(program(first))
            && want.enumerate().all(|(at, w)| args.get(at) == Some(&w))
    });
    if let Some(line) = named {
        acts.push(Act::InTree {
            command: line.clone(),
            dir: dir.clone(),
        });
    }
}

/// What a `treehouse` command with `args`, run in `dir`, does to trees.
fn treehouse(args: &[&str], dir: &Path) -> Vec<Act> {
    let (mut verb, mut targets, mut is_all) = (None, Vec::new(), false);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if let Some(flag) = arg.strip_prefix("--") {
            let (name, has_value) = flag
                .split_once('=')
                .map_or((flag, false), |(n, _)| (n, true));
            if !has_value && VALUED.contains(&format!("--{name}").as_str()) {
                args.next();
            }
            is_all |= name == "all";
        } else if arg.starts_with('-') {
            continue;
        } else if verb.is_none() {
            verb = Some(*arg);
        } else {
            targets.push(arg.to_string());
        }
    }
    let Some(verb @ ("return" | "destroy")) = verb else {
        return Vec::new();
    };
    if is_all {
        return vec![Act::PoolWide(format!("treehouse {verb} --all"))];
    }
    let give_back = |target| Act::GiveBack {
        verb: verb.to_string(),
        target,
        dir: dir.to_path_buf(),
    };
    if targets.is_empty() {
        return vec![give_back(None)];
    }
    targets.into_iter().map(|t| give_back(Some(t))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn here() -> PathBuf {
        PathBuf::from("/p/27/app")
    }

    fn give_back(verb: &str, target: Option<&str>, dir: &str) -> Act {
        Act::GiveBack {
            verb: verb.into(),
            target: target.map(str::to_string),
            dir: PathBuf::from(dir),
        }
    }

    #[test]
    fn splits_a_command_where_a_shell_would_and_keeps_quoted_words_whole() {
        assert_eq!(
            commands(r#"cd /p/28 && FOO=1 treehouse return "a b" --force; echo 'x;y' | wc"#),
            [
                vec!["cd", "/p/28"],
                vec!["FOO=1", "treehouse", "return", "a b", "--force"],
                vec!["echo", "x;y"],
                vec!["wc"],
            ]
        );
        assert_eq!(commands("(cd x) "), [vec!["cd", "x"]]);
    }

    #[test]
    fn finds_the_return_of_tree_28_as_it_was_run() {
        let command = r#"cd /p/28/app && devstack down && treehouse return /p/28/app --if-lease-holder "claude:feature/end-860" --force"#;
        let in_tree = ["devstack down".to_string()];
        assert_eq!(
            acts(command, &here(), &in_tree),
            [
                Act::InTree {
                    command: "devstack down".into(),
                    dir: PathBuf::from("/p/28/app"),
                },
                give_back("return", Some("/p/28/app"), "/p/28/app"),
            ]
        );
    }

    #[test]
    fn a_return_names_its_tree_by_path_name_or_where_it_runs() {
        assert_eq!(
            acts("treehouse return", &here(), &[]),
            [give_back("return", None, "/p/27/app")]
        );
        assert_eq!(
            acts("/usr/bin/treehouse --root /p destroy 28 31", &here(), &[]),
            [
                give_back("destroy", Some("28"), "/p/27/app"),
                give_back("destroy", Some("31"), "/p/27/app"),
            ]
        );
        assert_eq!(
            acts("treehouse return --all --force", &here(), &[]),
            [Act::PoolWide("treehouse return --all".into())]
        );
        assert_eq!(
            acts("bash -lc 'cd ../28 && treehouse return'", &here(), &[]),
            [give_back("return", None, "/p/27/app/../28")]
        );
    }

    #[test]
    fn other_commands_and_other_treehouse_verbs_are_nothing_to_judge() {
        for command in [
            "treehouse status --json",
            "treehouse get --lease --branch return",
            "git status",
            "echo treehouse return",
            "devstack up",
        ] {
            assert_eq!(
                acts(command, &here(), &["devstack down".into()]),
                [],
                "{command}"
            );
        }
    }
}
