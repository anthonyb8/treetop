# Issues

A project in eight issues: treetop's Issues screen, a Rust issues UI purpose-built for working with treetop, replacing the Go linear-tui fork.
It is one program with two screens, Trees and Issues: Tab switches between them, and `treetop issues` opens straight onto Issues.
Every issue shows the tree and agent working on it, and Enter puts an agent on it.
Each issue is one PR into `main` and ships on its own as a tagged release.

## Goal

Linear work happens in one place, next to the agents doing it.
Today it is split: the linear-tui fork is Go, which is hard to change, and it knows nothing about trees or agents, while treetop's tickets tab only lists and picks up.
The Issues screen keeps what the fork does well (issues on top, the description and comments below) and adds the edits used every day: status, labels, assignee, creating, editing and commenting.
It also answers "is anyone working on this?" on every row.
It is for one developer working across a work and a personal Linear workspace.

## Success

- The linear-tui fork is no longer opened: everything it was used for happens on the Issues screen.
- Status, labels and assignee change in two keystrokes, and the row shows the change at once.
- An issue is created or edited, and a comment posted, without leaving the terminal.
- Mine, all, by project, by status, by label and by cycle are each one key away.
- Every issue with a tree shows its tree number and its agent's state, read live from treetop's own state.
- Moving the cursor never waits on the network: the description and comments of the issue under it appear from cache or load behind.

## Scope

**In:** a Cargo workspace; a Linear API crate; the Issues screen with the list on top and the description and comments below; `treetop issues`; views and the filter line; status, labels and assignee pickers; adding comments; creating and editing issues; switching between sources; a WORK column and pick-up on Enter.

**Out:**

- Editing in ticket systems other than Linear: another ticket plugin's issues show on the Issues screen through the plugin contract, to read, filter and pick up, but not to edit. Editing there would be its own project.
- A separate binary: the screen is part of `treetop`.
- Boards, roadmaps, projects and cycles as screens of their own: they are filters on the issue list here.
- Notifications, inbox and real-time updates from webhooks: the list refreshes on focus and on a timer.
- Attachments, images and sub-issue trees: the description links to them.
- Editing comments after they are posted, reactions and mentions beyond plain `@name` text.

## Design

**Workspace.**
Three crates in one repository, one binary:

| Crate | Kind | Holds |
|---|---|---|
| `linear` | library | the GraphQL client, its types, and a cache of each team's states, labels, members, projects and cycles |
| `issues` | library | the Issues screen: its state, keys and drawing, over a `Host` trait for what treetop knows |
| `treetop` | binary | the Trees screen, which shows Issues on Tab and implements `Host` |

`linear` knows nothing about screens, and `issues` nothing about trees: the boundary is the `Host` trait.

```rust
pub trait Host {
    /// Who is working on the issue here: a tree and its agent's state.
    fn work_on(&self, issue: &str) -> Option<Work>;
    /// Enter on an issue: what treetop does with it.
    fn pick(&mut self, issue: &Issue) -> Option<String>;
}
```

treetop implements it from its live trees and sidecars, and Enter runs the existing pick-up transaction.
`treetop issues` run outside a pooled repository has no trees, and Enter opens the issue in the browser instead.

**Layout.**
The issue list fills the top half, one row per issue: id, status, priority, title, labels, assignee, and WORK ("4 · waiting").
The bottom half shows the issue under the cursor: its title, its fields on one line, the description rendered from Markdown (headings, lists, code, links), then its comments, oldest first, each with author and age.
`→` moves the focus to the bottom half to scroll it; `←` comes back.

**Views.**
A view is a saved filter line, and the filter line from treetop's tickets tab carries over (`mine label:Bug state:"In Review" project:x cycle:current words`).
Number keys switch between the built-in views:

| Key | View |
|---|---|
| `1` | mine: open, assigned to you |
| `2` | all open in the team |
| `3` | by project: grouped under project headings |
| `4` | by status: grouped under status headings, in workflow order |
| `5` | current cycle |

`/` edits the filter line of the view on screen; views of your own go in the config as named filter lines.

**Edits.**
Each edit is a picker or a compose box, applied at once on screen and sent behind; a failure puts the old value back and says why in the status line.

| Key | Does |
|---|---|
| `s` | status: a picker of the team's states, in workflow order |
| `l` | labels: a picker with a mark per label, Enter applies the set |
| `a` | assignee: a picker of team members, `me` first |
| `c` | comment: a compose box, Markdown, Alt+Enter for new lines |
| `n` | new issue: title, then status, labels, assignee and project as pickers, then the description in `$EDITOR` |
| `e` | edit: the title inline, the description in `$EDITOR` |
| `o` | open in the browser |
| `w` | switch source: work and personal |
| Enter | pick it up: an agent on it in a new tree |

Pickers filter as you type, like treetop's workspace picker.
A description is written in `$EDITOR` because it is long Markdown; a title and a comment are written in place.

**Data.**
`linear` loads a team's states, labels, members, projects and cycles once per session and on `r`, and issues per view.
An issue's description and comments load when the cursor first rests on it and are cached by its `updatedAt`, so moving back is instant.
Requests go over HTTPS from the process itself, not one `curl` per call, because a picker or a cursor move cannot wait on a process start.

**Sources.**
The Issues screen reads the same sources as the rest of treetop's config and switches between them with `w`, so work and personal are one key apart.
It opens on the source of the repository it runs in, else the first.

- **Schema:** N/A - no database; issue detail is cached in memory only.
- **Data:** Linear's GraphQL API, `https://api.linear.app/graphql`, with each source's personal API key.
- **Edge / vendors / env:** each source's `key_env`; `$EDITOR` for descriptions.

## Breakdown

Issues are created from `harness template issue` once this spec is agreed, so the Issue column holds titles until then.

| # | Issue | Delivers | Depends on |
|---|---|---|---|
| 1 | Cargo workspace | treetop split into the `linear`, `issues` and `treetop` crates, Linear called in process, sources renamed `[issues.<name>]`; nothing else changes | None |
| 2 | Linear client | issues with description and comments, team metadata cached, mutations for state, labels, assignee, comments, create and update | 1 |
| 3 | Issues screen: list and detail | Tab and `treetop issues` open the new screen: list on top, rendered description and comments below, source switching; the tickets tab goes | 2 |
| 4 | Views and filters | the five built-in views, grouping by project and status, the filter line, views from config | 3 |
| 5 | Status, labels and assignee | the three pickers with instant apply and rollback on failure | 3 |
| 6 | Comments | posting a comment from a compose box | 3 |
| 7 | Create and edit issues | `n` and `e`, with `$EDITOR` for descriptions | 5 |
| 8 | Work and pick-up | the WORK column from treetop's trees and agents, Enter picks up through `Host`; `src/linear.rs` goes | 3 |

Issues 4 to 8 depend only on 3 and can land in any order after it, except 7, whose pickers come from 5.

## Rollout

Each issue merges to `main`, then gets a version bump and a `v*` tag, which publishes the release through `.github/workflows/release.yml`.
The linear-tui fork stays usable until issue 7 ships, then is retired.
Rollback is reinstalling the previous version with `TREETOP_VERSION=<previous tag> sh`; no data migrates, since nothing is stored but the config.

## Risks

- **An edit applied on screen fails at Linear.** Guard: the old value comes back on the row and the status line says why; nothing is applied twice.
- **Linear's rate limit.** Guard: team metadata is loaded once per session, detail is cached by `updatedAt`, and the list refreshes on a timer of a minute, not on every key.
- **The workspace split breaks the release.** Guard: issue 1 changes no behaviour and its acceptance includes a tagged release that installs and runs.
- **`$EDITOR` and the TUI fight over the terminal.** Guard: treetop leaves its screen before the editor runs and comes back after, as it does for a shell.

## Open decisions

None. Settled before building:

- **HTTP:** Linear is called in process with `ureq` and `rustls`, not one `curl` per request, so pickers and cursor moves never wait on a process start and nothing depends on a system `curl`.
- **Writing descriptions:** in `$EDITOR`; titles and comments are written in place.
- **The config's name for sources:** `[issues.<name>]`, renamed from the revamp's `[tickets.<name>]` while it is unreleased, so no config has to migrate.
