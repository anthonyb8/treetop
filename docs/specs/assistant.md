# Assistant

A project in six issues: the coordinator becomes an assistant that keeps a briefing on screen, proposes actions you take with one key, and wakes on its own when agents or PRs change.
It stays one Claude Code background session in the main checkout; what changes is that treetop shows what it produces, instead of you reading its chat.
Each issue is one PR into `main` and ships on its own as a tagged release.

## Goal

Knowing where every agent stands, and what to do next, takes one glance at treetop rather than a conversation.
Today the coordinator is a chat that knows about the agents: it speaks only when spoken to, everything it says lives in a transcript, and every action it offers is a question you answer in prose.
So checking in means opening its chat, asking, reading, and typing "yes" to whatever it suggests.
The assistant turns that around: it watches for you, writes down where things stand in a pane that is always on screen, and turns each next step into a card that one key carries out.
It is for one developer running several agents across a pool, who wants to steer them, not supervise them.

## Success

- The coordinator's chat is opened only for a real conversation, not to find out where things stand.
- An agent finishing, getting stuck on you, or its PR failing CI shows in the briefing within a minute, without anything typed.
- The next step for each agent that needs one is a card, and taking it is one key.
- Nothing is sent, started or returned that a card did not show first and you did not accept.
- A quiet afternoon of agents working costs a handful of assistant turns, not one per minute.

## Scope

**In:** a briefing and cards written by the assistant through new `treetop` commands; an assistant pane on the Trees screen; carrying out an accepted card through treetop's own actions; waking the assistant on agent and PR events; a one-line command bar from any screen; the assistant reading issues and picking one up as a card.

**Out:**

- Acting without a card being accepted. Every action stays one key from you; trusting it with some actions on its own is an open decision below, and a later project if wanted.
- Creating or editing Linear issues as cards. The Issues screen does that; the assistant may suggest it in the briefing.
- Talking to the assistant by voice or from outside treetop, such as Slack.
- More than one assistant per workspace.

## Design

### What you see

The Trees screen gains an assistant pane on the right when the terminal is wide enough, 160 columns or more, and below the list otherwise; `a` shows or hides it, and it stays as you left it.

```
  #  STATUS  TICKET   BRANCH              AGENT            SAYS              │ Assistant                          updated 2m ago
  28 leased  END-860  feature/end-860...  ● END-860 (t28)  ✗ CI failed on…  │
  33 leased  END-805  feature/end-805...  ○ pickup END-805 ✓ PR #389 merged  │ Needs you
  17 leased  END-879  feature/end-879...  ◐ seed insights  ? which bucket…   │ • 17 asks which bucket the hero image goes in;
  20 leased           enterprise-branch   ○ enterprise     ✓ done            │   it suggests `branding/`.
                                                                             │
                                                                             │ Done
                                                                             │ • 33 merged END-805 (#389); its tree can go back.
                                                                             │ • 20 finished the enterprise branch sync.
                                                                             │
                                                                             │ Working
                                                                             │ • 28 is fixing a lint failure in CI.
                                                                             │
                                                                             │ Suggested                       y take  n drop  e edit
                                                                             │ › Answer 17: "Put it in branding/."
                                                                             │   Return 33: END-805 merged, nothing unpushed.
                                                                             │   Start END-812, next in the cycle, in a new tree.
 Enter open  : ask  a assistant  y take  ...                                  treetop · app
```

The briefing is Markdown the assistant writes, drawn with the Issues screen's renderer, and says how old it is.
Cards sit under it, newest first. `→` gives the pane the keys and `←` or Esc gives them back to the list (`Tab` stays the switch to the Issues screen); in the pane `↑`/`↓` move between cards, `y` takes the marked card, `n` drops it, and `e` opens its message or task in the compose box to change it before taking it.
A card that has been taken shows how it went (`sent`, `started in tree 34`, `returned`, or the error) until the next briefing replaces it.

The pane ends in a box you type to it in; `:` puts the cursor there:

```
│ You  what should I do first?                     │
│ Start with 4: clearing the startup dialog is a   │
│ quick fix. Then 6. 1 is only a test.             │
│ ──────────────────────────────────────────────── │
│ Needs you                                        │
│ ...                                              │
│ ──────────────────────────────────────────────── │
│ → tree 17 · seed insightiq social account data   │
│ › @17 put it in branding/                        │
```

Enter sends it to the assistant with the tree under the cursor as context; your question shows at the top of the pane, and the assistant's answer under it, a few lines it writes with `treetop say`, until the next question; the pane says `thinking...` until the answer comes.
A message starting with `@` goes to that agent instead: `@` takes a tree number, `main`, or the start of an agent's words (`@fix-log` for "fix the login redirect"), the line above the box says who it will reach, Tab completes it to the tree's number, and several `@`s send to each.
On the Issues screen, where the pane is not, `:` opens the compose box with the issue under the cursor as context.
`c` and the chat stay as they are, for a real conversation.

### What the assistant writes

The assistant writes into treetop through four commands, added to the ones it may run without asking:

| Command | Does |
|---|---|
| `treetop say` | Puts the Markdown on stdin up as the answer to what was asked in the box. |
| `treetop brief` | Replaces the briefing with the Markdown on stdin. |
| `treetop propose <send\|dispatch\|pick\|return\|open> <args> --why "<text>"` | Adds a card: `send <tree> <message>`, `dispatch <new\|main\|N> <task>`, `pick <issue>`, `return <tree>`, `open <url>`. Prints the card's id. |
| `treetop cards` | Prints the cards as JSON, with each one's state and outcome, so it knows what you took. |

They write one state file per workspace, `$XDG_STATE_HOME/treetop/<workspace>/assistant.json`, through a temporary file and a rename, so the screen never reads half of it:

```json
{
  "briefing": { "markdown": "…", "at": 1792500000 },
  "cards": [
    { "id": "c17", "action": "send", "args": ["17", "Put it in branding/."],
      "why": "17 asks which bucket the hero image goes in.",
      "state": "open", "outcome": null, "at": 1792500000 }
  ]
}
```

A card's `state` is `open`, `taken`, `dropped` or `failed`, and `outcome` is what treetop said when it ran it.
Cards older than a day, and taken or dropped ones once the next briefing lands, are pruned.

### Taking a card

treetop carries out a card itself, through the action that key would have run, never by asking the assistant to:

- `send` is `m`'s send to that tree's agent;
- `dispatch` is `n`'s start, in a new tree, the main checkout or tree N;
- `pick` is the Issues screen's `p`;
- `return` opens the usual return dialog for that tree, which still lists its unpushed work and asks again;
- `open` is the Issues screen's `o`: the link opens in the browser, or is copied where there is no browser.

So a card can do nothing a key could not, and the assistant never runs `send`, `dispatch`, `pick` or `return` itself any more: those leave its allowed commands, and its prompt says to propose instead.

### Waking it

treetop already notices an agent coming to need you or finishing (it rings the bell), and a tree's PR changing stage through its ship plugin.
It now also sends those to the assistant, batched: events are collected for 30 seconds after the first, then sent as one message, at most one message every 2 minutes.
The message is a line per event and a reminder of what to do with it:

```
Events since your last turn:
- tree 17 (seed insightiq social account data) needs you: asks which bucket the hero image goes in
- tree 33 PR #389 merged
- tree 28 PR #391 CI failed: lint
Run `treetop status`, then update the briefing with `treetop brief` and add any cards with `treetop propose`. Say nothing else.
```

A cold assistant is started again by its plugin's `send`, as a message to any cold agent is.
Which events wake it is a setting, all of them by default:

```toml
[assistant]
wake = ["needs_you", "done", "pr"]
```

`wake = []` turns waking off, and the assistant updates only when asked through `:` or `c`.

### Issues

The assistant may also run `treetop tickets` to read the repository's issues, and propose `pick` cards for ones that look next, such as the cycle's unassigned issues.
The command bar's context includes the issue under the cursor when it is opened from the Issues screen.

### The assistant's prompt

The coordinator's system prompt changes from answering in chat to keeping the briefing: on every turn, read `treetop status`, rewrite the briefing as needs you, done, working, and add a card for each next step it would take; answer in chat only when talked to in chat.
It keeps its name, `treetop-coordinator`, so a running one is found again; the screen calls it the assistant.

## Breakdown

| # | Issue | Delivers | Depends on |
|---|---|---|---|
| 1 | _to create_ | `treetop brief`, `propose` and `cards`, the state file, and the coordinator allowed and prompted to use them instead of `send` and `dispatch` | None |
| 2 | _to create_ | the assistant pane on the Trees screen: the briefing drawn from the state file, the cards, `a`, `→` and `←` | 1 |
| 3 | _to create_ | taking and dropping cards with `y`, `n` and `e`, through treetop's own actions, with each card's outcome | 2 |
| 4 | _to create_ | waking the assistant on needs-you, done and PR events, batched, with `[assistant] wake` | 1 |
| 5 | _to create_ | the `:` command bar on every screen, sending what is on screen as context | 2 |
| 6 | _to create_ | `treetop tickets` for the assistant, `pick` cards, and the issue under the cursor as context | 3, 5 |

## Rollout

Each issue ships as its own release, in the order above.
Issue 1 changes nothing on screen: the coordinator starts keeping the state file, which issue 2 then shows, so the pane never appears empty.
Waking, issue 4, can be turned off with `wake = []` if it costs or interrupts more than it helps; rolling it back is that setting.
The state file is new and nothing else reads it, so any issue can be reverted by its release, and deleting the file resets the assistant's screen.

## Risks

- **Cost.** Every wake is an assistant turn. Batching for 30 seconds, at most one message every 2 minutes, and waking only on the three events keep a working afternoon to a handful of turns; `wake = []` stops it.
- **Acting on bad reasoning.** The assistant can be wrong about what an agent needs. Nothing happens until a card is taken, each card says why, and `return` still goes through its dialog.
- **Instructions inside reports.** Agents' reports and issue text reach the assistant, and could tell it to do something. It can only write the briefing and cards; every action is still a card you take.
- **A stale briefing.** The pane shows the briefing's age, and the rows beside it are always live, so an old briefing is visible as old.
- **A crowded screen.** On a narrow terminal the pane moves below the list, and `a` hides it.

## Open decisions

1. **Where the pane goes.** A right-hand column on wide terminals and below the list on narrow ones (recommended); always below the list; or a screen of its own on a key, like Issues.
2. **How much it may do without a card.** Nothing (recommended for this project); answering an agent's question when you have said "you decide" for it; or sending any message to an agent, keeping dispatch and return behind cards.
3. **What wakes it by default.** Needs you, done and PR changes (recommended); needs you only; or nothing until asked.
4. **What to call it.** "Assistant" on screen and keep the session named `treetop-coordinator` (recommended); rename both; or keep "coordinator".
