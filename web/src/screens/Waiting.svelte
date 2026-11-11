<script lang="ts">
  import { flip } from "svelte/animate";
  import type { WaitingKind } from "../api";
  import { isElsewhere, loadWaiting, openEntry, waiting } from "../waiting.svelte";
  import { Pull, PULL_PX } from "../pull.svelte";
  import { ago } from "../time";

  let scroller: HTMLElement | undefined = $state();
  let isLoading = $state(false);
  let count = $derived(waiting.list?.entries.length ?? 0);

  async function refresh() {
    isLoading = true;
    await loadWaiting();
    isLoading = false;
  }
  const pull = new Pull(() => void refresh());

  /** What the entry's mark says, for a screen reader. */
  const labels: Record<WaitingKind, string> = {
    needs_you: "needs you",
    failed: "PR failed",
    died: "agent died",
  };

  /** What finished or merged, on one line at the foot. */
  let also = $derived(
    (waiting.list?.also ?? [])
      .map((f) => `${isElsewhere(f) ? `${f.workspace} ` : ""}${f.tree} ${f.kind === "merged" ? "merged" : "finished"}`)
      .join(" · "),
  );
</script>

<div class="screen">
  <header>
    <h1>Waiting</h1>
    <p class="sub">
      {#if !waiting.list}
        {" "}
      {:else if count}
        <b>{count}</b> waiting on you
      {:else}
        All caught up
      {/if}
    </p>
  </header>

  {#if waiting.failure && waiting.list}
    <p class="banner" role="status">{waiting.failure} - showing what waited when last asked.</p>
  {/if}

  <main
    bind:this={scroller}
    ontouchstart={(event) => pull.start(event, scroller)}
    ontouchmove={(event) => pull.move(event)}
    ontouchend={() => pull.end()}
    style:--pulled="{pull.pulled}px"
  >
    <div class="pull" class:ready={pull.pulled >= PULL_PX} aria-hidden="true">
      <svg class:spin={isLoading && pull.pulled > 0} width="18" height="18" viewBox="0 0 24 24" fill="none"
        stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12a9 9 0 1 1-3-6.7L21 8" /><path d="M21 3v5h-5" /></svg>
    </div>

    {#if !waiting.list}
      {#if waiting.failure}
        <div class="state">
          <p>{waiting.failure}.</p>
          <button type="button" class="ghost" onclick={() => refresh()}>Try again</button>
        </div>
      {:else}
        <div class="state" aria-busy="true">
          <div class="skeleton"></div>
          <div class="skeleton"></div>
        </div>
      {/if}
    {:else if !count}
      <div class="state empty">
        <svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"
          stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
          ><circle cx="12" cy="12" r="9" /><path d="M8.5 12.5l2.5 2.5 4.5-5.5" /></svg>
        <p>Nothing is waiting on you.</p>
        <p class="hint">An agent asking, a PR whose checks fail and an agent that dies, in every workspace, wait here until they are dealt with.</p>
      </div>
    {:else}
      <div class="list">
        {#each waiting.list.entries as entry (entry.key)}
          <div animate:flip={{ duration: 250 }}>
            <button type="button" class="entry" onclick={() => openEntry(entry)}
              aria-label="tree {entry.tree}{isElsewhere(entry) ? ` in ${entry.workspace}` : ''}, {labels[entry.kind]}">
              <span class="mark {entry.kind}" aria-hidden="true">
                {#if entry.kind === "needs_you"}
                  <svg viewBox="0 0 16 16" width="16" height="16"><circle cx="8" cy="8" r="6.5" fill="currentColor" /></svg>
                {:else}
                  <svg viewBox="0 0 16 16" width="16" height="16"><path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor"
                      stroke-width="2" stroke-linecap="round" /></svg>
                {/if}
              </span>
              <span class="body">
                <span class="top">
                  <span class="tree mono">{entry.tree}</span>
                  <span class="title">{entry.title ?? "no branch"}</span>
                  {#if entry.ago !== null}<span class="when">{ago(entry.ago)}</span>{/if}
                </span>
                <span class="what">{entry.what || labels[entry.kind]}</span>
                {#if isElsewhere(entry)}
                  <span class="where mono">{entry.workspace}</span>
                {/if}
              </span>
              <svg class="chevron" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
            </button>
          </div>
        {/each}
      </div>
    {/if}
    {#if waiting.list && also}
      <p class="also"><span class="tick" aria-hidden="true">✓</span> Also: {also}</p>
    {/if}
  </main>
</div>

<style>
  main > :global(*) {
    flex-shrink: 0;
  }
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  header {
    flex: none;
    padding: calc(var(--safe-top) + 12px) 16px 12px;
    border-bottom: 1px solid var(--bg-high);
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .sub {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--muted);
  }
  .sub b {
    font-weight: 600;
    color: var(--yellow);
  }
  .banner {
    margin: 0;
    padding: 8px 16px;
    font-size: 13px;
    background: var(--yellow-bg);
    color: var(--yellow);
  }
  main {
    flex: 1;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    display: flex;
    flex-direction: column;
    gap: 16px;
    padding: 16px 16px 24px;
    transform: translateY(var(--pulled));
    transition: transform 0.2s;
  }
  .pull {
    position: absolute;
    left: 0;
    right: 0;
    top: -34px;
    display: flex;
    justify-content: center;
    color: var(--muted);
    opacity: min(1, calc(var(--pulled) / 70px));
  }
  .pull.ready {
    color: var(--green);
  }
  .spin {
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .list {
    display: flex;
    flex-direction: column;
    border-radius: 14px;
    background: var(--bg-raised);
    overflow: hidden;
  }
  .list > :global(* + *) {
    border-top: 1px solid var(--line);
  }
  .entry {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    width: 100%;
    min-height: 64px;
    padding: 12px 12px 12px 14px;
    border: 0;
    background: transparent;
    color: var(--fg);
    font: inherit;
    text-align: left;
  }
  .entry:active {
    background: var(--bg-high);
  }
  .mark {
    flex: none;
    display: flex;
    padding-top: 2px;
  }
  .mark.needs_you {
    color: var(--yellow);
  }
  .mark.failed,
  .mark.died {
    color: var(--red);
  }
  .body {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .top {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .tree {
    flex: none;
    font-size: 14px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 15px;
    font-weight: 500;
    color: var(--fg-strong);
  }
  .when {
    flex: none;
    font-size: 12px;
    color: var(--muted);
  }
  .what {
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
    font-size: 14px;
    line-height: 1.35;
    color: var(--fg);
  }
  .where {
    align-self: flex-start;
    padding: 1px 8px;
    border-radius: 9px;
    background: var(--bg-high);
    color: var(--muted);
    font-size: 11px;
  }
  .chevron {
    flex: none;
    align-self: center;
    color: var(--line-strong);
  }
  .also {
    margin: 0;
    padding: 0 4px;
    font-size: 13px;
    line-height: 1.4;
    color: var(--muted);
  }
  .tick {
    color: var(--green);
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
  .empty {
    align-items: center;
    padding-top: 64px;
    text-align: center;
  }
  .empty p:first-of-type {
    color: var(--fg-strong);
    font-weight: 600;
  }
  .hint {
    max-width: 280px;
    font-size: 13px;
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
    height: 64px;
    border-radius: 14px;
    background: var(--bg-raised);
    animation: pulse 1.4s ease-in-out infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.5;
    }
  }
</style>
