# Mobile

A project in six issues: `treetop serve` puts treetop on your phone as an installable web app, with the pool, each tree, and each agent's chat, so agents can be checked on and steered away from the server without a terminal.
Everything goes through treetop's own server on your tailnet; nothing depends on the Claude app or Claude Code's Remote Control.
Each issue is one PR into `main` and ships on its own as a tagged release.

## Goal

Checking on agents, talking to them and steering them works from an iPhone, away from the server and without a terminal.
Today everything treetop does needs an SSH session and a full-width terminal, so an agent that blocks on a question while you are away waits until you are back at a keyboard.
Claude Code's Remote Control can show a session in the Claude app, but it routes the conversation through Anthropic's relay, needs an organization toggle, lists sessions with no sense of trees, branches or PRs, and splits one job across two apps.
This project keeps it in one place: treetop's view of the pool and each agent's conversation, side by side, served from the machine the agents run on.
It is for one developer, on their own server, reached over their own tailnet.

## Success

- From the phone's home screen, one tap shows every held tree with its agent's state, branch, git state and PR checks, no older than a few seconds.
- From a tree, one tap shows its agent's conversation as it happens, and a line typed there reaches the agent.
- An agent blocked on a permission prompt or a question can be answered from the phone.
- The phone buzzes when an agent needs you, finishes or dies, and when a PR's checks fail or it merges.
- Starting an agent in a fresh tree, resuming a dead one and returning a finished tree each take no more than three taps.
- Nothing on the server is reachable from outside the tailnet, and nothing leaves it except push notifications and the calls to the issue sources in the config, which the Issues screen makes from the server.

## Scope

**In:**

- `treetop serve`, a `tiny_http` server inside the `treetop` binary, serving a JSON API, live streams, and the built web app embedded in the binary.
- The web app, in Svelte and TypeScript under `web/`.
- An installable web app (a PWA) with the pool, a tree, its agent's chat and a new agent, and from ANB-27 the Issues screen beside the pool.
- A chat view of each agent's conversation, read from its session transcript, with a reply box.
- Answering what the agent waits on in the chat: a question's options as buttons, a permission with Allow and Deny, or your own words (ANB-21, which replaced ANB-18's terminal view).
- The actions `send`, `dispatch`, `resume` and `return`, parking a tree as `z` does, sharing and opening a port as `p` does, and switching workspaces, as `w` does.
- Web push: an agent needing you, finishing or dying; a PR's checks failing or merging.

**Out:**

- Claude Code's Remote Control and the Claude app. treetop starts agents as it does today, without `--remote-control`.
- Destroying a tree, renaming a branch, ship runs, handoffs and the diff. They stay in the terminal, where their dialogs show what is at stake; any of them can be a later issue.
- The assistant's pane.
- A native iOS app or an App Store listing. The open decisions say why.
- Reaching the server from outside the tailnet, or anyone but its owner.

## Design

### How the pieces talk

```
iPhone                              server (tailnet: "server")
┌───────────────┐   HTTPS, tailnet  ┌──────────────────┐   127.0.0.1   ┌───────────────────────┐
│ treetop PWA   │ ────────────────► │ tether share     │ ────────────► │ treetop serve         │
│ (home screen) │ ◄──── push ────┐  │ cert + identity  │               │ pool, actions, chats  │
└───────────────┘                │  └──────────────────┘               └───┬───────────┬───────┘
                                 │                                         │           │ transcript,
                       Apple push service ◄─────── Web Push ───────────────┘           │ claude attach
                                                                                       ▼
                                                                          ┌───────────────────────┐
                                                                          │ claude --bg (agents)  │
                                                                          └───────────────────────┘
```

The web app talks only to `treetop serve`, over the tailnet.
The server reads the agents the way the TUI does: their transcripts and job files on disk, and `claude attach` in a terminal treetop owns.
The only traffic leaving the server is push notifications, which carry a line of text, such as "17 needs you", never the conversation.

### What you see

Four screens, sized for a phone held upright; the mockups are in the "treetop mobile" canvas.

**Pool**, the home screen: the trees in treetop's groups and order, each a card of two lines, trees waiting on you first, then the groups, then the free count and a New agent button.
The marks are the TUI's: `●` busy, bold when it needs you, `○` idle, `◌` cold, `✓` done, `✗` dead.
The page refreshes every five seconds while it is open, and on pull-down.

**Tree**, a tap on a card: the agent's state and, when it is blocked, its question; Open chat; then git, ports, the PR and its checks, and the agent's timeline; Resume and Return at the bottom.
Return asks first, naming the branch and its unpushed and uncommitted counts, as the TUI's dialog does, then returns the tree as `r` does, stopping its agents first.
Anything else running in the tree, such as a dev server, keeps it from the phone: `treetop return`'s own guard would refuse any tree with a live agent, which is nearly every tree, and stopping something other than an agent is a call for treetop's return dialog, which lists what would stop.

**Chat**, from Open chat or a long-press on a card:

```
┌────────────────────────────────────┐
│ ‹ 17   END-123 fix-login  ● waiting│
├────────────────────────────────────┤
│ you                         13:40  │
│ Fix the login redirect loop on     │
│ expired sessions.                  │
│                                    │
│ claude                             │
│ The guard redirects before the     │
│ refresh finishes. Moving it after  │
│ the token check.                   │
│ ▸ Edited src/auth/guard.ts  +12 −4 │
│ ▸ Ran npm test  ✗ 1 failed         │
│                                    │
│ claude                      14:20  │
│ Which auth provider should the     │
│ login use, Clerk or Supabase?      │
├────────────────────────────────────┤
│ ┌ Auth  Which auth provider? ────┐ │
│ │ [ Clerk ]  [ Supabase ]        │ │
│ │ [ Answer ]                     │ │
│ └────────────────────────────────┘ │
│ [ Message 17…               ] [→]  │
└────────────────────────────────────┘
```

Messages are yours and the agent's, in order, with each tool call folded to one line (`Edited`, `Ran`, `Read`) that expands on a tap.
It follows the conversation live while open, scrolled to the end unless you have scrolled up.
The reply box sends with `treetop send`, so a line typed here reaches the agent the way `m` does, whether it is busy, idle or cold.
When the agent waits on you, a card above the reply box shows what on, and answers it: a question's options as buttons, a permission with Allow and Deny, and the reply box for your own words.

The first build had a fourth screen, the agent's own terminal, for answering prompts. It attached a second client to the session at phone width, and Claude Code draws a session at one size for every client, so the chat popup on the desktop shrank while it was open; it was dropped for answers in the chat (ANB-21).

**New agent**, from the pool's button: a text box for the task, a choice of a fresh tree or a tree you pick, the base and an optional ticket, as `n` and `N` offer.

**Issues**, a tab beside the pool, as Tab switches them in the terminal: the terminal's views, a source switcher, search over ids and titles, and filters, with each row saying who is working on it (ANB-27).
An issue opens to its description, links and comments, and its agent's question when it has one.
Status, assignee, labels, a comment and new issues change from there (ANB-28), and Pick up puts an agent on it in a fresh tree, after asking, since a stray tap would start a real agent (ANB-29).
A source on a plugin other than Linear lists, reads and picks up, and offers no edits.

### The server

`treetop serve [--port 7777]` runs the server in the foreground and shares itself; `treetop serve --install` keeps it up as a systemd user unit.
It is built on `tiny_http`, which is synchronous like the rest of treetop, so it calls the same blocking code the CLI does, refreshing the pool and running treehouse, git and `claude`, with no async runtime added to the binary.
Each request is handled on a thread from a small pool; a live view holds one thread while it is open, which one phone never runs short of.

It listens on `127.0.0.1` only.
`tether share 7777` puts it on the tailnet at `https://server.<tailnet>.ts.net:7777` with a real certificate, which iOS needs before it installs a web app or delivers its push, and prints the address and a QR code for the phone.
[tether](https://github.com/anthonyb8/tether) is a separate tool, and treetop does not depend on it: `tether share` is `tailscale serve` underneath, so `tailscale serve --bg --https 7777 http://127.0.0.1:7777` does the same where tether is not installed, and the README gives both.
Tailscale adds the caller's login as `Tailscale-User-Login`; the server answers only requests carrying the login in the config's `[mobile] owner`, and nothing without the header, so a process on the server that skips Tailscale gets nothing either.

It reuses what the TUI reads, not a second copy of it:

- The pool from the same refresh clocks (`src/refresh.rs`), run in a thread of the server, so a page never waits on `treehouse status`, which takes seconds.
- Each tree as `status_json` builds it for `treetop status` (`src/cli.rs:237`), which already carries the agents, reports, git counts, ports, sidecar and ship state.
- Actions through the same functions as the CLI: `send`, `dispatch`, `resume` and `give_back` (`src/cli.rs:378`, `:495`, `:519`, `:573`).
- The blocked question from the job's `needs`, as `src/reports.rs:141` reads it.
- Workspaces from `src/workspaces.rs`.

Parking and ports reach the same code too: `POST /api/trees/<n>/park` takes the state wanted rather than a flip and writes the tree's `treetop.json`, and `POST /api/ports/<port>/share` runs the configured `share` command, for a port one of the pool's trees serves and no other.

The server speaks JSON and server-sent events; the screens are drawn on the phone by the web app below.
Its routes: `GET /api/pool` and `GET /api/trees/<n>`, each the matching part of `status_json`; `GET /api/trees/<n>/chat`, a live stream of server-sent events; and a POST per action.
The built web app, the manifest and the icons are embedded in the binary with `include_bytes!`, so installing treetop stays one file.
Every action is a POST that checks `Origin` against the server's own host.

### The web app

The web app is Svelte 5 with TypeScript, in `web/` at the repository's root, built by Vite to static files; there is no SvelteKit and no server-side rendering, since `treetop serve` only hands out the built files and the API.
It is one page whose screens slide over each other as a stack, Pool, Tree, Chat, with Svelte's built-in transitions, so moving between them keeps each screen's scroll and state the way a native app does.
The API's shapes are TypeScript types in `web/src/api.ts`, written to match `status_json`; a test on the Rust side checks a sample of the JSON against the fields those types read, so a renamed field fails the build rather than the phone.

What makes it feel like an app, and each is part of the issue that adds the screen:

- A line you send shows in the chat at once, marked as sending until the server takes it, and as failed with a retry if it does not.
- A new reply is laid out whole, so nothing reflows, and its words fade up in a rhythm taken from their length and punctuation, with what came after it held back until it is done.
- A chat holds its place: scrolled to the end it follows new messages, scrolled up it stays put and shows a "new messages" pill; a tool call you opened stays open, and what you are typing is kept.
- The pool's cards move into their new order when a tree's state changes, rather than the list jumping.
- The app shows the last pool it saw, marked as stale, while the server is out of reach.
  The service worker keeps no copy of the app (ANB-52): a stored copy once left a phone on a blank screen after an update, so every open loads the page from the server.

### Developing on the phone

While the web app is being built, `npm run dev` in `web/` runs Vite on port 5173 with live reload, proxying `/api` to a `treetop serve` on 7777, and `tether share 5173` puts it on the iPhone, so each save shows on the phone as it would ship.
Vite needs the `*.ts.net` host in `server.allowedHosts` and its live-reload socket pointed at the shared HTTPS port; the recipe belongs in tether's per-stack docs (ANB-12), and `web/vite.config.ts` carries it.

### The chat view

Claude Code writes each session's conversation to `~/.claude/projects/<project>/<session id>.jsonl`, one record per line; treetop already reads it for the chat sidebar's context and usage figures.
The server renders the user and assistant messages from it, folding each tool call and its result into one line, and leaves out what the TUI does not show either: system records, hooks, thinking.
While a chat is open the server follows the file and sends each new message as a server-sent event, which the web app appends; there is no polling.
Like every other field read from Claude Code's files, the records are undocumented, so one it does not recognise is skipped rather than failing the view.

### Answering prompts

What the agent waits on comes from the transcript: its newest tool call with no result yet, while the agent waits on you. An `AskUserQuestion` call carries its questions and options, which the chat shows as buttons; any other is a permission.
`POST /api/trees/<n>/answer` takes `allow`, `deny` or `say` with words. It attaches to the session with `claude attach` in a terminal of its own, as `treetop send` does, waits until `chat::input` reads the screen as a dialog, presses Enter or Esc, waits for the dialog to go, and detaches.
`say` presses Esc, then sends its words as `treetop send` does: a question's picks, or your own words, go in as a message, so the answer does not depend on how Claude Code numbers and lays out its options.
With no dialog on the screen nothing is pressed, and the answer fails with why, except `say`, whose words then go in as a message.
Claude Code writes an `AskUserQuestion` call to the transcript only once it has been answered (ANB-30), so while the agent waits on you with no such call in its chat, `GET /api/trees/<n>/prompt` reads the question and its options off the agent's screen, attached as for an answer, and the chat shows that as the buttons.
A prompt of several questions shows only its open tab's options on the screen, so it is not read, and is answered in words.
While the agent waits on you, a line typed in the reply box always goes through `say`, found prompt or not, since a dialog on the screen would swallow a plain message.

### Push

The server keeps a VAPID key pair in treetop's config directory and one subscription per installed device, and sends with the Web Push protocol to Apple's push service, which the server reaches outbound.
It pushes on the changes the TUI already sees: an agent going to needs-you, done or dead, and a held tree's PR checks failing or merging.
A push names the tree and what happened, such as "17 · END-123 fix-login needs you", and opens that tree's chat when tapped.
It never carries the agent's words.

### Installing it on an iPhone

What a user does, for the README:

1. On the server: run `treetop serve --install`, which shares it with `tether share`; without tether, `tailscale serve --bg --https 7777 http://127.0.0.1:7777` does that part.
2. On the iPhone: install the Tailscale app, sign in to the same tailnet, and turn the VPN on.
3. Open Safari, not another browser, at the address `tether share` printed, such as `https://server.<tailnet>.ts.net:7777`, or scan its QR code with the camera.
4. Tap Share, then Add to Home Screen, and Add.
5. Open treetop from the home screen icon. It runs full screen, with its own entry in the app switcher.
6. Tap Allow when it asks for notifications; iOS only offers push to a web app opened from the home screen.

- **Schema:** none. Push subscriptions and the VAPID keys are files in treetop's config directory, as the workspace list is.
- **Data:** the pool snapshot the refresh clocks keep, read by every page; actions invalidate it by asking the slow clock to run, as the TUI does after a return. Chats are read live, never cached.
- **Services / vendors / env:** Tailscale (`tailscale serve`, through `tether share` where tether is installed, and the `Tailscale-User-Login` header); Apple's Web Push service. New config: a `[mobile]` table with `owner` and `port`. No env vars.

## Breakdown

| # | Issue | Delivers | Depends on |
|---|---|---|---|
| 1 | _ID once created_ | `treetop serve` shared with `tether share` or `tailscale serve`, owner-only; the `web/` Svelte app and its release build; the read-only Pool screen with the workspace switcher; installable to the home screen | None |
| 2 | _ID once created_ | The Tree screen: agent state and question, git, ports, PR, timeline | 1 |
| 3 | _ID once created_ | The Chat screen: the conversation live from the transcript, tool calls folded, and the reply box through `send` | 2 |
| 4 | _ID once created_ | The Terminal view: the agent's own screen and a key row, for permission prompts and questions; replaced by ANB-21, answers in the chat | 3 |
| 5 | _ID once created_ | Actions: new agent in a fresh tree or a picked one, resume, return with its confirm and guard | 2 |
| 6 | _ID once created_ | Web push for needs-you, done, dead, CI failing and merged, opening the tree's chat | 3 |

## Rollout

Each issue ships as a tagged release from `main`, in order; there are no migrations, and nothing changes for anyone who never runs `treetop serve`.
From issue 1, `.github/workflows/release.yml` builds the web app with `npm ci && npm run build` in `web/` before `cargo build`, and the tests run `npm run check` alongside `cargo test`.
The manual smoke check for each is the install steps above on a real iPhone, then the screen or action the issue adds, once.
For issue 4, the check is answering a real permission prompt and a real multiple-choice question from the phone, and seeing the agent carry on.
Rollback is the previous release, or stopping `treetop serve`.

## Risks

- **Claude Code's transcript format changes.** The chat view is built on undocumented records. An unknown record is skipped, and a question or permission it cannot read still shows as the agent's own "needs you" words, answered in the reply box.
- **An action from the phone does the wrong thing.** Return asks first and stops nothing but the tree's agents; destroy is not offered at all; an answer presses a key only on a screen showing a prompt.
- **Someone else on the tailnet reaches the server.** Only the configured owner's Tailscale login is answered, and the server is not bound to any other address.
- **A lost phone.** It reaches the server only while signed in to the tailnet; removing the device in Tailscale's admin console cuts it off.
- **iOS clears a web app's storage** after weeks unused. It stores nothing but the push subscription, which it registers again on next open.

## Open decisions

- **Technical: buttons for prompts instead of the terminal view?** Decided in ANB-21: buttons in the chat, read from the transcript rather than the screen, and the terminal view dropped.
- **Product: native app instead of a PWA?** Native feels a little better and pushes more reliably, but needs a Mac with Xcode, a paid Apple developer account, and TestFlight or re-signing every 7 days, for an app with one user. Recommendation: PWA.
