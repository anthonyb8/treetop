# treetop

An htop-style view of a [treehouse](https://github.com/kunchenguid/treehouse) worktree pool.
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
| `/` | filter by number, branch or holder; Enter keeps it, Esc clears it |
| `r` | return the marked trees, or the one under the cursor, to the pool |
| `D` | destroy them from disk, after reviewing treehouse's preview of each |
| `Tab` | open the side-by-side diff of the tree under the cursor |
| `L` | show or hide the log of every action's output; PgUp and PgDn scroll it |
| Enter | open the tree: a tmux window inside tmux, else a shell |
| `Ctrl-R` | list the pool again now |
| `q`, Esc | quit |

The columns are what a destroy would lose.
CHANGED counts uncommitted files and UNPUSHED counts commits on no remote; `?` means git could not read a held tree, which deserves a look before it goes.
PROCS counts processes treehouse found running inside the tree.

treetop refreshes on two clocks, the way htop stays fast.
Processes come straight from `/proc` every 2 seconds and git counts every 4, which together cost under 5% of one core.
The pool itself comes from `treehouse status`, which takes seconds of CPU, so it is listed only when `git worktree list` changes (a tree leased, returned or destroyed), after every action, on `Ctrl-R`, and every 2 minutes as a backstop.
The summary line says how old that listing is.

On opening, treetop draws the last listing it saved under `$XDG_CACHE_HOME/treetop` (else `~/.cache/treetop`) at once, in italics and marked "cached listing", until a live one lands.

## The diff

`Tab` opens the tree under the cursor in a full-screen, side-by-side diff, the way a pull request shows it: everything its branch changes against the pool's base (`base_branch` in `treehouse.toml`, else `origin/HEAD`), measured from their merge base, plus uncommitted and untracked files.
Removed lines sit on the left and the added lines that replace them on the right, row by row, with line numbers on both sides; within a changed line, the words that differ are highlighted.
Every file runs in one scroll, each under a header with its path and its additions and deletions, and the header of the file you are reading stays pinned to the top.
Colours come only from the terminal's 16 ANSI colours, as in Claude Code's dark-ansi theme, so the diff follows the terminal's palette.

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

## What the actions run

Both run in the background, one tree at a time, while treetop stays on screen; each row's STATUS shows queued, returning, destroying, returned, destroyed or failed.
Their output goes to the log (`L`), and a failure also shows its last line in the status line.

- **Return** first shows a dialog listing each tree's uncommitted files, unpushed commits and running processes, or "unknown changes" where git cannot read it. On `y` it stops the tree's stack with `harness stack down` when the harness is on PATH, then runs `treehouse return --force`: that dialog replaces treehouse's own prompt before discarding changes.
- **Destroy** runs `treehouse destroy`'s dry run with every `--include-*` flag in the background and shows it in a dialog per tree. `y` stops the stack and runs it with `--yes`; `n` keeps the tree.

`q` while an action runs waits for it to finish; `Ctrl-C` quits at once and leaves the running command to finish on its own.

The tree treetop was started in is never a target, because returning or destroying it would kill the shell standing in it.

## Entering a tree

Inside tmux, Enter opens the tree in a new window named for its branch (the part after the last `/`), or `tree N` when it has none, and treetop stays open in its own window.
When a window of the session already has a pane in that tree, Enter switches to it instead of opening another.

Outside tmux, Enter suspends treetop and opens `$SHELL` in the tree; `exit` comes back.
The list does not refresh while that shell is open.
