<script lang="ts">
  import type { Snippet } from "svelte";
  import { cubicOut } from "svelte/easing";
  import { fade, fly } from "svelte/transition";

  /** A sheet that rises from the bottom over whatever is on screen. */
  let {
    title,
    dismiss = "Cancel",
    ondismiss,
    onclose,
    children,
    footer,
  }: {
    title: string;
    /** The word on the button at its top right. */
    dismiss?: string;
    ondismiss: () => void;
    /** A tap outside it; `ondismiss` unless said otherwise. */
    onclose?: () => void;
    children: Snippet;
    footer?: Snippet;
  } = $props();
</script>

<div class="backdrop" transition:fade={{ duration: 160 }}>
  <button type="button" class="outside" aria-label="Close" onclick={onclose ?? ondismiss}></button>
  <div class="sheet" role="dialog" aria-modal="true" aria-label={title} transition:fly={{ y: 320, duration: 240, easing: cubicOut, opacity: 1 }}>
    <div class="top">
      <h2>{title}</h2>
      <button type="button" class="dismiss" onclick={ondismiss}>{dismiss}</button>
    </div>
    <div class="body">{@render children()}</div>
    {#if footer}<div class="foot">{@render footer()}</div>{/if}
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 5;
    display: flex;
    flex-direction: column;
    background: rgb(0 0 0 / 0.5);
  }
  .outside {
    flex: 1;
    min-height: 64px;
    border: 0;
    background: none;
  }
  .sheet {
    flex: none;
    display: flex;
    flex-direction: column;
    max-height: calc(100% - 64px);
    padding-bottom: var(--bottom);
    border-radius: 18px 18px 0 0;
    background: var(--bg-raised);
  }
  .top {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 16px 4px;
  }
  h2 {
    margin: 0;
    min-width: 0;
    font-size: 17px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .dismiss {
    flex: none;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 16px;
  }
  .body {
    flex: 0 1 auto;
    min-height: 0;
    overflow-y: auto;
    padding: 0 16px 8px;
  }
  .foot {
    flex: none;
    padding: 4px 16px 0;
  }
</style>
