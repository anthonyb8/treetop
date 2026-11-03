<script lang="ts">
  import type { Git } from "../api";

  let { git }: { git: Git | "unknown" | null } = $props();
</script>

{#if git === "unknown"}
  <span class="mono unknown" title="git could not read this tree">─</span>
{:else if git}
  {#if git.unpushed}<span class="mono" style:color="var(--green)" aria-label="{git.unpushed} unpushed">⇡{git.unpushed}</span>{/if}
  {#if git.staged}<span class="mono" style:color="var(--yellow)" aria-label="{git.staged} staged">+{git.staged}</span>{/if}
  {#if git.unstaged}<span class="mono" style:color="var(--orange)" aria-label="{git.unstaged} unstaged">!{git.unstaged}</span>{/if}
  {#if git.untracked}<span class="mono" style:color="var(--blue)" aria-label="{git.untracked} untracked">?{git.untracked}</span>{/if}
{/if}

<style>
  span {
    font-size: 12px;
    white-space: nowrap;
  }
  .unknown {
    color: var(--yellow);
  }
</style>
