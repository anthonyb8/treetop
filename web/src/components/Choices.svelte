<script lang="ts">
  /** A sheet's list of choices, each marked when picked. */
  let {
    options,
    picked,
    onpick,
  }: {
    options: { key: string; label: string }[];
    picked: string[];
    onpick: (key: string) => void;
  } = $props();
</script>

<div class="list">
  {#each options as option (option.key)}
    {@const isPicked = picked.includes(option.key)}
    <button type="button" class="row" class:on={isPicked} aria-pressed={isPicked} onclick={() => onpick(option.key)}>
      {option.label}
      {#if isPicked}
        <svg width="18" height="18" viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 8.5l3 3 6-7" fill="none"
          stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" /></svg>
      {/if}
    </button>
  {/each}
</div>

<style>
  .list {
    display: flex;
    flex-direction: column;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    min-height: 48px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--fg);
    font-size: 16px;
    text-align: left;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
  .row.on {
    color: var(--fg-strong);
    font-weight: 600;
  }
  svg {
    flex: none;
    color: var(--green);
  }
</style>
