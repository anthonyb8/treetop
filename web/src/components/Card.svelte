<script lang="ts">
  import type { Card, Mark as MarkKind } from "../api";
  import { link } from "../nav.svelte";
  import { ago } from "../time";
  import GitCounts from "./GitCounts.svelte";
  import Mark from "./Mark.svelte";
  import PortButton from "./PortButton.svelte";
  import PrBadge from "./PrBadge.svelte";

  let { card, loud = false }: { card: Card; loud?: boolean } = $props();

  // What a quiet card says under its title when its agent said nothing: the
  // state its mark draws, not the agent's name over again.
  const states: Record<MarkKind, string | null> = {
    waiting: "waiting on you",
    busy: "working",
    idle: "idle",
    cold: "cold",
    done: "done",
    failed: "its process died",
    unknown: null,
    "no-agent": "no agent",
    none: null,
  };
  let line = $derived(card.says ?? states[card.mark] ?? card.agent ?? card.branch ?? "");
  let href = $derived(`/trees/${encodeURIComponent(card.tree)}`);
</script>

{#if loud}
  <div class="card loud">
    <a class="open" {href} onclick={link} aria-label="tree {card.tree}">
      <div class="top">
        <Mark mark={card.mark} />
        <span class="tree loud-tree mono">{card.tree}</span>
        <span class="title">{card.title ?? card.branch ?? "no branch"}</span>
        {#if card.pr}<PrBadge pr={card.pr} />{/if}
      </div>
      {#if card.says}<p class="question">{card.says}</p>{/if}
    </a>
    <div class="meta">
      <GitCounts git={card.git} />
      {#each card.ports as port (port)}<PortButton {port} />{/each}
      {#if card.active_ago !== null}<span class="when">{ago(card.active_ago)}</span>{/if}
      <span class="answer" aria-hidden="true">Answer ›</span>
    </div>
  </div>
{:else}
  <div class="card quiet">
    <a class="open" {href} onclick={link} aria-label="tree {card.tree}">
      <div class="top">
        <Mark mark={card.mark} />
        <span class="tree mono">{card.tree}</span>
        <span class="title">{card.title ?? card.branch ?? "no branch"}</span>
      </div>
      <p class="line">{line}{#if card.more} <span class="more">+{card.more}</span>{/if}</p>
    </a>
    {#if card.active_ago !== null}<span class="when side">{ago(card.active_ago)}</span>{/if}
    <div class="meta side">
      <GitCounts git={card.git} />
      {#each card.ports as port (port)}<PortButton {port} />{/each}
      {#if card.pr}<PrBadge pr={card.pr} />{/if}
    </div>
  </div>
{/if}

<style>
  .card {
    position: relative;
    padding: 13px 14px;
    min-width: 0;
  }
  .card:has(.open:active) {
    background: var(--bg-high);
  }
  .open {
    color: inherit;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  /* The whole card opens its tree, but for the ports on it. */
  .open::after {
    content: "";
    position: absolute;
    inset: 0;
  }
  /* A quiet card is two lines: what it is and what it says on the left,
     when and what it serves on the right. */
  .quiet {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    column-gap: 10px;
  }
  .quiet .open {
    grid-column: 1;
    grid-row: 1 / 3;
  }
  .side {
    grid-column: 2;
    justify-self: end;
  }
  .when.side {
    grid-row: 1;
    align-self: center;
  }
  .meta.side {
    grid-row: 2;
    align-self: end;
  }
  .loud {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    border-radius: 16px;
    background: var(--yellow-bg);
    border: 1px solid var(--yellow);
  }
  .loud .open {
    gap: 10px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .tree {
    flex: none;
    font-size: 14px;
    color: var(--muted);
  }
  .loud-tree {
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
  .question {
    margin: 0;
    font-size: 15px;
    line-height: 1.45;
    color: var(--fg-strong);
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .line {
    margin: 0;
    padding-left: 24px;
    font-size: 13px;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .more {
    color: var(--fg);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: var(--muted);
  }
  .meta :global(.port) {
    z-index: 1;
  }
  .when {
    flex: none;
    font-size: 12px;
    color: var(--muted);
  }
  .answer {
    margin-left: auto;
    font-size: 13px;
    font-weight: 600;
    color: var(--yellow);
  }
</style>
