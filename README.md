# treetop

An htop-style view of a [treehouse](https://github.com/kunchenguid/treehouse) worktree pool.
See which coding agent works in each tree, whether it is waiting on you and what the tree serves, and jump to the agent in one key.
Ask an assistant where every agent stands and what to do next, send an agent a task in a fresh tree or a tree you pick, and read what each agent reports back on its tree's row.
Switch between the pools of every repository you use it in without leaving it.
Mark trees, then return them to the pool or destroy them, with git's view of each tree on screen so nothing unpushed goes by accident.
Follow each tree's pull request through CI and review to the merge, send a failure back to its agent in one key, and see which issues a merge leaves ready.
Check on it all from your phone too, with `treetop serve` (see [On your phone](#on-your-phone)).

treetop only talks to treehouse through its CLI (`treehouse status --json`, `get --lease`, `lease`, `return`, `destroy`), so a treehouse upgrade keeps working without a fork.

## Install

On Linux (x86_64 or arm64):

```sh
curl -fsSL https://raw.githubusercontent.com/anthonyb8/treetop/main/install.sh | sh
```

It installs the latest release to `~/.local/bin` when that is on PATH, else `/usr/local/bin`, after checking the download against its published SHA-256.
Set `TREETOP_VERSION=v0.1.0` before `sh` to install a specific release.
From source: `cargo install --git https://github.com/anthonyb8/treetop`.

Once installed, `treetop update` installs the latest release over itself in one step, checked against its SHA-256 as the script checks it, and `treetop update v0.1.0` a given one.
It restarts `treetop serve`'s service where `treetop serve --install` set one up, so the phone gets the new version too.
An open treetop checks for a newer release every hour and says `treetop v0.1.60 out: U updates` in its summary; `U` installs it as `treetop update` does and starts it in its place, on the same workspace, once no action is under way.
One whose binary was replaced from outside, by `treetop update` or install.sh, says `treetop updated: U restarts` instead.

## Release

Bump `version` in `Cargo.toml` in a PR and merge it: the push to main releases that version.

`.github/workflows/release.yml` runs on every push to main and releases only when `Cargo.toml`'s version has no tag yet, so a merge that does not bump it publishes nothing.
It builds and tests the web app in `web/`, runs the tests, builds static musl binaries for x86_64 and arm64 with the web app embedded, and publishes them with checksums as a GitHub release, which creates the tag at the merge.
Pushing a `v*` tag by hand still releases that tag, refused when it does not match `Cargo.toml`.

## Use

Run `treetop` from anywhere inside a pooled repository; run anywhere else, it opens the workspace you used last (see [Workspaces](#workspaces)).

| Key | Does |
|---|---|
| Enter | give the keys to the chat of the agent in the tree under the cursor, in the window under the table, in Normal, where Enter again starts Insert, starting a cold one again, else go to the tree itself (see [Agent chats](#agent-chats) and [Entering a tree](#entering-a-tree)); on a group's heading, fold the group or open it again, and on the main checkout's, open the chat of its agent when it has one |
| Ctrl+Q | (or F13 to F24, such as a Caps Lock remapped to F19) give the keys to the window under the table, in Normal; from Insert, drop to Normal; from Normal, give them back to the table; with two windows, move them round the table, the left window and the right one (see [Normal and Insert](#normal-and-insert)) |
| Ctrl+P | open the command palette: every action, the tree under the cursor's first, found by typing a few of its words, a tree number or a branch, and run with Enter |
| Ctrl+F | zoom: on the table, the table over the whole screen; in a window, the windows, with the table folded to their trees' rows; again, back |
| Ctrl+\\ | show two windows under the table, the tree under the cursor on the left and the next tree down on the right, or one again |
| `←` `→` | step through the window's tabs: chat, diff, PR, run, shell and info, from the table or from the window in Normal |
| `t` | go to the tree itself, even when its agent has a chat: its tmux pane or a window in it, else a shell there (see [Entering a tree](#entering-a-tree)) |
| `n` | start a new agent in the tree under the cursor: a free one is leased for it, on a branch named for the task, and a held one keeps its branch; on the main checkout's heading, start one in the main checkout, for work that needs no tree |
| `N` | start a new agent in a fresh tree, on a branch named for the task (see [New agents](#new-agents)) |
| `m` | write a message to the agents in the marked trees, or the tree under the cursor |
| `e` | go to what waits on you, the one the banner names first, and again to the next, from the table or a chat in Normal (see [What waits on you](#what-waits-on-you)) |
| `E` | show "While you were away" again, what waits on you to go through one at a time |
| `:` | ask the assistant about the tree under the cursor, or write `@17 ...` for that tree's agent (see [The assistant](#the-assistant)) |
| `a` | open the assistant's chat, where it answers, starting it if there is none |
| `d` | show the side-by-side diff of the tree under the cursor in the window's diff tab, with the keys |
| `r` | return the marked trees, or the one under the cursor, to the pool; its dialog names each tree's branch, its uncommitted and unpushed counts, the pid of every live process a return would stop and any [keel run](#keel-runs) that holds it, and `D` in it destroys them from disk instead, after reviewing treehouse's preview of each |
| Space | mark or unmark the tree under the cursor |
| `x` | put the tree under the cursor on the Backburner, or bring it back, with the cursor following it; `X` folds or opens Backburner |
| `z` | park the tree under the cursor, setting it aside under Parked, or bring it back; `Z` opens or folds Parked |
| `/` | filter by number, branch, holder or agent; Enter keeps it, Esc clears it |
| `o` | open the port the tree serves, `http://localhost:<port>`, in the browser, or the `browser` the config names (see [Config](#config)), or copy it to the clipboard where there is no desktop; a tree serving several, such as a frontend and a backend, offers a menu of them |
| `p` | share the port the tree serves with your phone and show its address as a QR code to scan with the phone's camera; it runs `tether share <port>`, or the `share` the config names (see [Config](#config)); pressed again on a port already shared, it takes the share down; a tree serving several offers the same menu as `o` |
| `P` | put the port the tree serves on the internet, for anyone with the link, and copy that link to the clipboard; it runs `tether share --public <port>`, or the `publish` the config names; pressed again on a port already public, it takes it off the internet, as `x` in its popup does, with `tether unshare <port>`, or the `unpublish` the config names; a tree serving several offers the same menu as `o` |
| `Q` | show a shared port's QR code and address again, without sharing anything: treetop reads them back from `tether shares`, so a port shared before it started shows too; on a port that is not shared it says which key shares it |
| `v` | show the tree's PR in the window's PR tab, with the keys: its checks, review threads and CodeRabbit's comments (see [The PR view](#the-pr-view)) |
| `O` | open the tree's PR the same way (see [After the agent](#after-the-agent)) |
| `f` | send what failed on the tree's PR, a check's log or a review's requested changes, to its agent |
| `R` | resume the tree's stopped or failed agent in the tree, with its conversation and the flags it was started with, rather than a fresh dispatch that loses them |
| `h` | hand the work of the tree's agent to a new agent in the same tree, on a handoff the old one writes and you read over first (see [Handoffs](#handoffs)) |
| `s` | start a ship run of the tree's branch, such as no-mistakes', after a dialog; on a run under way, show it |
| `b` | rename the branch of the tree under the cursor |
| `w` | switch to another workspace: the pool of another repository treetop has been opened in |
| `C` | scratch chats, tied to no tree: start one, open one, or delete one (see [Scratch chats](#scratch-chats)) |
| Tab | switch to the Issues screen, and back (see [Issues](#issues)) |
| `j` `k` / arrows, `g` `G` | move |
| Esc | clear the filter, then the marks |
| `U` | install a newer treetop release and restart on it, or restart on one installed from outside (see [Install](#install)) |
| `?` | show every key |
| `q` | quit |

In a true-colour terminal (`COLORTERM=truecolor`) treetop draws in gruvbox-material (medium), as Neovim does: lualine's sections for the bars and the table header, `CursorLine` for the selected row, `WinSeparator` for borders. Elsewhere it uses the terminal's 16 ANSI colours.

The columns are what a destroy would lose.
GIT says what is not safe yet, in a Powerlevel10k prompt's symbols: `⇡2` commits on no remote, `+1` staged, `!3` unstaged and `?4` untracked files, each only when there are some, so a clean tree is blank.
`─` means git could not read a held tree, which deserves a look before it goes.
PORTS lists every TCP port the tree's processes listen on, lowest first, so a frontend and a backend both show; the processes themselves are in the info tab and the return dialog.
A port shared with your phone carries a `p` after it, and one on the internet a `P`: the key that put it there, so what anyone with the link can open is on the row rather than only in the popup that shared it.
treetop reads which ports are shared back from `tether shares`, or the `shares` the config names, every 10 seconds and at once after `p`, `P` or `x`, so a share made outside treetop, or before it started, is marked too.
`o` opens the port in the browser on a desktop, and where the tree serves several it opens a menu of them, each with the process listening on it, picked with its number or `j`/`k` and Enter; on a server reached over SSH it copies the URL to the clipboard of the computer you are typing at, through the terminal (OSC 52; inside tmux, `set-clipboard on`).
AGENT names the coding agent working in the tree, from the sources under [Agents](#agents); `no agent` on a held tree marks one nobody is working in.
Where a tree has several, it names the most pressing live one, waiting on you, then busy, then idle, the most recently active first, and counts the other live ones as `+N`; a stopped agent shows only where nothing is live, and Enter opens the same agent the row names.
The assistant is never a tree's agent.
A Claude Code session under a config dir other than the default one is labelled with it, `● fix-login [keel]` for one under `~/.keel/claude` (see [Config](#config)).
Its mark is `●` in bold when the tree's agent needs you, `✓` when it is done and `✗` when its process died, as on a restart (see [What agents say](#what-agents-say)), else the agent's own status: `●` busy, `○` idle, `◌` cold.
The list is in groups, each under a heading with a blank line above it: a group per base the held trees were cut from (see [New agents](#new-agents)), the pool's base first and the rest by name, then Backburner, then the free trees, then Parked.
The repository's main checkout is no row of its own: it heads its branch's group, `main · main checkout`, with its ports, agent and git state.
treetop keeps it on its upstream: it fetches every five minutes, and whenever the shared refs move, as an agent's fetch or push moves them, it fast-forwards the checkout and says so on the status line.
It only ever fast-forwards, and only a checkout on a branch with an upstream, with no tracked changes, no commits of its own and no agent busy in it; one it leaves shows how far behind it is as `⇣25` in its git state.
`treetop serve` does the same, so the checkout keeps up while no terminal has treetop open.
Work happens in trees, but `n` on that heading starts an agent in the main checkout itself, on whatever it has checked out and with no branch named for it, for work that needs no tree; Enter on the heading then opens its chat rather than folding the group.
The assistant is never shown there, and neither is a stopped session, since sessions run there in place are not its agent.
Within a group, trees waiting on you come first, then those with the most recently active agent, so trees nobody has worked in for a while sink to the bottom.
An agent's last activity is its latest report, or when its Claude Code transcript last grew.
The cursor stops on headings too, and Enter on one folds its group to the heading, which then says how many of its trees wait on you, or opens it again; a filter opens every group and leaves out those it finds nothing in.
Trees still being worked on, but not right now, can go on the Backburner with `x`: they go under the Backburner heading below the trees in hand, open, and the cursor goes with them.
Trees held for later but not worked on at all can be parked with `z`: they go under the Parked heading at the bottom, folded, and `Z` opens it.
A tree sits on one of the two at a time, so `z` on a tree on the Backburner parks it, and `x` on a parked one puts it on the Backburner.
A tree on either stays there even when it needs you, sorting to the top of it, a filter still finds it, and returning a tree clears it, since both are kept in its `treetop.json`, as `backburner` and `parked`.
RUN shows only once [keel](#keel-runs) lists a run: the playbook the tree's run follows and the step it is on out of the playbook's, `bug-fix 2/6`, then `paused` or another state when it is not active, `!2` when two of its decisions are flagged for attention, and `stale` when no process was in the tree at keel's last sweep.
PR is where the tree's branch stands after its agent: its pull request's number and stage, or the step of a pipeline it is going through (see [After the agent](#after-the-agent)).
The pane below shows the tree under the cursor in full: its path and holder, its agents, ports and processes, everything its agent last said with its latest steps, and the outcome of its last action with the end of its output.

treetop refreshes on two clocks, the way htop stays fast.
Processes and their ports come straight from `/proc` every 2 seconds, and agents every 4.
Git counts are checked every 4 seconds too, but git runs only for a tree inotify saw change: each held tree's tracked directories and git dir are watched, and a commit, fetch or push in the shared refs rereads every tree's unpushed count.
Every tree is read in full every 2 minutes as a backstop, and a tree inotify cannot watch is read every time.
Together that costs under 5% of one core.
The pool itself comes from `treehouse status`, which takes seconds of CPU, so it is listed only when `git worktree list` changes (a tree leased, returned or destroyed, but not a commit), after every action, and every 2 minutes as a backstop.
The summary line says how old that listing is.

On opening, treetop draws the last listing it saved under `$XDG_CACHE_HOME/treetop` (else `~/.cache/treetop`) at once, in italics and marked "cached listing", until a live one lands.

## The diff

`d` shows the tree under the cursor in a side-by-side diff, in the window's diff tab with the keys, the way a pull request shows it: everything its branch changes against its base, the branch it was cut from (see [New agents](#new-agents)) or else the pool's (`base_branch` in `treehouse.toml`, else `origin/HEAD`), measured from their merge base, plus uncommitted and untracked files.
Removed lines sit on the left and the added lines that replace them on the right, row by row, with line numbers on both sides; within a changed line, the words that differ are highlighted.
Every file runs in one scroll, each under a header with its path and its additions and deletions, and the header of the file you are reading stays pinned to the top.
Code is coloured as an editor colours it, by the grammars [bat](https://github.com/sharkdp/bat) ships, for every language they cover, such as Rust, TypeScript, Svelte, TOML and Markdown; a file with none, such as plain text, stays one colour.
Each hunk is coloured on its own, since the lines between hunks are not there to carry a comment or a string across.
The diff opens plain and its colours come in a moment later, read in the background; past 20,000 lines the rest stays plain.
Its colours are Neovim's under gruvbox-material (medium): its syntax groups for code, `DiffDelete` and `DiffAdd` behind lines, a brighter shade of each behind the words that differ, `LineNr` for numbers, `diffLine` for hunks, `diffFile` for paths.
In a terminal without true colour, code stays one colour, since 16 colours cannot tell a keyword from a type beside the diff's own red and green.

| Key | Does |
|---|---|
| `j` `k` | a line |
| Space, `b` | a page |
| `d` `u` | half a page |
| `n` `p` | the next or previous file |
| `h` `l` | sideways, for lines wider than their half; `←` and `→` step the window's tabs |
| `g` `G` | the top or the end |
| `Ctrl-R` | read the diff again |
| Esc, `Tab`, `q`, Ctrl+Q | give the keys back to the table; the diff stays in its tab |

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

- **Agent plugins** (see [Plugins](#plugins)), each asked what agents it sees. Claude Code's is built in: Claude Code's `EnterWorktree` moves a session into a tree without changing its process's working directory, so the plugin reads its running sessions from `~/.claude/sessions` (or `$CLAUDE_CONFIG_DIR`, or each dir `[claude] config_dirs` lists, see [Config](#config)), checks each pid is the one that started the session, and takes the tree from the latest `cwd` in the session's transcript.
  This gives the session's name and whether it is busy, idle or waiting on you.
  It also reports every background job in `~/.claude/jobs` with no running session as a **cold** agent, drawn `◌`: Enter attaches to it, which starts it again with its conversation kept.
  An agent a plugin reports, running or cold, shows in the tree whose `treetop.json` names its job, else in the tree it last worked in when that was since the tree's lease began, so one started by hand stays on its tree while a tree that has been returned and handed out again never shows the last lease's agent, even one still running.
  It stays there however long ago it finished; only what waits on you forgets a question a day old whose session has stopped, and a finished job after a day.
  These files are internal to Claude Code, so a release that changes them blanks this source until treetop catches up.
- **A process** standing in the tree whose name, or the script its interpreter runs, is a known agent: `claude`, `codex`, `aider`, `opencode`, `gemini`, `goose`, `amp` or `cursor-agent`.
  `TREETOP_AGENTS=one,two` adds names. This sees any agent started inside a tree, by name only.

A waiting agent is drawn in the warning colour, a busy one in green and an idle one muted; `·` means the source cannot tell.

A held tree keeps its background agents awake while treetop runs, so an agent never comes apart from its tree.
Claude Code's daemon retires a background session that has sat idle for about an hour; treetop pins each one in a held tree, as Ctrl+T in `claude agents` does, so the daemon leaves it running, and starts any that has gone cold again in the background, with its conversation and no new message, at most once every ten minutes.
A cold one with no conversation saved to resume is left cold, and a wake that comes up as a new session rather than the cold one removes the new one, so waking never piles up sessions.
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

A repository can add hooks of its own under `[repos]`.
Its `on_return` runs in each of its trees after the global one, for what only it needs, such as clearing a Rust tree's build leftovers so a returned tree does not keep tens of gigabytes of old test binaries.
Its `on_catch_up` runs in its main checkout each time treetop fast-forwards it (see [Use](#use)), given ten minutes, so what new trees are seeded from, such as installed packages, keeps up with the base; a failure shows on the status line.

```toml
[repos."~/treetop"]
on_return = ["cargo", "clean", "-p", "treetop", "-p", "issues", "-p", "linear"]

[repos."~/endo/app"]
on_catch_up = ["harness", "provision"]
```
One that fails is written to the log and never stops the start or return it belongs to.
A config file treetop cannot parse is reported in the log, and no hooks run.

`[guard] in_tree` names commands of your own that act on the tree they run in, such as stopping its dev stack, each as the words it starts with; the guard hook lets an agent run them only in a tree it holds:

```toml
[guard]
in_tree = ["docker compose down"]
```

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

`browser` is the command `o` and `O` open a URL with, in place of `xdg-open` (or `open` on macOS).
Each `{url}` in its words becomes the URL, else the URL is added last.
It runs whether or not there is a desktop session, so on a server reached over SSH it can hand the URL to the browser on the computer you are typing at, as [tether](https://github.com/anthonyb8/tether)'s `tether open` does.
It runs off the screen's thread, with the status line saying it is opening until it is done.
Where it will not start, or exits unsuccessfully within five seconds, the URL is copied to the clipboard instead, with the first line of its error on the status line, such as `tether: no browser connected; copied http://localhost:5173`; one still running by then, as a browser it started itself is, counts as opened.
Being a top-level key, it goes above the first `[table]` in the file:

```toml
browser = ["tether", "open"]
```

`share` is the command `p` shares a port with so the phone can reach it, and `treetop serve` its own, `tether share` when the config names none.
Each `{port}` in its words becomes the port, else the port is added last.
It runs off the screen's thread, and the first line it prints that starts with `http` is the address treetop draws as a QR code, in black on white whatever the theme, so the phone's camera reads it.
With [tether](https://github.com/anthonyb8/tether), that address is the port on your tailnet over HTTPS, so the phone needs only the Tailscale app; `tether unshare <port>` stops sharing it.
Where the command fails, or prints no address within 30 seconds, as `tether share` does on a tailnet without Serve while it waits for you to enable it, the popup says what it wrote instead.
Like `browser`, it goes above the first `[table]`:

```toml
share = ["tether", "share"]
```

`publish` is the command `P` puts a port on the internet with, `tether share --public` when the config names none, and `unpublish` the one `x` takes it down with, `tether unshare` when it names none.
`{port}` works in both as in `share`, and the first line `publish` prints that starts with `http` is the link treetop copies to the clipboard and shows as a QR code.
With tether, the link is the port on Tailscale Funnel, on the first free of HTTPS ports 443, 8443 and 10000, so three ports can be public at once, and whoever opens it needs nothing installed.
Funnel must be allowed in the tailnet's policy; where it is not, the popup shows the link tether prints to allow it.
tailscaled must also let you write its serve config, which it does for root and for the operator user: where neither `p` nor `P` works and the popup says `Access denied: serve config denied`, `sudo tailscale set --operator=$USER` once is what it wants.
`tether unshare` stops the port's tailnet share along with its public one, so `p` and `P` both take a port all the way down, and `p` shares it again afresh.

```toml
publish = ["tether", "share", "--public"]
unpublish = ["tether", "unshare"]
```

`shares` is the command the PORTS column reads the marks from, `tether shares` when the config names none.
It takes no port, and prints a line an address, as `https://host:5173 -> http://localhost:5173`, with ` (public)` on the ones the internet can reach; treetop marks the local port each line forwards to, and anything else it prints is ignored.
Where the command is missing or fails, no port is marked and nothing is said: sharing a port reports its own failures.

```toml
shares = ["tether", "shares"]
```

`[claude]` lists the Claude Code config dirs treetop finds sessions in, for a setup that runs some sessions under a `CLAUDE_CONFIG_DIR` of their own, as keel's `keel claude` does with `~/.keel/claude`:

```toml
[claude]
config_dirs = ["~/.claude", "~/.keel/claude"]
```

Sessions, background jobs and their reports are read from every dir, and each session is attached, messaged, resumed, kept awake, stopped and cleared under the dir it came from: its attach command is `env CLAUDE_CONFIG_DIR=<dir> claude attach <id>` for any dir but the default one, which is never named, since naming it would move Claude Code's `.claude.json`.
A new agent starts under the first dir listed.
Plan usage is read from the default dir's login.
With no `[claude]` table it is `$CLAUDE_CONFIG_DIR`, else `~/.claude`, as Claude Code picks.

`treetop tickets` lists the repository's open tickets from its source, `--mine` those assigned to you, and any other words search their titles; it is the quickest check that a key and team are right.
treetop calls `https://api.linear.app/graphql` over HTTPS itself, so the key never appears in a process list.

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
| `find` | `dirs` | `agents`: each with `id`, `name`, `dir`, `live`, and optionally `pid`, `status` (`busy`, `idle` or `waiting`), `background`, `attach`, `context`: `{ "used", "window", "parts" }`, `parts` each `{ "name", "tokens" }`, largest first, and `spend`: `{ "input", "cache_read", "cache_write", "output" }`, the tokens its session has spent |
| `attach` | `id` | `argv`, the command that opens its chat in a terminal |
| `send` | `id`, `text` | `delivered`, for an agent that is not running |
| `usage` | none | `limits`, each `{ "name", "percent", "resets_at" }`, and `at`, when they were read, both in seconds since the epoch; the plan its agents run on, which the chat's sidebar shows |
| `resume` | `id`, or `dir` and optionally `since`, seconds since the epoch | `id` it runs under now; starts a stopped or failed agent again with its conversation and the flags it was started with: job `id`, else the newest one not running that worked in `dir`, written since `since`; Claude Code's runs `claude --bg --resume` with the job's `respawnFlags`, where it was started |

`describe` may also give `screen`, which tells treetop how to type a message into a running agent's chat: the input box is a line starting with `input` between two lines starting with `rule`, and a screen with no input box but one of `dialogs` on it is waiting on an answer.
Dim text in the box is the chat's own hint, such as Claude Code's for a queued message, and counts as empty.
One of `away` on the screen's last few lines means the chat has left its agent for a list of other sessions, which `←` in an empty input box opens; treetop keeps `←` from it there, and opens the agent's chat again when it shows.
Claude Code's is `{"input": "❯", "rule": "─", "dialogs": ["Do you want to", "❯ 1."], "away": ["describe a task for a new session", "enter to return"]}`; without one, treetop types on a timer.
| `stop` | `id` | nothing; the agent stops and is no longer kept awake |
| `remove` | `id` | nothing; the agent is deleted, running or not, so it is found nowhere again; Claude Code's is `claude rm` |
| `keep` | `ids` | `woken`, the ids it started again; keeps the agents of held trees running, starting cold ones again |
| `clear` | `dir` | `removed`, the ids it deleted, and `failed`; deletes every agent that worked in `dir`, in any state, failed ones included, when its tree is returned or destroyed |

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
| `view` | `dir`, the tree, and `branch` | `pr`: the branch's PR read whole for the PR view, or null for none: `number`, `title`, `url`, `state`, `draft`, `author`, `base`, `branch`, `head`, `body`, `additions`, `deletions`, `files`, `commits`, `review`, `merge`, `checks` (each `name`, `outcome`, `workflow`, `seconds`, `url`, `log`), `threads` (each `author`, `path`, `line`, `resolved`, `code`, `code_line`, `body`, `replies`) and `bot`, CodeRabbit's review (`reviewed`, `reviewing`, `findings`: each `kind`, `severity`, `path`, `lines`, `title`, `body`, `fix`, `prompt`, `resolved`) |
| `run` | `dir`, `branch` and `intent` | once the run is under way, which goes on after the call returns |
| `attach` | `dir` and `branch` | `argv`, the command that shows the run under way in a terminal |

`stage` is one of `running`, `gate`, `failed`, `draft`, `checks`, `ci_failed`, `changes`, `review`, `approved`, `merged`, `closed` or `idle`; any other reads as `running`.
`gate`, `failed`, `ci_failed` and `changes` need you.
`label` is the few characters the PR column shows, such as `#12 ✗ ci`, `detail` the lines the info tab adds, and `head` the commit the PR or run is on.
`idle` means it has nothing current to say, such as a pipeline whose run ended, so the next plugin's answer shows.

Claude Code's plugin is built in, and `treetop plugin claude` serves it on stdin and stdout like any other; a plugin of your own named `claude` replaces it.
Two ship plugins are built in the same way, `treetop plugin github` and `treetop plugin no-mistakes`.
treetop starts new agents with it unless `TREETOP_NEW_AGENT` names a command.

## What the actions run

Both run in the background, up to four trees at once, while treetop stays on screen; each row's STATUS shows queued, returning, destroying, returned, destroyed or failed.
The info tab shows the end of each tree's last action's output, and a failure also shows its last line in the status line.

- **Return** first shows a dialog listing each tree's uncommitted files, unpushed commits and running processes, or "unknown changes" where git cannot read it. On `y`, for each tree at once:
  1. it removes the tree's background agents through their plugin, running, cold or failed, so none is found in the tree when it is leased again; a plugin that answers `clear` removes every one that worked in the tree, those it does not show included; a session that moved in with `EnterWorktree` runs from elsewhere, so treehouse would not stop it, and removing it can hand the tree back already, through the `WorktreeRemove` hook, which the return then takes as done; an agent its plugin cannot remove is stopped instead, so the tree stops keeping it awake;
  2. runs the `on_return` hook, if the config names one;
  3. runs `treehouse return --force`, which stops the processes left in the tree (the dialog replaces treehouse's own prompt), with `--if-lease-id` for a tree treetop leased, so a lease someone has taken since is never returned;
  4. deletes the branch treetop named for it when its work is in the pool's base branch, or when its PR merged with the branch where it is now, which covers a squash or rebase merge; a branch with commits after the merge is kept;
  5. removes `treetop.json`.

  The row turns available as soon as its return finishes.
- **Destroy** runs `treehouse destroy`'s dry run with every `--include-*` flag in the background and shows it in a dialog per tree. `y` removes the tree's background agents as a return does, then runs it with `--yes`; `n` keeps the tree.

`q` while an action runs waits for it to finish; `Ctrl-C` quits at once and leaves the running command to finish on its own.

The tree treetop was started in is never a target, because returning or destroying it would kill the shell standing in it, and neither is the main checkout.

## Agent chats

The Trees screen is two windows over the bottom bar: the table on top, and under it a window on the tree under the cursor, with the [banner](#what-waits-on-you) across the top while anything waits on you.
The table's window is as tall as its rows, up to just under half the screen; the window under it takes the rest.
The window that has the keys has its border in the title colour, as Neovim marks the window the cursor is in.

The window under the table names the tree and its branch in its border, and has its tabs in a bar on its first row; `←` and `→` step through them, and a click on one shows it:

- **chat**: the agent's chat, live, where an agent offers one.
- **diff**: the tree's diff, as `d` shows it (see [The diff](#the-diff)), on a held tree; the main checkout's heading has no diff or PR tab, as `d` and `v` do nothing there.
- **PR**: the tree's PR, as `v` shows it (see [The PR view](#the-pr-view)), on a held tree, named with what the PR column says of it.
- **run**: the steps of the keel run that holds the tree, the one under way picked out, while an active or paused run holds it (see [keel runs](#keel-runs)).
- **shell**: a shell in a held tree, live, in `$SHELL`, run in a terminal of treetop's own as a chat is; Ctrl+Q gives it the keys, and it keeps running behind the screen as a chat does.
- **info**: the tree in full (see [The info tab](#the-info-tab)).

**Ctrl+\\** on the table puts two windows side by side under it: the tree the cursor was on stays in the left one, marked ◧, and the cursor moves to the next tree down, in the right one, marked ◨; each window's tree carries its mark in the table.
The cursor drives the window that last had the keys, so moving it changes only that one, and the other keeps its tree and tab, its chat or shell running live behind.
Ctrl+Q moves the keys round the table, the left window and the right one; giving a window the keys puts the cursor on its tree.
A window given the keys with nothing in it to type into, its info or run, keeps them from the table: Esc or `q` gives them back, and `←` and `→` step through its tabs.
Ctrl+\\ again goes back to one window, the one that last had the keys.
A terminal too narrow for both shows the window the cursor drives alone, its title saying `1 of 2` or `2 of 2`, and Ctrl+Q still moves between them.
The double is kept per workspace under `$XDG_STATE_HOME/treetop`, so treetop opens it again as it was left.
Inside a chat, Ctrl+\\ still goes to the next agent's chat (see [Agent chats](#agent-chats)).

**Ctrl+P** opens the command palette over whatever is on screen: every action treetop has, those on the tree under the cursor first and then the pool's, each with the key that does the same at its right.
Typing narrows it to the commands with every word typed, those starting with the first word first; a tree number or a branch offers to open that tree, as Enter on it does, clearing a filter or opening a folded group that hid it.
Up and Down pick a command, Enter runs it as its key does on the table, and Esc closes it.
In a tree's agent chat treetop takes Ctrl+P for it, as it takes Ctrl+Q; a shell keeps it, as its history's previous line, and so do the assistant's and scratch chats.

**Ctrl+F** zooms: on the table, to the screen treetop had before its windows, the table unboxed over the screen above the bar with the tree under the cursor in full in a pane under it, and the keys; in a window, the windows take it, the table folded to the rows of their trees where the terminal has room for them.
Ctrl+F again zooms back out.
In an agent's chat treetop takes Ctrl+F for this, as it takes Ctrl+Q; a shell keeps it, as cursor-forward and as page down in vim and less.

Moving the cursor keeps the tab, so the diff tab shows each tree's diff in turn and the PR tab each tree's PR, once the cursor rests on it; a PR read before shows as it was last read, and `v` reads it afresh.
A tab the tree under the cursor has not got shows its info instead.
At the right of the bar, how full the agent's context is and what its session has spent.
Once the cursor has rested on a tree for a moment, the chat tab shows its agent's chat, so moving down the list opens no chat on the way; a tree whose agent is not running says Enter starts it again, since opening it would.

Enter gives the chat's window the keys in Normal and Enter again starts Insert, ready to type to, while a click in the chat goes straight to Insert; Ctrl+Q gives the window the keys in Normal too (see [Normal and Insert](#normal-and-insert)), and a click outside it gives them back to the table, with the chat still running on screen.
An agent's question counts as seen once its chat has had the keys, not when it only showed in the window.
The chat's terminal is the size of the inside of the window, under its tab bar, and follows it when the terminal is resized or the banner comes or goes.

The assistant's chat (`a`) and the scratch chats (`C`) belong to no tree, so they open in a popup over treetop instead, the way `d` opens the diff; so does a tree's chat given the keys over the Issues screen, or on a terminal too short for the window, and one without the keys there stays out of sight.
The pool stays on screen, dimmed, round the popup's border, so a chat never passes for a Claude Code session of your own; a bar on the popup's first row, inside its border, names the agent, with the keys out at its right.
Where the popup is at least 100 columns wide, a sidebar at its right takes 30 of them and shows how full the agent's context is: tokens used, the window, and a bar that turns to warning colours from 75%, as the agent nears compacting it.
Under that is an estimate of where those tokens come from: the memory files and skills listing Claude Code put in before the first request, the rest of that request as the system prompt and tools, and everything since as the conversation.
Claude Code's numbers come from its transcript: the tokens its latest request sent, against a window of a million tokens for a Claude 5 model, a `[1m]` one or one already past 200k, else 200k.
An agent that cannot say shows "not known for this agent".
Below it, Usage shows how much of your plan is used, a bar for each limit, such as the five-hour and weekly ones, with when each resets and how old the reading is, then what this session has spent: its fresh input, cache reads, cache writes and output, each request counted once, its subagents' included.
Claude Code's limits are asked of Anthropic as `/usage` asks, with Claude Code's login from `~/.claude/.credentials.json`, once a minute and every five minutes while that fails.
treetop only reads the login: an expired one is left for Claude Code to renew, since renewing it here would sign Claude Code out, and until then the sidebar shows the reading Claude Code last kept in `~/.claude.json`, with its age.
At the bottom is the chat's tree: its git state and PR as the list's columns show them, then under Ports each port it serves with the process listening on it, then under PR its link and what its plugin says of it, such as its checks and review.
**Ctrl+Q** always takes the keys back, whatever the chat is doing: from a popup, back to the list as you left it, and from a tree's chat in Insert, to Normal.
**Ctrl+\\** goes straight to the next agent's chat, down the list as filtered, skipping trees with none and wrapping at the end; the cursor follows, so Ctrl+Q lands on the tree you were last in.
Neither key is a Claude Code default.
The agent's session keeps running after you leave.

A chat shows "opening" until its first screen has finished drawing, and is drawn a whole frame at a time, so it never flashes half-drawn.
A chat you leave keeps running in the background with its screen current, so Enter on it again is instant; treetop keeps the 8 most recently left, and ends them when their agent goes away or treetop quits.

treetop runs the chat in a terminal of its own and draws it, so nothing the chat does to its terminal reaches yours, and treetop keeps refreshing behind it.
Keys, pastes and the mouse go to the chat: the wheel scrolls back through it, three lines a step, and clicks work as they do in Claude itself.
Over the window's chat while the table has the keys, the wheel still scrolls it, wherever treetop has the mouse.
treetop takes the mouse while a chat has the keys, and while the banner is up for its clicks; elsewhere the terminal and tmux keep it.
From the keyboard, Ctrl+O opens Claude's transcript view, where j/k move a line and Ctrl+u/Ctrl+d half a page; PgUp and PgDn also work where a keyboard has them.

A background Claude Code session (started with `claude --bg`, or backgrounded) opens with `claude attach`, which never starts a second copy.
Ctrl+Z also leaves it.
A chat only ever shows the agent it was opened on: `←` on an empty prompt, which would open Claude's own list of sessions, does nothing, and if the chat reaches that list another way, such as double Ctrl+C, treetop opens the agent's chat again at once and says so in the status line.

An agent with an `attach` command in its `agent.json` opens the same way.
An agent running in a tmux pane, such as an interactive `claude`, belongs to that pane and cannot be opened elsewhere: Enter goes to the tree instead, which inside tmux switches to its pane.

### Normal and Insert

A window with the keys is in one of two modes, as in vim, and says which in the bottom-left corner of its border, as Neovim's status line does: `NORMAL` in grey, or `INSERT` in blue, with the key that leaves it beside it.

**Normal** gives the keys to treetop.
`←` and `→` step through the window's tabs, from a chat as from any other.
`j` and `k` scroll a line and `d` and `u` half a page, on the chat's history as on the diff and the PR, whose other keys work as ever.
`i` or Enter on a chat or shell starts Insert.
Esc, `q` or Ctrl+Q gives the keys back to the table; with two windows, Ctrl+Q moves them on to the other one first.

**Insert** gives every key to the chat or shell, Esc too, which interrupts Claude Code.
**Ctrl+Q** drops back to Normal.
F13 to F24 are Ctrl+Q everywhere in treetop, so a Caps Lock remapped to one of them, such as F19 (with keyd on Linux or Karabiner-Elements on a Mac, on the machine whose keyboard you type on, over SSH too), does all Ctrl+Q does in one key, while Esc still interrupts Claude Code.
Ctrl+\\, Ctrl+F and Ctrl+P keep working as before, and a shell keeps Ctrl+F and Ctrl+P for itself.

Ctrl+Q from the table, Enter on a tree and an agent opened with `e` land in Normal, so nothing typed reaches an agent by accident; a second Enter starts Insert, and a click in the chat lands in Insert, ready to type.
The diff, PR, run and info tabs have nothing to type into, so they are only ever in Normal.
The bar at the bottom lists the mode's keys while a window has them.
The mouse wheel over a window scrolls what it shows, from the table while the banner is up and from Normal on a chat or shell; on a diff or PR with the keys, the terminal keeps the mouse, so text selects there as ever.

## The assistant and dispatch

treetop is where agents are sent out, and the assistant is who keeps track of them for you.

`n` opens the compose box in the window under the table, the one the cursor drives, for a new agent in the tree under the cursor: a free tree is leased for it with `treehouse lease`, and one already held is started in as it is.
`N` opens it for a new agent in a fresh tree from the pool instead, leased with `treehouse get --lease` when the job runs, so two quick dispatches never share one.
`:` opens it to the assistant, about the tree under the cursor (see below).

Enter sends it, Alt+Enter breaks the line, Esc cancels, Ctrl+U clears the text and Ctrl+W a word; a paste keeps its line breaks.
The text wraps and the box grows with it.

### The assistant

`:` sends what you type to the assistant with the tree under the cursor as context.
It answers in its chat, which `a` opens.
A message that starts with `@` goes to an agent instead, not the assistant: `@17 put it in branding/` types it into tree 17's agent's chat, as `m` would.
`@` takes a tree number, `main`, or the start of an agent's words, `@fix-log` for "fix the login redirect", and several `@`s send to each; one that names no tree, or more than one, opens the box again on what you typed, with the status line saying why.
On the Issues screen, `:` opens the box with the issue under the cursor as context.

It is `claude --bg --name treetop-assistant`, started in the main checkout the first time you send it something, with a system prompt that makes it the pool's assistant rather than a coder.
A message to it is sent the way `m` sends one, typed into its chat behind the screen.
treetop finds it again by its name: while it runs, and for a day after it stops, through `claude attach`, which starts it again with its conversation kept.
After that a new one starts.
A coordinator from before the assistant, named `treetop-coordinator`, is never taken for it, since it acts itself; it is left off the rows, and a new assistant starts beside it.
It is never a tree's agent: a badge at the right of the bottom bar shows it instead, `assistant · answering` while it works on your message, `asks you`, `idle` or `stopped`, and none before it first starts.

It sees and manages the other agents through treetop commands it may run without asking.
Anything they do not cover, such as returning a tree with work left in it, stays a request to you, for you to carry out with treetop's keys:

| Command | Does |
|---|---|
| `treetop status` | prints JSON: each held tree's branch, uncommitted and unpushed counts and ports, its agents and their state, each background agent's latest report (the question it is waiting on, its answer once done, else what it is doing now), and its PR's stage as `ship`; idle trees are only counted, and `ready` lists the issues the merged trees' issues leave ready |
| `treetop tickets [--mine] [words]` | prints the repository's open issues |
| `treetop dispatch <new\|main\|N> <task>` | starts an agent on a task in a fresh tree, the main checkout or tree N |
| `treetop resume <tree\|job>` | starts a stopped or failed agent again in its tree, with its conversation and the flags it was started with, as `R` does |
| `treetop send <tree\|agent> <message>` | types a message into an agent's chat, as `m` does |
| `treetop return <tree>` | gives a tree back to the pool, as `r` does, when the guard below lets it |

`treetop status` answers in milliseconds while treetop is open on the workspace or `treetop serve` serves it: each writes the workspace's status every two seconds once it has read the pool, under `$XDG_CACHE_HOME/treetop`, and `status` prints that when it is under ten seconds old.
With neither running it reads the pool itself, which takes seconds, `treehouse status` most of them.

`treetop return` never stops a process for whoever runs it.
It refuses a tree that any live process works in, other than the caller's own: a process standing in it, or an agent treetop places in it.
Beyond that, the caller may return the tree its own agent works in, or, when it is no agent, the tree it stands in; any other tree only once its PR has merged or it is on no branch, with nothing uncommitted and nothing unpushed.
It also refuses a tree a [keel run](#keel-runs) holds, active or paused, whoever asks, and says to stop the run with `keel run stop` first.
treehouse's own `--if-lease-holder` checks only the name it is handed, so this is the check to go through; everything it refuses is yours to return with `r`, whose dialog names what would be lost.
`treetop guard <tree>` gives the same verdict without acting, and `--in-tree` the verdict on a command that acts on the tree it runs in, which the caller may run only in a tree it holds, or in one back in the pool that nothing runs in.

### The guard hook

Agents give trees back with `treehouse return` and `destroy` as often as through treetop, so `treetop hook pre-tool-use` holds their shell commands to the same guard as Claude Code's PreToolUse hook.
It splits a Bash command into the commands it runs, following `cd`s and `sh -c`, and denies, with the guard's reason for the agent to read:

- `treehouse return` or `destroy` of a tree `treetop return` would refuse;
- `treehouse return --all` and `destroy --all`, which take every tree in the pool;
- a command `[guard] in_tree` names (see [Config](#config)), run in a tree the agent does not hold.

Anything else gets no answer, so Claude Code's own permissions decide as before; the hook never allows what they would not.
It reads words, not what a shell would run, so it is a guard rail against an agent's mistake rather than a sandbox.
Register it in `~/.claude/settings.json`:

```json
"hooks": {
  "PreToolUse": [
    { "matcher": "Bash", "hooks": [{ "type": "command", "command": "treetop hook pre-tool-use" }] }
  ]
}
```

`treetop pick` remains, for you and any other agent, from any shell.
Each command prints only what it is asked for, so a question costs the assistant one small read rather than a crawl through every transcript.

### Scratch chats

`C` lists scratch chats: Claude Code sessions for thinking something through, tied to no tree.
The first line starts a new one, idle, and opens its chat as soon as it runs; Enter on a chat opens it again, starting it if it stopped, and `x` twice deletes it with `claude rm`.
Each line shows how old the chat is and what it last said.
They all run in one folder, `$XDG_STATE_HOME/treetop/scratch` (else `~/.local/state/treetop/scratch`), outside every repository, so they never land on a tree, never open as asks, are left out of the summary, and are the same list in every workspace.
Claude Code runs a background session only in a folder you have trusted, so before the first one, run `claude` in that folder once and accept its trust prompt; until then the list says so.

### New agents

Every tree is leased with `treetop` as the holder, and a tree treetop leases always gets a branch: `<git user.name>/<slug>`.
The compose box's branch line says it is named from the task when it starts; Down moves to that line to name it yourself, starting from the task's words, and Up comes back; a tree someone already holds keeps its branch.

A branch you did not name, and the agent, are named by a small model that reads the task: `can you scan he project and cmpiel a list` shows as "Scan project for useful tools" on `<user>/scan-project-tools`, not under its first words.
It is `claude -p` on Haiku with no tools, no saved session and no user settings, given the task on its standard input, and the start never waits for it: the agent starts under the task's first words, and the answer, a few seconds later, renames the branch while nothing has been pushed from it and gives the agent its title.
treetop keeps the titles in `titles.json` under its state directory and shows them wherever it names the agent; Claude Code's own `claude agents` keeps the first words, since a running session's name cannot change.
When the model cannot answer within ten seconds, or its answer names a branch that exists or git would refuse, the task's own words stay.
A ticket's name and branch, and a branch you edited, are always kept.
A fresh tree from `N` also has a base line under it: the branch the tree is cut from, the pool's base unless you pick another.
On it, `←` and `→` step through the repository's branches, local and on origin, the pool's base first and the rest most recently committed first.
Typing there narrows them to those that start with what you type, then those that contain it, ignoring case, and `←` and `→` step through the matches; Backspace and Ctrl+U widen them again.
It only ever names a real branch: when what you type matches none, the line says so, and Enter keeps the box open until it does.
treetop leases the tree with `treehouse get --lease --base <branch>` and records the base in the tree's `treetop.json`, so its diff goes against it and the info tab says `from <branch>`.
`n` on a free tree cannot take a base, since `treehouse lease` takes the tree as it is.
`b` renames the branch of the tree under the cursor, or names one on a detached HEAD.

The agent is `claude --bg --name <the task's first words> -- <task>`, which Claude Code's own agent view lists too; `TREETOP_NEW_AGENT` names another command, which gets the task as its last argument.
An empty task starts the agent idle and opens its chat as soon as it shows up, so its first message is typed there.

A start runs as steps, each with its own timeout: lease the tree (`treehouse get --lease --branch`), check it is a clean worktree, name the branch, write `treetop.json`, start the agent, and wait for `claude agents` to list it, since `claude --bg` exits as soon as the session is queued.
The window the box was written in follows the start as it runs: each step ticked as it ends, one it did not need marked as such, and the one under way, then its chat, once the agent runs and its tree is listed, with the keys; the row of a fresh tree reads `starting...` in the table meanwhile.
A step that fails is marked in the window with why, until Esc; Esc while it runs hides it, and the start carries on, its chat left for Enter.
The window holds no keys while a start runs, so a second start can follow the first at once; in a double, the other window keeps its tree.
When a step fails, the ones before it are undone in reverse: the agent stopped, the record removed, the branch deleted and the tree returned. The row shows "failed" and the status line which step failed and why.
The start's log ends with how long each step took, such as `took 1.4s: lease 0.0s branch 0.0s record 0.0s agent 1.4s attach 0.0s`, so a slow start says which step was slow.
Up to four actions run at once, each on its own tree, so a slow one never holds up the rest.

### Messages

`m` opens the compose box to write to the agents in the marked trees, or the tree under the cursor.
A running agent gets it typed into its chat, the same `claude attach` that Enter opens, behind the screen if the chat is not open.
treetop reads the chat's screen as it goes, by the patterns the agent's plugin gives: it waits for an empty input box, pastes the text, presses Enter once the paste shows, and counts it sent once the box empties again.
A dialog on screen, or a draft already in the box, stops it rather than typing into either, and the status line says which; so does a box that never shows, or a paste or Enter that does not land within a few seconds.
An agent mid-turn queues it, as Claude Code queues anything typed while it works.
A cold agent gets it through its plugin instead, which for Claude Code is `claude --bg --resume`: the session starts again with the message as its next turn, under the same job.
Agents with no chat to type into, such as an interactive session or a bare process, are left out, and the status line says how many.

### Handoffs

An agent whose context has filled with more than its work needs can hand that work to a fresh one, in the same tree and on the same branch, rather than compacting.
`h` on its tree types a request into its chat, as `m` would, asking it to write the prompt it would want to be handed to `.treetop-handoff-<time>.md` in the tree, and to stop there.
The row's STATUS says `handoff...` meanwhile, and `h` again stops waiting.
Once the agent has stopped working and you are free on the list, treetop reads the file, takes it out of the tree, and opens it in the compose box, which grows to the screen for it and shows it from the top until you type, which brings its end into view.
Read it over there, add to it, or press Ctrl+E to edit it in `$VISUAL`, else `$EDITOR`; Enter starts the new agent on it, and Esc drops it.
The new agent runs under the old one's name, and once it runs, the tree's `treetop.json` names it and the old one is stopped, so `R`, the reports and keeping the tree awake follow the new one.
The old conversation is kept: `claude attach <id>`, with the id the info tab's last action names, opens it.
A start that fails leaves the old agent working, as it was.
Only a tree treetop leased can hand off, since its `treetop.json` is what moves the tree to the new agent, and only a running background agent can be asked; a stopped one is started again with `R` first.
With `TREETOP_NEW_AGENT` set, a handoff is refused before it starts anything, since treetop cannot name a custom command's agent in the tree.
In an agent's chat, from 75% of its context the sidebar says so: Ctrl+Q until the table has the keys, then `h`.

### What agents say

Every background agent's latest report is read from Claude Code's job files under `~/.claude/jobs` and marks its tree's row in AGENT: `●` while it needs you, `✓` once it is done. What it said, the question it waits on, its answer, else what it is doing now, is in the info tab, and a question opens its chat (see [Asks open their chat](#asks-open-their-chat)).
A report belongs to the tree whose `treetop.json` names its job, or where its agent runs now, so a recycled tree never shows the last agent's words.
An agent sitting at a question dialog or permission prompt needs you too, though its report still says working, since only its chat can answer it.
Trees waiting on you come first in the list, and the cursor stays on its tree as rows move.
An agent whose process died stays on its tree as a cold agent, so Enter starts it again with its conversation, as the tree keeping it awake also does.
Finished, stopped and failed reports go after a day; working and waiting ones stay.
The assistant's own reports are left off its row, since it speaks through its chat, which `a` opens.

### Asks open their chat

An agent that asks you something, a question or a permission prompt, gets its chat opened for you with the keys, in the window under the table, so you answer it there in full.
It waits until you are free on the list: never over a chat, one read in Normal too, the compose box, a filter, the diff, a dialog or the Issues screen.
Giving the keys back to the table, with Ctrl+Q from Normal, opens the next ask, so once you are done with other work they come to you one after another until none is left.
Each ask opens once: one you have seen, opened for you or asked while its chat was open, stays shut until that agent asks something new, and its row keeps its `●` for Enter.
A finished agent never opens, nor does a busy one whose last report still asks, nor one with no chat here, such as `claude` in a tmux pane.
A filter that hides the asking tree is cleared, so the cursor lands on it.
Opening treetop, or coming back to a workspace, opens only what is waiting there that you have not seen yet.
What you have seen is kept per workspace under `$XDG_STATE_HOME/treetop` (else `~/.local/state/treetop`), so neither a switch nor a restart opens it again.

### What waits on you

Something waits on you while an agent asks you something or sits at a prompt, a tree's PR has failed its checks, or an agent has died, in this workspace or another.
It stops waiting once that stops being true: the agent is answered, a new push passes, the agent is resumed with `R`, or the tree is returned.
An agent still asking is the one ask however often it writes, so it rings once.
So there is nothing to open or dismiss, and an agent that finished or a PR that merged never waits.

While anything waits, a banner runs across the top of the screen, yellow for an ask and red for a failure: the tree, what it waits for, its ticket and branch with the question or failure, how many more wait, and `e`.
It never takes the keys, so what you type keeps going where it was, a chat in Insert included, and it stays bright over a popup.
`e` from the table, or from a chat or shell in Normal, opens the first: an agent's chat, with its question at the bottom; for a PR, the cursor on its tree with its checks in the info tab, where `f` sends a failure to its agent; for a dead agent, the cursor on its tree, where `R` resumes it.
`e` again goes on to the next, round them all, and says so when nothing waits; a click on the banner opens the one it names.
The oldest comes first: each is as old as it has waited, from when it happened, or from when treetop first saw it where that is not known, as for a PR.

Inside tmux, the status line of every window shows `● 2 waiting · 17 22` at its right for as long as anything waits, and clears once nothing does or treetop quits.
treetop sets it on its session over the global `status-right`, and unsets it again, so your own `status-right` is left as it was.
When something starts waiting, treetop rings the terminal bell, once, so it is heard from any window; the assistant finishing an answer rings it too, and shows it in the status line for five seconds.

Back after ten minutes or more with no key typed in any tmux window, or in treetop outside tmux, your first key opens "While you were away", and goes no further, so it reaches no agent; typing in another window meanwhile keeps you present, so it never opens.
It lists what still waits, the oldest first with how long ago, and at its foot what finished or merged while you were away.
Enter opens the first, as `e` would, and the sheet stands aside until that one stops waiting, such as once you answer the agent; then the keys come back to the table and the sheet shows again with the next first, until it says you are all caught up.
`s` puts the first off till last, and Esc closes it.
`E` shows it again as you left it.

Another workspace's entries carry its name, such as `keel:3`, and opening one switches there first; an agent with no chat, such as `claude` in a tmux pane, is gone to inside tmux, as `t` does.
Its background agents are placed in their trees by the workspace's last listing; its PRs that failed, and its agents waiting at a prompt in a tmux pane, come from a read of its pool every three minutes, for the workspaces opened in the last week, so they can show a few minutes late.
With only one workspace nothing extra is read.
The `w` picker counts what waits in each workspace after its held trees, `●` for what needs you and `✗` for what failed.

### The info tab

The window under the table's info tab shows the tree under the cursor in full: where it is, who holds it, its agents, ports and processes, everything its agent last said with its latest steps, its PR, and the outcome of its last action.
Only this workspace's agents open their chat; agents elsewhere are counted in the summary.
The summary counts the trees that need you, and, as "elsewhere", agents outside the pool waiting on you, which have no row.
A question more than a day old whose session no longer runs counts nowhere, since its process ended without saying so and nothing is left to answer it.
These job files are internal to Claude Code, like its session files, so a release that changes them blanks those marks until treetop catches up.

## keel runs

[keel](https://github.com/anthonyb8/keel) records each routed task as a run: the playbook it follows, the step it is on, its goal, and the decisions flagged for attention.
treetop reads them with `keel status --json` every 5 seconds, which only reads keel's files, and joins each run to the tree whose path it names.
The RUN column, the info tab's `run` line and the phone's tree card and screen show it; with no `keel` on PATH there is no column and nothing is asked again.
keel keeps a tree whose run is active or paused out of its own sweeps and hooks, since its steps are not done even with every change pushed.
treetop's return dialog and the phone's sheet warn of such a run, and `treetop return` and the guard hook refuse the tree to agents until `keel run stop` ends the run.

## After the agent

Once an agent is done, its tree's work still has a PR to get through: checks, review, the merge.
The PR column follows it, from the repository's ship plugins, so treetop knows no forge, CI or gate of its own.

- **GitHub**, built in and the default, reads every PR of the repository with one `gh pr list` and matches them to the trees' branches, the newest PR winning where a branch has several.
  It uses your own `gh` login, so treetop holds no token, and it only reads: it never pushes, opens, merges or reviews a PR.
  The column shows `#12 draft`, `#12 ci …` while checks run, `#12 ✗ ci`, `#12 changes`, `#12 review`, `#12 approved`, `#12 merged` or `#12 closed`; the info tab adds the link, the checks and the review.
  With no `gh`, no login or no GitHub remote, the column stays empty and the info tab says why.
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

## The PR view

`v` shows the PR of the tree under the cursor in the window's PR tab with the keys, read afresh each time, since review comments land without a push; the last read shows while the new one comes in.
It has four tabs, which `1` to `4` switch, as do `h` and `l`; `j` and `k` move between items, and `d` and `u` scroll through one too long for the screen.

- **Overview**: who wants to merge what, where its checks, review, CodeRabbit, open threads and merge stand, and its description.
- **Checks**: each check with its workflow and how long it ran, failures first and cancelled runs after them; a failed one shows the end of its log below, read for the first five failures, and Enter opens a check on GitHub.
- **Comments**: each review thread with the lines of code it is on, coloured as the diff colours them, and each comment on the whole PR, its Markdown drawn as the description's is; a resolved thread folds to one line until it is selected.
- **CodeRabbit**, on a repository CodeRabbit reviews: its comments as issues and suggestions, each with its severity, place and title, and the selected one with its text and suggested change. Its nitpicks fold into one line, which `z` opens.

`f` sends the selected item to the tree's agents, as `m` sends a message: a failed check with its log, a thread with its code and replies, or a CodeRabbit comment as the prompt CodeRabbit writes for agents.
`F` on the CodeRabbit tab sends every open comment but the nitpicks at once.
`O` opens the PR in the browser, `^R` reads it again, and Esc, `q` or Ctrl+Q gives the keys back to the table, the PR staying in its tab.

It reads through the repository's first ship plugin that answers `view`: GitHub's reads `gh pr view`, the PR's review threads over GraphQL, and the failed checks' logs, with your own `gh` login, and still only reads.
CodeRabbit's comments are read from the markers it leaves in them; one it writes in a shape treetop does not know shows as a plain comment rather than going missing.

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
The preview below the list scrolls with the mouse wheel over it, `J` and `K` a line, and `PgDn` and `PgUp` a page, so an issue can be read without opening it; the wheel over the list moves through the issues.
On the Issues screen treetop takes the mouse, so selecting text there takes Shift as you drag.
`w` switches to the next issue source, so a work and a personal workspace are one key apart, and `o` opens the issue in the browser.

Number keys switch between views: `1` your open issues and `2` everyone's, both grouped by status as Linear groups them, `3` grouped by project, `4` grouped by status in the team's workflow order, and `5` the current cycle.
Views of your own go on `6` to `9`, in name order, from `[views]` in the config, each a line of filters: `mine`, and `assignee:`, `creator:` (a name, or `me`), `priority:` (`urgent`, `high`, `medium`, `low` or `none`), `label:`, `state:` (a status, or `any`), `project:` and `cycle:` (`current` or a number), a value with spaces in double quotes, other words searched for, and `group:project` or `group:status`:

```toml
[views]
bugs = "label:Bug group:status"
review = "mine state:\"In Review\""
filed = "creator:me priority:urgent"
```

The line above the list says what it shows: whose issues, which status, and whatever else narrows them, such as `my issues · open · label Bug · “login”`.
`/` searches: the list narrows as you type, by id, title, label and assignee, and once typing rests Linear is searched too, by title or issue number, so older issues show; Enter keeps the search and Esc clears it.
`f` opens the filters: assignee (anyone, you, nobody, or a teammate), status (open, all, or one), priority, creator (anyone, you, or a teammate), label, project, cycle, and grouping, each picked from the team's own with Enter, the list following at once; `x` clears one and `c` all of them.
In a grouped list, `z` folds the group under the cursor to its heading and `z` or Enter on the heading opens it again, as Linear folds a status; `Z` folds every group, or opens them all.
`m` switches between yours and everyone's, `r` reads the list and the team again, and `?` shows these keys.

`s` changes the issue's status, `l` its labels, `a` its assignee and `P` its project, each from a picker of the team's own states, labels, members and projects that narrows as you type; labels are marked and unmarked with Tab and applied together with Enter, and the assignee picker offers you first, then nobody, then everyone else.
The change shows on the row at once and is sent behind it; if Linear refuses it, the row goes back to what it was and the bar says why.
`x` marks the issue under the cursor and moves down, and `X` marks every issue shown, or unmarks them all.
While issues are marked, `s`, `a` and `P` set the status, assignee or project of every one of them at once, sent to Linear as one batch; any it refuses go back as they were, the marks clear once it is sent, and Esc clears them without changing anything.
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
A step that fails undoes the ones before it, as any start does; marking the ticket started is the exception, and a failure there only shows in the info tab, since the agent is already working.
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
Each workspace has its own assistant; a stopped one is only ever taken up again in the pool it works in.

## On your phone

`treetop serve` puts treetop on your phone as an app on its home screen: every tree in the list's groups and order, with its agent's mark, what the agent last said, its git state, ports and PR, the trees waiting on you first.
It is a web app served by treetop itself, with nothing to install on the phone but Tailscale.
The Issues tab beside the trees is the Issues screen for your thumb: the same views and sources, who is working on each issue, status, assignee, labels, comments and new issues, and Pick up to put an agent on one.

On the server, name your Tailscale login in the config, since the server answers no one else:

```toml
[mobile]
owner = "you@example.com"   # `tailscale status --json` shows it as LoginName
port = 7777                 # the default
```

Then install it as a service, which starts it now, at every boot and again after a crash:

```sh
treetop serve --install
```

It runs in the workspace treetop would open where you run this, with the `PATH` you ran it with, since systemd's own lacks `~/.local/bin` and the commands the server starts.
A service has none of your shell's variables, so a key an issue source names in `key_env` goes in `~/.config/treetop/env` instead, as `NAME=value` lines, and treetop reads it wherever a variable is not set:

```sh
install -m 600 /dev/null ~/.config/treetop/env
echo 'LINEAR_API_KEY_WORK=lin_api_...' >> ~/.config/treetop/env
```

treetop refuses the file while anyone else can read it.
It says whether lingering is on, which the service needs to run at boot and while you are logged out; `loginctl enable-linger` turns it on.
It then shares the port with your tailnet over HTTPS, which iOS needs before it installs a web app, with the `share` the config names, `tether share` by default, and prints the address and a QR code to scan.
Where sharing fails, it says why and the server keeps running on 127.0.0.1.

The service is the systemd user unit `~/.config/systemd/user/treetop-serve.service`, which runs this binary; a unit of that name you wrote yourself is left alone, and the install stops and says so.
After installing a new treetop, `systemctl --user restart treetop-serve` picks it up without stopping your agents.
`treetop serve --uninstall` stops and removes it, and leaves the port shared until `tether unshare <port>`.
`--port N` picks another port for the service.

To run it in a terminal instead, `treetop serve` shares itself the same way and stops when you stop it.
Without tether it says sharing failed and serves on 127.0.0.1, and this shares it instead:

```sh
tailscale serve --bg --https 7777 http://127.0.0.1:7777
```

`treetop serve` listens on 127.0.0.1 only and shows the pool treetop would open where it is started, with every workspace a tap away.
It answers a request only when `tailscale serve` names the owner as the one asking, in `Tailscale-User-Login`; anyone else on the tailnet gets nothing.
A process on the server itself can reach the port and claim to be anyone, but those already run as you.

On the iPhone:

1. Install Tailscale, sign in to the same tailnet, and turn it on.
2. Scan the QR code `treetop serve --install` printed with the camera, which opens Safari, or open its address there, such as `https://server.your-tailnet.ts.net:7777`.
3. Tap Share, then Add to Home Screen, then Add.
4. Open treetop from its icon. It runs full screen, and a new treetop on the server reaches it the next time it opens.

Tap a tree for what the terminal's info tab says about it: its agent and what it asks or last said, its git state, the ports it serves and what serves them, its PR with its checks and a link to it, and its agent's timeline, each status line it wrote once.
Back, the button or the phone's own gesture, lands on the pool where you left it.

A port is a button wherever it shows: on a tree's card, beside the main checkout, on the tree's screen and at the top of its chat.
A tap shares it as `p` does, with the `share` the config names, and opens it; where the phone holds that window back, a link to it stays at the foot of the screen to tap instead, as does why, if it could not be shared.
Only a port one of the pool's trees is serving can be shared this way, and it stays shared until `tether unshare <port>`.

On the tree's screen, `Public link` beside a port puts it on the internet as `P` does, with the `publish` the config names, for sending to someone off the tailnet.
The link is copied as it arrives, and the foot of the screen shows it with `Share`, which opens the phone's share sheet to text or post it, and `Stop`, which takes it down with the `unpublish` the config names.
A public port's button reads `Public`, in green, and a tap opens the share sheet with its link straight away.
Only a port one of the pool's trees is serving can go public, as with sharing.

Swipe a card left and let go to park the tree, as `z` does, or swipe a parked card to bring it back; a short swipe springs back.
Park and Unpark are also at the top of the tree's screen, with Backburner and Off the backburner beside them; the pool shows the Backburner's trees under their own heading, after the bases'.

Open chat, on a tree whose agent is a background Claude Code session, shows its conversation as it happens, read from the session's transcript on the server: what you sent, what Claude wrote, and each tool call folded to one line, such as `Edited src/guard.ts +12 −4` or `Ran the tests ✗`, that opens to what it printed.
Scrolled to the end it follows new messages; scrolled up, it stays put and a pill says how many came in.
While the agent works, dots at the foot of the chat say what it is doing, such as `Run the tests` or `Editing src/guard.ts`; a message that comes in eases into place, and a new reply from Claude comes up word by word, pausing a little at commas, full stops and the ends of paragraphs, with the chat gliding after it.
Claude Code writes a reply down only once it is finished, so this paces the finished reply rather than following it as it is made, for four seconds at most; what the agent did next waits for the reply before it, so a burst reads in the order it happened, and asking the phone for less motion shows each at once.
A line typed at the bottom goes to the agent as `treetop send` sends it, typed into its chat, or starting a stopped agent again with it; it shows at once as sending, and as failed with why and a retry if it could not go.
What you were typing is kept when you leave the chat.
While the agent waits on you, what it waits on sits above the box, and you answer it there.
A question with options shows them as buttons, several questions at once each under its header, and Answer sends your picks; a command it wants to run, or a file it wants to change, shows with Allow and Deny.
Typing in the box answers in your own words instead.
Allow and Deny press Enter and Esc in the agent's session, and only once its screen shows the prompt has the keys; an option, or your own words, put the prompt away with Esc and go in as a message, so an answer holds however Claude Code lays its options out.
Each answer attaches to the session only for that moment, so the chat popup on your desktop keeps its size.

New agent, at the foot of the pool, starts an agent on a task in a fresh tree cut from the base you pick, as `N` does, and opens its tree once it is going; New agent here, on a tree, starts one in that tree on its branch, as `n` does.
On a tree whose agent has stopped or died, Resume starts it again with its conversation, as `R` does.
Return… gives the tree back to the pool, as `r` does, after a sheet that names its branch and what it holds that is not pushed or committed, and warns of a keel run that holds it; its agents are stopped first.
Anything else running in the tree, such as a dev server, keeps it from the phone, and the refusal says what, since stopping it is a call to make at treetop with the return dialog's list of what would stop.
The pool refreshes every five seconds while the app is open, and when you pull down.

The bell in the pool's header turns on notifications, which iOS offers only to the app opened from the home screen.
Your phone then buzzes when an agent comes to need you, finishes or dies, and when a tree's PR fails its checks or merges, each once; tapping one opens that tree's chat, or the tree for a PR, in the tree's own workspace, whichever one the phone was showing.
A notification names the tree and what happened, such as `17 · END-123 fix-login` and `Needs you`, never what the agent wrote, and goes encrypted for your phone through Apple's push service, the one thing the server sends off your tailnet.
The server keeps its push key and the phones that turned notifications on in `push/` beside the config, readable by you alone; a phone that turns them off in Settings is dropped the next time a push finds it gone.

Inbox, in the tab bar, keeps what the notifications said in one place after you swipe them away.
It lists the same entries from every workspace, in the same order under the same headings, and its tab counts what you have not opened yet.
A tap opens an entry as its notification would, the agent's chat or the tree for a PR or an agent that died, switching to its workspace first; swiping one left dismisses it.
What you open or dismiss is kept in `inbox.json` under `$XDG_STATE_HOME/treetop` (else `~/.local/state/treetop`), with each ask whose chat the desktop opened.
With the server out of reach while it is open, it shows the pool as it last saw it and says so.
It keeps no copy of itself on the phone, so every open loads the treetop the server runs now; with the server out of reach, it does not open at all.
If it has not started 8 seconds after opening, it clears anything an older treetop left on the phone and tries once more, and then says it did not start rather than showing a blank screen.

### Working on the app

The app is Svelte and TypeScript in `web/`; `npm run build` there builds it into `web/dist`, which the next `cargo build` embeds.
A treetop built without it serves a page saying so.
`npm run dev` serves it on port 5173 with live reload, passing `/api` to a `treetop serve` on 7777, so with `TREETOP_SHARED=1 npm run dev` and `tether share 5173` each save shows on the phone.
`npm run check` type-checks it, and `npm run e2e` runs its journeys in a browser at an iPhone's size against the built app, which on a server without a browser's libraries runs in Playwright's image as `web/playwright.config.ts` shows.

## Entering a tree

`t` goes to the tree under the cursor, and Enter does too on a tree whose agent has no chat to open here, or no agent at all.

Inside tmux, a tree with an agent switches to the pane that agent runs in, in whichever session it is, and never opens a new one.
The pane is the one whose process is the agent, its parent or its grandparent, so a pane an agent once used and something else uses now is never mistaken for it.
`prefix` `l` comes back to treetop.

Otherwise it opens the tree in a new window named for its branch (the part after the last `/`), or `tree N` when it has none, and treetop stays open in its own window.
When a window of the session already has a pane in that tree, it switches to it instead of opening another.

Outside tmux, treetop suspends and opens `$SHELL` in the tree; `exit` comes back.
The list does not refresh while that shell is open.
