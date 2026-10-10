# Who's in the tree

A project in three issues: what each tree is serving, which agent is working in it, and one key to land in that agent's chat.
Each issue is one PR into `main` and ships on its own as a tagged release.

## Goal

With several coding agents running at once, treetop should answer four questions at a glance and get you there in one key: who is working in this tree, are they waiting on me, what is it serving, and is anything left behind.
treetop already shows a tree's state (branch, holder, processes, uncommitted and unpushed work, and the diff); the missing pieces are the agent and the servers.
Today the HOLDER column says who leased a tree, not which session is working in it, and PROCS counts processes without saying that one of them is a dev server on port 5223.
It is for whoever runs several agents over a treehouse pool from a terminal, with any agent and any setup around it: no wrapper, hook or config file is required.

## Success

- An agent that enters or leaves a tree shows in its row within 4 seconds, with its name and, where the agent exposes it, whether it is busy, idle or waiting on you.
- A server that starts listening inside a tree shows its port in the row within 2 seconds, and one key opens it in the browser.
- Enter on a tree with an interactive agent lands in that agent's tmux pane, in one keypress.
- A leased tree with no agent and nothing running stands out as left behind.
- Left idle, treetop still uses under 5% of one core.
- treetop calls nothing but `treehouse`, `git`, `tmux` and, for `o`, the desktop's URL opener.

## Scope

**In:** listening ports per tree, read from `/proc`; a taller detail pane; opening a port in the browser; agent detection from three sources (a process scan, an open holder-file convention, and Claude Code's own session files); an AGENT column; Enter jumping to the agent's pane; marking orphaned trees; removing treetop's call to `harness stack down`.

**Out:**

- Changing treehouse, or asking it to record agents.
- Messaging or controlling agents, such as asking one to release its tree: an agent leaves a tree through its own command, and treetop's part is making what is left behind obvious.
- Ports of containers started from a tree (a database in Docker): `docker-proxy` runs as root outside the tree, so nothing ties its sockets to one.
- A treetop config file: the one knob, extra agent names, is an environment variable.
- Starting or stopping dev servers.
- Attaching to a background agent session that has no tmux pane; issue 4 adds it, after the first three shipped.

## Design

**Ports come from `/proc` alone.**
Every listening TCP socket appears in `/proc/net/tcp` or `/proc/net/tcp6` with state `0A` and its inode, and every process's open sockets appear as `socket:[<inode>]` links under `/proc/<pid>/fd`.
Matching the two gives each tree's ports without root, `ss` or a new crate.
Measured on this machine: tree 7's vite process (cwd `7/app/ui`) holds inode 497207588, which `/proc/net/tcp6` lists as LISTEN on port 0x1467, which is 5223, the same answer `ss -ltnp` gives.
The fd scan only covers processes already assigned to a tree, a few dozen rather than the 800-odd on the machine, and the two tables are read once per tick.
The earlier spec measured the full fd scan at 12.5 ms across every process, so this stays well inside the 5% budget.

**Agents come from what each agent already leaves behind.**
There are three sources, each in its own module under `src/agents/`, each answering one question: which agent sessions are alive, and which directory is each working in?
treetop maps those directories to trees with the existing `Tree::contains`.
A source that cannot read its files yields nothing; the AGENT column goes blank rather than showing an error.
Where two sources see the same tree, the more specific wins: holder file, then Claude Code, then the process scan.

- **Process scan, for any agent started inside a tree.** A process in the tree whose `comm`, or whose `cmdline` argv[0] or argv[1] basename, is a known agent: `claude`, `codex`, `aider`, `opencode`, `gemini`, `goose`, `amp`, `cursor-agent`. `cmdline` covers agents running under node, whose `comm` is `node` or `MainThread`. `TREETOP_AGENTS=a,b` adds names. It gives a name and a pid, never a status.
- **Holder file, an open convention.** Any agent or wrapper may write `agent.json` into the tree's own git directory (`git rev-parse --git-dir`, which for a pool tree is `<main>/.git/worktrees/<id>`), so it never shows in `git status`. Its fields are `name` and `pid`, and optionally `status` (`busy`, `idle` or `waiting`). treetop ignores it once `pid` is dead, so a crashed agent leaves nothing stale on screen. The README documents it as a public format.
- **Claude Code, for agents that move into a tree mid-session.** Claude Code's `EnterWorktree` moves a session into a tree without changing its process's cwd, so the process scan cannot see it. Instead:
  - treetop reads its live session list, `~/.claude/sessions/<pid>.json`, or `$CLAUDE_CONFIG_DIR/sessions`. Each file has `sessionId`, `name`, `status` (busy, idle or waiting), `kind` (interactive or bg) and `procStart`.
  - A session counts as live when `/proc/<pid>` exists and its `stat` field 22 (start time in clock ticks) equals `procStart`; that rules out a recycled pid. Measured: the two match exactly.
  - The session's tree comes from its transcript, `~/.claude/projects/*/<sessionId>.jsonl`: every entry carries a `"cwd"`, which after `EnterWorktree` is the tree. treetop reads only the last 64 KB and takes the last `cwd` in it.
  - These files are internal to Claude Code and undocumented, so this source is the one most likely to break; see Risks.

**Agent reads run on the 4-second git clock**, not the 2-second process clock.
Agents move between trees far less often than processes start, and the transcript tail is the most expensive read in the project.

**Jumping to an agent.**
Inside tmux, Enter on a tree with an agent switches to the pane it runs in: `switch-client` when it is in another tmux session, then `select-window`, then `select-pane`.
It never creates a pane.
The pane is found by process, not by any id an agent records: it is the pane whose `#{pane_pid}` is the agent, its parent or its grandparent, read from `stat` field 4.
A recorded pane id can be stale, because sessions started one after another in the same pane record the same id, and matching by process needs nothing from the agent, so the jump works for any agent started from a tmux pane.
A background session (`kind: bg`) has no pane and is skipped.
Otherwise Enter does what it does today.

**No harness.**
`src/jobs.rs` `stack_down` runs `harness stack down` before every return and destroy, which ties treetop to one person's setup.
It goes.
`treehouse return` already stops processes left in the tree, and the PORTS column shows anything still listening before you confirm.

- **Schema:** N/A - treetop has no database.
- **Data:** N/A - no backend; the interfaces are `/proc`, files the agents write, and `treehouse`, `git` and `tmux` on PATH.
- **Edge / vendors / env:** reads `TREETOP_AGENTS` and `CLAUDE_CONFIG_DIR` (default `~/.claude`); opens URLs with `xdg-open`, or `open` on macOS.

## Breakdown

| # | Issue | Delivers | Depends on |
|---|---|---|---|
| 1 | Ports and a richer detail pane | a PORTS column, ports in a taller detail pane, `o` to open one, and no harness call | None |
| 2 | Agent detection | an AGENT column fed by the process scan, the holder file and Claude Code | 1 |
| 3 | Jump to the agent, and orphans | Enter lands in the agent's pane; trees with no agent stand out | 2 |
| 4 | Agent chat in place | `c` opens a background agent's chat inside treetop and Ctrl+] always comes back | 2 |

Issue 2 depends on 1 because both widen `Process` and the detail pane; 3 needs 2's agents to know where to jump.

## Rollout

Each issue merges to `main`, then gets a version bump and a `v*` tag, which publishes the release through `.github/workflows/release.yml`.
After each release, reinstall with the `curl ... | sh` line and run the issue's acceptance checks on the real pool, in tmux and outside it.
Rollback is reinstalling the previous version with `TREETOP_VERSION=<previous tag> sh`.
The harness loses treetop's `harness stack down` in issue 1; anyone relying on it stops the stack themselves before returning a tree.

## Risks

- **Claude Code changes its files in a release.** Its session list and transcript format are internal. Guard: the source lives in one module, reads fields by name and tolerates extra ones, yields nothing rather than an error on anything unexpected, and its tests run on fixtures copied from the current format so a change shows up as one failing module.
- **The fd scan costs more than expected.** Guard: it covers only processes already in a tree, and issue 1's acceptance measures idle CPU against the 5% budget.
- **A port shows twice or under the wrong name.** A server listening on both `127.0.0.1` and `::1` has two sockets. Guard: ports are shown by number, once, and both loopback forms read as `localhost`.
- **The wrong agent claims a tree.** Two sources can disagree, for example a stale holder file. Guard: a holder file counts only while its pid is alive, and a second agent in the same tree shows as `+1` rather than hiding.
- **Losing the harness call leaves a stack running after a return.** Containers do not live in the tree, so neither treehouse nor treetop stops them. Guard: noted in Rollout; stopping them belongs to whatever started them.

## Open decisions

- **Attaching to a background Claude session.** Settled by issue 4: `claude attach <id>` opens a running background session without starting a second copy.

---

# Issue 1: Ports and a richer detail pane

## Why

A tree's dev server is the quickest way to see what an agent built, but treetop only says how many processes run in a tree, not that one of them serves port 5223.
Finding it means `ss -ltnp` in another terminal and matching pids to trees by hand.
The detail pane is four rows tall and has no room to say more.
And treetop runs `harness stack down` before returns, which ties a general tool to one setup.

## Outcome

Each row shows the ports its tree is serving, the detail pane lists them with the process behind each, and `o` opens the tree's first port in the browser.

## Repro

N/A - not a bug.

## Scope

**In:**

- Reading `/proc/net/tcp` and `tcp6` once per fast tick, keeping LISTEN rows as inode to port.
- Reading `/proc/<pid>/fd/*` for each process already assigned to a tree, and matching `socket:[<inode>]` links.
- A PORTS column: the lowest port and a count of the rest, `:5223 +1`, muted `-` when there is none.
- A taller Info pane: the path and lease line, an agent line (empty until issue 2), each port as `localhost:5223  node (1245653)`, then the processes.
- `o` opens `http://localhost:<lowest port>` of the tree under the cursor, with the result in the status line.
- The bottom bar counts trees that are serving.
- Removing `stack_down` and its callers from `src/jobs.rs`, and the README lines that describe it.

**Out:** agents (issue 2); UDP and Unix sockets; ports bound to an address other than loopback or any (shown, but `o` opens only localhost).

**Layers:**

- **UI:** a PORTS column; the Info pane grows from 4 rows to fit its lines; `o` and its status line; the bottom bar count.
- **Data:** `Process` gains `ports: Vec<u16>`; `live::processes` fills it.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** `xdg-open` or `open` on PATH for `o`.
- **Tests:** a pure `parse_listen` on literal `tcp` and `tcp6` text (LISTEN rows kept, others dropped, IPv4 and IPv6 hex ports decoded); matching a fixed set of fd links to inodes; the PORTS cell for no ports, one, and several.
- **Risk:** CPU cost of the fd reads (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] On a tree running a dev server (tree 7 serves vite on 5223), PORTS shows `:5223` within 2 s of opening treetop.
- [ ] Stop the server: the port disappears within 2 s; start it again: it returns within 2 s.
- [ ] The Info pane lists `localhost:5223` with the process name and pid that `ss -ltnp` shows for it.
- [ ] Press `o` on that tree: the page opens in the browser; on a tree with no port, the status line says there is nothing to open.
- [ ] Return a tree: no `harness` command runs (checked with the `L` log), and treehouse's return stops the tree's processes.
- [ ] `grep -rn harness src/ README.md` finds nothing.
- [ ] Left idle for a minute with three trees serving, treetop uses under 5% of one core.

## Approach

- `src/live.rs:14-25` `running` and `:45-61` `processes` are where the fd read and the `Process` field join; `assign` (`:29-41`) stays pure.
- `src/pool.rs:12-16` `Process` gains the field; `src/app.rs:202-206` `apply_processes` already carries the whole struct, so no new `Update` variant is needed.
- `src/ui.rs:219-237` holds the header, `row()` and widths, the three places a column is added; `detail_pane` (`:249-285`) and the layout (`:56-71`) give the Info pane its height.
- `src/jobs.rs:117-135` `stack_down` and its call sites go; `README.md:87-88` drops the stack wording.
- Keys are handled in `src/app.rs:439-558`; `o` returns a new `Outcome` that `src/main.rs` runs, as Enter does.

## Open decisions

None.

## Links

**Project:** Who's in the tree. **Blocked by:** None. **Blocks:** Issue 2.

---

# Issue 2: Agent detection

## Why

With several agents at once, nothing on screen says which agent is in which tree.
HOLDER shows whoever leased the tree, which for a tree an agent took is a branch name, and it stays the same after that agent has gone.
PROCS cannot see an agent that moved into a tree mid-session: Claude Code's `EnterWorktree` leaves its process's cwd where it was launched.
On this machine, trees 7, 10 and 20 look busy, with running stacks and editors, but no agent is in any of them, while the one live agent working in a tree (tree 11) shows PROCS 0.

## Outcome

Each row names the agent working in its tree and, where the agent says so, whether it is busy, idle or waiting on you.

## Repro

N/A - not a bug.

## Scope

**In:**

- `src/agents/` with one module per source: `procs.rs` (process scan), `holder.rs` (holder file) and `claude.rs` (Claude Code), each returning agents as name, pid, optional status, whether it runs in the background, and the directory they work in.
- Merging the three by tree with the precedence holder file, Claude Code, process scan.
- An AGENT column: a status glyph and the name; waiting in the attention colour, busy in the normal colour, idle muted; a second agent as `+1`.
- The agent line in the Info pane: name, source, status, and kind where known.
- The `/` filter matching agent names.
- The bottom bar: `N agents · M waiting`.
- Documenting the holder-file format in the README.

**Out:** jumping to agents and orphans (issue 3); writing holder files for any agent.

**Layers:**

- **UI:** the AGENT column, the Info pane's agent line, the filter, the bottom bar.
- **Data:** a new `Update::Agents` sent on the git clock, `App::apply_agents`, an `agents` field on `Tree` carried over in `replace_trees`, as `apply_processes` does for processes.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** reads `TREETOP_AGENTS` and `CLAUDE_CONFIG_DIR`.
- **Tests:** the name match through `cmdline` for a node-hosted agent and a `TREETOP_AGENTS` name; a holder file with a live pid, a dead pid, and malformed JSON; a session file and a transcript tail copied from the current Claude Code format mapping to the right tree; a session whose `procStart` does not match being dropped; precedence when two sources claim one tree.
- **Risk:** Claude Code's internal format (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] A Claude Code session that runs `EnterWorktree` into a tree shows in that tree's row within 4 s, with its session name and status.
- [ ] When that session is waiting on you, its row shows waiting in the attention colour.
- [ ] Start `codex` (or any listed agent) from a shell inside a tree: it shows within 4 s, named, with no status; quit it and it disappears.
- [ ] Write an `agent.json` with your shell's pid into a tree's git dir: it shows; kill that shell and it disappears.
- [ ] Trees with stacks but no agent (7, 10 and 20 on this machine) show no agent.
- [ ] Typing an agent's name after `/` filters to its tree.
- [ ] With `~/.claude` absent or unreadable, treetop runs as before with no error.
- [ ] Left idle for a minute, treetop uses under 5% of one core.

## Approach

- The process scan extends `src/live.rs` with a `cmdline` read for processes already in trees; `assign` already gives each one its tree.
- `Tree::contains` (`src/pool.rs:45-47`) maps each agent's directory to a tree, so `/p/40` never matches `/p/4`.
- `src/refresh.rs:113-141` `fast` sends `Work` on even ticks; `Agents` goes out on the same tick.
- `src/app.rs:158-170` `visible` takes the agent name as a fourth field; `src/ui.rs:96` holds the bottom bar's counts.
- The transcript tail is a seek to 64 KB before the end and a scan for the last `"cwd":"`; no JSON parse of the whole file.

## Open decisions

None.

## Links

**Project:** Who's in the tree. **Blocked by:** Issue 1. **Blocks:** Issue 3.

---

# Issue 3: Jump to the agent, and orphans

## Why

Knowing which agent is in a tree is half the job; checking its work means finding its chat among tmux windows by hand.
Enter today goes to a window open in the tree, which is often an editor or a shell, not the agent.
And a leased tree that no agent holds is the most likely to be forgotten: it ties up a slot, and often a running stack, until someone notices.

## Outcome

Enter on a tree with an interactive agent lands in that agent's chat, and trees nobody is working in stand out so they can be reviewed and returned.

## Repro

N/A - not a bug.

## Scope

**In:**

- Enter, inside tmux, on a tree with an agent in a pane: `switch-client -t <session>` only when that session differs, then `select-window`, then `select-pane`. It never creates a pane.
- Finding that pane by process: the one whose `#{pane_pid}` is the agent, its parent or its grandparent.
- Falling back to today's Enter when there is no agent, or no pane runs it.
- `no agent`, muted, in the AGENT column of any leased tree with no agent.
- A key that filters the list to leased trees with no agent, so they can be marked and returned with `r`.

**Out:** attaching to background sessions with no pane (see the project's Open decisions); Enter outside tmux, which keeps opening a shell.

**Layers:**

- **UI:** `no agent` in the AGENT column; the orphan filter and its footer hint.
- **Data:** `stat` field 4 (ppid) read for agent pids; `tmux list-panes -a` for pane pids across sessions.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** none.
- **Tests:** choosing the pane from literal `list-panes -a` output and a ppid chain, including a pane whose pid is not in the agent's lineage and an agent that is its pane's own process.
- **Risk:** a stale pane id sending you to the wrong chat; finding the pane by process rather than by id rules it out.

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] With a Claude Code chat running in a split window, Enter on its tree switches to that window and focuses the chat pane, not the pane beside it.
- [ ] With the chat in another tmux session, Enter switches the client to that session first.
- [ ] `prefix` + `l` returns to treetop with cursor, filter and marks unchanged.
- [ ] On a tree whose agent runs in no pane, such as a background session, Enter falls back to the tree's window.
- [ ] On a tree with no agent, Enter behaves as it does today.
- [ ] Leased trees with no agent show `no agent`, and the orphan filter lists exactly those.

## Approach

- `src/tmux.rs:51-67` `open` is where the agent pane is tried first; `window_in` (`:27-32`) stays the fallback.
- `list-panes -a -F '#{session_id}\t#{window_id}\t#{pane_id}\t#{pane_pid}'` gives every pane in one call, parsed by a pure function as `window_in` already is.
- `src/app.rs:537-541` returns `Outcome::Enter(tree)`; the tree now carries its agents, so `main.rs` needs no change beyond passing them.

## Open decisions

None beyond the project's, on background sessions.

## Links

**Project:** Who's in the tree. **Blocked by:** Issue 2. **Blocks:** None.

---

# Issue 4: Agent chat in place

## Why

Every agent working in the pool runs as a background Claude Code session, with no tmux pane, so issue 3's Enter can only open a window in its tree.
Checking on an agent means leaving treetop, finding the session and attaching by hand, then finding treetop again for the next one.

## Outcome

`c` opens the agent's chat inside treetop, over the whole screen the way `Tab` opens the diff; Ctrl+] always comes back to the list exactly as left, and `c` on the next tree opens that chat.

## Repro

N/A - not a bug.

## Scope

**In:**

- An attach command on each agent: `claude attach <first 8 characters of the session id>` for a background Claude Code session, the short id `claude --bg` prints; an optional `attach` array in `agent.json`; none for the process scan.
- `c`: opens the first attach command in the tree; inside tmux, an agent with a pane and no attach command is switched to, as Enter does; otherwise the status line says why nothing opened.
- The chat runs in a pseudo-terminal treetop owns, parsed by `vt100` and drawn in treetop's frame under a bar naming the agent and the key back.
- treetop reads every key first and keeps Ctrl+] to leave; the rest, and pastes, go to the chat.

Handing the real terminal to `claude attach` was tried first and failed in a real setup (tmux with `mouse on`): Claude's own ways out are unreliable from inside a dialog, and the terminal modes it switches on were not all switched off again, so treetop came back unusable.
Owning the terminal removes both: Ctrl+] cannot be swallowed by the chat, and the chat's modes only ever reach the parser.

**Out:** forwarding the mouse (PgUp and PgDn scroll instead); attaching to interactive sessions, which Claude Code does not offer; keeping a chat alive in the background after leaving it.

**Layers:**

- **UI:** the `c` key, the chat view and its bar, status messages.
- **Data:** `Agent.attach`; `src/chat.rs` owns the pseudo-terminal, the parser and key encoding.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** `claude` on PATH for Claude Code sessions; the `vt100` crate.
- **Tests:** key encoding for each key family and application cursor mode; Ctrl+] recognised however crossterm reports it; a parsed screen drawn with its colours, attributes and cursor; `c` choosing a chat, a pane or a message; the attach command from background and interactive session fixtures; `attach` read from `agent.json`.
- **Risk:** the attach id is the session id's prefix, which Claude Code documents only as the id `--bg` prints; a format change shows as the chat exiting at once.

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] On a tree with a background Claude Code session, `c` shows that session's chat inside treetop, and typing reaches it.
- [ ] Ctrl+] returns to treetop at once, from the prompt and from inside a Claude menu, with cursor, filter and marks unchanged.
- [ ] Ctrl+Z, which ends the attach client, also returns.
- [ ] Afterwards treetop's keys work and the terminal has no mouse or paste mode left on.
- [ ] Resizing while the chat is open redraws it at the new size.
- [ ] The session keeps running after leaving, and no `claude attach` process is left behind.
- [ ] `c` on a tree with no agent says so and stays in treetop.

## Approach

- `src/chat.rs` opens the terminal with `openpty`, starts the command in its own session with the terminal as its controlling terminal, reads its output on a thread, and on leaving ends its process group and reaps it off the UI thread.
- `src/main.rs` routes every event to the open chat, Ctrl+] excepted, polls at 16 ms while one is open, and enables bracketed paste only then.
- `src/ui.rs` `draw_screen` copies the parsed cells into the frame.
- Measured in tmux with the user's config: Ctrl+] leaves from inside Claude's command menu; the pane's mouse and paste flags read 0 afterwards; idle CPU with no chat open is unchanged.

## Open decisions

None.

## Links

**Project:** Who's in the tree. **Blocked by:** Issue 2. **Blocks:** None.
