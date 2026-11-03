<script lang="ts">
  import { flip } from "svelte/animate";
  import { ApiError, fetchPool, fetchWorkspaces, park, type Card as CardData, type Pool } from "../api";
  import Card from "../components/Card.svelte";
  import GitCounts from "../components/GitCounts.svelte";
  import Logo from "../components/Logo.svelte";
  import Notifications from "../components/Notifications.svelte";
  import PortButton from "../components/PortButton.svelte";
  import SwipeRow from "../components/SwipeRow.svelte";
  import { chooseWorkspace, nav } from "../nav.svelte";
  import { Pull, PULL_PX } from "../pull.svelte";
  import { remember, recall } from "../store";

  /** How often the pool is asked for while the screen is open. */
  const EVERY_MS = 5000;

  let workspaces = $state<string[]>([]);
  let pool = $state<Pool | null>(recall<Pool>(poolKey(nav.workspace)));
  // Why the last ask failed; the pool on show is then the last one seen.
  let failure = $state<string | null>(null);
  let isLoading = $state(false);
  let isParkedOpen = $state(false);
  let parkFailure = $state<string | null>(null);
  let scroller: HTMLElement | undefined = $state();

  function poolKey(name: string | null): string {
    return `pool:${name ?? ""}`;
  }

  async function load() {
    isLoading = true;
    try {
      const asked = nav.workspace;
      const fresh = await fetchPool(asked);
      // A switch while this was on its way shows the new workspace, not this.
      if (asked !== nav.workspace) return;
      pool = fresh;
      nav.needs = fresh.needs_you.length;
      failure = null;
      remember(poolKey(asked), fresh);
    } catch (err) {
      failure = err instanceof ApiError ? err.message : "Something went wrong";
    } finally {
      isLoading = false;
    }
  }

  async function loadWorkspaces() {
    try {
      const list = await fetchWorkspaces();
      workspaces = list.workspaces;
      if (nav.workspace && !list.workspaces.includes(nav.workspace)) nav.workspace = null;
    } catch {
      // The pool's own failure says why; the switcher just stays as it was.
    }
  }

  /** Parks a card or brings it back; the row stays gone once the pool says so. */
  async function swipeAside(card: CardData) {
    parkFailure = null;
    try {
      await park(nav.workspace, card.tree, !card.parked);
      await load();
    } catch (err) {
      parkFailure = err instanceof ApiError ? err.message : "It did not go through";
    }
  }

  function choose(name: string) {
    chooseWorkspace(name);
    pool = recall<Pool>(poolKey(name));
    void load();
  }

  // Asks every few seconds while the app is on screen, and at once when it
  // comes back to it.
  $effect(() => {
    void load();
    void loadWorkspaces();
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, EVERY_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  });

  const pull = new Pull(() => void load());

  let isEmpty = $derived(
    pool !== null &&
      pool.needs_you.length === 0 &&
      pool.groups.every((g) => g.cards.length === 0) &&
      pool.parked.length === 0,
  );
  let shownName = $derived(pool?.workspace ?? nav.workspace ?? "treetop");
  // The pool at a glance, as the icon's bars draw it: what waits on you, what
  // else is held, and what is free.
  let held = $derived(pool ? pool.groups.reduce((sum, g) => sum + g.cards.length, 0) : 0);
</script>

<div class="screen">
  <header>
    <div class="brand">
      <Logo />
      <h1 class="mono">treetop</h1>
      <div class="tools">
        <Notifications />
        <label class="switcher">
          <span class="visually-hidden">Workspace</span>
          <span class="current">{shownName}</span>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
          <select value={shownName} onchange={(e) => choose(e.currentTarget.value)} disabled={workspaces.length < 2}>
            {#each workspaces.length ? workspaces : [shownName] as name (name)}
              <option value={name}>{name}</option>
            {/each}
          </select>
        </label>
      </div>
    </div>
    {#if pool}
      <div class="glance">
        <div class="bar" aria-hidden="true">
          {#if pool.needs_you.length}<span class="needs" style:flex-grow={pool.needs_you.length}></span>{/if}
          {#if held}<span class="held" style:flex-grow={held}></span>{/if}
          {#if pool.free}<span class="free-bar" style:flex-grow={pool.free}></span>{/if}
        </div>
        <p class="sub">
          <span><b class="needs-n">{pool.needs_you.length}</b> need{pool.needs_you.length === 1 ? "s" : ""} you</span>
          <span><b class="held-n">{held}</b> held</span>
          <span><b>{pool.free}</b> free</span>
        </p>
      </div>
    {/if}
  </header>

  {#if failure && pool}
    <p class="banner" role="status">{failure} - showing the pool as it last was.</p>
  {:else if pool?.is_stale}
    <p class="banner quiet" role="status">Listing the pool…</p>
  {/if}
  {#if parkFailure}<p class="banner failed" role="alert">{parkFailure}</p>{/if}

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

    {#if !pool}
      {#if failure}
        <div class="state">
          <p>{failure}.</p>
          <button type="button" class="ghost" onclick={() => load()}>Try again</button>
        </div>
      {:else}
        <div class="state" aria-busy="true">
          <div class="skeleton"></div>
          <div class="skeleton"></div>
          <div class="skeleton short"></div>
        </div>
      {/if}
    {:else}
      {#if pool.error}
        <p class="banner error" role="alert">treehouse: {pool.error}</p>
      {/if}

      {#if pool.needs_you.length}
        <section aria-label="Needs you">
          <h2 class="loud">Needs you</h2>
          <div class="stack">
            {#each pool.needs_you as card (card.tree)}
              <div animate:flip={{ duration: 250 }}>
                <SwipeRow label="Park" isRounded onswipe={() => swipeAside(card)}><Card {card} loud /></SwipeRow>
              </div>
            {/each}
          </div>
        </section>
      {/if}

      {#each pool.groups as group (group.base)}
        {#if group.cards.length || group.checkout}
          <section aria-label={group.base || "trees"}>
            <h2 class="base">
              <span class="mono">{group.base || "trees"}</span>
              {#if group.checkout}
                <span class="checkout">
                  main checkout
                  <GitCounts git={group.checkout.git} />
                  {#each group.checkout.ports as port (port)}<PortButton {port} />{/each}
                </span>
              {/if}
            </h2>
            {#if group.cards.length}
              <div class="list">
                {#each group.cards as card (card.tree)}
                  <div animate:flip={{ duration: 250 }}>
                    <SwipeRow label="Park" onswipe={() => swipeAside(card)}><Card {card} /></SwipeRow>
                  </div>
                {/each}
              </div>
            {/if}
          </section>
        {/if}
      {/each}

      {#if isEmpty}
        <div class="state">
          <p>No trees are held in {pool.workspace}.</p>
          <p class="hint">Start an agent from treetop and it shows here.</p>
        </div>
      {/if}

      <section class="tail" aria-label="Free and parked">
        <div class="tail-row">
          <p class="free">{pool.free} free {pool.free === 1 ? "tree" : "trees"}</p>
          {#if pool.parked.length}
            <button type="button" class="fold" aria-expanded={isParkedOpen} onclick={() => (isParkedOpen = !isParkedOpen)}>
              <span>Parked · {pool.parked.length}</span>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
                stroke-linecap="round" aria-hidden="true"><path d={isParkedOpen ? "M6 15l6-6 6 6" : "M6 9l6 6 6-6"} /></svg>
            </button>
          {/if}
        </div>
        {#if isParkedOpen && pool.parked.length}
          <div class="list">
            {#each pool.parked as card (card.tree)}
              <SwipeRow label="Unpark" onswipe={() => swipeAside(card)}><Card {card} /></SwipeRow>
            {/each}
          </div>
        {/if}
      </section>
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
  }
  header {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: calc(var(--safe-top) + 10px) 16px 14px;
    border-bottom: 1px solid var(--bg-high);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
  }
  h1 {
    flex: 1;
    margin: 0;
    font-size: 20px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  .glance {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .bar {
    display: flex;
    gap: 4px;
    height: 6px;
  }
  .bar span {
    flex-basis: 0;
    border-radius: 3px;
  }
  .needs {
    background: var(--yellow);
  }
  .held {
    background: var(--green);
  }
  .free-bar {
    background: var(--line);
  }
  .sub {
    display: flex;
    gap: 14px;
    margin: 0;
    font-size: 13px;
    color: var(--muted);
  }
  .sub b {
    font-weight: 600;
    color: var(--fg-strong);
  }
  .sub .needs-n {
    color: var(--yellow);
  }
  .sub .held-n {
    color: var(--green);
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .switcher {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 36px;
    max-width: 160px;
    padding: 0 12px 0 14px;
    border-radius: 18px;
    border: 1px solid var(--line);
    background: var(--bg-raised);
    color: var(--fg-strong);
    font-size: 15px;
    font-weight: 500;
  }
  .current {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .switcher select {
    position: absolute;
    inset: 0;
    opacity: 0;
    width: 100%;
    font-size: 16px;
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
  .banner.quiet {
    background: var(--bg-raised);
    color: var(--muted);
  }
  .banner.error {
    margin: 0 0 4px;
    border-radius: 10px;
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
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    justify-content: space-between;
    gap: 4px 12px;
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  h2.loud {
    color: var(--yellow);
  }
  /* A base is a branch, so it reads as one. */
  h2.base {
    font-size: 13px;
    font-weight: 500;
    letter-spacing: 0;
    text-transform: none;
  }
  .checkout {
    display: flex;
    gap: 8px;
    align-items: center;
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 8px;
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
  .tail {
    gap: 8px;
  }
  .tail-row {
    display: flex;
    align-items: center;
    min-height: 48px;
    border-radius: 14px;
    background: var(--bg-raised);
  }
  .free {
    flex: 1;
    margin: 0;
    padding: 0 14px;
    font-size: 14px;
    color: var(--muted);
  }
  .fold {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: space-between;
    align-self: stretch;
    padding: 0 14px;
    border: 0;
    border-left: 1px solid var(--line);
    border-radius: 0 14px 14px 0;
    background: transparent;
    color: var(--fg-strong);
    font-size: 14px;
  }
  .fold :global(svg) {
    color: var(--muted);
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
  .hint {
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
    height: 72px;
    border-radius: 14px;
    background: var(--bg-raised);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .skeleton.short {
    height: 48px;
    width: 60%;
  }
  @keyframes pulse {
    50% {
      opacity: 0.5;
    }
  }
</style>
