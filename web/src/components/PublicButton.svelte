<script lang="ts">
  import { goPublic } from "../ports.svelte";

  /**
   * Puts a port a tree serves on the internet and copies its link, to send
   * to someone off the tailnet; where it is public already, at `url`, a tap
   * sends that link on.
   */
  let { port, url }: { port: number; url: string | null } = $props();
</script>

<button
  type="button"
  class="public"
  class:on={url !== null}
  aria-label={url ? `Send the public link to port ${port}` : `Get a public link to port ${port}`}
  onclick={() => goPublic(port, url)}
>
  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
    stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <circle cx="12" cy="12" r="9" /><path d="M3 12h18M12 3c2.5 2.6 3.8 5.6 3.8 9s-1.3 6.4-3.8 9c-2.5-2.6-3.8-5.6-3.8-9S9.5 5.6 12 3z" />
  </svg>
  <span>{url ? "Public" : "Public link"}</span>
</button>

<style>
  .public {
    position: relative;
    flex: none;
    display: inline-flex;
    white-space: nowrap;
    align-items: center;
    gap: 5px;
    min-height: 26px;
    padding: 0 9px;
    border-radius: 13px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--muted);
    font-size: 12px;
    font-weight: 400;
    letter-spacing: 0;
    text-transform: none;
  }
  /* Drawn small to sit in a row of text, and taps as a thumb does. */
  .public::before {
    content: "";
    position: absolute;
    inset: -9px -3px;
  }
  .public:active {
    background: var(--line-strong);
  }
  .public.on {
    border-color: var(--green);
    background: var(--green-bg);
    color: var(--green);
  }
  svg {
    flex: none;
  }
</style>
