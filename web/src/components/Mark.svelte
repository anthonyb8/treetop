<script lang="ts">
  import type { Mark } from "../api";

  let { mark }: { mark: Mark } = $props();

  // The list's marks: the shape and colour each state has in the terminal,
  // drawn rather than typed so every phone's fonts show them the same size.
  const marks: Record<Mark, { tone: string; label: string }> = {
    waiting: { tone: "var(--yellow)", label: "waiting on you" },
    busy: { tone: "var(--blue)", label: "busy" },
    idle: { tone: "var(--muted)", label: "idle" },
    cold: { tone: "var(--muted)", label: "cold" },
    done: { tone: "var(--green)", label: "done" },
    failed: { tone: "var(--red)", label: "its process died" },
    unknown: { tone: "var(--fg)", label: "agent" },
    "no-agent": { tone: "var(--muted)", label: "no agent" },
    none: { tone: "", label: "" },
  };
  let shown = $derived(marks[mark]);
</script>

{#if mark !== "none"}
  <svg class="mark" viewBox="0 0 16 16" width="16" height="16" role="img" aria-label={shown.label} style:color={shown.tone}>
    {#if mark === "waiting"}
      <circle cx="8" cy="8" r="6.5" fill="currentColor" />
    {:else if mark === "busy"}
      <circle cx="8" cy="8" r="5" fill="currentColor" />
    {:else if mark === "idle"}
      <circle cx="8" cy="8" r="4.75" fill="none" stroke="currentColor" stroke-width="1.5" />
    {:else if mark === "cold"}
      <circle cx="8" cy="8" r="4.75" fill="none" stroke="currentColor" stroke-width="1.5" stroke-dasharray="2 2" />
    {:else if mark === "done"}
      <path d="M3.5 8.5l3 3 6-7" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" />
    {:else if mark === "failed"}
      <path d="M4 4l8 8M12 4l-8 8" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" />
    {:else}
      <circle cx="8" cy="8" r="2" fill="currentColor" />
    {/if}
  </svg>
{/if}

<style>
  .mark {
    flex: none;
    display: block;
  }
</style>
