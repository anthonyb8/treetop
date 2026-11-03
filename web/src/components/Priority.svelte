<script lang="ts">
  import { PRIORITIES } from "../issues";

  /** An issue's priority as three rising bars, Linear's way, filled to its
   *  rank; urgent fills them red. */
  let { priority, isWorded = false }: { priority: number; isWorded?: boolean } = $props();

  // Urgent, high, medium, low: how many bars each fills.
  const filled = [0, 3, 3, 2, 1];
  let word = $derived(priority > 0 ? PRIORITIES[priority]?.toLowerCase() : null);
  let tone = $derived(priority === 1 ? "var(--red)" : priority === 2 ? "var(--yellow)" : "var(--fg)");
</script>

{#if word}
  <span class="priority" role="img" aria-label="{word} priority" title={word} style:--tone={tone}>
    {#each [5, 8, 12] as height, i (i)}
      <span class="bar" class:on={i < filled[priority]!} style:height="{height}px"></span>
    {/each}
  </span>
  {#if isWorded}<span class="word" style:color={tone}>{word}</span>{/if}
{/if}

<style>
  .priority {
    flex: none;
    display: inline-flex;
    align-items: flex-end;
    gap: 2px;
    height: 12px;
  }
  .bar {
    width: 3px;
    border-radius: 1px;
    background: var(--line-strong);
  }
  .bar.on {
    background: var(--tone);
  }
  .word {
    flex: none;
    font-size: 12px;
    font-weight: 500;
  }
</style>
