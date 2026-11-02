<script lang="ts">
  import { link, nav } from "../nav.svelte";

  /** The two screens treetop has, as Tab switches them in the terminal, and
   *  what the screen on show starts, one tap away. */
  let { tab }: { tab: "trees" | "issues" } = $props();

  let canAdd = $derived(tab === "trees" || nav.canCreate);
</script>

<nav aria-label="Screens">
  <a class="tab" class:on={tab === "trees"} href="/" onclick={link} aria-current={tab === "trees" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 22v-6" /><path d="M12 2L6 10h3l-4 6h14l-4-6h3z" /></svg>
    <span>Trees</span>
    {#if nav.needs > 0}<span class="badge" aria-label="{nav.needs} waiting on you">{nav.needs}</span>{/if}
  </a>
  <a class="tab" class:on={tab === "issues"} href="/issues" onclick={link} aria-current={tab === "issues" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M8.5 12.5l2.5 2.5 4.5-5.5" /></svg>
    <span>Issues</span>
  </a>
  {#if canAdd}
    <a class="add" href={tab === "trees" ? "/new" : "/issues/new"} onclick={link}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
        stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
      {tab === "trees" ? "New agent" : "New issue"}
    </a>
  {/if}
</nav>

<style>
  nav {
    flex: none;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px var(--bottom);
    border-top: 1px solid var(--bg-high);
    background: var(--bg);
  }
  .tab {
    position: relative;
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    min-height: 46px;
    color: var(--muted);
    font-size: 11px;
    font-weight: 600;
  }
  .tab.on {
    color: var(--green);
  }
  .badge {
    position: absolute;
    top: 0;
    left: 50%;
    margin-left: 6px;
    min-width: 18px;
    height: 18px;
    padding: 0 5px;
    border-radius: 9px;
    background: var(--yellow);
    color: var(--on-accent);
    font-size: 11px;
    font-weight: 700;
    line-height: 18px;
    text-align: center;
  }
  .add {
    flex: none;
    width: 170px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 46px;
    border-radius: 12px;
    background: var(--green);
    color: var(--on-accent);
    font-size: 16px;
    font-weight: 600;
  }
</style>
