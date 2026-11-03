<script lang="ts">
  import { poke } from "../ports.svelte";

  /** A port a tree serves, which a tap opens on the phone. */
  let { port, process }: { port: number; process?: string } = $props();
</script>

<button type="button" class="port" aria-label="Open port {port}" onclick={() => poke(port)}>
  <span class="mono number">:{port}</span>
  {#if process}<span class="mono process">{process}</span>{/if}
  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.6"
    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M7 17L17 7M8 7h9v9" /></svg>
</button>

<style>
  .port {
    position: relative;
    flex: none;
    max-width: 100%;
    display: inline-flex;
    white-space: nowrap;
    align-items: center;
    gap: 4px;
    min-height: 26px;
    padding: 0 8px;
    border-radius: 13px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--blue);
    font-size: 12px;
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
  }
  /* The chip is drawn small to sit in a row of text, and taps as a thumb does. */
  .port::before {
    content: "";
    position: absolute;
    inset: -9px -3px;
  }
  .port:active {
    background: var(--line-strong);
  }
  .number,
  svg {
    flex: none;
  }
  .process {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
  }
</style>
