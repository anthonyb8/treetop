<script lang="ts">
  import {
    ApiError,
    changeIssue,
    commentOnIssue,
    fetchIssue,
    fetchIssueSources,
    fetchIssueTeam,
    fetchPool,
    pickIssue,
    type IssueChange,
    type IssueDetail,
    type IssueRow,
    type IssueTeam,
  } from "../api";
  import Choices from "../components/Choices.svelte";
  import Mark from "../components/Mark.svelte";
  import Priority from "../components/Priority.svelte";
  import Sheet from "../components/Sheet.svelte";
  import { secondsSince } from "../issues";
  import { markdown } from "../markdown";
  import { back, go, link, nav } from "../nav.svelte";
  import { recall, remember } from "../store";
  import { ago } from "../time";

  let { id }: { id: string } = $props();

  const EVERY_MS = 30000;

  let detail = $state<IssueDetail | null>(null);
  // undefined until read; null for a source that cannot change its issues.
  let team = $state<IssueTeam | null | undefined>(undefined);
  let source = $state<string | null>(null);
  let failure = $state<string | null>(null);
  // Why the last change did not go through.
  let refused = $state<string | null>(null);
  let sheet = $state<"status" | "assignee" | "labels" | "comment" | "pickup" | null>(null);
  let labels = $state<string[]>([]);
  let comment = $state("");
  let isSending = $state(false);
  let bases = $state<string[]>([]);
  let base = $state("");
  let isPicking = $state(false);
  let pickFailure = $state<string | null>(null);
  let changing = 0;

  /** The source the list had on show, or the repository's own. */
  async function resolveSource(): Promise<string | null> {
    if (nav.source) return nav.source;
    return (await fetchIssueSources(nav.workspace)).own;
  }

  async function load() {
    try {
      source ??= await resolveSource();
      const fresh = await fetchIssue(nav.workspace, source, id);
      // A change on its way answers for itself.
      if (changing === 0) detail = fresh;
      failure = null;
    } catch (err) {
      failure = err instanceof ApiError ? err.message : "Something went wrong";
    }
  }

  async function loadTeam() {
    try {
      source ??= await resolveSource();
      team = await fetchIssueTeam(nav.workspace, source);
    } catch {
      // The issue reads without it; only changing it needs to know.
    }
  }

  $effect(() => {
    void load();
    void loadTeam();
    comment = recall<string>(`draft:comment:${id}`) ?? "";
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, EVERY_MS);
    return () => clearInterval(timer);
  });

  $effect(() => {
    remember(`draft:comment:${id}`, comment);
  });

  /** Shows `shown` at once and sends `change` behind; a refusal puts the old
   *  value back and says why. */
  async function apply(what: string, shown: Partial<IssueRow>, change: IssueChange) {
    if (!detail) return;
    const before = detail.issue;
    detail = { ...detail, issue: { ...before, ...shown } };
    sheet = null;
    refused = null;
    changing += 1;
    try {
      const row = await changeIssue(nav.workspace, source, id, change);
      if (detail) detail = { ...detail, issue: { ...row, is_ready: before.is_ready } };
    } catch (err) {
      if (detail) detail = { ...detail, issue: before };
      refused = `The ${what} did not change: ${err instanceof ApiError ? err.message : "something went wrong"}.`;
    } finally {
      changing -= 1;
    }
  }

  function chooseState(stateId: string) {
    const state = team?.states.find((s) => s.id === stateId);
    if (state && state.id !== detail?.issue.state_id) {
      void apply("status", { state: state.name, state_id: state.id }, { state_id: state.id });
    } else {
      sheet = null;
    }
  }

  function chooseAssignee(key: string) {
    const person = team?.members.find((m) => m.id === key);
    const assignee = person?.id ?? null;
    if (assignee !== detail?.issue.assignee_id) {
      void apply("assignee", { assignee: person?.name ?? null, assignee_id: assignee }, { assignee_id: assignee });
    } else {
      sheet = null;
    }
  }

  function openLabels() {
    labels = [...(detail?.issue.label_ids ?? [])];
    sheet = "labels";
  }

  function toggleLabel(labelId: string) {
    labels = labels.includes(labelId) ? labels.filter((l) => l !== labelId) : [...labels, labelId];
  }

  function applyLabels() {
    const names = (team?.labels ?? []).filter((l) => labels.includes(l.id)).map((l) => l.name);
    void apply("labels", { labels: names, label_ids: labels }, { label_ids: labels });
  }

  async function send() {
    const body = comment.trim();
    if (!body || isSending) return;
    isSending = true;
    refused = null;
    try {
      const posted = await commentOnIssue(nav.workspace, source, id, body);
      if (detail) detail = { ...detail, comments: [...detail.comments, posted] };
      comment = "";
      sheet = null;
    } catch (err) {
      refused = `The comment did not go: ${err instanceof ApiError ? err.message : "something went wrong"}.`;
      sheet = null;
    } finally {
      isSending = false;
    }
  }

  function openPickUp() {
    pickFailure = null;
    sheet = "pickup";
    fetchPool(nav.workspace)
      .then((pool) => {
        bases = pool.bases;
        if (!base && pool.bases[0]) base = pool.bases[0];
      })
      .catch(() => {
        // Without the list the pool's own base is used.
      });
  }

  async function pickUp() {
    if (isPicking) return;
    isPicking = true;
    pickFailure = null;
    try {
      const started = await pickIssue(nav.workspace, source, id, base || null);
      sheet = null;
      go(`/trees/${encodeURIComponent(started.tree)}`);
    } catch (err) {
      pickFailure = err instanceof ApiError ? err.message : "The agent did not start";
    } finally {
      isPicking = false;
    }
  }

  let issue = $derived(detail?.issue ?? null);
  let card = $derived(issue?.card ?? null);
  let canChange = $derived(!!team);
  let where = $derived([issue?.project ?? "No project", issue?.cycle].filter(Boolean).join(" · "));
  let labelsText = $derived(issue?.labels.join(", ") || "No labels");
  let readyWhy = $derived.by(() => {
    const done = (detail?.links ?? []).filter((l) => l.kind === "blocked by" && l.is_done).map((l) => l.id);
    if (done.length === 0) return "Nothing blocks this any more.";
    return `${done.join(", ")} ${done.length === 1 ? "is" : "are"} done, so nothing blocks this.`;
  });
  const said: Record<string, string> = { waiting: "waiting on you", "no-agent": "no agent" };
</script>

<div class="screen">
  <header>
    <div class="top">
      <button type="button" class="back" onclick={back}>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
          stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>
        Issues
      </button>
      <div class="tools">
        {#if canChange}
          <a class="tool" href="/issues/{encodeURIComponent(id)}/edit" onclick={link} aria-label="Edit the issue">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 20h4L19 9l-4-4L4 16z" /><path d="M13 7l4 4" /></svg>
          </a>
        {/if}
        {#if issue?.url}
          <a class="tool" href={issue.url} target="_blank" rel="noreferrer" aria-label="Open in the browser">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 4h6v6" /><path d="M20 4l-9 9" /><path d="M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" /></svg>
          </a>
        {/if}
      </div>
    </div>
    <div class="name">
      <span class="id mono">{id}</span>
      {#if issue}<Priority priority={issue.priority} isWorded />{/if}
    </div>
    {#if issue}<h1>{issue.title}</h1>{/if}
  </header>

  {#if refused}
    <p class="refused" role="alert">{refused}</p>
  {/if}

  <main>
    {#if !detail}
      {#if failure}
        <div class="state">
          <p>{failure}.</p>
          <button type="button" class="ghost" onclick={() => load()}>Try again</button>
        </div>
      {:else}
        <div class="state" aria-busy="true">
          <div class="skeleton tall"></div>
          <div class="skeleton"></div>
        </div>
      {/if}
    {:else if issue}
      {#if failure}
        <p class="banner" role="status">{failure} - showing it as it last was.</p>
      {/if}

      <div class="fields">
        <div class="chips">
          {#if canChange}
            <button type="button" class="chip status" aria-label="Status: {issue.state}" onclick={() => (sheet = "status")}>
              {issue.state}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--muted)" stroke-width="2"
                stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
            </button>
            <button type="button" class="chip" aria-label="Assignee: {issue.assignee ?? 'Nobody'}" onclick={() => (sheet = "assignee")}>
              {issue.assignee ?? "Nobody"}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--muted)" stroke-width="2"
                stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
            </button>
            <button type="button" class="chip labels" aria-label="Labels: {labelsText}" onclick={openLabels}>
              <span class="clip">{labelsText}</span>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--muted)" stroke-width="2"
                stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
            </button>
          {:else}
            <span class="chip fixed status">{issue.state}</span>
            <span class="chip fixed">{issue.assignee ?? "Nobody"}</span>
            <span class="chip fixed labels"><span class="clip">{labelsText}</span></span>
          {/if}
        </div>
        <p class="where">{where}</p>
      </div>

      {#if card}
        <section class="agent" class:loud={card.needs_you} aria-label="Agent">
          <div class="who">
            <Mark mark={card.mark} />
            <span class="tree">Tree {card.tree}</span>
            <span class="said" class:loud={card.needs_you}>· {said[card.mark] ?? card.mark}</span>
            {#if card.active_ago !== null}<span class="when">{ago(card.active_ago)}</span>{/if}
          </div>
          {#if card.says}
            <p class="says">{card.needs_you ? `“${card.says}”` : card.says}</p>
          {/if}
        </section>
      {:else if issue.is_ready}
        <section class="ready" aria-label="Ready">
          <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true"><path d="M3.5 8.5l3 3 6-7" fill="none"
            stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" /></svg>
          <span><strong>Ready.</strong> {readyWhy}</span>
        </section>
      {/if}

      <section aria-label="Description">
        {#if detail.description?.trim()}
          <div class="prose">{@html markdown(detail.description)}</div>
        {:else}
          <p class="none">No description.</p>
        {/if}
      </section>

      {#if detail.links.length}
        <section class="block" aria-label="Links">
          <h2>Links</h2>
          <div class="list">
            {#each detail.links as item (item.kind + item.id)}
              <a class="link" href="/issues/{encodeURIComponent(item.id)}" onclick={link}>
                <span class="line small">
                  <span>{item.kind}</span>
                  {#if item.state}<span class="link-state">{item.state}</span>{/if}
                </span>
                <span class="line">
                  <span class="link-id mono">{item.id}</span>
                  <span class="clip">{item.title}</span>
                </span>
              </a>
            {/each}
          </div>
        </section>
      {/if}

      {#if detail.comments.length}
        <section class="block" aria-label="Comments">
          <h2>Comments · {detail.comments.length}</h2>
          <div class="list">
            {#each detail.comments as item, at (at)}
              <article>
                <div class="comment-top">
                  <span class="author">{item.author}</span>
                  <span class="age">{ago(secondsSince(item.created_at))}</span>
                </div>
                <div class="prose small-prose">{@html markdown(item.body)}</div>
              </article>
            {/each}
          </div>
        </section>
      {/if}
    {/if}
  </main>

  {#if issue}
    <footer>
      {#if canChange}
        <button type="button" class="comment" onclick={() => (sheet = "comment")}>Comment</button>
      {/if}
      {#if card}
        <a class="primary" href="/trees/{encodeURIComponent(card.tree)}" onclick={link}>Open tree {card.tree}</a>
      {:else}
        <button type="button" class="primary" onclick={openPickUp}>Pick up</button>
      {/if}
    </footer>
  {/if}
</div>

{#if sheet === "status"}
  <Sheet title="Status" ondismiss={() => (sheet = null)}>
    <Choices options={(team?.states ?? []).map((s) => ({ key: s.id, label: s.name }))}
      picked={issue?.state_id ? [issue.state_id] : []} onpick={chooseState} />
  </Sheet>
{:else if sheet === "assignee"}
  <Sheet title="Assignee" ondismiss={() => (sheet = null)}>
    <Choices
      options={[...(team?.members ?? []).map((m) => ({ key: m.id, label: m.id === team?.me?.id ? `${m.name} (you)` : m.name })), { key: "", label: "Nobody" }]}
      picked={[issue?.assignee_id ?? ""]} onpick={chooseAssignee} />
  </Sheet>
{:else if sheet === "labels"}
  <Sheet title="Labels" ondismiss={() => (sheet = null)}>
    <Choices options={(team?.labels ?? []).map((l) => ({ key: l.id, label: l.name }))} picked={labels} onpick={toggleLabel} />
    {#snippet footer()}
      <button type="button" class="go" onclick={applyLabels}>Apply</button>
    {/snippet}
  </Sheet>
{:else if sheet === "comment"}
  <Sheet title="Comment" ondismiss={() => (sheet = null)}>
    <div class="field">
      <label for="comment-body">On {id}, in Markdown</label>
      <textarea id="comment-body" rows="5" bind:value={comment}></textarea>
    </div>
    {#snippet footer()}
      <button type="button" class="go" onclick={send} disabled={isSending || !comment.trim()}>
        {isSending ? "Sending…" : "Send"}
      </button>
    {/snippet}
  </Sheet>
{:else if sheet === "pickup"}
  <Sheet title="Pick up {id}" ondismiss={() => (sheet = null)}>
    <p class="explain">An agent starts on {id} in a fresh tree, and the issue shows that tree from then on.</p>
    {#if bases.length > 1}
      <div class="field">
        <label for="pick-base">Base</label>
        <select id="pick-base" class="mono" bind:value={base}>
          {#each bases as name (name)}<option value={name}>{name}</option>{/each}
        </select>
      </div>
    {/if}
    {#if pickFailure}<p class="sheet-failure" role="alert">{pickFailure}</p>{/if}
    {#snippet footer()}
      <button type="button" class="go" onclick={pickUp} disabled={isPicking}>
        {isPicking ? "Starting…" : "Pick up"}
      </button>
    {/snippet}
  </Sheet>
{/if}

<style>
  main > :global(*) {
    flex-shrink: 0;
  }
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  header {
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: calc(var(--safe-top) + 4px) 16px 14px;
    background: var(--bg-raised);
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
  }
  .back {
    display: flex;
    align-items: center;
    gap: 2px;
    min-height: 44px;
    margin-left: -6px;
    padding: 0 6px;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 16px;
  }
  .tools {
    display: flex;
    align-items: center;
    margin-right: -10px;
  }
  .tool {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    color: var(--blue);
  }
  .name {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .id {
    font-size: 15px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 600;
    line-height: 1.25;
    color: var(--fg-strong);
    text-wrap: pretty;
  }
  .refused {
    flex: none;
    margin: 0;
    padding: 8px 16px;
    font-size: 13px;
    background: var(--red-bg);
    color: var(--red);
  }
  .banner {
    margin: 0;
    padding: 8px 12px;
    border-radius: 10px;
    font-size: 13px;
    background: var(--yellow-bg);
    color: var(--yellow);
  }
  main {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 14px 16px 24px;
  }
  .fields {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }
  .chip {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
    max-width: 100%;
    min-height: 44px;
    padding: 0 10px 0 12px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-raised);
    color: var(--fg);
    font-size: 14px;
    font-weight: 500;
  }
  .chip.fixed {
    padding-right: 12px;
    border-color: var(--line);
  }
  .chip.status {
    color: var(--blue);
  }
  .chip.labels {
    color: var(--purple);
  }
  .chip svg {
    flex: none;
  }
  .clip {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .where {
    margin: 0;
    font-size: 13px;
    color: var(--muted);
  }
  .agent {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
    border-radius: 14px;
    background: var(--bg-raised);
  }
  .agent.loud {
    background: var(--yellow-bg);
    border: 1px solid var(--yellow);
  }
  .who {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
    font-size: 14px;
  }
  .tree {
    font-weight: 600;
    color: var(--fg-strong);
  }
  .said {
    color: var(--muted);
  }
  .said.loud {
    color: var(--yellow);
  }
  .when {
    margin-left: auto;
    font-size: 12px;
    color: var(--muted);
  }
  .says {
    margin: 0;
    font-size: 15px;
    line-height: 1.45;
    color: var(--fg-strong);
  }
  .ready {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 14px;
    border-radius: 14px;
    background: var(--green-bg);
    color: var(--green);
    font-size: 14px;
  }
  .ready svg {
    flex: none;
  }
  .ready strong {
    font-weight: 600;
  }
  .none {
    margin: 0;
    font-size: 15px;
    color: var(--muted);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h2 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .list {
    display: flex;
    flex-direction: column;
    border-radius: 14px;
    background: var(--bg-raised);
    overflow: hidden;
  }
  .list > :global(* + *) {
    border-top: 1px solid var(--line);
  }
  .link {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-height: 44px;
    padding: 10px 14px;
    color: inherit;
  }
  .line {
    display: flex;
    gap: 8px;
    min-width: 0;
    font-size: 14px;
  }
  .line.small {
    font-size: 12px;
    color: var(--muted);
  }
  .link-state {
    margin-left: auto;
    color: var(--blue);
  }
  .link-id {
    flex: none;
    font-size: 13px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  article {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px 14px;
  }
  .comment-top {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    font-size: 13px;
  }
  .author {
    font-weight: 600;
    color: var(--fg-strong);
  }
  .age {
    font-size: 12px;
    color: var(--muted);
  }
  .small-prose {
    font-size: 14px;
    line-height: 1.45;
  }
  footer {
    flex: none;
    display: flex;
    gap: 8px;
    padding: 8px 16px var(--bottom);
    border-top: 1px solid var(--bg-high);
    background: var(--bg);
  }
  .comment {
    flex: 1;
    min-height: 46px;
    border-radius: 12px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 16px;
    font-weight: 600;
  }
  .primary {
    flex: 2;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: 46px;
    border: 0;
    border-radius: 12px;
    background: var(--green);
    color: var(--on-accent);
    font-size: 16px;
    font-weight: 600;
  }
  .go {
    width: 100%;
    min-height: 46px;
    border: 0;
    border-radius: 12px;
    background: var(--green);
    color: var(--on-accent);
    font-size: 16px;
    font-weight: 600;
  }
  .go:disabled {
    background: var(--bg-high);
    color: var(--muted);
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 4px;
  }
  label {
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
  }
  textarea,
  select {
    width: 100%;
    padding: 12px;
    border-radius: 12px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 16px;
    line-height: 1.45;
  }
  textarea {
    resize: none;
  }
  select {
    min-height: 48px;
  }
  .explain {
    margin: 4px 0 8px;
    font-size: 15px;
    line-height: 1.45;
    color: var(--fg);
  }
  .sheet-failure {
    margin: 12px 0 0;
    padding: 10px 12px;
    border-radius: 10px;
    background: var(--red-bg);
    color: var(--red);
    font-size: 14px;
  }
  .state {
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 24px 4px;
    color: var(--muted);
    font-size: 15px;
  }
  .state p {
    margin: 0;
  }
  .ghost {
    align-self: flex-start;
    min-height: 44px;
    padding: 0 16px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-weight: 600;
  }
  .skeleton {
    height: 64px;
    border-radius: 14px;
    background: var(--bg-raised);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .skeleton.tall {
    height: 110px;
  }
  @keyframes pulse {
    50% {
      opacity: 0.5;
    }
  }
</style>
