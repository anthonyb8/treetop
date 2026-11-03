<script lang="ts">
  import { ApiError, dispatch, fetchPool, type Pool } from "../api";
  import { back, go, nav } from "../nav.svelte";
  import { recall, remember } from "../store";

  /** The tree to start in, when the screen was opened from one. */
  let { tree = null }: { tree?: string | null } = $props();

  let pool = $state<Pool | null>(recall<Pool>(`pool:${nav.workspace ?? ""}`));
  let task = $state("");
  let where = $state<"fresh" | "here">("fresh");
  let base = $state("");
  let isStarting = $state(false);
  let failure = $state<string | null>(null);

  $effect.pre(() => {
    task = recall<string>("draft:new") ?? "";
    where = tree ? "here" : "fresh";
  });
  $effect(() => {
    remember("draft:new", task);
  });
  $effect(() => {
    fetchPool(nav.workspace)
      .then((p) => (pool = p))
      .catch(() => {});
  });

  let bases = $derived(pool?.bases ?? []);
  $effect(() => {
    if (!base && bases[0]) base = bases[0];
  });

  async function start(event: SubmitEvent) {
    event.preventDefault();
    const text = task.trim();
    if (!text || isStarting) return;
    isStarting = true;
    failure = null;
    try {
      const started = await dispatch(nav.workspace, {
        task: text,
        tree: where === "here" ? tree : null,
        base: where === "fresh" ? base || null : null,
      });
      task = "";
      remember("draft:new", "");
      go(`/trees/${encodeURIComponent(started.tree)}`);
    } catch (err) {
      failure = err instanceof ApiError ? err.message : "The agent did not start";
    } finally {
      isStarting = false;
    }
  }
</script>

<div class="screen">
  <header>
    <button type="button" class="cancel" onclick={back}>Cancel</button>
    <h1>New agent</h1>
    <span class="spacer" aria-hidden="true"></span>
  </header>

  <form id="new-agent" onsubmit={start}>
    <div class="field">
      <label for="task">Task</label>
      <textarea id="task" rows="6" placeholder="What should the agent do?" bind:value={task}></textarea>
    </div>

    <fieldset>
      <legend>Where</legend>
      <div class="segments">
        <label class="choice" class:on={where === "fresh"}>
          <input type="radio" name="where" value="fresh" bind:group={where} />
          <span class="name">A fresh tree</span>
          <span class="hint">{pool ? `${pool.free} free` : "from the pool"}</span>
        </label>
        {#if tree}
          <label class="choice" class:on={where === "here"}>
            <input type="radio" name="where" value="here" bind:group={where} />
            <span class="name">Tree {tree}</span>
            <span class="hint">keeps its branch</span>
          </label>
        {/if}
      </div>
    </fieldset>

    {#if where === "fresh" && bases.length}
      <div class="base">
        <label for="base">Base</label>
        <span class="picked mono">{base}</span>
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2"
          stroke-linecap="round" aria-hidden="true"><path d="M6 9l6 6 6-6" /></svg>
        <select id="base" bind:value={base}>
          {#each bases as name (name)}<option value={name}>{name}</option>{/each}
        </select>
      </div>
      <p class="note">It gets a branch named for the task, off {base}.</p>
    {/if}

    {#if failure}<p class="failure" role="alert">{failure}</p>{/if}
  </form>

  <footer>
    <button type="submit" form="new-agent" class="go" disabled={!task.trim() || isStarting}>
      {isStarting ? "Starting…" : "Start agent"}
    </button>
  </footer>
</div>

<style>
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: calc(var(--safe-top) + 6px) 16px 10px;
    border-bottom: 1px solid var(--bg-high);
  }
  .cancel {
    min-height: 44px;
    padding: 0;
    border: 0;
    background: none;
    color: var(--blue);
    font-size: 16px;
  }
  .spacer {
    width: 52px;
  }
  h1 {
    margin: 0;
    font-size: 17px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  form {
    flex: 1;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 22px;
    padding: 20px 16px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  label,
  legend {
    font-size: 13px;
    font-weight: 600;
    color: var(--muted);
  }
  textarea {
    width: 100%;
    padding: 14px;
    border-radius: 14px;
    border: 1px solid var(--line-strong);
    background: var(--bg-raised);
    color: var(--fg-strong);
    font-size: 16px;
    line-height: 1.45;
    resize: none;
  }
  /* One ring when it has the keys: its own border, not a second outline. */
  textarea:focus,
  textarea:focus-visible {
    outline: none;
    border-color: var(--blue);
    box-shadow: 0 0 0 1px var(--blue);
  }
  fieldset {
    margin: 0;
    padding: 0;
    border: 0;
  }
  legend {
    margin-bottom: 8px;
  }
  /* Where it runs, as a two-way switch. */
  .segments {
    display: grid;
    grid-auto-columns: minmax(0, 1fr);
    grid-auto-flow: column;
    gap: 4px;
    padding: 4px;
    border-radius: 14px;
    background: var(--bg-raised);
  }
  .choice {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 2px;
    min-height: 52px;
    border-radius: 10px;
    font-weight: 400;
  }
  .choice.on {
    background: var(--bg-high);
  }
  .choice:has(input:focus-visible) {
    outline: 2px solid var(--blue);
  }
  .choice input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .name {
    font-size: 15px;
    font-weight: 500;
    color: var(--muted);
  }
  .on .name {
    font-weight: 600;
    color: var(--fg-strong);
  }
  .hint {
    font-size: 12px;
    color: var(--muted);
  }
  .on .hint {
    color: var(--green);
  }
  .base {
    position: relative;
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 52px;
    padding: 0 14px;
    border-radius: 14px;
    background: var(--bg-raised);
    color: var(--muted);
  }
  .base label {
    flex: 1;
    font-size: 15px;
    font-weight: 400;
  }
  .picked {
    font-size: 14px;
    color: var(--fg-strong);
  }
  .base select {
    position: absolute;
    inset: 0;
    width: 100%;
    opacity: 0;
    font-size: 16px;
  }
  .note {
    margin: -14px 4px 0;
    font-size: 13px;
    line-height: 1.45;
    color: var(--muted);
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
    padding: 8px 16px var(--bottom);
    border-top: 1px solid var(--bg-high);
  }
  .go {
    width: 100%;
    min-height: 52px;
    border-radius: 16px;
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
