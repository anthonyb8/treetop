<script lang="ts">
  import type { Card } from "../api";
  import { link } from "../nav.svelte";
  import { ago } from "../time";
  import GitCounts from "./GitCounts.svelte";
  import Mark from "./Mark.svelte";
  import PortButton from "./PortButton.svelte";
  import PrBadge from "./PrBadge.svelte";

  let { card, loud = false }: { card: Card; loud?: boolean } = $props();

  // A card waiting on you leads with what it is asking; the others with
  // what their agent last said, else who works there.
  let isDirty = $derived(
    card.git === "unknown" ||
      (card.git !== null && card.git.unpushed + card.git.staged + card.git.unstaged + card.git.untracked > 0),
  );
  let hasMeta = $derived(isDirty || card.ports.length > 0 || (loud && card.active_ago !== null));
  let line = $derived(
    card.says ?? (card.agent ? card.agent : card.mark === "no-agent" ? "no agent" : (card.branch ?? "")),
  );
</script>

<div class="card" class:loud>
  <a class="open" href="/trees/{encodeURIComponent(card.tree)}" onclick={link} aria-label="tree {card.tree}">
  <div class="top">
    <Mark mark={card.mark} />
    <span class="tree mono">{card.tree}</span>
    <span class="title">{card.title ?? card.branch ?? "no branch"}</span>
    {#if card.pr}<PrBadge pr={card.pr} />{/if}
  </div>
  {#if loud && card.says}
    <p class="question">“{card.says}”</p>
  {:else}
    <div class="row">
      <p class="line">{line}{#if card.more} <span class="more">+{card.more}</span>{/if}</p>
      {#if card.active_ago !== null}<span class="when">{ago(card.active_ago)}</span>{/if}
    </div>
  {/if}
  </a>
  {#if hasMeta}
    <div class="meta">
      <GitCounts git={card.git} />
      {#each card.ports as port (port)}<PortButton {port} />{/each}
      {#if loud && card.active_ago !== null}<span class="when">{ago(card.active_ago)}</span>{/if}
    </div>
  {/if}
</div>

<style>
  .card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px;
    min-width: 0;
  }
  .card:has(.open:active) {
    background: var(--bg-high);
  }
  .open {
    color: inherit;
    display: flex;
    flex-direction: column;
    gap: inherit;
    min-width: 0;
  }
  /* The whole card opens its tree, but for the ports on it. */
  .open::after {
    content: "";
    position: absolute;
    inset: 0;
  }
  .loud {
    gap: 8px;
    border-radius: 14px;
    background: var(--yellow-bg);
    border: 1px solid var(--yellow);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  .tree {
    flex: none;
    font-size: 15px;
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
  }
  .loud .title {
    font-weight: 600;
    color: var(--fg-strong);
  }
  .question {
    margin: 0;
    font-size: 14px;
    line-height: 1.4;
    color: var(--fg-strong);
    display: -webkit-box;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: 12px;
    min-width: 0;
  }
  .line {
    flex: 1;
    min-width: 0;
    margin: 0;
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
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 8px;
    font-size: 12px;
    color: var(--muted);
  }
  .meta :global(.port) {
    z-index: 1;
  }
  .when {
    flex: none;
    margin-left: auto;
    font-size: 12px;
    color: var(--muted);
  }
</style>
