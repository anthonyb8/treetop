<script lang="ts">
  import type { Pr, Stage } from "../api";

  let { pr }: { pr: Pr } = $props();

  // As the PR column colours a stage: failures loudest, merged in purple.
  function tone(stage: Stage): [string, string] {
    switch (stage) {
      case "ci_failed":
      case "failed":
        return ["var(--red)", "var(--red-bg)"];
      case "changes":
      case "gate":
        return ["var(--yellow)", "var(--yellow-bg)"];
      case "merged":
        return ["var(--purple)", "var(--purple-bg)"];
      case "approved":
        return ["var(--green)", "var(--green-bg)"];
      case "review":
      case "checks":
      case "running":
        return ["var(--blue)", "var(--bg-high)"];
      default:
        return ["var(--muted)", "var(--bg-high)"];
    }
  }
  let [fg, bg] = $derived(tone(pr.stage));
</script>

<span class="badge mono" style:color={fg} style:background={bg}>{pr.label || pr.stage}</span>

<style>
  .badge {
    flex: none;
    max-width: 9.5em;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
    padding: 3px 8px;
    border-radius: 6px;
  }
</style>
