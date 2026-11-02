<script lang="ts">
  import { PRIORITIES } from "../issues";

  let { priority }: { priority: number } = $props();

  const tones = ["", "var(--red)", "var(--yellow)", "var(--fg)", "var(--muted)"];
  let word = $derived(priority > 0 ? PRIORITIES[priority]?.toLowerCase() : null);
</script>

{#if word}<span class="priority" class:urgent={priority === 1} style:color={tones[priority]}>{word}</span>{/if}

<style>
  .priority {
    flex: none;
    font-size: 12px;
    font-weight: 500;
  }
  .urgent {
    font-weight: 700;
  }
</style>
