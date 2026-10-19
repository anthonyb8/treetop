# treetop

An htop-style view of a [treehouse](https://github.com/kunchenguid/treehouse) worktree pool.
See which coding agent works in each tree, whether it is waiting on you and what the tree serves, and jump to the agent in one key.
Ask a coordinator agent about every other agent at once, send an agent a task in a fresh tree, a tree you pick or the main checkout, and read what each agent reports back on its tree's row.
Switch between the pools of every repository you use it in without leaving it.
Mark trees, then return them to the pool or destroy them, with git's view of each tree on screen so nothing unpushed goes by accident.
Follow each tree's pull request through CI and review to the merge, send a failure back to its agent in one key, and see which issues a merge leaves ready.

treetop only talks to treehouse through its CLI (`treehouse status --json`, `get --lease`, `lease`, `return`, `destroy`), so a treehouse upgrade keeps working without a fork.

## Install

On Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/anthonyb8/treetop/main/install.sh | sh
```

It installs the latest release to `~/.local/bin` when that is on PATH, else `/usr/local/bin`, after checking the download against its published SHA-256.
Set `TREETOP_VERSION=v0.1.0` before `sh` to install a specific release.
From source: `cargo install --git https://github.com/anthonyb8/treetop`.

## Release

Bump `version` in `Cargo.toml`, then push a matching tag:

```sh
git tag v0.1.0 && git push origin v0.1.0
```

`.github/workflows/release.yml` refuses a tag that does not match `Cargo.toml`, runs the tests, builds static musl binaries for x86_64 and arm64, and publishes them with checksums as a GitHub release.

## Use

Run `treetop` from anywhere inside a pooled repository; run anywhere else, it opens the workspace you used last (see [Workspaces](#workspaces)).

| Key | Does |
|---|---|
| Enter | open the agent of the tree under the cursor: its chat in a popup, starting a cold one again, else the tree itself (see [Agent chats](#agent-chats) and [Entering a tree](#entering-a-tree)) |
| `n` | start a new agent in a fresh tree, on a branch named for the task (see [New agents](#new-agents)) |
| `m` | write a message to the agents in the marked trees, or the tree under the cursor |
| `c` | ask the coordinator; send nothing to open its chat (see [The coordinator](#the-coordinator)) |
| `d` | open the side-by-side diff of the tree under the cursor in a popup |
| `r` | return the marked trees, or the one under the cursor, to the pool; `D` in its dialog destroys them from disk instead, after reviewing treehouse's preview of each |
| Space | mark or unmark the tree under the cursor |
| `/` | filter by number, branch, holder or agent; Enter keeps it, Esc clears it |
| `o` | open the lowest port the tree serves, `http://localhost:<port>`, in the browser, or copy it to the clipboard where there is no desktop |
| `O` | open the tree's PR the same way (see [After the agent](#after-the-agent)) |
| `f` | send what failed on the tree's PR, a check's log or a review's requested changes, to its agent |
| `s` | start a ship run of the tree's branch, such as no-mistakes', after a dialog; on a run under way, show it |
| `b` | rename the branch of the tree under the cursor |
| `w` | switch to another workspace: the pool of another repository treetop has been opened in |
| Tab | switch to the Issues screen, and back (see [Issues](#issues)) |
| `j` `k` / arrows, `g` `G` | move |
| Esc | clear the filter, then the marks |
| `?` | show every key |
| `q` | quit |

In a true-colour terminal (`COLORTERM=truecolor`) treetop draws in gruvbox-material (medium), as Neovim does: lualine's sections for the bars and the table header, `CursorLine` for the selected row, `WinSeparator` for borders. Elsewhere it uses the terminal's 16 ANSI colours.

The columns are what a destroy would lose.
CHANGED counts uncommitted files and UNPUSHED counts commits on no remote; `?` means git could not read a held tree, which deserves a look before it goes.
PROCS counts processes running inside the tree, and PORTS the TCP ports they listen on: the lowest, and how many more.
`o` opens the lowest in the browser on a desktop; on a server reached over SSH it copies the URL to the clipboard of the computer you are typing at, through the terminal (OSC 52; inside tmux, `set-clipboard on`).
AGENT names the coding agent working in the tree, from the sources under [Agents](#agents); `no agent` on a held tree marks one nobody is working in.
SAYS is its agent's latest word (see [What agents say](#what-agents-say)).
Trees waiting on you sort to the top, set apart from the rest by a blank line; below it come the main checkout, then the other trees with the most recently active agent first, so trees nobody has worked in for a while sink to the bottom.
An agent's last activity is its latest report, or when its Claude Code transcript last grew.
PR is where the tree's branch stands after its agent: its pull request's number and stage, or the step of a pipeline it is going through (see [After the agent](#after-the-agent)).
The pane below shows the tree under the cursor in full: its path and holder, its agents, ports and processes, everything its agent last said with its latest steps, and the outcome of its last action with the end of its output.
The first row, `main`, is the repository's main checkout, so agents working in place show too; it is never returned or destroyed, and the summary's tree counts leave it out.

treetop refreshes on two clocks, the way htop stays fast.
Processes and their ports come straight from `/proc` every 2 seconds, and agents every 4.
Git counts are checked every 4 seconds too, but git runs only for a tree inotify saw change: each held tree's tracked directories and git dir are watched, and a commit, fetch or push in the shared refs rereads every tree's unpushed count.
Every tree is read in full every 2 minutes as a backstop, and a tree inotify cannot watch is read every time.
Together that costs under 5% of one core.
The pool itself comes from `treehouse status`, which takes seconds of CPU, so it is listed only when `git worktree list` changes (a tree leased, returned or destroyed, but not a commit), after every action, and every 2 minutes as a backstop.
The summary line says how old that listing is.

On opening, treetop draws the last listing it saved under `$XDG_CACHE_HOME/treetop` (else `~/.cache/treetop`) at once, in italics and marked "cached listing", until a live one lands.

## The diff

`d` opens the tree under the cursor in a side-by-side diff, in a popup over the pool, the way a pull request shows it: everything its branch changes against the pool's base (`base_branch` in `treehouse.toml`, else `origin/HEAD`), measured from their merge base, plus uncommitted and untracked files.
Removed lines sit on the left and the added lines that replace them on the right, row by row, with line numbers on both sides; within a changed line, the words that differ are highlighted.
Every file runs in one scroll, each under a header with its path and its additions and deletions, and the header of the file you are reading stays pinned to the top.
Its colours are the diff highlight groups Neovim uses under gruvbox-material (medium): `DiffDelete` and `DiffAdd` behind lines, `LineNr` for numbers, `diffLine` for hunks, `diffFile` for paths.

| Key | Does |
|---|---|
| `j` `k` | a line |
| Space, `b` | a page |
| `d` `u` | half a page |
| `n` `p` | the next or previous file |
| `h` `l` | sideways, for lines wider than their half |
| `g` `G` | the top or the end |
| `Ctrl-R` | read the diff again |
| Esc, `Tab`, `q` | back to the pool |

The diff is read in the background and holds still while it is open; reopening it after the tree's counts have moved reads it again.
Only the rows on screen are drawn, so a diff of fifty thousand lines scrolls as fast as a short one.

## Agents

treetop finds agents from what they already leave behind, so nothing has to wrap or configure them.
A pid two sources both see counts once, where the more specific source saw it; a source it cannot read adds nothing and shows no error.

- **`agent.json`**, an open convention any agent or wrapper can follow: a file in the tree's own git directory (`git rev-parse --git-dir`), where `git status` never sees it.
  It counts while its `pid` is alive, so an agent that crashes leaves nothing stale behind.
  `status`, one of `busy`, `idle` or `waiting`, is optional, and so is `attach`, the command that opens the agent's chat in a terminal, which Enter runs.

  ```json
  { "name": "fix-login", "pid": 4242, "status": "waiting", "attach": ["my-agent", "attach", "fix-login"] }
  ```

- **Agent plugins** (see [Plugins](#plugins)), each asked what agents it sees. Claude Code's is built in: Claude Code's `EnterWorktree` moves a session into a tree without changing its process's working directory, so the plugin reads its running sessions from `~/.claude/sessions` (or `$CLAUDE_CONFIG_DIR`), checks each pid is the one that started the session, and takes the tree from the latest `cwd` in the session's transcript.
  This gives the session's name and whether it is busy, idle or waiting on you.
  It also reports every background job in `~/.claude/jobs` with no running session as a **cold** agent, drawn `◌`: Enter attaches to it, which starts it again with its conversation kept.
  A cold agent shows in the tree whose `treetop.json` names its job, else in the tree it last worked in when that was since the tree's lease began, so one started by hand stays on its tree while a tree that has been returned and handed out again never shows the last agent.
  It stays there however long ago it finished; only the inbox forgets a finished job after a day.
  These files are internal to Claude Code, so a release that changes them blanks this source until treetop catches up.
- **A process** standing in the tree whose name, or the script its interpreter runs, is a known agent: `claude`, `codex`, `aider`, `opencode`, `gemini`, `goose`, `amp` or `cursor-agent`.
  `TREETOP_AGENTS=one,two` adds names. This sees any agent started inside a tree, by name only.

A waiting agent is drawn in the warning colour, a busy one in green and an idle one muted; `·` means the source cannot tell.

A held tree keeps its background agents awake while treetop runs, so an agent never comes apart from its tree.
Claude Code's daemon retires a background session that has sat idle for about an hour; treetop pins each one in a held tree, as Ctrl+T in `claude agents` does, so the daemon leaves it running, and starts any that has gone cold again in the background, with its conversation and no new message, at most once every ten minutes.
The main checkout keeps none, and a tree being returned stops keeping its agents: the return stops and unpins them.
An agent treetop starts in a pool tree is told that the tree is its own, so it works there rather than taking a second tree with `EnterWorktree` and leaving the first empty.

When treetop leases a tree to start an agent, it records the lease in `treetop.json` in the same git directory: the agent, when it leased the tree, and later its job, ticket and branch.
A return removes it, so a tree handed out again never carries the last agent's record.
`treetop status` includes it as each tree's `sidecar`.

## Config

`~/.config/treetop/config.toml` (or under `$XDG_CONFIG_HOME`) is optional; with no file, treetop runs as it ships.
It holds what belongs to one setup rather than to treetop, such as stopping a dev stack when a tree is returned:

```toml
[hooks]
on_dispatch = ["notify-send", "agent started"]
on_return = ["harness", "stack", "down"]
```

A hook is a command and its arguments, run in the tree with `TREETOP_TREE`, `TREETOP_BRANCH` and `TREETOP_TICKET` set, and given two minutes.
One that fails is written to the log and never stops the start or return it belongs to.
A config file treetop cannot parse is reported in the log, and no hooks run.

Issues come from named sources, each a ticket plugin and the settings it is given; Linear's plugin is built in and the default.
A repository picks its source by its main checkout's path under `[repos]`, and one without an entry has none, so an unrelated repository shows no one's issues.
Its Issues screen says what to add, and `w` shows a source anyway.
Each Linear source names the environment variable holding its personal API key (Linear's Settings > Security & access > Personal API keys), so a work and a personal workspace each get their own:

```toml
[issues.work]
key_env = "LINEAR_API_KEY_WORK"
team = "END"

[issues.personal]
key_env = "LINEAR_API_KEY_PERSONAL"
team = "ANT"

[repos."~/endo/app"]
issues = "work"
```

A source with `finish = true` has a ticket marked done once its tree's PR merges; it is off by default, since Linear's own GitHub integration often does that already:

```toml
[issues.personal]
key_env = "LINEAR_API_KEY_PERSONAL"
team = "ANT"
finish = true
```

A repository's PRs come from its ship plugins, named under `[repos]` in the order they are asked (see [After the agent](#after-the-agent)).
With no `ship`, a repository uses GitHub's alone, and `ship = []` leaves the PR column out:

```toml
[repos."~/endo/app"]
issues = "work"
ship = ["no-mistakes", "github"]
```

`treetop tickets` lists the repository's open tickets from its source, `--mine` those assigned to you, and any other words search their titles; it is the quickest check that a key and team are right.
The Linear plugin calls `https://api.linear.app/graphql` over HTTPS from treetop itself, so the key never appears in a process list.

## Plugins

A plugin is any executable file in `~/.config/treetop/plugins` (or `$XDG_CONFIG_HOME/treetop/plugins`) that speaks JSON on stdin and stdout; `treetop plugins` lists what it finds and why it cannot use one.
treetop runs it once per call, writes one request and closes stdin, then reads one reply:

```json
{"protocol": 1, "op": "describe", "config": {}, "args": {}}
{"ok": true, "result": {"protocol": 1, "name": "my-agent", "kind": "agent", "ops": ["start", "find"]}}
{"ok": false, "error": {"code": "not_found", "message": "no job 0908e7f3"}}
```

- `describe` says what the plugin is: its `protocol`, which must be 1, its `name`, its `kind` (`agent`, `tickets` or `ship`) and the `ops` it answers.
- An error's `code` is one of `not_found`, `unsupported`, `auth`, `conflict`, `unavailable` or `failed`; any other reads as `failed`. A non-zero exit or a reply that is not JSON fails with what the plugin wrote to stderr.
- Each call has a timeout: 3 s for `describe` and `find`, 10 s for `list`, `get`, `status` and `attach`, 30 s for anything else. The plugin runs in its own process group, which is killed at the timeout, so it must not leave a child holding its stdout once it exits.
- Fields are only ever added within a protocol, so a plugin should ignore the ones it does not know.

An agent plugin answers these ops:

| Op | Args | Result |
|---|---|---|
| `start` | `dir`, `name`, `prompt`, `isolated` (`dir` is a tree of the agent's own, to stay in) | `id`, once the agent runs |
| `find` | `dirs` | `agents`: each with `id`, `name`, `dir`, `live`, and optionally `pid`, `status` (`busy`, `idle` or `waiting`), `background` and `attach` |
| `attach` | `id` | `argv`, the command that opens its chat in a terminal |
| `send` | `id`, `text` | `delivered`, for an agent that is not running |

`describe` may also give `screen`, which tells treetop how to type a message into a running agent's chat: the input box is a line starting with `input` between two lines starting with `rule`, and a screen with no input box but one of `dialogs` on it is waiting on an answer.
One of `away` on the screen's last few lines means the chat has left its agent for a list of other sessions, which `←` in an empty input box opens; treetop keeps `←` from it there, and opens the agent's chat again when it shows.
Claude Code's is `{"input": "❯", "rule": "─", "dialogs": ["Do you want to", "❯ 1."], "away": ["describe a task for a new session", "enter to return"]}`; without one, treetop types on a timer.
| `stop` | `id` | nothing; the agent stops and is no longer kept awake |
| `keep` | `ids` | `woken`, the ids it started again; keeps the agents of held trees running, starting cold ones again |

A ticket plugin answers these, with its source's settings as `config`:

| Op | Args | Result |
|---|---|---|
| `list` | `filters`: `mine`, `label`, `state`, `project`, `cycle` (`current` or a number) and `query`, each optional | `tickets`: each with `id`, `title`, `state`, `labels`, and optionally `assignee`, `project`, `cycle`, `url` and `branch` |
| `get` | `id` | the ticket, with its `description`, and optionally `links`: each `{ "id", "kind", "done" }`, `kind` being `blocks`, `blocked by` or `related` |
| `branch` | `id` | `branch`, the name its system gives the ticket's branch |
| `start` | `id` | nothing; marks it started and assigns it to the key's user |
| `finish` | `id` | nothing; marks it done, where its source sets `finish = true` |

A ship plugin says where each tree's branch stands after its agent:

| Op | Args | Result |
|---|---|---|
| `status` | `dir`, the main checkout, and `trees`: each `{ "dir", "branch" }` | `branches`: one per branch it has something to say about, each with `branch`, `stage`, and optionally `label`, `url`, `head` and `detail` |
| `logs` | `dir`, the tree, and `branch` | `text`: what failed and the end of its output, for the tree's agent |
| `run` | `dir`, `branch` and `intent` | once the run is under way, which goes on after the call returns |
| `attach` | `dir` and `branch` | `argv`, the command that shows the run under way in a terminal |

`stage` is one of `running`, `gate`, `failed`, `draft`, `checks`, `ci_failed`, `changes`, `review`, `approved`, `merged`, `closed` or `idle`; any other reads as `running`.
`gate`, `failed`, `ci_failed` and `changes` need you.
`label` is the few characters the PR column shows, such as `#12 ✗ ci`, `detail` the lines the detail pane adds, and `head` the commit the PR or run is on.
`idle` means it has nothing current to say, such as a pipeline whose run ended, so the next plugin's answer shows.

Claude Code's plugin is built in, and `treetop plugin claude` serves it on stdin and stdout like any other; a plugin of your own named `claude` replaces it.
Linear's is built in the same way, as `treetop plugin linear`, and so are two ship plugins, `treetop plugin github` and `treetop plugin no-mistakes`.
treetop starts new agents with it unless `TREETOP_NEW_AGENT` names a command.

## What the actions run

Both run in the background, up to four trees at once, while treetop stays on screen; each row's STATUS shows queued, returning, destroying, returned, destroyed or failed.
The detail pane shows the end of each tree's last action's output, and a failure also shows its last line in the status line.

- **Return** first shows a dialog listing each tree's uncommitted files, unpushed commits and running processes, or "unknown changes" where git cannot read it. On `y`, for each tree at once:
  1. it stops the tree's background agents through their plugin, since a session that moved in with `EnterWorktree` runs from elsewhere and treehouse would not stop it, and so the tree stops keeping them awake;
  2. runs the `on_return` hook, if the config names one;
  3. runs `treehouse return --force`, which stops the processes left in the tree (the dialog replaces treehouse's own prompt), with `--if-lease-id` for a tree treetop leased, so a lease someone has taken since is never returned;
  4. deletes the branch treetop named for it when its work is in the pool's base branch, or when its PR merged with the branch where it is now, which covers a squash or rebase merge; a branch with commits after the merge is kept;
  5. removes `treetop.json`.

  The row turns available as soon as its return finishes.
- **Destroy** runs `treehouse destroy`'s dry run with every `--include-*` flag in the background and shows it in a dialog per tree. `y` runs it with `--yes`; `n` keeps the tree.

`q` while an action runs waits for it to finish; `Ctrl-C` quits at once and leaves the running command to finish on its own.

The tree treetop was started in is never a target, because returning or destroying it would kill the shell standing in it, and neither is the main checkout.

## Agent chats

Enter opens the chat of the agent in the tree under the cursor in a popup over treetop, the way `d` opens the diff.
The pool stays on screen, dimmed, round the popup's border, so a chat never passes for a Claude Code session of your own; the popup's title names the tree, branch and agent, and its bottom edge the keys out.
The chat's terminal is the size of the inside of the popup, and follows it when the terminal is resized.
**Ctrl+Q** always comes back to the list as you left it, whatever the chat is doing, and Enter on another tree opens that one.
**Ctrl+\\** goes straight to the next agent's chat, down the list as filtered, skipping trees with none and wrapping at the end; the cursor follows, so Ctrl+Q lands on the tree you were last in.
Neither key is a Claude Code default.
The agent's session keeps running after you leave.

A chat shows "opening" until its first screen has finished drawing, and is drawn a whole frame at a time, so it never flashes half-drawn.
A chat you leave keeps running in the background with its screen current, so Enter on it again is instant; treetop keeps the 8 most recently left, and ends them when their agent goes away or treetop quits.

treetop runs the chat in a terminal of its own and draws it, so nothing the chat does to its terminal reaches yours, and treetop keeps refreshing behind it.
Keys, pastes and the mouse go to the chat: the wheel scrolls back through it, three lines a step, and clicks work as they do in Claude itself.
treetop takes the mouse only while a chat is open; on the list, the terminal and tmux keep it.
From the keyboard, Ctrl+O opens Claude's transcript view, where j/k move a line and Ctrl+u/Ctrl+d half a page; PgUp and PgDn also work where a keyboard has them.

A background Claude Code session (started with `claude --bg`, or backgrounded) opens with `claude attach`, which never starts a second copy.
Ctrl+Z also leaves it.
The popup only ever shows the agent it was opened on: `←` on an empty prompt, which would open Claude's own list of sessions, does nothing, and if the chat reaches that list another way, such as double Ctrl+C, treetop opens the agent's chat again at once and says so in the status line.

An agent with an `attach` command in its `agent.json` opens the same way.
An agent running in a tmux pane, such as an interactive `claude`, belongs to that pane and cannot be opened elsewhere: Enter goes to the tree instead, which inside tmux switches to its pane.

## The coordinator and dispatch

treetop is where agents are sent out, and the coordinator is who you ask about them.

`n` opens the compose box in the middle of the screen for a new agent in a fresh tree from the pool, leased with `treehouse get --lease` when the job runs, so two quick dispatches never share one.
`c` opens it to the coordinator instead: one Claude Code session in the main checkout that oversees every other agent (see below); a message goes to it as `m` sends one, and nothing typed opens its chat.

Enter sends it, Alt+Enter breaks the line, Esc cancels, Ctrl+U clears the text and Ctrl+W a word; a paste keeps its line breaks.
The text wraps and the box grows with it.

### The coordinator

The coordinator is `claude --bg --name treetop-coordinator`, started in the main checkout the first time you send it something, with a system prompt that makes it the pool's overseer rather than a coder.
A message to it is sent the way `m` sends one: typed into its chat behind the screen, while you stay on the list, and the status line says when it is in.
Enter with nothing typed opens its chat in the same popup as any agent's, which is where its answers are read.
If none is running, one starts with your message as its first prompt; with nothing typed, it starts idle and its chat opens.
treetop finds it again by its name: while it runs, and for a day after it stops, through `claude attach`, which starts it again with its conversation kept.
After that a new one starts.

It sees and steers the other agents through three treetop commands, which it may run without asking:

| Command | Does |
|---|---|
| `treetop status` | prints JSON: each held tree's branch, uncommitted and unpushed counts and ports, its agents and their state, each background agent's latest report (the question it is waiting on, its answer once done, else what it is doing now), and its PR's stage as `ship`; idle trees are only counted, and `ready` lists the issues the merged trees' issues leave ready |
| `treetop send <tree\|agent> <message>` | types the message into the chat of every agent in that tree (a number or `main`), or of the agent of that name, and submits it |
| `treetop dispatch <new\|main\|N> <task>` | starts a new agent on the task in a fresh tree, the main checkout or tree N |
| `treetop pick <ID>` | picks a ticket up: an agent on it in a fresh tree, on its branch (see [Issues](#issues)) |

They work from any shell too.
Each prints only what it is asked for, so a question costs the coordinator one small read rather than a crawl through every transcript.
It is told to run `treetop status` before answering anything about the agents, to lead with what is waiting on you, and to say what it will send or dispatch and wait for a yes unless you have told it to go ahead.

### New agents

Every tree is leased with `treetop` as the holder, and a tree treetop leases always gets a branch: `<git user.name>/<the task as a slug>`, shown on the compose box's branch line.
Up or Down moves to that line to change the name before sending; a tree someone already holds keeps its branch.
`b` renames the branch of the tree under the cursor, or names one on a detached HEAD.

The agent is `claude --bg --name <the task's first words> -- <task>`, which Claude Code's own agent view lists too; `TREETOP_NEW_AGENT` names another command, which gets the task as its last argument.
An empty task starts the agent idle and opens its chat as soon as it shows up, so its first message is typed there.

A start runs as steps, each with its own timeout: lease the tree (`treehouse get --lease --branch`), check it is a clean worktree, name the branch, write `treetop.json`, start the agent, and wait for `claude agents` to list it, since `claude --bg` exits as soon as the session is queued.
When a step fails, the ones before it are undone in reverse: the agent stopped, the record removed, the branch deleted and the tree returned. The row shows "failed" and the status line which step failed and why.
Up to four actions run at once, each on its own tree, so a slow one never holds up the rest.

### Messages

`m` opens the compose box to write to the agents in the marked trees, or the tree under the cursor.
A running agent gets it typed into its chat, the same `claude attach` that Enter opens, behind the screen if the chat is not open.
treetop reads the chat's screen as it goes, by the patterns the agent's plugin gives: it waits for an empty input box, pastes the text, presses Enter once the paste shows, and counts it sent once the box empties again.
A dialog on screen, or a draft already in the box, stops it rather than typing into either, and the status line says which; so does a box that never shows, or a paste or Enter that does not land within a few seconds.
An agent mid-turn queues it, as Claude Code queues anything typed while it works.
A cold agent gets it through its plugin instead, which for Claude Code is `claude --bg --resume`: the session starts again with the message as its next turn, under the same job.
Agents with no chat to type into, such as an interactive session or a bare process, are left out, and the status line says how many.

### What agents say

Every background agent's latest report is read from Claude Code's job files under `~/.claude/jobs` and shown on its tree's row, in SAYS: the question while it needs you, the answer once it is done, else what it is doing now.
A report belongs to the tree whose `treetop.json` names its job, or where its agent runs now, so a recycled tree never shows the last agent's words.
An agent sitting at a question dialog or permission prompt needs you too, though its report still says working, since only its chat can answer it.
Trees waiting on you come first in the list, and the cursor stays on its tree as rows move.
Finished and stopped reports go after a day; working and waiting ones stay.
The coordinator's own reports are left off its row, since it is talked to through `c`.

When an agent comes to need you, or finishes, treetop rings the terminal bell, and inside tmux shows it in the status line for five seconds, so it is heard from any window; the coordinator finishing an answer rings it too.
The summary counts the trees that need you, and, as "elsewhere", agents outside the pool waiting on you, which have no row.
These job files are internal to Claude Code, like its session files, so a release that changes them blanks SAYS until treetop catches up.

## After the agent

Once an agent is done, its tree's work still has a PR to get through: checks, review, the merge.
The PR column follows it, from the repository's ship plugins, so treetop knows no forge, CI or gate of its own.

- **GitHub**, built in and the default, reads every PR of the repository with one `gh pr list` and matches them to the trees' branches, the newest PR winning where a branch has several.
  It uses your own `gh` login, so treetop holds no token, and it only reads: it never pushes, opens, merges or reviews a PR.
  The column shows `#12 draft`, `#12 ci …` while checks run, `#12 ✗ ci`, `#12 changes`, `#12 review`, `#12 approved`, `#12 merged` or `#12 closed`; the detail pane adds the link, the checks and the review.
  With no `gh`, no login or no GitHub remote, the column stays empty and the detail pane says why.
- **no-mistakes**, built in and used only where `ship` names it, reads the branch's pipeline run with `no-mistakes axi status` in its tree, whoever started it: `s`, the agent, or a `git push` through the gate.
  While the run is under way the column shows its step, `nm review …`, or `nm review ⏸` when it waits at a gate; a run that failed on the commit the tree is still on shows `nm ✗ review`.
  Once the run ends, the next plugin's answer shows, so `ship = ["no-mistakes", "github"]` follows a tree from its gate to its merge.

treetop asks every minute, at once when a push lands in the repository's refs, and after `f` or `s`, but never twice within 15 seconds; a plugin it cannot reach is asked every five minutes instead.
A failed check, a review asking for changes, a gate and a failed run need you: the tree sorts to the top, the summary counts it, and the bell rings once as it comes to need you.

`f` sends what failed to the tree's agents, as `m` sends a message: a line saying what it is, such as `CI failed on #12. Fix it and push.`, then the failing checks with the end of each failed Actions run's log, or the review's requested changes and its comments on lines, or the pipeline step's log.
A cold agent gets it through its plugin, which starts it again.

`s` starts a ship run of the tree's branch through the first of the repository's ship plugins that can, after a dialog naming the branch, the plugin and the intent, since the run pushes the branch and opens its PR.
The intent is the tree's issue, its title and text, else the task its agent was dispatched with, which `treetop.json` records as `task`.
On a tree whose run is under way, `s` opens the run in the popup instead, `no-mistakes attach` for no-mistakes, which is where a gate is answered; Ctrl+Q comes back.

A merged tree says so in the return dialog, and `/merged` filters the list to merged trees to mark and return together.
When a tree that records its issue merges, treetop follows it up in the background: it marks the issue done where its source sets `finish = true`, then reads the issues it blocks, and names each one with nothing else blocking it and nobody working on it in the status line, `END-1 merged · END-2 ready`.
Those show `ready` in the Issues screen's WORK column, where `p` picks one up.

## Issues

treetop has two screens, Trees and Issues; Tab switches between them, and `treetop issues` opens straight onto Issues.
The Issues screen shows the repository's issues from its issue source in the config (see [Config](#config)): the list on top, your own open issues first, the most recently updated at the top, and below it the issue under the cursor with its fields, its description rendered from Markdown, and its comments.
Everything loads in the background, and an issue's description and comments are kept once read, so moving the cursor never waits on the network.
Each issue picked up in a tree shows it in the WORK column, with its agent's state, `4 · waiting` in the warning colour when the agent waits on you.
Enter or `→` opens the issue under the cursor on the whole screen, to scroll with j/k, Space and PgUp, g and G, or the mouse wheel; Esc or `←` goes back to the list.
The keys that change an issue work the same from the list and from inside it.
An open issue lists the issues it is linked to: its parent, its sub-issues, the ones it blocks or is blocked by, related ones and duplicates, and the issues of its team its text mentions.
Tab moves between those links and the issue's images, and Enter or a click opens the one marked: an issue opens in its place, and Esc goes back along the issues opened, then to the list.
Images show in the text, read with the source's key, in the terminal's own graphics where it has them (Kitty, Ghostty, WezTerm, iTerm2, through tmux with `allow-passthrough on`) and in coloured blocks anywhere else.
An image opened fills the screen; `←` and `→` go to the issue's other images, and `o` opens it in the browser.
While an issue is open treetop takes the mouse, so selecting text there takes Shift as you drag.
`w` switches to the next issue source, so a work and a personal workspace are one key apart, and `o` opens the issue in the browser.

Number keys switch between views: `1` your open issues and `2` everyone's, both grouped by status as Linear groups them, `3` grouped by project, `4` grouped by status in the team's workflow order, and `5` the current cycle.
Views of your own go on `6` to `9`, in name order, from `[views]` in the config, each a line of filters: `mine`, and `assignee:`, `label:`, `state:` (a status, or `any`), `project:` and `cycle:` (`current` or a number), a value with spaces in double quotes, other words searched for, and `group:project` or `group:status`:

```toml
[views]
bugs = "label:Bug group:status"
review = "mine state:\"In Review\""
```

The line above the list says what it shows: whose issues, which status, and whatever else narrows them, such as `my issues · open · label Bug · “login”`.
`/` searches: the list narrows as you type, by id, title, label and assignee, and once typing rests Linear is searched too, by title or issue number, so older issues show; Enter keeps the search and Esc clears it.
`f` opens the filters: assignee (anyone, you, nobody, or a teammate), status (open, all, or one), label, project, cycle, and grouping, each picked from the team's own with Enter, the list following at once; `x` clears one and `c` all of them.
In a grouped list, `z` folds the group under the cursor to its heading and `z` or Enter on the heading opens it again, as Linear folds a status; `Z` folds every group, or opens them all.
`m` switches between yours and everyone's, `r` reads the list and the team again, and `?` shows these keys.

`s` changes the issue's status, `l` its labels and `a` its assignee, each from a picker of the team's own states, labels and members that narrows as you type; labels are marked and unmarked with Tab and applied together with Enter, and the assignee picker offers you first, then nobody, then everyone else.
The change shows on the row at once and is sent behind it; if Linear refuses it, the row goes back to what it was and the bar says why.
`c` writes a comment on the issue: Markdown, Alt+Enter for a new line, Enter to post it under the issue's other comments; one Linear refuses opens again with its text, so nothing written is lost.

`n` opens a form for a new issue, and `e` the same form on the issue under the cursor.
It fills the screen: the title, the status, priority, labels, assignee and project, and the description in a box below.
Tab and Shift+Tab move between the fields; on a field set from a list, Enter or typing opens its picker.
The description takes Markdown as it is typed, Enter for a new line, and Ctrl+E writes it in your editor (`$VISUAL`, else `$EDITOR`, else `vi`) instead.
Ctrl+S saves: a new issue needs a title and starts in the team's first open state, assigned to you; an edit sends only what changed.
Esc closes the form, and once something is written in it, asks for a second Esc first.
A new issue appears at the top of the list once Linear has it, and one Linear refuses, new or edited, opens again in the form as it was written.

A Linear source gets all of this; an issue source on another ticket plugin lists and reads through the plugin contract, and its edits say they cannot be made.

`p` picks the issue up, from the list or from inside it, in one step:

1. a fresh tree is leased on the branch the ticket system names for the ticket (Linear's `branchName`), or on that branch as it already is, here or on `origin`, when the ticket was picked up before;
2. an agent starts there, its session named for the ticket (`END-123 fix the login redirect`), its first message the ticket's id, title, text and link;
3. once it runs, the ticket is marked started and assigned to you.

The tree's `treetop.json` records the ticket, so the TICKET column shows it, the filter finds it, and `p` on a ticket already in a tree goes to that tree rather than starting a second agent.
On the trees, `i` opens the issue the tree under the cursor works on, on the whole screen, and Esc comes back to the trees.
A tree started without an issue gets one the same way: `i` on it, or `I` on any leased tree to change it, shows the Issues list to choose from, with search and filters, and Enter records the issue in the tree's `treetop.json`, so the TICKET and WORK columns show it at once.
A step that fails undoes the ones before it, as any start does; marking the ticket started is the exception, and a failure there only shows in the detail pane, since the agent is already working.
`treetop pick END-123` does the same from a shell.

The first message comes from `prompt` in the config when it is set, with `{id}`, `{title}`, `{description}`, `{url}` and `{branch}` filled in:

```toml
prompt = "Work on {id}: {title}\n\n{description}\n\n{url}"
```

## Workspaces

A workspace is a repository with a treehouse pool, shown by its main checkout.
`w` opens a list of every repository treetop has been opened in, most recent first, with how many trees each holds and the one on screen marked; the cursor starts on the last other one, so `w` and Enter go back and forth between two.
Enter switches the whole screen to that pool, and the badge at the end of the bottom bar names the workspace on screen.

treetop adds a repository to the list the first time it is opened there, so run it once in each pooled repository to make it a workspace.
The list lives in `$XDG_CACHE_HOME/treetop/workspaces.json` (else `~/.cache/treetop`); a repository that has been deleted drops off it.
treetop never scans treehouse's own directories for pools, keeping to treehouse's CLI.

A switch waits while an action is running in the pool on screen.
The chats left open in the old pool are ended, which leaves their agents running.
Each workspace has its own coordinator; a stopped one is only ever taken up again in the pool it works in.

## Entering a tree

Enter on a tree whose agent has no chat to open here, or no agent at all, goes to the tree.

Inside tmux, a tree with an agent switches to the pane that agent runs in, in whichever session it is, and never opens a new one.
The pane is the one whose process is the agent, its parent or its grandparent, so a pane an agent once used and something else uses now is never mistaken for it.
`prefix` `l` comes back to treetop.

Otherwise it opens the tree in a new window named for its branch (the part after the last `/`), or `tree N` when it has none, and treetop stays open in its own window.
When a window of the session already has a pane in that tree, it switches to it instead of opening another.

Outside tmux, treetop suspends and opens `$SHELL` in the tree; `exit` comes back.
The list does not refresh while that shell is open.
