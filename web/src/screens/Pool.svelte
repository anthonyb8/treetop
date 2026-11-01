<script lang="ts">
  import { flip } from "svelte/animate";
  import { ApiError, fetchPool, fetchWorkspaces, type Pool } from "../api";
  import Card from "../components/Card.svelte";
  import GitCounts from "../components/GitCounts.svelte";
  import { remember, recall } from "../store";

  /** How often the pool is asked for while the screen is open. */
  const EVERY_MS = 5000;
  /** How far a pull at the top must go before it refreshes. */
  const PULL_PX = 70;

  const chosen = recall<string>("workspace");
  let workspace = $state<string | null>(chosen);
  let workspaces = $state<string[]>([]);
  let pool = $state<Pool | null>(recall<Pool>(poolKey(chosen)));
  // Why the last ask failed; the pool on show is then the last one seen.
  let failure = $state<string | null>(null);
  let isLoading = $state(false);
  let isParkedOpen = $state(false);
  let pulled = $state(0);
  let scroller: HTMLElement | undefined = $state();

  function poolKey(name: string | null): string {
    return `pool:${name ?? ""}`;
  }

  async function load() {
    isLoading = true;
    try {
      const fresh = await fetchPool(workspace);
      pool = fresh;
      failure = null;
      remember(poolKey(workspace), fresh);
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
      if (workspace && !list.workspaces.includes(workspace)) workspace = null;
    } catch {
      // The pool's own failure says why; the switcher just stays as it was.
    }
  }

  function choose(name: string) {
    workspace = name;
    remember("workspace", name);
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

  // Pull down at the top to refresh, as iOS lists do.
  let startY: number | null = null;
  function onTouchStart(event: TouchEvent) {
    startY = scroller && scroller.scrollTop <= 0 ? (event.touches[0]?.clientY ?? null) : null;
  }
  function onTouchMove(event: TouchEvent) {
    if (startY === null) return;
    const dy = (event.touches[0]?.clientY ?? startY) - startY;
    pulled = Math.max(0, Math.min(dy * 0.5, PULL_PX * 1.4));
  }
  function onTouchEnd() {
    if (pulled >= PULL_PX) void load();
    pulled = 0;
    startY = null;
  }

  let isEmpty = $derived(
    pool !== null &&
      pool.needs_you.length === 0 &&
      pool.groups.every((g) => g.cards.length === 0) &&
      pool.parked.length === 0,
  );
  let shownName = $derived(pool?.workspace ?? workspace ?? "treetop");
</script>

<div class="screen">
  <header>
    <div class="heading">
      <h1 class="mono">treetop</h1>
      <p class="sub">
        {#if pool}
          {pool.needs_you.length} need{pool.needs_you.length === 1 ? "s" : ""} you · {pool.free} free
        {:else}
          &nbsp;
        {/if}
      </p>
    </div>
    <label class="switcher">
      <span class="visually-hidden">Workspace</span>
      <span class="current">{shownName}</span>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
        stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
      <select value={shownName} onchange={(e) => choose(e.currentTarget.value)} disabled={workspaces.length < 2}>
        {#each workspaces.length ? workspaces : [shownName] as name (name)}
          <option value={name}>{name}</option>
        {/each}
      </select>
    </label>
  </header>

  {#if failure && pool}
    <p class="banner" role="status">{failure} - showing the pool as it last was.</p>
  {:else if pool?.is_stale}
    <p class="banner quiet" role="status">Listing the pool…</p>
  {/if}

  <main
    bind:this={scroller}
    ontouchstart={onTouchStart}
    ontouchmove={onTouchMove}
    ontouchend={onTouchEnd}
    style:--pulled="{pulled}px"
  >
    <div class="pull" class:ready={pulled >= PULL_PX} aria-hidden="true">
      <svg class:spin={isLoading && pulled > 0} width="18" height="18" viewBox="0 0 24 24" fill="none"
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
              <div animate:flip={{ duration: 250 }}><Card {card} loud /></div>
            {/each}
          </div>
        </section>
      {/if}

      {#each pool.groups as group (group.base)}
        {#if group.cards.length || group.checkout}
          <section aria-label={group.base || "trees"}>
            <h2>
              <span>{group.base || "trees"}</span>
              {#if group.checkout}
                <span class="checkout">
                  main checkout
                  <GitCounts git={group.checkout.git} />
                  {#each group.checkout.ports as port (port)}<span class="mono port">:{port}</span>{/each}
                </span>
              {/if}
            </h2>
            {#if group.cards.length}
              <div class="list">
                {#each group.cards as card (card.tree)}
                  <div animate:flip={{ duration: 250 }}><Card {card} /></div>
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

      <section class="tail">
        <p class="free">{pool.free} free {pool.free === 1 ? "tree" : "trees"}</p>
        {#if pool.parked.length}
          <button type="button" class="fold" aria-expanded={isParkedOpen} onclick={() => (isParkedOpen = !isParkedOpen)}>
            <span>Parked · {pool.parked.length}</span>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" aria-hidden="true"><path d={isParkedOpen ? "M6 15l6-6 6 6" : "M6 9l6 6 6-6"} /></svg>
          </button>
          {#if isParkedOpen}
            <div class="list">
              {#each pool.parked as card (card.tree)}<Card {card} />{/each}
            </div>
          {/if}
        {/if}
      </section>
    {/if}
  </main>
</div>

<style>
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: calc(var(--safe-top) + 12px) 16px 12px;
    background: var(--bg-raised);
  }
  .heading {
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  .sub {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--muted);
  }
  .switcher {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 44px;
    max-width: 50%;
    padding: 0 12px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 14px;
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
    padding: 16px 16px calc(var(--safe-bottom) + 24px);
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
  .checkout {
    display: flex;
    gap: 8px;
    align-items: baseline;
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
  }
  .port {
    color: var(--blue);
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
    gap: 10px;
  }
  .free {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
  }
  .fold {
    display: flex;
    align-items: center;
    justify-content: space-between;
    min-height: 44px;
    padding: 0 14px;
    border-radius: 12px;
    border: 1px solid var(--line);
    background: transparent;
    color: var(--fg);
    font-size: 14px;
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
