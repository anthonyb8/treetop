# Responsive treetop

A project in three issues: live refresh at htop speed, return and destroy without leaving the TUI, and a diff pane.
Each issue is one PR into `main` and ships on its own as a tagged release.

## Goal

treetop should feel like htop: the pool on screen the instant it opens, numbers that move within a couple of seconds of reality, and every action done from inside the TUI.
Today it waits seconds for a first paint, lags ten seconds or more behind the machine, and hands the terminal over to treehouse for every return or destroy.
It is for whoever manages a treehouse pool from a terminal, which today is one developer with 16 trees.

## Success

- treetop paints the pool in under 100 ms of opening, from its last listing, and live data replaces it without a flicker.
- A process starting inside a tree shows up in its row within 2 seconds, and a file changing within 4.
- Returning and destroying trees never leaves the TUI; the cursor, filter and marks survive the whole action.
- Deciding whether a tree is safe to destroy no longer needs a second terminal to run git.

## Scope

**In:** a split refresh (fast `/proc` and git reads, slow `treehouse status`), a startup cache, in-TUI return and destroy run in the background, and a pane showing a tree's uncommitted files and unpushed commits.

**Out:**

- Stack controls (`harness stack up` and `down` as user actions): specific to one setup, not to treehouse pools.
  Return and destroy keep stopping a stack when `harness` is on PATH, as they do now.
- Enter outside tmux keeps opening a shell, because a shell needs the terminal; inside tmux it already stays in the TUI.
- A line-by-line diff viewer: the diff pane shows files and commits, and Enter opens the tree for anything deeper.
- Changing treehouse itself, including reporting the slow `status` upstream: the split makes it invisible to treetop, so it is left alone.

## Design

**Why htop is fast and treetop is not.**
htop re-reads `/proc` every refresh: hundreds of tiny reads of state the kernel already holds in memory.
treetop instead runs `treehouse status --json` every refresh, which measured 2.5 to 4.9 s per call on this machine, about 1.7 s of it in the kernel.
The same question treetop asks of it, "which processes are inside a tree", takes 6 ms read straight from `/proc/<pid>/cwd` across 827 processes, and 12.5 ms more to include open file descriptors.
Git counts cost about 40 ms per held tree.
What treehouse spends its time on is unconfirmed: neither `strace` nor `perf` was available.

**The split.**
Pool state (which trees exist, their status, branch, holder and lease) only changes when something leases or returns a tree, so it can come slowly.
Processes and git state change constantly, and both are cheap to read directly, so they come fast.

- **Slow listing:** `treehouse status --json`, whenever `git worktree list --porcelain` changes (a lease onto a branch, a return or a destroy, read in 3 ms every fast tick), at once after any action, on the refresh key, and every 2 minutes as a backstop. Listing every 30 s instead would cost about 10% of a core on its own. It stays the only source of pool state, as `src/pool.rs` already insists.
- **Fast tick:** every 2 s, one pass over `/proc` that assigns each process to the tree its cwd is under, which matches treehouse's own count process for process. Git counts for held trees run in parallel on every second tick, because git child processes are most of treetop's idle CPU. Git runs with `--no-optional-locks` so a status check never takes the index lock from whoever is working in the tree.
- **Cache:** the last merged listing written to `$XDG_CACHE_HOME/treetop/<hash of the main checkout path>.json`, read at startup and drawn marked as stale until the first slow listing lands.

**Background jobs.**
One worker thread runs actions from a queue, one tree at a time, so two git operations never contend for the same repository's locks.
Each job reports its state (queued, running, done, failed) and its captured output back to the UI over a channel; the UI never blocks on a child process.

- **Schema:** N/A - treetop has no database.
- **Data:** N/A - no backend; the interfaces are `treehouse` and `git` on PATH, and `/proc`.
- **Edge / vendors / env:** reads `XDG_CACHE_HOME` (default `~/.cache`); nothing else new.

## Breakdown

| # | Issue | Delivers | Depends on |
|---|---|---|---|
| 1 | Refresh at htop speed | instant first paint, 2 s process and 4 s git updates, a refresh key and freshness indicator | None |
| 2 | Return and destroy inside the TUI | background actions with in-TUI confirms, previews, per-row progress and an output log | 1 |
| 3 | Diff pane | a tree's uncommitted files and unpushed commits, inside treetop | None |

Issue 2 depends on 1 because both rework the refresher in `src/main.rs`; 3 touches only the detail pane and can land in either order.

## Rollout

Each issue merges to `main`, then gets a version bump and a `v*` tag, which publishes the release through `.github/workflows/release.yml`.
After each release, reinstall with the `curl ... | sh` line and run the issue's acceptance checks on the real pool, in tmux and outside it.
Rollback is reinstalling the previous version: `TREETOP_VERSION=v0.1.2 sh` with the installer.

## Risks

- **Process counts disagree with treehouse.** treehouse may count a process differently, for example by open files rather than cwd. Guard: issue 1's acceptance compares both on a tree with a running stack.
- **`treehouse return --force` skips treehouse's own discard prompt.** treetop's confirm becomes the only guard against losing uncommitted work. Guard: issue 2's confirm names every tree's changed and unpushed counts, and treats `?` (git cannot read it) as unknown work rather than none.
- **Returning a tree kills processes in it, including shells in tmux windows opened from treetop.** Guard: the confirm shows each tree's process count.

## Open decisions

None. The refresh key is `Ctrl-R`, because `r` already returns and `R` would read as its loud twin. treehouse is left unchanged.

---

# Issue 1: Refresh at htop speed

## Why

treetop draws nothing for 2.5 to 4.9 s after opening, while `treehouse status` runs, and then shows data up to 10 s plus one listing old.
The measurements are in the project's Design section: `treehouse status` costs seconds, while the same process question asked of `/proc` costs 6 ms.

## Outcome

treetop shows the pool the moment it opens, and its processes follow the machine within 2 seconds and its git counts within 4.

## Repro

N/A - not a bug.

## Scope

**In:** the fast tick, the slow listing, the cache and merging the three; a freshness indicator in the summary line ("updated 3s ago", and a marker while a slow listing runs); the refresh key.

**Out:** anything about actions (issue 2); changing treehouse.

**Layers:**

- **UI:** summary line gains the freshness indicator; rows drawn from the cache are styled as stale until live data lands.
- **Data:** `pool::load` splits into a slow `status()` and a fast `probe(trees)` that fills processes and git work; the refresher runs both on their own clocks.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** reads `XDG_CACHE_HOME`.
- **Tests:** unit tests for assigning `/proc` entries to trees (a fixture list of pid and cwd pairs, including `/p/40` against `/p/4`), and for merging a slow listing with fast data without losing the cursor.
- **Risk:** process counts that disagree with treehouse (see the project's Risks); a corrupt or old-format cache must be ignored, never fatal.

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] Open treetop in a repository opened before: the pool appears at once, marked stale, and turns live within 5 s.
- [ ] Open it in a repository never opened before: "reading the pool..." shows until the first listing, as now.
- [ ] Start `npm run dev` in a tree: within 2 s its PROCS count rises, and it falls within 2 s of stopping it.
- [ ] Edit a file in a tree: within 4 s its CHANGED count rises.
- [ ] On a tree with a running stack, PROCS matches the count `treehouse status` reports for it.
- [ ] Lease a tree from another terminal with `th`: it appears within a few seconds, or at once on `Ctrl-R`.
- [ ] The summary line shows how old the data is, and shows a marker while a slow listing is running.
- [ ] Left idle for a minute, treetop uses under 5% of one core.

## Approach

- `src/pool.rs:104` `load` runs `treehouse status` and then git per tree in sequence (`src/pool.rs:117`, `:135`); split it there.
- `src/main.rs:34` `REFRESH` and `src/main.rs:53` `spawn_refresher` become two clocks; the existing `paused` flag (`src/main.rs:100`) pauses both.
- `src/ui.rs:48` `summary_line` and its "reading the pool..." line (`src/ui.rs:55`) take the freshness indicator.
- Read `/proc/<pid>/cwd` and `/proc/<pid>/fd/*` with `std::fs`; no new crate is needed, and a process that vanishes mid-read is skipped.

## Open decisions

None - the refresh key is `Ctrl-R`, settled in the project.

## Links

**Project:** Responsive treetop. **Blocked by:** None. **Blocks:** Issue 2.

---

# Issue 2: Return and destroy inside the TUI

## Why

`r` and `D` leave the TUI: treetop drops out of its screen, treehouse's output scrolls through the terminal, and a "Press Enter to go back" prompt ends it (`src/actions.rs:96`, `:103`; `src/main.rs:107`).
While that runs the list is gone, and returning several trees means reading through each one's output in turn.

## Outcome

Returning and destroying happen inside treetop: you confirm in a dialog, keep moving around while the work runs, and watch each row change as it finishes.

## Repro

N/A - not a bug.

## Scope

**In:**

- A background worker that runs actions from a queue, one tree at a time, with captured output.
- Return: a confirm dialog listing each tree with its changed, unpushed and process counts and saying uncommitted changes are discarded; on `y`, stop the stack when `harness` is on PATH, then `treehouse return <path> --force`.
- Destroy: the dry run runs in the background and its text shows in a scrollable dialog per tree; `y` runs it with `--yes`, anything else skips that tree.
- Each row's STATUS cell shows its job while one is queued, running, just done or failed: queued, returning, destroying, returned, destroyed, failed. A separate column would cost width on every row for a state most rows never have.
- A log pane, toggled with `L`, holding each command's captured output; a failure also shows in the status line.

**Out:** Enter outside tmux (still a shell); stack controls; running several trees' actions at once.

**Layers:**

- **UI:** the confirm dialog (`src/ui.rs:231`) grows counts and a scrollable preview; job states in the STATUS cell; a log pane.
- **Data:** `src/actions.rs` stops writing to the terminal and returns captured output; a job queue and its channel join the main loop.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** none.
- **Tests:** unit tests for the job queue's state transitions, for the confirm text marking `?` trees as unknown work, and that leaving and re-entering the dialog cannot run a job twice.
- **Risk:** `--force` removes treehouse's own prompt (see the project's Risks); a job still running when treetop quits must finish or be reported, not abandoned silently.

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] Mark two trees and press `r`: the dialog lists both with their changed, unpushed and process counts.
- [ ] Press `y`: the screen never leaves treetop, both rows show "returning" in turn, then turn available.
- [ ] While they run, move the cursor and type a filter: the UI responds at once.
- [ ] Press `D` on a tree: treehouse's dry run appears inside a dialog; `n` leaves the tree untouched.
- [ ] Press `D` again and `y`: the tree is removed and its row disappears.
- [ ] Make an action fail (for example a tree whose folder is gone): its row shows "failed", the status line says why, and `L` shows the command's output.
- [ ] Press `q` while a job runs: treetop says it is waiting for that job, then quits when it is done.

## Approach

- `src/actions.rs:49` `return_tree` and `:60` `destroy_tree` capture output instead of inheriting the terminal, and `:35` `ask` disappears in favour of the dialog.
- `src/main.rs:107` stops calling `leave_screen` for actions.
- `src/app.rs:155` already hands a confirmed `Pending` to the loop; it becomes a job pushed onto the queue.
- `treehouse return --force` is "Clean, reset, and return without prompting" per `treehouse return --help`; destroy's preview is its default dry-run output.

## Open decisions

None.

## Links

**Project:** Responsive treetop. **Blocked by:** Issue 1. **Blocks:** None.

---

# Issue 3: Diff pane

## Why

CHANGED and UNPUSHED say how much work a tree holds but not what it is, so deciding whether a tree can go means opening it and running git there.

## Outcome

One key shows, inside treetop, which files a tree has changed and which commits exist nowhere else.

## Repro

N/A - not a bug.

## Scope

**In:** `Tab` switches the detail pane between the tree's info and its diff, made of `git status --short`, `git diff --stat`, and `git log --oneline HEAD --not --remotes`; the pane grows to half the screen while showing the diff and scrolls with `PgUp` and `PgDn`; the diff loads in the background and follows the cursor.

**Out:** line-by-line diffs (Enter opens the tree for those); diffs of available trees, which treehouse keeps clean.

**Layers:**

- **UI:** the detail pane (`src/ui.rs`, `detail_pane`) gains a diff mode.
- **Data:** a `pool::diff(path)` that runs the three git commands; loaded off the UI thread, cached per tree until the next fast tick changes its counts.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** none.
- **Tests:** a unit test that the diff text is assembled from fixed git output, including an empty tree and an unreadable one.
- **Risk:** a tree with thousands of changed files; the pane caps its lines and says how many were left out.

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] Put the cursor on a tree with uncommitted changes and press `Tab`: its changed files and a diff stat appear within a second.
- [ ] Move to a tree with unpushed commits: their one-line summaries appear.
- [ ] Move to a clean tree: the pane says there is nothing to lose.
- [ ] Move to a tree git cannot read (`?`): the pane says so.
- [ ] Press `Tab` again: the detail pane returns to the tree's info.

## Approach

- The detail pane is `detail_pane` in `src/ui.rs`, fed by `App::current`; the diff is one more field on `App` keyed by tree path.
- The git commands match `src/pool.rs:135` `work`, which already counts the same changes.

## Open decisions

None.

## Links

**Project:** Responsive treetop. **Blocked by:** None. **Blocks:** None.
