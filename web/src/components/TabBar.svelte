<script lang="ts">
  import { inbox } from "../inbox.svelte";
  import { link, nav } from "../nav.svelte";

  /** The screens treetop has, as Tab switches them in the terminal, the
   *  inbox the desktop keeps beside the list, and what the screen on show
   *  starts, one tap away. */
  let { tab }: { tab: "trees" | "inbox" | "issues" } = $props();

  let canAdd = $derived(tab !== "issues" || nav.canCreate);
  let unseen = $derived(inbox.list?.unseen ?? 0);
</script>

<nav aria-label="Screens">
  <a class="tab" class:on={tab === "trees"} href="/" onclick={link} aria-current={tab === "trees" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
      stroke-linecap="round" aria-hidden="true"><path d="M10 4h4M8 8h8M6 12h12M4 16h16M12 19.5v1.5" /></svg>
    <span>Trees</span>
    {#if nav.needs > 0}<span class="badge" aria-label="{nav.needs} waiting on you">{nav.needs}</span>{/if}
  </a>
  <a class="tab" class:on={tab === "inbox"} href="/inbox" onclick={link} aria-current={tab === "inbox" ? "page" : undefined}>
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
      stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"
      ><path d="M3 13h5l1.5 3h5L16 13h5" /><path d="M5.5 5h13L21 13v6H3v-6z" /></svg>
    <span>Inbox</span>
    {#if unseen > 0}<span class="badge" aria-label="{unseen} new in the inbox">{unseen}</span>{/if}
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
