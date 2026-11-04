<script lang="ts">
  import { ApiError, fetchTree, giveBack, park, resume, type ReportState, type Tree } from "../api";
  import GitCounts from "../components/GitCounts.svelte";
  import Mark from "../components/Mark.svelte";
  import PortButton from "../components/PortButton.svelte";
  import PublicButton from "../components/PublicButton.svelte";
  import PrBadge from "../components/PrBadge.svelte";
  import { back, go, link, nav } from "../nav.svelte";
  import { ago } from "../time";

  let { name }: { name: string } = $props();

  const EVERY_MS = 5000;

  let tree = $state<Tree | null>(null);
  let failure = $state<string | null>(null);
  let isGone = $state(false);

  async function load() {
    try {
      tree = await fetchTree(nav.workspace, name);
      failure = null;
      isGone = false;
    } catch (err) {
      isGone = err instanceof ApiError && err.status === 404;
      failure = err instanceof ApiError ? err.message : "Something went wrong";
    }
  }

  $effect(() => {
    void load();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, EVERY_MS);
    return () => clearInterval(timer);
  });

  const said: Record<ReportState, { label: string; tone: string }> = {
    blocked: { label: "waiting on you", tone: "var(--yellow)" },
    working: { label: "working", tone: "var(--blue)" },
    done: { label: "done", tone: "var(--green)" },
    stopped: { label: "stopped", tone: "var(--muted)" },
    failed: { label: "its process died", tone: "var(--red)" },
  };

  // A step's state in a word, so the timeline's column stays narrow.
  const stepWord: Record<ReportState, string> = {
    blocked: "asked",
    working: "working",
    done: "done",
    stopped: "stopped",
    failed: "died",
  };

  function clock(at: number | null): string {
    if (at === null) return "";
    return new Date(at * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  }

  /** The action under way, and why the last one failed. */
  let doing = $state<"resume" | "return" | "park" | null>(null);
  let actionFailure = $state<string | null>(null);
  let isConfirming = $state(false);
  let isMenuOpen = $state(false);
  let isCopied = $state(false);

  async function copyPath() {
    if (!tree) return;
    try {
      await navigator.clipboard.writeText(tree.path);
      isCopied = true;
      setTimeout(() => (isCopied = false), 1500);
    } catch {
      // Without clipboard access the path stays on screen to select by hand.
    }
  }

  async function act(what: "resume" | "return") {
    doing = what;
    actionFailure = null;
    try {
      if (what === "resume") {
        await resume(nav.workspace, name);
        await load();
      } else {
        await giveBack(nav.workspace, name);
        isConfirming = false;
        go("/");
      }
    } catch (err) {
      actionFailure = err instanceof ApiError ? err.message : "It did not go through";
      isConfirming = false;
    } finally {
      doing = null;
    }
  }

  async function togglePark() {
    if (!tree) return;
    isMenuOpen = false;
    doing = "park";
    actionFailure = null;
    try {
      await park(nav.workspace, name, !tree.card.parked);
      await load();
    } catch (err) {
      actionFailure = err instanceof ApiError ? err.message : "It did not go through";
    } finally {
      doing = null;
    }
  }

  let card = $derived(tree?.card);
  // An agent that is not running can be started again with its
  // conversation; one that is has nothing to resume.
  let canResume = $derived(
    !!tree && tree.agents.length > 0 && !tree.agents.some((a) => a.status && a.status !== "cold"),
  );
  let agent = $derived(tree?.agents[0] ?? null);
  // The path as a shell shows it, so it breaks at its slashes, not mid-name.
  let shortPath = $derived(tree?.path.replace(/^\/(home|Users)\/[^/]+/, "~") ?? "");
  let isDirty = $derived(
    card?.git === "unknown" ||
      (card?.git != null && card.git.unpushed + card.git.staged + card.git.unstaged + card.git.untracked > 0),
  );
</script>

<div class="screen">
  <header>
    <div class="bar">
      <button type="button" class="back" onclick={back}>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
          stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>
        Trees
      </button>
      {#if card}
        <button type="button" class="more" aria-label="More" aria-expanded={isMenuOpen} onclick={() => (isMenuOpen = !isMenuOpen)}>
          <svg width="22" height="22" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"
            ><circle cx="5" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><circle cx="19" cy="12" r="1.8" /></svg>
        </button>
      {/if}
    </div>
    <div class="name">
      <span class="number mono" class:loud={card?.needs_you}>{name}</span>
      <h1>{card?.title ?? card?.branch ?? `tree ${name}`}</h1>
    </div>
    {#if card?.branch}
      <p class="branch mono">
        {card.branch}{#if tree?.base}<span class="from" aria-label="from">←</span>{tree.base}{/if}
        {#if card.parked}<span class="parked">parked</span>{/if}
      </p>
    {/if}
  </header>

  {#if isMenuOpen && card}
    <button type="button" class="menu-back" aria-label="Close menu" onclick={() => (isMenuOpen = false)}></button>
    <div class="menu">
      <button type="button" disabled={doing !== null} onclick={togglePark}>{card.parked ? "Unpark" : "Park"}</button>
      <button
        type="button"
        class="danger"
        disabled={doing !== null}
        onclick={() => {
          isMenuOpen = false;
          isConfirming = true;
        }}>Return…</button
      >
    </div>
  {/if}

  <main>
    {#if !tree}
      {#if failure}
        <div class="state">
          <p>{isGone ? `Tree ${name} is not in this pool any more.` : `${failure}.`}</p>
          {#if !isGone}<button type="button" class="ghost" onclick={() => load()}>Try again</button>{/if}
        </div>
      {:else}
        <div class="state" aria-busy="true">
          <div class="skeleton tall"></div>
          <div class="skeleton"></div>
        </div>
      {/if}
    {:else if card}
      {#if failure}
        <p class="banner" role="status">
          {isGone ? `Tree ${name} is not in this pool any more.` : `${failure} - showing it as it last was.`}
        </p>
      {/if}

      <section class="agent" class:loud={card.needs_you} aria-label="Agent">
        <div class="who">
          <Mark mark={card.mark} />
          {#if agent}
            <span class="agent-name">{agent.name}</span>
            {#if tree.report}
              <span class="said" style:color={said[tree.report.state].tone}>· {said[tree.report.state].label}</span>
            {:else if agent.status}
              <span class="said">· {agent.status}</span>
            {/if}
            {#if card.active_ago !== null}<span class="when">{ago(card.active_ago)}</span>{/if}
          {:else}
            <span class="agent-name muted">No agent works here</span>
          {/if}
        </div>
        {#if tree.report?.says}
          <p class="says" class:question={tree.report.state === "blocked"}>{tree.report.says}</p>
        {:else if tree.task}
          <p class="says">{tree.task}</p>
        {/if}
        {#if agent?.has_chat && card.needs_you}
          <div class="ways">
            <a class="open-chat" href="/trees/{encodeURIComponent(name)}/chat" onclick={link}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
                stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
                ><path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z" /></svg>
              Answer in chat
            </a>
          </div>
        {/if}
        {#if tree.agents.length > 1}
          <p class="others">
            Also here: {tree.agents.slice(1).map((a) => `${a.name}${a.status ? ` (${a.status})` : ""}`).join(", ")}
          </p>
        {/if}
      </section>

      <section class="facts" aria-label="Tree">
        <div class="fact">
          <span class="key">Git</span>
          {#if card.git === "unknown"}
            <span class="muted">git could not read it</span>
          {:else if isDirty}
            <span class="value"><GitCounts git={card.git} /></span>
          {:else}
            <span class="muted">clean</span>
          {/if}
        </div>
        <div class="fact">
          <span class="key">{tree.ports.length > 1 ? "Ports" : "Port"}</span>
          <span class="value list">
            {#each tree.ports as port (port.port)}
              <span class="port-row">
                <PortButton port={port.port} process={port.process} />
                <PublicButton port={port.port} url={port.public} />
              </span>
            {:else}
              <span class="muted">none</span>
            {/each}
          </span>
        </div>
        <div class="fact">
          <span class="key">PR</span>
          {#if tree.pr}
            <span class="pr">
              <PrBadge pr={tree.pr} />
              {#if tree.pr.url}<a href={tree.pr.url} target="_blank" rel="noreferrer">Open</a>{/if}
            </span>
            {#each tree.pr.detail as line, i (i)}<span class="small">{line}</span>{/each}
          {:else if tree.pr_unknown}
            <span class="muted">unknown: {tree.pr_unknown}</span>
          {:else}
            <span class="muted">none</span>
          {/if}
        </div>
        <button type="button" class="fact copy" aria-label="Copy path {tree.path}" onclick={copyPath}>
          <span class="key">Path</span>
          <span class="mono small path"
            >{#each shortPath.split("/") as part, i (i)}{i ? "/" : ""}<wbr />{part}{/each}</span
          >
          <span class="muted small">{isCopied ? "copied" : "tap to copy"}</span>
        </button>
      </section>

      {#if tree.report?.steps.length}
        <section class="timeline" aria-label="Timeline">
          <h2>Timeline</h2>
          <ol>
            {#each [...tree.report.steps].reverse() as step, i (i)}
              <li style:--tone={step.state ? said[step.state].tone : "var(--muted)"}>
                <span class="dot" aria-hidden="true"></span>
                <span class="detail" class:first={i === 0}>
                  <span class="visually-hidden">{step.state ? stepWord[step.state] : ""}:</span>
                  {step.detail}
                </span>
                <span class="mono at">{clock(step.at)}</span>
              </li>
            {/each}
          </ol>
        </section>
      {/if}
    {/if}
  </main>

  {#if tree && card}
    <footer>
      {#if actionFailure}<p class="failure" role="alert">{actionFailure}</p>{/if}
      <div class="actions">
        {#if canResume}
          <button type="button" class="action" disabled={doing !== null} onclick={() => act("resume")}>
            {doing === "resume" ? "Resuming…" : "Resume"}
          </button>
        {:else}
          <a class="action" href="/trees/{encodeURIComponent(name)}/new" onclick={link}>New agent here</a>
        {/if}
        {#if agent?.has_chat}
          <a class="action primary" href="/trees/{encodeURIComponent(name)}/chat" onclick={link}>Open chat</a>
        {/if}
      </div>
    </footer>
  {/if}

  {#if isConfirming && tree && card}
    <div class="sheet-back" role="presentation" onclick={() => (isConfirming = false)}></div>
    <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="return-title">
      <h2 id="return-title">Return tree {name}?</h2>
      <p class="mono branch-line">{card.branch ?? "no branch"}</p>
      <ul>
        {#if card.git === "unknown"}
          <li class="warn">git could not read it, so what it holds is not known</li>
        {:else if card.git && (card.git.unpushed || card.git.staged || card.git.unstaged || card.git.untracked)}
          {#if card.git.unpushed}<li class="warn">{card.git.unpushed} commit{card.git.unpushed === 1 ? "" : "s"} on no remote</li>{/if}
          {#if card.git.staged + card.git.unstaged + card.git.untracked}
            <li class="warn">
              {card.git.staged + card.git.unstaged + card.git.untracked} uncommitted file{card.git.staged + card.git.unstaged + card.git.untracked === 1 ? "" : "s"}
            </li>
          {/if}
        {:else}
          <li>Nothing uncommitted or unpushed.</li>
        {/if}
        {#if tree.agents.length}<li>Its agent{tree.agents.length === 1 ? " is" : "s are"} stopped first.</li>{/if}
      </ul>
      <p class="note">The tree goes back to the pool for the next agent.</p>
      <div class="actions">
        <button type="button" class="action" onclick={() => (isConfirming = false)}>Cancel</button>
        <button type="button" class="action danger solid" disabled={doing !== null} onclick={() => act("return")}>
          {doing === "return" ? "Returning…" : "Return"}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  /* In a scrolling column, a section that clips its corners would shrink to
     nothing once the content outgrows the screen. */
  main > :global(*) {
    flex-shrink: 0;
  }
  .screen {
    position: relative;
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: calc(var(--safe-top) + 4px) 16px 14px;
    border-bottom: 1px solid var(--bg-high);
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .back:disabled {
    opacity: 0.6;
  }
  .back {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: 44px;
    margin-left: -6px;
    padding: 0 6px;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 16px;
  }
  .name {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .number {
    flex: none;
    padding: 1px 7px;
    border-radius: 7px;
    background: var(--bg-high);
    font-size: 15px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  .number.loud {
    background: var(--yellow);
    color: var(--on-accent);
  }
  .more {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    margin-right: -10px;
    border: 0;
    border-radius: 22px;
    background: none;
    color: var(--muted);
  }
  .more:active {
    background: var(--bg-high);
  }
  .menu-back {
    position: absolute;
    inset: 0;
    z-index: 2;
    border: 0;
    background: transparent;
  }
  .menu {
    position: absolute;
    z-index: 3;
    top: calc(var(--safe-top) + 48px);
    right: 12px;
    min-width: 180px;
    display: flex;
    flex-direction: column;
    padding: 6px;
    border-radius: 14px;
    border: 1px solid var(--line);
    background: var(--bg-high);
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.45);
  }
  .menu button {
    min-height: 44px;
    padding: 0 12px;
    border: 0;
    border-radius: 10px;
    background: none;
    color: var(--fg-strong);
    font-size: 15px;
    text-align: left;
  }
  .menu button:active {
    background: var(--line);
  }
  .menu .danger {
    color: var(--red);
  }
  h1 {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 21px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .branch {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .from {
    margin: 0 6px;
    color: var(--line-strong);
  }
  .parked {
    margin-left: 8px;
    padding: 1px 6px;
    border-radius: 6px;
    background: var(--bg-high);
    font-family: var(--sans);
    font-size: 12px;
  }
  main {
    flex: 1;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px;
  }
  section {
    border-radius: 14px;
    background: var(--bg-raised);
  }
  .agent {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
  }
  .agent.loud {
    background: var(--yellow-bg);
    border: 1px solid var(--yellow);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font-size: 14px;
  }
  .agent-name {
    font-weight: 600;
    color: var(--fg-strong);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .said {
    flex: none;
    color: var(--muted);
  }
  .when {
    margin-left: auto;
    flex: none;
    font-size: 12px;
    color: var(--muted);
  }
  .says {
    margin: 0;
    font-size: 15px;
    line-height: 1.45;
    color: var(--fg);
  }
  .says.question {
    color: var(--fg-strong);
  }
  .open-chat {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 44px;
    border-radius: 12px;
    background: var(--yellow);
    color: var(--on-accent);
    font-size: 15px;
    font-weight: 600;
  }
  .ways {
    display: grid;
    gap: 8px;
  }
  .others {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
  }
  /* The tree's facts as four tiles, so the timeline below stays on screen. */
  .facts {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
    background: none;
  }
  /* A tile is half the screen, so a long path or PR line breaks anywhere
     rather than pushing the tile wider. */
  .fact {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    min-width: 0;
    overflow-wrap: anywhere;
    padding: 12px;
    border: 0;
    border-radius: 14px;
    background: var(--bg-raised);
    font-size: 14px;
    text-align: left;
    color: inherit;
  }
  .fact.copy:active {
    background: var(--bg-high);
  }
  .key {
    font-size: 12px;
    color: var(--muted);
  }
  .value {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 10px;
    min-width: 0;
    max-width: 100%;
  }
  .value :global(span) {
    font-size: 15px;
  }
  .value.list {
    flex-direction: column;
    align-items: flex-start;
  }
  .port-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    max-width: 100%;
  }
  .pr {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .muted {
    color: var(--muted);
  }
  .small {
    font-size: 12px;
  }
  .path {
    color: var(--fg);
  }
  .timeline {
    padding: 14px;
  }
  h2 {
    margin: 0 0 10px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .timeline ol {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
  }
  /* Each step a dot on a rail, newest first and in its state's colour. */
  .timeline li {
    position: relative;
    display: grid;
    grid-template-columns: 10px minmax(0, 1fr) auto;
    gap: 12px;
    padding-bottom: 12px;
    font-size: 14px;
    line-height: 1.4;
  }
  .timeline li:last-child {
    padding-bottom: 0;
  }
  .timeline li:not(:last-child)::before {
    content: "";
    position: absolute;
    left: 4px;
    top: 16px;
    bottom: 0;
    width: 2px;
    background: var(--line);
  }
  .dot {
    width: 10px;
    height: 10px;
    margin-top: 5px;
    border-radius: 5px;
    background: var(--tone);
  }
  .detail {
    overflow-wrap: anywhere;
  }
  .detail.first {
    color: var(--tone);
  }
  .at {
    font-size: 12px;
    line-height: 20px;
    color: var(--muted);
  }
  footer {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 16px var(--bottom);
    border-top: 1px solid var(--bg-high);
  }
  .actions {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px;
  }
  .action {
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 48px;
    border-radius: 12px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 15px;
    font-weight: 600;
  }
  .action.primary {
    border: 0;
    background: var(--green);
    color: var(--on-accent);
  }
  .action:only-child {
    grid-column: 1 / -1;
  }
  .action.danger {
    border-color: var(--red);
    background: transparent;
    color: var(--red);
  }
  .action.danger.solid {
    background: var(--red);
    color: var(--on-accent);
  }
  .action:disabled {
    opacity: 0.6;
  }
  .failure {
    margin: 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--red-bg);
    color: var(--red);
    font-size: 14px;
  }
  .sheet-back {
    position: absolute;
    inset: 0;
    background: rgb(0 0 0 / 0.5);
  }
  .sheet {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 20px 16px calc(var(--bottom) + 8px);
    border-radius: 18px 18px 0 0;
    background: var(--bg-raised);
    box-shadow: 0 -8px 24px rgb(0 0 0 / 0.4);
  }
  .sheet h2 {
    margin: 0;
    font-size: 18px;
    letter-spacing: 0;
    text-transform: none;
    color: var(--fg-strong);
  }
  .branch-line {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .sheet ul {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 15px;
  }
  .warn {
    color: var(--yellow);
  }
  .note {
    margin: 0 0 6px;
    font-size: 13px;
    color: var(--muted);
  }
  .banner {
    margin: 0;
    padding: 8px 12px;
    border-radius: 10px;
    font-size: 13px;
    background: var(--yellow-bg);
    color: var(--yellow);
  }
  .state {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 24px 4px;
    color: var(--muted);
    font-size: 15px;
  }
  .state p {
    margin: 0;
  }
  .ghost {
    align-self: flex-start;
    min-height: 44px;
    padding: 0 16px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-weight: 600;
  }
  .skeleton {
    height: 120px;
    border-radius: 14px;
    background: var(--bg-raised);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .skeleton.tall {
    height: 160px;
  }
  @keyframes pulse {
    50% {
      opacity: 0.5;
    }
  }
</style>
