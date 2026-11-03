<script lang="ts">
  import {
    ApiError,
    fetchIssueSources,
    fetchIssueTeam,
    fetchIssues,
    type IssueGroup,
    type IssueList,
    type IssueSources,
    type IssueTeam,
    type IssueView,
  } from "../api";
  import Choices from "../components/Choices.svelte";
  import IssueRow from "../components/IssueRow.svelte";
  import Sheet from "../components/Sheet.svelte";
  import { filterWords, lineOf, type IssueFilters } from "../issues";
  import { chooseSource, nav } from "../nav.svelte";
  import { Pull, PULL_PX } from "../pull.svelte";
  import { recall, remember } from "../store";

  /** Whether this is the screen on show; underneath another it stays as it was. */
  let { isActive }: { isActive: boolean } = $props();

  /** How often the list is asked for while on screen: Linear's own limits
   *  are far above this, and the pool is what changes by the second. */
  const EVERY_MS = 30000;
  /** How long typing rests before a search is asked for. */
  const SEARCH_REST_MS = 250;

  type FilterKey = keyof IssueFilters;

  let sources = $state<IssueSources | null>(recall<IssueSources>("issue-sources"));
  // undefined until read; null for a source that cannot change its issues.
  let team = $state<IssueTeam | null | undefined>(undefined);
  let viewName = $state<string | null>(recall<string>("issue-view"));
  let filters = $state<IssueFilters>({});
  let list = $state<IssueList | null>(null);
  let failure = $state<string | null>(null);
  let isLoading = $state(false);
  let isSearching = $state(false);
  let query = $state("");
  // Groups folded or opened by hand; the others follow whether they are closed.
  let folds = $state<Record<string, boolean>>({});
  let sheet = $state<"filters" | FilterKey | null>(null);
  let scroller: HTMLElement | undefined = $state();

  let shown = $derived<string | null>(
    nav.source && sources?.sources.includes(nav.source) ? nav.source : (sources?.own ?? null),
  );
  let view = $derived<IssueView | undefined>(sources?.views.find((v) => v.name === viewName) ?? sources?.views[0]);
  let line = $derived(isSearching ? `state:any ${query.trim()}` : view ? lineOf(view, filters) : "");
  let group = $derived<IssueGroup>(isSearching ? "none" : (filters.group ?? view?.group ?? "none"));
  let words = $derived([filterWords(filters), filters.group ? `group:${filters.group}` : ""].filter(Boolean).join(" "));
  let isMine = $derived(/(^|\s)mine(\s|$)/.test(line));
  let rows = $derived((list?.sections ?? []).flatMap((section) => section.issues));
  let count = $derived(rows.length);
  let working = $derived(rows.filter((issue) => issue.card).length);
  let isWaitingForWords = $derived(isSearching && !query.trim());

  function cacheKey(source: string, asked: string, grouped: IssueGroup): string {
    return `issues:${nav.workspace ?? ""}:${source}:${asked}:${grouped}`;
  }

  let asked = 0;
  async function load() {
    if (!shown || !view || isWaitingForWords) return;
    const seq = ++asked;
    const [source, lineAsked, groupAsked] = [shown, line, group];
    isLoading = true;
    try {
      const fresh = await fetchIssues(nav.workspace, source, lineAsked, groupAsked);
      if (seq !== asked) return;
      list = fresh;
      failure = null;
      remember(cacheKey(source, lineAsked, groupAsked), fresh);
    } catch (err) {
      if (seq !== asked) return;
      failure = err instanceof ApiError ? err.message : "Something went wrong";
    } finally {
      if (seq === asked) isLoading = false;
    }
  }

  async function loadSources() {
    try {
      const fresh = await fetchIssueSources(nav.workspace);
      sources = fresh;
      remember("issue-sources", fresh);
    } catch (err) {
      if (!sources) failure = err instanceof ApiError ? err.message : "Something went wrong";
    }
  }

  // What the sources are, whenever the screen comes back or the workspace
  // changes.
  $effect(() => {
    if (isActive) void loadSources();
    void nav.workspace;
  });

  // What the source's issues can be set to, for the filters and the tab bar.
  $effect(() => {
    const source = shown;
    team = undefined;
    if (!source) return;
    let isCurrent = true;
    fetchIssueTeam(nav.workspace, source)
      .then((read) => {
        if (!isCurrent) return;
        team = read;
        nav.canCreate = read !== null;
      })
      .catch(() => {
        // The list says what is wrong; the filters stay as they were.
      });
    return () => {
      isCurrent = false;
    };
  });

  // Asks when what is on show changes, every so often while it is on screen,
  // and at once on coming back to it.
  $effect(() => {
    if (!isActive || !shown || !view) return;
    list = isWaitingForWords ? { sections: [] } : recall<IssueList>(cacheKey(shown, line, group));
    failure = null;
    if (isWaitingForWords) return;
    const first = setTimeout(() => void load(), isSearching ? SEARCH_REST_MS : 0);
    const timer = setInterval(() => {
      if (document.visibilityState === "visible") void load();
    }, EVERY_MS);
    const onVisible = () => {
      if (document.visibilityState === "visible") void load();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      clearTimeout(first);
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  });

  const pull = new Pull(() => void load());

  function chooseView(name: string) {
    viewName = name;
    remember("issue-view", name);
    filters = {};
    folds = {};
  }

  function startSearch() {
    isSearching = true;
    query = "";
  }

  function stopSearch() {
    isSearching = false;
    query = "";
  }

  function isFolded(heading: string, isClosed: boolean): boolean {
    return folds[heading] ?? isClosed;
  }

  function setFilter(key: FilterKey, value: string | undefined) {
    filters = { ...filters, [key]: value };
    sheet = "filters";
  }

  function current<K extends FilterKey>(key: K): string {
    return filters[key] ?? "";
  }

  // Each filter's choices, its current word, and how a row says it.
  let choices = $derived.by((): Record<FilterKey, { key: string; label: string }[]> => {
    const named = (list: { name: string }[]) => list.map((n) => ({ key: n.name, label: n.name }));
    return {
      assignee: [
        { key: "any", label: "Anyone" },
        { key: "mine", label: "You" },
        { key: "unassigned", label: "Nobody" },
        ...named((team?.members ?? []).filter((m) => m.id !== team?.me?.id)),
      ],
      state: [{ key: "", label: "Open" }, { key: "any", label: "Any, closed too" }, ...named(team?.states ?? [])],
      label: [{ key: "", label: "Any" }, ...named(team?.labels ?? [])],
      project: [{ key: "", label: "Any" }, ...named(team?.projects ?? [])],
      cycle: [
        { key: "", label: "Any" },
        { key: "current", label: "This cycle" },
        ...(team?.cycles ?? []).map((c) => ({ key: String(c.number), label: c.name ?? `Cycle ${c.number}` })),
      ],
      group: [
        { key: "none", label: "None" },
        { key: "status", label: "Status" },
        { key: "project", label: "Project" },
      ],
    };
  });

  const titles: Record<FilterKey, string> = {
    assignee: "Assignee",
    state: "Status",
    label: "Label",
    project: "Project",
    cycle: "Cycle",
    group: "Group by",
  };

  /** A filter's word: its own, else what the view says. */
  function pickedFilter(key: FilterKey): string {
    if (key === "group") return group;
    if (key === "assignee") return current(key) || (isMine ? "mine" : "any");
    return current(key);
  }

  function shownFilter(key: FilterKey): string {
    const word = pickedFilter(key);
    return choices[key].find((c) => c.key === word)?.label ?? word;
  }

  // A source that cannot say what it holds is grouped, but not filtered by
  // what it does not tell.
  let filterKeys = $derived<FilterKey[]>(team ? ["assignee", "state", "label", "project", "cycle", "group"] : ["group"]);
</script>

<div class="screen">
  {#if isSearching}
    <header class="searching">
      <label class="search">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
          stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="M20 20l-3.5-3.5" /></svg>
        <span class="visually-hidden">Search issues by id or title</span>
        <!-- svelte-ignore a11y_autofocus -->
        <input type="search" placeholder="Ids and titles" bind:value={query} autofocus />
      </label>
      <button type="button" class="cancel" onclick={stopSearch}>Cancel</button>
    </header>
  {:else}
    <header>
      <div class="bar">
        <div class="heading">
          <h1>Issues</h1>
          <p class="sub">{list ? `${count} ${count === 1 ? "issue" : "issues"} · ${working} with an agent` : " "}</p>
        </div>
        <div class="tools">
          <button type="button" class="tool" aria-label="Search issues" onclick={startSearch}>
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" aria-hidden="true"><circle cx="11" cy="11" r="7" /><path d="M20 20l-3.5-3.5" /></svg>
          </button>
          <label class="switcher">
            <span class="visually-hidden">Issue source</span>
            <span class="current">{shown ?? "no source"}</span>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
            <select value={shown ?? ""} onchange={(e) => chooseSource(e.currentTarget.value)}
              disabled={(sources?.sources.length ?? 0) < 2 && shown !== null}>
              {#if !shown}<option value="">no source</option>{/if}
              {#each sources?.sources ?? [] as name (name)}
                <option value={name}>{name}</option>
              {/each}
            </select>
          </label>
        </div>
      </div>
      {#if shown}
        <div class="views">
          <nav aria-label="Views">
            {#each sources?.views ?? [] as item (item.name)}
              <button type="button" aria-pressed={item.name === view?.name} onclick={() => chooseView(item.name)}>
                <span class:on={item.name === view?.name}>{item.name}</span>
              </button>
            {/each}
          </nav>
          <button type="button" class="filter" class:on={words} aria-label="Filters" onclick={() => (sheet = "filters")}>
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
              stroke-linecap="round" aria-hidden="true"><path d="M3 6h18M7 12h10M10 18h4" /></svg>
          </button>
        </div>
      {/if}
      {#if words}
        <div class="words">
          <span class="mono">{words}</span>
          <button type="button" onclick={() => (filters = {})}>Clear</button>
        </div>
      {/if}
    </header>
  {/if}

  {#if failure && list}
    <p class="banner" role="status">{failure} - showing the list as it last was.</p>
  {/if}

  <main
    bind:this={scroller}
    ontouchstart={(event) => pull.start(event, scroller)}
    ontouchmove={(event) => pull.move(event)}
    ontouchend={() => pull.end()}
    style:--pulled="{pull.pulled}px"
  >
    <div class="pull" class:ready={pull.pulled >= PULL_PX} aria-hidden="true">
      <svg class:spin={isLoading && pull.pulled > 0} width="18" height="18" viewBox="0 0 24 24" fill="none"
        stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M21 12a9 9 0 1 1-3-6.7L21 8" /><path d="M21 3v5h-5" /></svg>
    </div>

    {#if sources && !shown}
      <div class="state">
        <p>{sources.why ?? "No issue source is set up."}</p>
        {#if sources.sources.length}<p class="hint">The source switcher above shows the others.</p>{/if}
      </div>
    {:else if !list}
      {#if failure}
        <div class="state">
          <p>{failure}.</p>
          <button type="button" class="ghost" onclick={() => load()}>Try again</button>
        </div>
      {:else}
        <div class="state" aria-busy="true">
          <div class="skeleton"></div>
          <div class="skeleton"></div>
          <div class="skeleton short"></div>
        </div>
      {/if}
    {:else}
      {#each list.sections as section (section.heading)}
        {@const folded = isFolded(section.heading, section.is_closed)}
        <section aria-label={section.heading || "issues"}>
          {#if section.heading}
            <button type="button" class="fold" aria-expanded={!folded}
              onclick={() => (folds = { ...folds, [section.heading]: !folded })}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
                stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={folded ? "M9 6l6 6-6 6" : "M6 9l6 6 6-6"} /></svg>
              <span class="name">{section.heading}</span>
              <span>{section.issues.length}</span>
            </button>
          {/if}
          {#if !folded}
            <div class="list">
              {#each section.issues as issue (issue.id)}
                <IssueRow {issue} showStatus={group !== "status"}
                  showAssignee={!isMine && issue.assignee_id !== team?.me?.id} />
              {/each}
            </div>
          {/if}
        </section>
      {/each}
      {#if count === 0 && !isWaitingForWords}
        <div class="state">
          <p>No issues match.</p>
          <p class="hint">{isSearching ? "Search looks at ids and titles." : "Change the filters or the view."}</p>
          {#if words}<button type="button" class="ghost" onclick={() => (filters = {})}>Clear filters</button>{/if}
        </div>
      {/if}
    {/if}
  </main>
</div>

{#if sheet}
  {@const key = sheet}
  <Sheet
    title={key === "filters" ? "Filters" : titles[key]}
    dismiss={key === "filters" ? "Clear" : "Back"}
    ondismiss={() => {
      if (key === "filters") filters = {};
      sheet = key === "filters" ? null : "filters";
    }}
    onclose={() => (sheet = null)}
  >
    {#if key === "filters"}
      <div class="rows">
        {#each filterKeys as row (row)}
          <button type="button" class="filter-row" onclick={() => (sheet = row)}>
            <span class="label">{titles[row]}</span>
            <span class="value" class:set={filters[row]}>{shownFilter(row)}</span>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="var(--muted)" stroke-width="2"
              stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M9 6l6 6-6 6" /></svg>
          </button>
        {/each}
      </div>
    {:else}
      <Choices options={choices[key]} picked={[pickedFilter(key)]} onpick={(word) => setFilter(key, word || undefined)} />
    {/if}
    {#snippet footer()}
      {#if key === "filters"}
        <button type="button" class="go" onclick={() => (sheet = null)}>
          Show {count} {count === 1 ? "issue" : "issues"}
        </button>
      {/if}
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
  }
  header {
    flex: none;
    border-bottom: 1px solid var(--bg-high);
  }
  .searching {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: calc(var(--safe-top) + 12px) 16px 12px;
  }
  .search {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 44px;
    padding: 0 12px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-high);
    color: var(--muted);
  }
  .search svg {
    flex: none;
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: none;
    color: var(--fg-strong);
    font-size: 16px;
  }
  .cancel {
    flex: none;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 16px;
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: calc(var(--safe-top) + 12px) 16px 8px;
  }
  .heading {
    min-width: 0;
  }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .sub {
    margin: 2px 0 0;
    font-size: 13px;
    color: var(--muted);
  }
  .tools {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .tool {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border-radius: 22px;
    border: 0;
    background: transparent;
    color: var(--muted);
  }
  .tool:active {
    background: var(--bg-high);
  }
  .switcher {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 36px;
    max-width: 160px;
    padding: 0 12px 0 14px;
    border-radius: 18px;
    border: 1px solid var(--line);
    background: var(--bg-raised);
    color: var(--fg-strong);
    font-size: 15px;
  }
  .current {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .switcher select {
    position: absolute;
    inset: 0;
    opacity: 0;
    width: 100%;
    font-size: 16px;
  }
  .views {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 8px 4px 0;
  }
  nav {
    flex: 1;
    min-width: 0;
    display: flex;
    gap: 2px;
    overflow-x: auto;
    padding-left: 12px;
    scrollbar-width: none;
    /* Views past the edge fade out rather than being cut through. */
    mask-image: linear-gradient(to right, #000 calc(100% - 32px), transparent);
  }
  nav::-webkit-scrollbar {
    display: none;
  }
  nav button {
    flex: none;
    min-height: 44px;
    padding: 5px 0;
    border: 0;
    background: none;
  }
  nav span {
    display: block;
    padding: 7px 14px;
    border-radius: 18px;
    border: 1px solid transparent;
    color: var(--muted);
    font-size: 14px;
    font-weight: 500;
    white-space: nowrap;
  }
  nav span::first-letter {
    text-transform: uppercase;
  }
  nav span.on {
    border-color: var(--line-strong);
    background: var(--bg-high);
    color: var(--fg-strong);
    font-weight: 600;
  }
  .filter {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border: 0;
    background: none;
    color: var(--muted);
  }
  .filter.on {
    color: var(--green);
  }
  .words {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    min-height: 44px;
    padding: 0 16px;
    border-top: 1px solid var(--bg-high);
    font-size: 12px;
    color: var(--fg);
  }
  .words .mono {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .words button {
    flex: none;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 14px;
  }
  .banner {
    flex: none;
    margin: 0;
    padding: 8px 16px;
    font-size: 13px;
    background: var(--yellow-bg);
    color: var(--yellow);
  }
  main {
    position: relative;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 8px 16px 24px;
    transform: translateY(var(--pulled));
    transition: transform 0.2s;
  }
  .pull {
    position: absolute;
    left: 0;
    right: 0;
    top: -34px;
    display: flex;
    justify-content: center;
    color: var(--muted);
    opacity: min(1, calc(var(--pulled) / 70px));
  }
  .pull.ready {
    color: var(--green);
  }
  .spin {
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  section {
    display: flex;
    flex-direction: column;
  }
  .fold {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 12px;
    text-align: left;
  }
  .name {
    font-weight: 600;
    letter-spacing: 0.08em;
    text-transform: uppercase;
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
  .hint {
    font-size: 13px;
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
    height: 86px;
    border-radius: 14px;
    background: var(--bg-raised);
    animation: pulse 1.4s ease-in-out infinite;
  }
  .skeleton.short {
    height: 48px;
    width: 60%;
  }
  @keyframes pulse {
    50% {
      opacity: 0.5;
    }
  }
  .rows {
    display: flex;
    flex-direction: column;
  }
  .filter-row {
    display: flex;
    align-items: center;
    gap: 12px;
    width: 100%;
    min-height: 48px;
    padding: 0;
    border: 0;
    background: none;
    text-align: left;
  }
  .filter-row + .filter-row {
    border-top: 1px solid var(--line);
  }
  .label {
    font-size: 16px;
    color: var(--fg-strong);
  }
  .value {
    margin-left: auto;
    font-size: 15px;
    color: var(--muted);
  }
  .value.set {
    color: var(--fg-strong);
  }
  .filter-row svg {
    flex: none;
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
</style>
