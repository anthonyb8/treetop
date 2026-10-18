# After the agent

A project in five issues: what happens to a tree's work once its agent is done, from the pull request through CI and review to the merge, and from the merge back into the project the work came from.
Each issue is one PR into `main` and ships on its own as a tagged release.

## Goal

treetop follows work from the issue to the agent to the tree, then goes quiet at the point where the work is least finished.
Once an agent reports done, its row says so for a day and nothing more: not whether a PR exists, whether CI passed, whether a reviewer asked for changes, or whether it merged.
Finding out means a browser tab per PR, and acting on it means copying a failing check's log into the agent's chat by hand.
Merged trees then sit leased until someone notices, and the issues they unblocked wait until someone remembers to look.

This build carries treetop through to the merge and back: each tree shows where its PR stands, a failure goes back to its agent in one key, a merged tree is offered back to the pool, and the merge moves its project on.
It does so through a new plugin kind, so treetop learns no one's CI, gate or forge: GitHub through `gh` is built in as the default, and a validation pipeline such as no-mistakes plugs in the same way.
It is for whoever runs several agents over a treehouse pool, with any agent and any setup around it.

## Success

- A tree whose branch has a PR shows its number and stage within a minute of the PR changing, and within seconds of a push treetop sees.
- A failing check or a review asking for changes counts as needing you: the row sorts to the top, and the bell rings once.
- One key sends the agent what failed, with the log, without opening a browser.
- A merged tree says so, its return deletes its branch even after a squash merge, and nothing unmerged is ever deleted.
- When a merge leaves another issue with nothing blocking it, treetop says that issue is ready.
- With no `gh`, no GitHub remote or no network, treetop runs as it does today, with no error on screen.
- Left idle with twenty held trees, treetop still uses under 5% of one core, and stays far inside GitHub's API rate limit.

## Scope

**In:** a `ship` plugin kind and its contract; a built-in GitHub plugin over `gh`; a PR column and the PR in the detail pane; failures and review requests needing you; sending a failure to the tree's agent; merged trees in the return dialog and squash-safe branch deletion; ticket links and an optional `finish` in the ticket contract; ready issues after a merge; starting a run from treetop and a built-in no-mistakes plugin; the stage in `treetop status`.

**Out:**

- Merging, approving, closing or retargeting a PR, or changing its reviewers or labels: treetop reads a PR and never decides one.
- Pushing or opening a PR outside a ship plugin's `run`: treetop has no push of its own.
- Returning a merged tree without being asked: an agent may still be working in it, so the return stays a key and a dialog.
- Forges other than GitHub as built-ins: GitLab or Gitea is a ship plugin of its own, through the same contract.
- Dependency graphs, boards or a project screen: the merge only names the issues it made ready.
- Reading review comments line by line inside treetop: the agent gets them, and `O` opens the PR.

## Design

**The flow.**

1. An agent finishes and pushes, by itself, through a gate such as no-mistakes, or because `s` started a run.
2. The tree's PR column follows the work: `nm review ⏸` while a pipeline waits at a gate, `#12 ci …` while checks run, `#12 ✗ ci` or `#12 changes` when it needs you, `#12 approved`, `#12 merged`.
3. On a failure, `f` sends the agent what failed and the end of its log; the agent fixes it and pushes, and the column moves on.
4. On a merge, the row says `#12 merged`, the summary counts merged trees, and `r` returns it, deleting its branch.
5. If the tree had a ticket, the merge marks it done where the source is set to, and names any issue it blocked that is now ready, which shows `ready` in the Issues screen's WORK column for `p` to pick up.

**The `ship` plugin kind.**
A third kind beside `agent` and `tickets`, through the plugin contract unchanged: one process per call, JSON in and out, fields only ever added.

| Op | Args | Result |
|---|---|---|
| `status` | `dir` (the main checkout), `trees`: each `{ "dir", "branch" }` | `branches`: one entry per branch it has something to say about (below) |
| `logs` | `dir` (the tree), `branch` | `text`: what failed and the end of its output, ready to send to an agent |
| `run` | `dir` (the tree), `branch`, `intent` | `started`, once the run is under way; it must return within the 30 s limit, so a long run goes on in the background |
| `attach` | `dir` (the tree), `branch` | `argv`, the command that shows the run in a terminal |

A branch's entry:

```json
{ "branch": "anthony/fix-login", "stage": "ci_failed", "label": "#12 ✗ ci",
  "url": "https://github.com/o/r/pull/12", "head": "4f2a9c1…",
  "detail": ["test (ubuntu) failed", "lint passed"] }
```

`stage` is one of `running`, `gate`, `failed`, `draft`, `checks`, `ci_failed`, `changes`, `review`, `approved`, `merged`, `closed` or `idle`; any other reads as `running`.
`gate`, `failed`, `ci_failed` and `changes` need you; `failed` is a pipeline run that failed on the commit the tree is still on.
Each tree's own directory is in `trees` because a pipeline such as no-mistakes answers per working copy, while GitHub answers for the whole repository from `dir`.
`label` is the plugin's own few characters for the column, and `detail` its lines for the detail pane.
`head` is the commit the PR or run is on, which the merge check below relies on.
`idle` means it has nothing current to say, such as a pipeline whose run ended.

**Which plugins a repository uses.**
`[repos."<path>"]` gains `ship`, a list of ship plugins in order:

```toml
[repos."~/endo/app"]
issues = "work"
ship = ["no-mistakes", "github"]
```

For each branch the first plugin with a stage other than `idle` wins, so a pipeline's gate shows while it runs and the PR's review state once it ends.
A repository with no `ship` entry uses `["github"]`; `ship = []` turns the column off.

**The built-in GitHub plugin.**
`treetop plugin github`, over `gh`, so the user's own login and host carry over and no token is handled.
`status` is one call per repository, not per branch: `gh pr list --state all --limit 100 --json number,headRefName,headRefOid,state,isDraft,reviewDecision,statusCheckRollup,url,mergedAt`, matched to the branches it was asked about, the newest PR winning where a branch has several.
`logs` reads the failing run with `gh run view --log-failed` and the requested changes with `gh pr view --json reviews,comments`, and keeps the last 200 lines.
It answers `unavailable` with no `gh`, no login or no GitHub remote, which blanks the column.
It has no `run`.

**When treetop asks.**
Every 60 seconds, at once after a refs change inotify already reports (`src/watch.rs` sees a push land in `refs/remotes`), after `s` and after `f`, and only for held trees with a branch.
An `unavailable` answer backs off to five minutes.
At a call a minute, the GitHub plugin spends about 60 of the 5,000 GraphQL points an hour gives.

**Needing you.**
`App::needs_you` gains the stage: `gate`, `failed`, `ci_failed` and `changes` count, sort the row up and ring the bell once on the change into them, as an agent coming to wait does.
The summary counts them with the waiting agents, and counts merged trees on their own.

**Sending a failure back.**
`f` on a tree whose stage needs you asks its first plugin for `logs`, then sends the text to the tree's agents the way `m` does, prefixed with one line saying what it is, such as `CI failed on PR #12 (test (ubuntu)). Fix it and push:`.
A cold agent gets it through its plugin, which starts it again; a tree with no agent says so and sends nothing.

**Merged trees.**
A tree is merged when its stage is `merged` and the PR's `head` is the tree's branch tip.
The return dialog says `merged as #12` beside its counts.
`delete_if_merged` deletes the branch when git sees it in the base, as now, or when the tree is merged by this rule, which covers squash and rebase merges where `merge-base --is-ancestor` says no.
A branch with commits after the merged head is kept, and the log says why.
`/merged` filters the list to merged trees, to mark and return together.

**Back into the project.**
The ticket contract gains two optional parts, so other ticket plugins follow:

- `get` may give `links`: each `{ "id", "kind", "done" }`, `kind` being `blocks`, `blocked by` or `related`. The Linear plugin fills it from the relations the Issues screen already reads.
- `finish` takes `id` and marks the ticket done, in the team's first `completed` state. A source sets `finish = true` to have it called; it is off by default, because Linear's own GitHub integration often does this already, and calling it twice is harmless.

When a tree with a ticket turns merged, treetop, in the background: calls `finish` where the source says to; reads the ticket's `blocks` links; and for each, reads that issue and calls it ready when every `blocked by` link is done, it is not done itself, and no tree works on it.
Ready issues are named in the status line (`END-12 merged · END-124 ready`) and in `treetop status`, and show `ready` in the WORK column on the Issues screen, where `p` picks one up as it does any issue.

**Starting a run.**
`s` on a tree asks its first plugin with `run` to start one, after a dialog naming the branch, the plugin and the intent, since a run pushes and opens a PR.
The intent is the tree's ticket title and text, else the task it was dispatched with: `treetop.json` gains `task`, written at dispatch.
`s` on a tree whose run is under way opens the plugin's `attach` command in the chat popup, as Enter opens an agent, which is where a gate is answered.
A tree whose plugins have no `run` says so.

**The no-mistakes plugin.**
Built in as `treetop plugin no-mistakes`, used only where a repository's `ship` names it.
`status` runs `no-mistakes axi status` in each held tree that has a run, and maps its run and step state to a stage: an active step to `running`, a step waiting at an approval gate to `gate`, a finished run to `idle`, so the GitHub plugin takes over for the PR.
`run` starts `no-mistakes axi run --intent <intent>` in its own process group, detached, and returns once `axi status` shows the run; `attach` gives `no-mistakes attach`; `logs` gives `no-mistakes axi logs --step <failed step>`.
It works the same whether treetop, the agent or a `git push` started the run, because it only reads state.

**`treetop status`.**
Each held tree gains `ship`: its stage, label and URL, so the coordinator can answer "what is in review" in one read; and the reply gains `ready`, the issues the last merges made ready.

- **Schema:** N/A - treetop has no database. `treetop.json` gains an optional `task`; version stays 1, since fields are only added.
- **Data:** N/A - no backend; the new interfaces are `gh` and, where configured, `no-mistakes`, both called through plugins.
- **Edge / vendors / env:** `gh` on PATH and logged in for the GitHub plugin; `no-mistakes` on PATH where a repository names it.

## Breakdown

| # | Issue | Delivers | Depends on |
|---|---|---|---|
| 1 | Ship plugins and the PR column | the `ship` kind, the GitHub plugin's `status`, the PR column and detail lines, needing you and the bell, `O` to open the PR, `ship` in `treetop status` | None |
| 2 | Send a failure to the agent | `logs`, and `f` sending a failure with its log to the tree's agents | 1 |
| 3 | Merged trees go back | `merged` in the return dialog and the filter, squash-safe branch deletion | 1 |
| 4 | Back into the project | ticket `links` and `finish`, ready issues after a merge, `ready` in WORK | 3 |
| 5 | Ship from treetop, and no-mistakes | `run` and `attach`, `s`, `task` in `treetop.json`, the no-mistakes plugin | 2 |

Issues 2 and 3 can be built in parallel; 4 needs 3's merge rule to know when to act, and 5 needs 2's `logs` for its failures.

## Rollout

Each issue merges to `main`, then gets a version bump and a `v*` tag, which publishes the release through `.github/workflows/release.yml`.
After each release, reinstall with the `curl ... | sh` line and run the issue's acceptance checks on a real pool with real PRs.
Issue 1 turns the PR column on for every repository with a GitHub remote; `ship = []` turns it off without a rollback.
Rollback is reinstalling the previous version with `TREETOP_VERSION=<previous tag> sh`; `treetop.json` files with `task` are read by older versions, which ignore it.

## Risks

- **A branch is deleted with work on it.** Guard: the squash rule deletes only when the PR's merged head is the branch tip, so a commit made after the merge keeps the branch; tests cover a squash merge, a merge with a later commit, and a closed unmerged PR.
- **GitHub's rate limit or a slow `gh`.** Guard: one call per repository a minute, the plugin's 10 s timeout, and a five-minute backoff on `unavailable`; the column goes stale rather than blocking anything.
- **The PR column lies about which PR.** A branch name reused after an old PR closed matches both. Guard: the newest PR for the branch wins, and only an open or merged PR whose head is the branch tip counts as merged.
- **no-mistakes changes its agent interface.** `axi` output is TOON and the plugin parses it. Guard: the plugin lives in one module with fixtures copied from the current output, yields `idle` on anything it cannot read, and the GitHub plugin still shows the PR.
- **`finish` marks the wrong ticket done.** Guard: off by default, only called for the ticket in the merged tree's `treetop.json`, and only once the merge rule holds.
- **`f` sends a huge log into a chat.** Guard: the plugin keeps the last 200 lines, and treetop caps the message at 16 KB.

## Open decisions

- **Is the GitHub plugin on by default?** Settled: on, since it only reads and blanks itself when it cannot; `ship = []` turns it off.
- **Is no-mistakes built in or a plugin file?** Settled: built in, used only where `ship` names it.
- **Does `no-mistakes axi` offer JSON?** Settled: no; `axi status` has no format flag, so the plugin reads the TOON it prints, from fixtures copied from real output.
- **Does `f` need a confirmation?** Settled: no; it only types into the tree's own agent, as `m` does, and the status line says it was sent.

---

# Issue 1: Ship plugins and the PR column

## Why

Once an agent reports done, treetop says nothing more about its work.
Whether it has a PR, whether CI passed and whether review asked for changes is checked one browser tab at a time, so a red check or a review can sit for hours while the row looks finished.

## Outcome

Each held tree shows its PR and where it stands, a failing check or a request for changes puts the tree at the top and rings the bell, and `O` opens the PR.

## Repro

N/A - not a bug.

## Scope

**In:**

- `Kind::Ship` in the plugin contract, with `status` and its reply as the project's Design gives it; its timeout is 10 s.
- `ship` under `[repos]`, a list, defaulting to `["github"]`.
- The built-in GitHub plugin's `status` over `gh pr list`, served as `treetop plugin github`.
- A slow ship clock: every 60 s, at once on a refs change, five minutes after `unavailable`.
- A PR column between UNPUSHED and the end, showing `label`, coloured by stage; the PR's URL and `detail` lines in the detail pane.
- `needs_you` counting `gate`, `ci_failed` and `changes`, the bell on the change into one, and the summary counting merged trees.
- `O` opens the PR's URL, or copies it where there is no desktop, as `o` does a port.
- `ship` on each held tree in `treetop status`.
- The README's Plugins and Config sections.

**Out:** `logs` and `f` (issue 2); merged trees in the return (issue 3); `run` and `attach` (issue 5).

**Layers:**

- **UI:** the PR column, its colours, detail lines, the summary count, `O`, `?` help.
- **Data:** `Tree` gains `ship: Option<Ship>`, carried over in `replace_trees` as processes and agents are; a `Update::Ship` from a ship thread.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** `gh` on PATH.
- **Tests:** mapping `gh pr list` JSON to stages (open, draft, checks pending, a failed check, changes requested, approved, merged, closed); the newest PR winning for a reused branch; the first non-`idle` plugin winning; `needs_you` for each stage; the bell ringing once per change; the `ship` list parsed from config with its default.
- **Risk:** the rate limit (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] On a tree whose branch has an open PR with checks running, the PR column shows its number and `ci …` within a minute of opening treetop.
- [ ] Push a commit that fails CI: within a minute of the check failing, the row shows `✗ ci`, sorts to the top, and the bell rings once.
- [ ] Request changes on the PR: the row shows `changes` and needs you.
- [ ] Merge the PR: the row shows `merged` and the summary counts one merged tree.
- [ ] `O` opens the PR in the browser; on a tree with no PR the status line says there is none.
- [ ] `treetop status` shows the tree's `ship` stage, label and URL.
- [ ] With `gh` logged out, the column is blank and nothing on screen is an error; the log says why.
- [ ] `ship = []` for the repository hides the column.
- [ ] Left idle for ten minutes with twenty held trees, treetop uses under 5% of one core, and `gh api rate_limit` shows under 100 GraphQL points spent.

## Approach

- `src/plugin.rs:40` `Kind` gains `Ship`, and `:148` `timeout` gives `status` 10 s; `src/builtin.rs:21` `handle` serves the built-in by name, as it does `claude` and `linear`.
- `src/config.rs:78` `Repo` gains `ship`.
- `src/refresh.rs:29` holds the clocks; the ship read gets its own thread so a slow `gh` never delays the fast tick.
- `src/watch.rs:244` `watch_refs` already reports a refs change; the ship thread takes it as a reason to ask at once.
- `src/ui.rs:389` is the header, `:346` `row` and `:423` `detail_lines` the other places a column goes.
- `src/app.rs:919` `needs_you` and `:366` the sort; `:1463` is `o`, beside which `O` goes.
- `src/cli.rs:170` `status_json` gains the field.

## Open decisions

The project's first, on the GitHub plugin being on by default.

## Links

**Project:** After the agent. **Blocked by:** None. **Blocks:** Issues 2 and 3.

---

# Issue 2: Send a failure to the agent

## Why

A failing check or a review asking for changes is work for the agent that wrote the code, but getting it there means opening the PR, finding the failing log, copying its end, opening the agent's chat and pasting it.

## Outcome

`f` on a tree that needs you because of its PR sends its agent what failed and the end of the log, and the agent gets on with the fix.

## Repro

N/A - not a bug.

## Scope

**In:**

- `logs` in the ship contract, with the GitHub plugin's answer from `gh run view --log-failed` for failed checks and `gh pr view --json reviews,comments` for requested changes, the last 200 lines.
- `f` on a tree whose stage is `ci_failed`, `changes` or `gate`: reads `logs` in the background, then sends it to the tree's agents as `m` does, with its one-line prefix, capped at 16 KB.
- The status line saying it was sent, or why not: no agent, no chat to type into, or the plugin's error.
- Asking for the stage again after a send.

**Out:** retrying CI or re-requesting review; a confirmation dialog (see the project's Open decisions).

**Layers:**

- **UI:** `f`, its status line, `?` help.
- **Data:** none beyond the `logs` reply.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** `gh` on PATH.
- **Tests:** the prefix for each stage; the cap; a tree with no agent sending nothing; a cold agent getting it through its plugin; the GitHub plugin's `logs` on recorded `gh` output.
- **Risk:** a large log (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] On a tree showing `✗ ci` with a background agent, `f` types the failing check's name and log tail into the agent's chat, and the agent starts on it.
- [ ] On a tree showing `changes`, `f` sends the review's requested changes and comments.
- [ ] On a cold agent, `f` starts it again with the failure as its next turn.
- [ ] On a tree with no agent, `f` says so and sends nothing.
- [ ] On a tree whose PR is fine, `f` says there is nothing to send.

## Approach

- The send reuses `src/dispatch.rs` and the cold path in `src/jobs.rs:787` `send_cold`, so `f` is a message whose text came from a plugin.
- Keys on the Trees screen are at `src/app.rs:1440-1468`.

## Open decisions

The project's last, on a confirmation.

## Links

**Project:** After the agent. **Blocked by:** Issue 1. **Blocks:** Issue 5.

---

# Issue 3: Merged trees go back

## Why

A merged tree stays leased until someone notices, holding a slot and often a running stack.
When it is returned, its branch is deleted only if git sees it in the base, which a squash or rebase merge on GitHub never satisfies, so merged branches pile up.

## Outcome

A merged tree says so in its row and its return dialog, `/merged` lists them to return together, and their return deletes their branches even after a squash merge.

## Repro

1. Pick up an issue in a tree, push its branch and open a PR.
2. Squash-merge the PR on GitHub.
3. Return the tree with `r`.

Should: delete the branch, since its work is in the base.
Does: keeps it, and the log says `kept branch ...: not merged into origin/main`.

## Scope

**In:**

- The merge rule: stage `merged` and the PR's `head` equal to the branch tip.
- `merged as #12` in the return dialog's line for the tree.
- `delete_if_merged` deleting a branch that meets the rule, keeping one with commits after the merged head, and logging which.
- The filter matching `merged` against merged trees.

**Out:** returning without being asked (see the project's Scope); deleting the remote branch, which GitHub's own setting handles.

**Layers:**

- **UI:** the dialog line, the filter.
- **Data:** the return job gets the tree's `ship` with it.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** none.
- **Tests:** the bug's repro end to end on a temporary repository with a squash-merged branch and a recorded `gh` answer; a branch with a commit after the merge kept; a closed unmerged PR kept; a branch git already sees merged deleted as before.
- **Risk:** deleting work (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] Squash-merge a tree's PR, then `r`: the dialog says `merged as #N`, and after the return the local branch is gone.
- [ ] Commit once more in a merged tree, then `r`: the branch is kept and the log says it has work after the merge.
- [ ] `/merged` shows only the merged trees; Space marks them and `r` returns them together.
- [ ] A tree with a closed, unmerged PR is never called merged.

## Approach

- `src/jobs.rs:756` `delete_if_merged` and `:799` `give_back`, which calls it.
- `src/app.rs:1468` opens the return dialog; its lines are drawn in `src/ui.rs`.
- The filter is `visible` at `src/app.rs:347`, which already matches number, branch, holder and agent.

## Open decisions

None.

## Links

**Project:** After the agent. **Blocked by:** Issue 1. **Blocks:** Issue 4.

---

# Issue 4: Back into the project

## Why

A merge is the end of one issue and often the start of the next: the issues it blocked can now be picked up.
Today nothing says so, and the ticket may also need moving to done by hand where no integration does it.

## Outcome

When a tree with a ticket merges, its ticket is marked done where the source is set to, and the issues it unblocked are named and marked `ready` on the Issues screen, one `p` from an agent.

## Repro

N/A - not a bug.

## Scope

**In:**

- `links` in the ticket contract's `get`, filled by the Linear plugin from the relations its client already reads.
- `finish` in the ticket contract, the Linear plugin moving the issue to its team's first `completed` state; `finish = true` on a source to have it called.
- On a tree turning merged: `finish` where set, then the ready check from the project's Design, in the background.
- The status line naming the merge and the ready issues; `ready` in the WORK column; `ready` in `treetop status`.

**Out:** a dependency graph or project screen; picking up ready issues without being asked.

**Layers:**

- **UI:** the status line, the WORK column.
- **Data:** `Ticket` gains `links`; the app keeps the set of ready issue ids until each is picked up or no longer ready.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** none.
- **Tests:** the ready check on issues with no blockers, one blocker done, one of two done, and one already in a tree; `finish` called only when the source sets it; the Linear plugin's `links` from a recorded API reply.
- **Risk:** marking the wrong ticket done (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] With END-2 blocked by END-1 only, merge END-1's PR: within a minute the status line says END-2 is ready, and the Issues screen shows `ready` on END-2.
- [ ] `p` on END-2 picks it up as it does any issue, and `ready` gives way to its tree.
- [ ] With END-3 blocked by END-1 and END-2, merging END-1 does not call END-3 ready.
- [ ] With `finish = true`, END-1 moves to Done once its PR merges; without it, treetop leaves END-1's status alone.

## Approach

- `src/tickets.rs:19` `Ticket` and `:74-83` the calls, where `finish` joins `start`.
- `src/linear_plugin.rs:97` lists the plugin's ops; `crates/linear/src/model.rs:270` already turns relations into `blocks` and `blocked by`.
- `crates/issues/src/host.rs:18` `work_on` is where `ready` reaches the WORK column.

## Open decisions

None beyond the project's.

## Links

**Project:** After the agent. **Blocked by:** Issue 3. **Blocks:** None.

---

# Issue 5: Ship from treetop, and no-mistakes

## Why

Getting a finished tree to a PR means attaching to its agent and asking it to ship, or running a gate by hand in the tree.
A validation pipeline such as no-mistakes stops at gates that wait on a person, and nothing in treetop shows that one is waiting.

## Outcome

`s` starts a tree's ship run after a dialog, the PR column follows the pipeline through its steps and gates, and `s` again opens the run to answer a gate.

## Repro

N/A - not a bug.

## Scope

**In:**

- `run` and `attach` in the ship contract.
- `s`: on a tree with no run under way, a dialog naming the branch, plugin and intent, then `run`; with one under way, `attach` in the chat popup.
- `task` in `treetop.json`, written at dispatch, for the intent of a tree with no ticket.
- The built-in no-mistakes plugin, as the project's Design gives it, used only where `ship` names it.

**Out:** answering gates from treetop's own keys rather than the pipeline's own screen; a `run` for the GitHub plugin.

**Layers:**

- **UI:** `s`, its dialog, the popup running `attach`.
- **Data:** `Sidecar` gains `task`.
- **Schema:** N/A - no database.
- **Edge / vendors / env:** `no-mistakes` on PATH where named.
- **Tests:** the intent from a ticket, from a task and from neither; the no-mistakes plugin mapping recorded `axi status` output for a running step, a gate, a passed and a failed run to stages; `run` returning once the run shows and leaving no child holding its stdout.
- **Risk:** the `axi` format (see the project's Risks).

**Roles:** N/A - single-user CLI.

## Acceptance

- [ ] In a repository with `ship = ["no-mistakes", "github"]`, `s` on a finished tree shows the dialog with the ticket's title as the intent; `y` starts a run and the column shows its steps.
- [ ] When the run stops at a gate, the row shows the gate, needs you and rings once; `s` opens `no-mistakes attach` in the popup, and Ctrl+Q comes back.
- [ ] Once the run opens the PR and ends, the column shows the PR from the GitHub plugin.
- [ ] A run started by the agent itself shows the same way.
- [ ] `s` on a tree in a repository with no `run` plugin says so.

## Approach

- `src/sidecar.rs:28` `Sidecar` gains `task`; the start job writes it in `src/jobs.rs:437` `start_steps`.
- The popup runs any `argv` already, as Enter does an agent's `attach`, in `src/chat.rs`.
- The plugin lives in its own module beside `src/linear_plugin.rs`, served from `src/builtin.rs`.

## Open decisions

The project's two on no-mistakes: built in or a file, and JSON or TOON.

## Links

**Project:** After the agent. **Blocked by:** Issue 2. **Blocks:** None.
