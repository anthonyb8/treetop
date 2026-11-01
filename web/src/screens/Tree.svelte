<script lang="ts">
  import { ApiError, fetchTree, type ReportState, type Tree } from "../api";
  import GitCounts from "../components/GitCounts.svelte";
  import Mark from "../components/Mark.svelte";
  import PrBadge from "../components/PrBadge.svelte";
  import { back, link, nav } from "../nav.svelte";
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

  let card = $derived(tree?.card);
  let agent = $derived(tree?.agents[0] ?? null);
  let isDirty = $derived(
    card?.git === "unknown" ||
      (card?.git != null && card.git.unpushed + card.git.staged + card.git.unstaged + card.git.untracked > 0),
  );
</script>

<div class="screen">
  <header>
    <button type="button" class="back" onclick={back}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
        stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>
      Pool
    </button>
    <div class="name">
      <span class="number mono">{name}</span>
      <h1>{card?.title ?? card?.branch ?? `tree ${name}`}</h1>
    </div>
    {#if card?.branch}
      <p class="branch mono">{card.branch}{tree?.base ? ` · from ${tree.base}` : ""}</p>
    {/if}
  </header>

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
          <p class="says" class:question={tree.report.state === "blocked"}>
            {tree.report.state === "blocked" ? `“${tree.report.says}”` : tree.report.says}
          </p>
        {:else if tree.task}
          <p class="says">{tree.task}</p>
        {/if}
        {#if agent?.has_chat}
          <a class="open-chat" class:loud={card.needs_you} href="/trees/{encodeURIComponent(name)}/chat" onclick={link}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
              stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
              ><path d="M21 12a8 8 0 0 1-11.6 7.1L4 20l1-4.6A8 8 0 1 1 21 12z" /></svg>
            Open chat
          </a>
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
          <span class="value">
            {#if card.git === "unknown"}
              <span class="muted">git could not read it</span>
            {:else if isDirty}
              <GitCounts git={card.git} />
            {:else}
              <span class="muted">clean, nothing unpushed</span>
            {/if}
          </span>
        </div>
        <div class="fact">
          <span class="key">Ports</span>
          <span class="value list">
            {#each tree.ports as port (port.port)}
              <span class="mono"><span class="port">:{port.port}</span> {port.process}</span>
            {:else}
              <span class="muted">none</span>
            {/each}
          </span>
        </div>
        <div class="fact">
          <span class="key">PR</span>
          <span class="value list">
            {#if tree.pr}
              <span class="pr">
                <PrBadge pr={tree.pr} />
                {#if tree.pr.url}<a href={tree.pr.url} target="_blank" rel="noreferrer">Open</a>{/if}
              </span>
              {#each tree.pr.detail as line, i (i)}<span class="muted small">{line}</span>{/each}
            {:else if tree.pr_unknown}
              <span class="muted">unknown: {tree.pr_unknown}</span>
            {:else}
              <span class="muted">none</span>
            {/if}
          </span>
        </div>
        <div class="fact">
          <span class="key">Path</span>
          <span class="value mono small path">{tree.path}</span>
        </div>
      </section>

      {#if tree.report?.steps.length}
        <section class="timeline" aria-label="Timeline">
          <h2>Timeline</h2>
          <ol>
            {#each [...tree.report.steps].reverse() as step, i (i)}
              <li>
                <span class="mono at">{clock(step.at)}</span>
                <span class="step-state" style:color={step.state ? said[step.state].tone : "var(--muted)"}>
                  {step.state ? stepWord[step.state] : ""}
                </span>
                <span class="detail">{step.detail}</span>
              </li>
            {/each}
          </ol>
        </section>
      {/if}
    {/if}
  </main>
</div>

<style>
  /* In a scrolling column, a section that clips its corners would shrink to
     nothing once the content outgrows the screen. */
  main > :global(*) {
    flex-shrink: 0;
  }
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: calc(var(--safe-top) + 4px) 16px 14px;
    background: var(--bg-raised);
  }
  .back {
    align-self: flex-start;
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
    align-items: baseline;
    gap: 10px;
    min-width: 0;
  }
  .number {
    font-size: 26px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  h1 {
    margin: 0;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 20px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .branch {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  main {
    flex: 1;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 16px calc(var(--safe-bottom) + 24px);
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
    min-height: 46px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 15px;
    font-weight: 600;
  }
  .open-chat.loud {
    border: 0;
    background: var(--yellow);
    color: var(--on-accent);
  }
  .others {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
  }
  .facts {
    display: flex;
    flex-direction: column;
    overflow: hidden;
  }
  .fact {
    display: grid;
    grid-template-columns: 56px minmax(0, 1fr);
    gap: 8px;
    padding: 12px 14px;
    font-size: 14px;
  }
  .fact + .fact {
    border-top: 1px solid var(--line);
  }
  .key {
    color: var(--muted);
  }
  .value {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
    min-width: 0;
  }
  .value.list {
    flex-direction: column;
  }
  .pr {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .port {
    color: var(--blue);
  }
  .muted {
    color: var(--muted);
  }
  .small {
    font-size: 12px;
  }
  .path {
    overflow-wrap: anywhere;
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
  ol {
    margin: 0;
    padding: 0;
    list-style: none;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  li {
    display: grid;
    grid-template-columns: 44px 60px minmax(0, 1fr);
    gap: 8px;
    font-size: 13px;
    line-height: 1.35;
  }
  .at {
    color: var(--muted);
  }
  .detail {
    overflow-wrap: anywhere;
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
