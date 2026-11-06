<script lang="ts">
  import { flip } from "svelte/animate";
  import { ApiError, type InboxEntry, type InboxKind } from "../api";
  import SwipeRow from "../components/SwipeRow.svelte";
  import { dismissEntry, inbox, loadInbox, openEntry } from "../inbox.svelte";
  import { Pull, PULL_PX } from "../pull.svelte";
  import { ago } from "../time";

  /** The headings unseen entries go under, as the desktop's inbox has them. */
  const headings: Record<InboxKind, string> = {
    needs_you: "Needs you",
    failed: "Failed",
    died: "Failed",
    done: "Done",
    merged: "Done",
  };

  let dismissFailure = $state<string | null>(null);
  let scroller: HTMLElement | undefined = $state();
  let isLoading = $state(false);

  /** The entries in runs under one heading each, in the order given. */
  let sections = $derived.by(() => {
    const out: { heading: string; entries: InboxEntry[] }[] = [];
    for (const entry of inbox.list?.entries ?? []) {
      const heading = entry.seen ? "Seen" : headings[entry.kind];
      const last = out.at(-1);
      if (last?.heading === heading) last.entries.push(entry);
      else out.push({ heading, entries: [entry] });
    }
    return out;
  });
  let needs = $derived(inbox.list?.entries.filter((e) => !e.seen && e.kind === "needs_you").length ?? 0);

  async function refresh() {
    isLoading = true;
    await loadInbox();
    isLoading = false;
  }
  const pull = new Pull(() => void refresh());

  async function dismiss(entry: InboxEntry) {
    dismissFailure = null;
    try {
      await dismissEntry(entry);
    } catch (err) {
      dismissFailure = err instanceof ApiError ? err.message : "It did not go through";
    }
  }

  /** What the entry's mark says, for a screen reader. */
  const labels: Record<InboxKind, string> = {
    needs_you: "needs you",
    failed: "PR failed",
    died: "agent died",
    done: "done",
    merged: "PR merged",
  };
</script>

<div class="screen">
  <header>
    <h1>Inbox</h1>
    <p class="sub">
      {#if !inbox.list}
        {" "}
      {:else if needs}
        <b>{needs}</b> need{needs === 1 ? "s" : ""} you{#if inbox.list.unseen > needs}{" · "}{inbox.list.unseen - needs} more new{/if}
      {:else if inbox.list.unseen}
        {inbox.list.unseen} new
      {:else}
        All caught up
      {/if}
    </p>
  </header>

  {#if inbox.failure && inbox.list}
    <p class="banner" role="status">{inbox.failure} - showing the inbox as it last was.</p>
  {/if}
  {#if dismissFailure}<p class="banner failed" role="alert">{dismissFailure}</p>{/if}

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

    {#if !inbox.list}
      {#if inbox.failure}
        <div class="state">
          <p>{inbox.failure}.</p>
          <button type="button" class="ghost" onclick={() => refresh()}>Try again</button>
        </div>
      {:else}
        <div class="state" aria-busy="true">
          <div class="skeleton"></div>
          <div class="skeleton"></div>
        </div>
      {/if}
    {:else if !inbox.list.entries.length}
      <div class="state empty">
        <svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6"
          stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
          ><path d="M3 13h5l1.5 3h5L16 13h5" /><path d="M5.5 5h13L21 13v6H3v-6z" /></svg>
        <p>Nothing needs you.</p>
        <p class="hint">Agents that ask, finish or die, and PRs that fail or merge, in every workspace, show here.</p>
      </div>
    {:else}
      {#each sections as section, i (`${section.heading}-${i}`)}
        <section aria-label={section.heading}>
          <h2 class={section.heading === "Seen" ? "" : section.entries[0]!.kind}>{section.heading}</h2>
          <div class="list">
            {#each section.entries as entry (entry.key)}
              <div animate:flip={{ duration: 250 }}>
                <SwipeRow label="Dismiss" onswipe={() => dismiss(entry)}>
                  <button type="button" class="entry" class:seen={entry.seen} onclick={() => openEntry(entry)}
                    aria-label="tree {entry.tree}{entry.workspace ? ` in ${entry.workspace}` : ''}, {labels[entry.kind]}">
                    <span class="mark {entry.kind}" aria-hidden="true">
                      {#if entry.kind === "needs_you"}
                        <svg viewBox="0 0 16 16" width="16" height="16"><circle cx="8" cy="8" r="6.5" fill="currentColor" /></svg>
                      {:else if entry.kind === "done" || entry.kind === "merged"}
                        <svg viewBox="0 0 16 16" width="16" height="16"><path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor"
                            stroke-width="2" stroke-linecap="round" stroke-linejoin="round" /></svg>
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
                      {#if entry.workspace}
                        <span class="where mono">{entry.workspace}</span>
                      {/if}
                    </span>
                    <svg class="chevron" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                      stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
                  </button>
                </SwipeRow>
              </div>
            {/each}
          </div>
        </section>
      {/each}
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
  .banner.failed {
    background: var(--red-bg);
    color: var(--red);
  }
  main {
    flex: 1;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    display: flex;
    flex-direction: column;
    gap: 20px;
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
  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h2 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  h2.needs_you {
    color: var(--yellow);
  }
  h2.failed,
  h2.died {
    color: var(--red);
  }
  h2.done,
  h2.merged {
    color: var(--green);
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
  .mark.done,
  .mark.merged {
    color: var(--green);
  }
  .seen .mark,
  .seen .what,
  .seen .title {
    opacity: 0.6;
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
