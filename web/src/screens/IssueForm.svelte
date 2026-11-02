<script lang="ts">
  import {
    ApiError,
    changeIssue,
    createIssue,
    fetchIssue,
    fetchIssueSources,
    fetchIssueTeam,
    type IssueChange,
    type IssueTeam,
    type Named,
  } from "../api";
  import Choices from "../components/Choices.svelte";
  import Sheet from "../components/Sheet.svelte";
  import { PRIORITIES } from "../issues";
  import { back, go, nav } from "../nav.svelte";
  import { recall, remember } from "../store";

  /** The issue to edit; null writes a new one. The fields are the terminal's. */
  let { id }: { id: string | null } = $props();

  type Pick = "status" | "priority" | "labels" | "assignee" | "project";

  interface Draft {
    title: string;
    description: string;
    state_id: string | null;
    priority: number;
    label_ids: string[];
    assignee_id: string | null;
    project_id: string | null;
  }

  const EMPTY: Draft = {
    title: "",
    description: "",
    state_id: null,
    priority: 0,
    label_ids: [],
    assignee_id: null,
    project_id: null,
  };

  let draft = $state<Draft>({ ...EMPTY });
  // What the issue held when it was opened, which only the changes are sent against.
  let before = $state<Draft | null>(null);
  let team = $state<IssueTeam | null>(null);
  let source = $state<string | null>(null);
  let sheet = $state<Pick | null>(null);
  let isSaving = $state(false);
  let failure = $state<string | null>(null);

  $effect.pre(() => {
    if (!id) draft = recall<Draft>("draft:issue") ?? { ...EMPTY };
  });
  $effect(() => {
    if (!id) remember("draft:issue", draft);
  });

  async function start() {
    try {
      source = nav.source ?? (await fetchIssueSources(nav.workspace)).own;
      team = await fetchIssueTeam(nav.workspace, source);
      if (!id) return;
      const read = await fetchIssue(nav.workspace, source, id);
      const issue = read.issue;
      const held: Draft = {
        title: issue.title,
        description: read.description ?? "",
        state_id: issue.state_id,
        priority: issue.priority,
        label_ids: issue.label_ids,
        assignee_id: issue.assignee_id,
        project_id: issue.project_id,
      };
      before = held;
      draft = { ...held };
    } catch (err) {
      failure = err instanceof ApiError ? err.message : "Something went wrong";
    }
  }

  $effect(() => {
    void start();
  });

  /** The name of `key` in `list`, else `otherwise`. */
  function nameOf(list: Named[] | undefined, key: string | null, otherwise: string): string {
    return list?.find((item) => item.id === key)?.name ?? otherwise;
  }

  let rows = $derived<{ key: Pick; label: string; value: string; tone: string }[]>([
    { key: "status", label: "Status", value: nameOf(team?.states, draft.state_id, "The team's default"), tone: "var(--blue)" },
    { key: "priority", label: "Priority", value: PRIORITIES[draft.priority] ?? "None", tone: "var(--fg)" },
    {
      key: "labels",
      label: "Labels",
      value: (team?.labels ?? []).filter((l) => draft.label_ids.includes(l.id)).map((l) => l.name).join(", ") || "None",
      tone: "var(--purple)",
    },
    { key: "assignee", label: "Assignee", value: nameOf(team?.members, draft.assignee_id, "Nobody"), tone: "var(--fg)" },
    { key: "project", label: "Project", value: nameOf(team?.projects, draft.project_id, "No project"), tone: "var(--fg)" },
  ]);

  let canSave = $derived(!!draft.title.trim() && !isSaving && !!team && (!id || !!before));

  /** What differs from the issue as it was opened, and nothing else. */
  function changes(was: Draft): IssueChange {
    const change: IssueChange = {};
    if (draft.title.trim() !== was.title) change.title = draft.title.trim();
    if (draft.description !== was.description) change.description = draft.description;
    if (draft.state_id && draft.state_id !== was.state_id) change.state_id = draft.state_id;
    if (draft.priority !== was.priority) change.priority = draft.priority;
    if (draft.assignee_id !== was.assignee_id) change.assignee_id = draft.assignee_id;
    if (draft.project_id !== was.project_id) change.project_id = draft.project_id;
    const sameLabels =
      draft.label_ids.length === was.label_ids.length && draft.label_ids.every((l) => was.label_ids.includes(l));
    if (!sameLabels) change.label_ids = draft.label_ids;
    return change;
  }

  async function save(event: SubmitEvent) {
    event.preventDefault();
    if (!canSave) return;
    isSaving = true;
    failure = null;
    try {
      if (id && before) {
        const change = changes(before);
        if (Object.keys(change).length) await changeIssue(nav.workspace, source, id, change);
        back();
        return;
      }
      const made = await createIssue(nav.workspace, source, {
        title: draft.title.trim(),
        description: draft.description.trim() || null,
        state_id: draft.state_id,
        assignee_id: draft.assignee_id,
        label_ids: draft.label_ids,
        project_id: draft.project_id,
        priority: draft.priority || null,
      });
      draft = { ...EMPTY };
      remember("draft:issue", draft);
      go(`/issues/${encodeURIComponent(made.id)}`);
    } catch (err) {
      failure = err instanceof ApiError ? err.message : "It did not save";
    } finally {
      isSaving = false;
    }
  }

  function toggleLabel(label: string) {
    const held = draft.label_ids;
    draft.label_ids = held.includes(label) ? held.filter((l) => l !== label) : [...held, label];
  }

  const none = { key: "", label: "None" };
</script>

<div class="screen">
  <header>
    <button type="button" class="cancel" onclick={back}>Cancel</button>
    <h1>{id ? `Edit ${id}` : "New issue"}</h1>
    <span class="spacer" aria-hidden="true"></span>
  </header>

  <form id="issue-form" onsubmit={save}>
    <div class="field">
      <label for="issue-title">Title</label>
      <input id="issue-title" type="text" placeholder="What needs doing?" bind:value={draft.title} />
    </div>
    <div class="rows">
      {#each rows as row (row.key)}
        <button type="button" class="row" onclick={() => (sheet = row.key)}>
          <span class="label">{row.label}</span>
          <span class="value" style:color={row.tone}>{row.value}</span>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--muted)" stroke-width="2"
            stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
        </button>
      {/each}
    </div>
    <div class="field">
      <label for="issue-description">Description</label>
      <textarea id="issue-description" rows="6" placeholder="Markdown" bind:value={draft.description}></textarea>
    </div>
    {#if failure}<p class="failure" role="alert">{failure}</p>{/if}
  </form>

  <footer>
    <button type="submit" form="issue-form" class="go" disabled={!canSave}>
      {#if isSaving}Saving…{:else if id}Save{:else}Create issue{/if}
    </button>
  </footer>
</div>

{#if sheet === "status"}
  <Sheet title="Status" ondismiss={() => (sheet = null)}>
    <Choices options={(team?.states ?? []).map((s) => ({ key: s.id, label: s.name }))} picked={[draft.state_id ?? ""]}
      onpick={(key) => { draft.state_id = key; sheet = null; }} />
  </Sheet>
{:else if sheet === "priority"}
  <Sheet title="Priority" ondismiss={() => (sheet = null)}>
    <Choices options={PRIORITIES.map((label, at) => ({ key: String(at), label }))} picked={[String(draft.priority)]}
      onpick={(key) => { draft.priority = Number(key); sheet = null; }} />
  </Sheet>
{:else if sheet === "labels"}
  <Sheet title="Labels" dismiss="Done" ondismiss={() => (sheet = null)}>
    <Choices options={(team?.labels ?? []).map((l) => ({ key: l.id, label: l.name }))} picked={draft.label_ids} onpick={toggleLabel} />
  </Sheet>
{:else if sheet === "assignee"}
  <Sheet title="Assignee" ondismiss={() => (sheet = null)}>
    <Choices options={[...(team?.members ?? []).map((m) => ({ key: m.id, label: m.name })), { ...none, label: "Nobody" }]}
      picked={[draft.assignee_id ?? ""]} onpick={(key) => { draft.assignee_id = key || null; sheet = null; }} />
  </Sheet>
{:else if sheet === "project"}
  <Sheet title="Project" ondismiss={() => (sheet = null)}>
    <Choices options={[...(team?.projects ?? []).map((p) => ({ key: p.id, label: p.name })), { ...none, label: "No project" }]}
      picked={[draft.project_id ?? ""]} onpick={(key) => { draft.project_id = key || null; sheet = null; }} />
  </Sheet>
{/if}

<style>
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  header {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: calc(var(--safe-top) + 6px) 16px 10px;
    background: var(--bg-raised);
  }
  .cancel {
    width: 72px;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 16px;
    text-align: left;
  }
  .spacer {
    width: 72px;
  }
  h1 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  form {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 22px;
    padding: 20px 16px;
  }
  form > :global(*) {
    flex-shrink: 0;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  label {
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
  }
  input,
  textarea {
    width: 100%;
    padding: 12px;
    border-radius: 12px;
    border: 1px solid var(--line-strong);
    background: var(--bg-raised);
    color: var(--fg-strong);
    font-size: 16px;
    line-height: 1.45;
  }
  input {
    min-height: 48px;
  }
  textarea {
    resize: none;
  }
  .rows {
    display: flex;
    flex-direction: column;
    border-radius: 14px;
    background: var(--bg-raised);
    overflow: hidden;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    min-height: 48px;
    padding: 0 14px;
    border: 0;
    background: none;
    text-align: left;
  }
  .row + .row {
    border-top: 1px solid var(--line);
  }
  .label {
    flex: none;
    font-size: 15px;
    color: var(--fg-strong);
  }
  .value {
    margin-left: auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 15px;
  }
  .row svg {
    flex: none;
  }
  .failure {
    margin: 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--red-bg);
    color: var(--red);
    font-size: 14px;
  }
  footer {
    flex: none;
    padding: 8px 16px var(--bottom);
    border-top: 1px solid var(--bg-high);
  }
  .go {
    width: 100%;
    min-height: 46px;
    border-radius: 12px;
    border: 0;
    background: var(--green);
    color: var(--on-accent);
    font-size: 16px;
    font-weight: 600;
  }
  .go:disabled {
    background: var(--bg-high);
    color: var(--muted);
  }
</style>
