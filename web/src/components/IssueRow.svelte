<script lang="ts">
  import type { IssueRow } from "../api";
  import { workOf } from "../issues";
  import { link } from "../nav.svelte";
  import Mark from "./Mark.svelte";
  import Priority from "./Priority.svelte";

  let {
    issue,
    showStatus,
    showAssignee,
  }: { issue: IssueRow; showStatus: boolean; showAssignee: boolean } = $props();

  let work = $derived(workOf(issue));
  let labels = $derived(issue.labels.join(", "));
  let assignee = $derived(showAssignee ? issue.assignee : null);
</script>

<a class="row" href="/issues/{encodeURIComponent(issue.id)}" onclick={link} aria-label="{issue.id} {issue.title}">
  <div class="top">
    <Priority priority={issue.priority} />
    <span class="id mono">{issue.id}</span>
    {#if work}
      <span class="work {work.tone}">
        {#if work.mark}<Mark mark={work.mark} />{/if}
        {work.text}
      </span>
    {/if}
  </div>
  <span class="title">{issue.title}</span>
  {#if showStatus || labels || assignee}
    <div class="meta">
      {#if showStatus}<span class="state">{issue.state}</span>{/if}
      {#if labels}<span class="labels">{labels}</span>{/if}
      {#if assignee}<span class="assignee">{assignee}</span>{/if}
    </div>
  {/if}
</a>

<style>
  .row {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 12px 14px;
    color: inherit;
  }
  .row:active {
    background: var(--bg-high);
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .id {
    flex: none;
    font-size: 13px;
    font-weight: 500;
    color: var(--muted);
  }
  /* Who works on it, as a pill tinted by how much it wants you. */
  .work {
    margin-left: auto;
    flex: none;
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 2px 9px 2px 7px;
    border-radius: 11px;
    background: var(--bg-high);
    font-size: 12px;
    font-weight: 500;
    color: var(--muted);
  }
  .work.loud {
    background: var(--yellow-bg);
    color: var(--yellow);
  }
  .work.busy {
    color: var(--blue);
  }
  .work.ready {
    padding-left: 9px;
    background: var(--green-bg);
    color: var(--green);
  }
  .title {
    font-size: 15px;
    font-weight: 500;
    color: var(--fg-strong);
    line-height: 1.3;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }
  .meta {
    display: flex;
    align-items: center;
    gap: 10px;
    min-width: 0;
    font-size: 12px;
  }
  .state {
    flex: none;
    color: var(--blue);
  }
  .labels {
    min-width: 0;
    color: var(--purple);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .assignee {
    margin-left: auto;
    flex: none;
    color: var(--muted);
  }
</style>
