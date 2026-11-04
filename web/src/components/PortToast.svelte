<script lang="ts">
  import { canShare, copyLink, dismiss, poking, sendLink, stopPublic } from "../ports.svelte";

  let poked = $derived(poking.current);
</script>

{#if poked}
  <div class="toast" class:failed={poked.why} role={poked.why ? "alert" : "status"}>
    {#if poked.why}
      <span class="words">Could not {poked.isPublic ? (poked.url ? "stop" : "publish") : "share"} :{poked.port}: {poked.why}</span>
    {:else if poked.stop === "stopping"}
      <span class="spinner" aria-hidden="true"></span>
      <span class="words">Stopping :{poked.port}…</span>
    {:else if poked.stop === "stopped"}
      <span class="words">:{poked.port} is no longer public</span>
    {:else if poked.isPublic && poked.url}
      <span class="words public">
        <span class="headline">{poked.isCopied ? "Public link copied" : "Public link ready"}</span>
        <span class="mono address">{poked.url}</span>
      </span>
      {#if canShare()}
        <button type="button" class="act primary" aria-label="Share the public link" onclick={sendLink}>Share</button>
      {:else if !poked.isCopied}
        <button type="button" class="act primary" aria-label="Copy the public link" onclick={copyLink}>Copy</button>
      {/if}
      <button type="button" class="act" aria-label="Stop publishing :{poked.port}" onclick={stopPublic}>Stop</button>
    {:else if poked.isPublic}
      <span class="spinner" aria-hidden="true"></span>
      <span class="words">Publishing :{poked.port}…</span>
    {:else if poked.url}
      <a class="open" href={poked.url} target="_blank" rel="noreferrer" onclick={dismiss}>Open :{poked.port}</a>
    {:else}
      <span class="spinner" aria-hidden="true"></span>
      <span class="words">Sharing :{poked.port}…</span>
    {/if}
    <button type="button" class="close" aria-label="Dismiss" onclick={dismiss}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
        stroke-linecap="round" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18" /></svg>
    </button>
  </div>
{/if}

<style>
  .toast {
    position: absolute;
    left: 12px;
    right: 12px;
    bottom: calc(var(--bottom) + 72px);
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 48px;
    padding: 0 4px 0 14px;
    border-radius: 12px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 14px;
    box-shadow: 0 6px 18px rgb(0 0 0 / 0.45);
  }
  .toast.failed {
    border-color: var(--red);
    background: var(--red-bg);
    color: var(--red);
  }
  .words {
    flex: 1;
    min-width: 0;
    padding: 8px 0;
    overflow-wrap: anywhere;
  }
  .public {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .headline {
    font-weight: 600;
  }
  .address {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
    font-size: 12px;
  }
  .act {
    flex: none;
    min-height: 32px;
    padding: 0 12px;
    border-radius: 8px;
    border: 1px solid var(--line-strong);
    background: none;
    color: var(--fg-strong);
    font-size: 14px;
    font-weight: 600;
  }
  .act.primary {
    border-color: var(--blue);
    background: var(--blue);
    color: var(--on-accent);
  }
  .open {
    flex: 1;
    align-self: stretch;
    display: flex;
    align-items: center;
    color: var(--blue);
    font-weight: 600;
  }
  .close {
    flex: none;
    width: 40px;
    height: 40px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 0;
    background: none;
    color: inherit;
  }
  .spinner {
    flex: none;
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid var(--line-strong);
    border-top-color: var(--blue);
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
</style>
