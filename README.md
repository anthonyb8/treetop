# treetop

An htop-style view of a [treehouse](https://github.com/kunchenguid/treehouse) worktree pool.
See which coding agent works in each tree, whether it is waiting on you and what the tree serves, and jump to the agent in one key.
Mark trees, then return them to the pool or destroy them, with git's view of each tree on screen so nothing unpushed goes by accident.

treetop only talks to treehouse through its CLI (`treehouse status --json`, `return`, `destroy`), so a treehouse upgrade keeps working without a fork.

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

Run `treetop` from anywhere inside a pooled repository.

| Key | Does |
|---|---|
| `j` `k` / arrows, `g` `G` | move |
| Space | mark or unmark the tree under the cursor |
| `u` | clear the marks |
| `/` | filter by number, branch, holder or agent; Enter keeps it, Esc clears it |
| `a` | show only held trees with no agent, or everything again |
| `o` | open the lowest port the tree serves, `http://localhost:<port>`, in the browser, or copy it to the clipboard where there is no desktop |
| `r` | return the marked trees, or the one under the cursor, to the pool |
| `D` | destroy them from disk, after reviewing treehouse's preview of each |
| `d` | open the side-by-side diff of the tree under the cursor |
| `L` | show or hide the log of every action's output; PgUp and PgDn scroll it |
| `t` | go to the tree: its agent's pane or a window inside tmux, else a shell |
| `c` | open the agent's chat inside treetop; Ctrl+] comes back (see [Agent chats](#agent-chats)) |
| `n` | start a new agent in the tree, leasing it first if it is available, and open its chat |
| `Ctrl-R` | list the pool again now |
| `q`, Esc | quit |

In a true-colour terminal (`COLORTERM=truecolor`) treetop draws in gruvbox-material (medium), as Neovim does: lualine's sections for the bars and the table header, `CursorLine` for the selected row, `WinSeparator` for borders. Elsewhere it uses the terminal's 16 ANSI colours.

The columns are what a destroy would lose.
CHANGED counts uncommitted files and UNPUSHED counts commits on no remote; `?` means git could not read a held tree, which deserves a look before it goes.
PROCS counts processes running inside the tree, and PORTS the TCP ports they listen on: the lowest, and how many more.
`o` opens the lowest in the browser on a desktop; on a server reached over SSH it copies the URL to the clipboard of the computer you are typing at, through the terminal (OSC 52; inside tmux, `set-clipboard on`).
AGENT names the coding agent working in the tree, from the sources under [Agents](#agents); `no agent` on a held tree marks one nobody is working in, which `a` lists on its own.

treetop refreshes on two clocks, the way htop stays fast.
Processes and their ports come straight from `/proc` every 2 seconds, and agents every 4.
Git counts are checked every 4 seconds too, but git runs only for a tree inotify saw change: each held tree's tracked directories and git dir are watched, and a commit, fetch or push in the shared refs rereads every tree's unpushed count.
Every tree is read in full every 2 minutes as a backstop, and a tree inotify cannot watch is read every time.
Together that costs under 5% of one core.
The pool itself comes from `treehouse status`, which takes seconds of CPU, so it is listed only when `git worktree list` changes (a tree leased, returned or destroyed, but not a commit), after every action, on `Ctrl-R`, and every 2 minutes as a backstop.
The summary line says how old that listing is.

On opening, treetop draws the last listing it saved under `$XDG_CACHE_HOME/treetop` (else `~/.cache/treetop`) at once, in italics and marked "cached listing", until a live one lands.

## The diff

`d` opens the tree under the cursor in a full-screen, side-by-side diff, the way a pull request shows it: everything its branch changes against the pool's base (`base_branch` in `treehouse.toml`, else `origin/HEAD`), measured from their merge base, plus uncommitted and untracked files.
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
  `status`, one of `busy`, `idle` or `waiting`, is optional, and so is `attach`, the command that opens the agent's chat in a terminal, which `c` runs.

  ```json
  { "name": "fix-login", "pid": 4242, "status": "waiting", "attach": ["my-agent", "attach", "fix-login"] }
  ```

- **Claude Code**, whose `EnterWorktree` moves a session into a tree without changing its process's working directory.
  treetop reads its running sessions from `~/.claude/sessions` (or `$CLAUDE_CONFIG_DIR`), checks each pid is the one that started the session, and takes the tree from the latest `cwd` in the session's transcript.
  This gives the session's name and whether it is busy, idle or waiting on you.
  These files are internal to Claude Code, so a release that changes them blanks this source until treetop catches up.
- **A process** standing in the tree whose name, or the script its interpreter runs, is a known agent: `claude`, `codex`, `aider`, `opencode`, `gemini`, `goose`, `amp` or `cursor-agent`.
  `TREETOP_AGENTS=one,two` adds names. This sees any agent started inside a tree, by name only.

A waiting agent is drawn in the warning colour, a busy one in green and an idle one muted; `·` means the source cannot tell.

## What the actions run

Both run in the background, one tree at a time, while treetop stays on screen; each row's STATUS shows queued, returning, destroying, returned, destroyed or failed.
Their output goes to the log (`L`), and a failure also shows its last line in the status line.

- **Return** first shows a dialog listing each tree's uncommitted files, unpushed commits and running processes, or "unknown changes" where git cannot read it. On `y` it runs `treehouse return --force`, which stops the processes left in the tree: that dialog replaces treehouse's own prompt before discarding changes. Anything started outside the tree, such as a container, is for whatever started it to stop.
- **Destroy** runs `treehouse destroy`'s dry run with every `--include-*` flag in the background and shows it in a dialog per tree. `y` runs it with `--yes`; `n` keeps the tree.

`q` while an action runs waits for it to finish; `Ctrl-C` quits at once and leaves the running command to finish on its own.

The tree treetop was started in is never a target, because returning or destroying it would kill the shell standing in it.

## Agent chats

`c` opens the chat of the agent in the tree under the cursor inside treetop, over the whole screen, the way `d` opens the diff.
**Ctrl+]** always comes back to the list as you left it, whatever the chat is doing, and `c` on another tree opens that one.
**Ctrl+\\** goes straight to the next agent's chat, down the list as filtered, skipping trees with none and wrapping at the end; the cursor follows, so Ctrl+] lands on the tree you were last in.
Neither key is a Claude Code default.
The agent's session keeps running after you leave.

A chat shows "opening" until its first screen has finished drawing, and is drawn a whole frame at a time, so it never flashes half-drawn.
A chat you leave keeps running in the background with its screen current, so `c` on it again is instant; treetop keeps the 8 most recently left, and ends them when their agent goes away or treetop quits.

treetop runs the chat in a terminal of its own and draws it, so nothing the chat does to its terminal reaches yours, and treetop keeps refreshing behind it.
Keys and pastes go to the chat; the mouse wheel does not. To scroll, Ctrl+O opens Claude's transcript view, where j/k move a line and Ctrl+u/Ctrl+d half a page; PgUp and PgDn also work where a keyboard has them.

A background Claude Code session (started with `claude --bg`, or backgrounded) opens with `claude attach`, which never starts a second copy.
Ctrl+Z also leaves it; double Ctrl+C or `←` on an empty prompt opens Claude's own list of sessions inside the chat instead.

An agent with an `attach` command in its `agent.json` opens the same way.
An agent running in a tmux pane, such as an interactive `claude`, belongs to that pane and cannot be opened elsewhere: inside tmux, `c` switches to its pane, as `t` does.

`n` starts a new agent in the tree under the cursor and opens its chat as soon as it shows up, so its first message is typed there.
A tree nobody holds is leased first, with `treetop` as the holder, so the pool cannot hand it to anyone else; the agent starts on its detached HEAD and makes its own branch.
The agent is `claude --bg`, which Claude Code's own agent view lists too, so the same chat opens from either; `TREETOP_NEW_AGENT` names another command.
Paired with `a`, it puts an agent to work in a tree nobody is working in.

## Entering a tree

Enter does nothing, so a stray press never takes you anywhere; `t` goes to the tree.

Inside tmux, `t` on a tree with an agent switches to the pane that agent runs in, in whichever session it is, and never opens a new one.
The pane is the one whose process is the agent, its parent or its grandparent, so a pane an agent once used and something else uses now is never mistaken for it.
`prefix` `l` comes back to treetop.

Otherwise `t` opens the tree in a new window named for its branch (the part after the last `/`), or `tree N` when it has none, and treetop stays open in its own window.
When a window of the session already has a pane in that tree, `t` switches to it instead of opening another.

Outside tmux, `t` suspends treetop and opens `$SHELL` in the tree; `exit` comes back.
The list does not refresh while that shell is open.
