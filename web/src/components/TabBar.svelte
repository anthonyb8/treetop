<script lang="ts">
  import { waiting } from "../waiting.svelte";
  import { link, nav } from "../nav.svelte";

  /** The screens treetop has, as Tab switches them in the terminal, what
   *  waits on you in every workspace, and what the screen on show starts,
   *  one tap away. */
  let { tab }: { tab: "trees" | "waiting" | "issues" } = $props();

  let canAdd = $derived(tab !== "issues" || nav.canCreate);
  let count = $derived(waiting.list?.entries.length ?? 0);
</script>

<nav aria-label="Screens">
  <a class="tab" class:on={tab === "trees"} href="/" onclick={link} aria-current={tab === "trees" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
      stroke-linecap="round" aria-hidden="true"><path d="M10 4h4M8 8h8M6 12h12M4 16h16M12 19.5v1.5" /></svg>
    <span>Trees</span>
    {#if nav.needs > 0}<span class="badge" aria-label="{nav.needs} waiting on you">{nav.needs}</span>{/if}
  </a>
  <a class="tab" class:on={tab === "waiting"} href="/waiting" onclick={link} aria-current={tab === "waiting" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
      ><circle cx="12" cy="12" r="8.5" /><circle cx="12" cy="12" r="3" /></svg>
    <span>Waiting</span>
    {#if count > 0}<span class="badge" aria-label="{count} waiting in every workspace">{count}</span>{/if}
  </a>
  <a class="tab" class:on={tab === "issues"} href="/issues" onclick={link} aria-current={tab === "issues" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9" /><path d="M8.5 12.5l2.5 2.5 4.5-5.5" /></svg>
    <span>Issues</span>
  </a>
  {#if canAdd}
    <a class="add" href={tab === "issues" ? "/issues/new" : "/new"} onclick={link}>
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
        stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M5 12h14" /></svg>
      {tab === "issues" ? "New issue" : "New agent"}
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
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    min-height: 48px;
    padding: 0 22px;
    border-radius: 24px;
    background: var(--green);
    color: var(--on-accent);
    font-size: 16px;
    font-weight: 600;
  }
</style>
