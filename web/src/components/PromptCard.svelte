<script lang="ts">
  import type { Answer, ToolCall } from "../api";
  import Mark from "./Mark.svelte";

  /**
   * What the agent waits on: a tool call with no result yet, which is a
   * question when it carries `ask` and a permission otherwise, else only
   * what its report says it needs.
   */
  let {
    tool,
    needs,
    onanswer,
  }: { tool: ToolCall | null; needs: string | null; onanswer: (given: Answer) => Promise<void> } = $props();

  /** The options picked for each question, by its index. */
  let picked = $state<string[][]>([]);
  let isBusy = $state(false);

  $effect.pre(() => {
    picked = (tool?.ask ?? []).map(() => []);
  });

  function pick(question: number, label: string, isMulti: boolean) {
    const now = picked[question] ?? [];
    picked[question] = isMulti
      ? now.includes(label)
        ? now.filter((l) => l !== label)
        : [...now, label]
      : [label];
  }

  let isComplete = $derived((tool?.ask ?? []).length > 0 && picked.every((p) => p.length > 0));

  /** The answer in words: the picked labels, each question's under its header when there are several. */
  function words(): string {
    const asked = tool?.ask ?? [];
    if (asked.length === 1) return (picked[0] ?? []).join(", ");
    return asked.map((q, i) => `${q.header || q.question}: ${(picked[i] ?? []).join(", ")}`).join("\n");
  }

  // What a tool call it waits on would do, as "Wants to run …".
  const doing: Record<string, string> = {
    Edited: "edit",
    Wrote: "write",
    Read: "read",
    Fetched: "fetch",
    Searched: "search",
    "Searched the web": "search the web for",
    "Asked an agent": "start an agent to",
    "Used skill": "use the skill",
  };
  function wants(verb: string): string {
    return doing[verb] ?? `use ${verb}`;
  }

  async function give(given: Answer) {
    isBusy = true;
    try {
      await onanswer(given);
    } finally {
      isBusy = false;
    }
  }
</script>

<section class="prompt" aria-label="Waiting on you">
  {#if tool?.ask?.length}
    <div class="questions">
    {#each tool.ask as q, i (i)}
      <div class="question">
        <p class="asks">
          {#if q.header}<span class="chip">{q.header}</span>{/if}
          {q.question}
        </p>
        <div class="choices" role="group" aria-label={q.header || q.question}>
          {#each q.options as option (option.label)}
            {@const isOn = picked[i]?.includes(option.label) ?? false}
            <button type="button" class="choice" class:on={isOn} aria-pressed={isOn} disabled={isBusy}
              onclick={() => pick(i, option.label, q.multi)}>
              <span class="label">{option.label}</span>
              {#if option.description}<span class="description">{option.description}</span>{/if}
            </button>
          {/each}
        </div>
        {#if q.multi}<p class="hint">Pick any that apply.</p>{/if}
      </div>
    {/each}
    </div>
    <button type="button" class="go" disabled={!isComplete || isBusy} onclick={() => give({ action: "say", text: words() })}>
      {isBusy ? "Answering…" : "Answer"}
    </button>
    <p class="hint">Or answer in your own words below.</p>
  {:else if tool}
    <p class="asks">
      <Mark mark="waiting" />
      {#if tool.verb === "Ran"}
        <span>Wants to: <strong>{tool.what ?? "run a command"}</strong></span>
      {:else}
        <span>Wants to {wants(tool.verb)} <span class="mono what">{tool.what ?? ""}</span></span>
      {/if}
    </p>
    {#if needs}<p class="needs-text">{needs}</p>{/if}
    <div class="row">
      <button type="button" class="no" disabled={isBusy} onclick={() => give({ action: "deny" })}>Deny</button>
      <button type="button" class="go" disabled={isBusy} onclick={() => give({ action: "allow" })}>
        {isBusy ? "Answering…" : "Allow"}
      </button>
    </div>
    <p class="hint">Or deny and say what to do instead, below.</p>
  {:else if needs}
    <p class="asks"><Mark mark="waiting" /><span><strong>Needs you:</strong> {needs}</span></p>
  {/if}
</section>

<style>
  .prompt {
    display: flex;
    flex-direction: column;
    gap: 10px;
    max-height: 56vh;
    padding: 12px;
    border-radius: 12px;
    border: 1px solid var(--yellow);
    background: var(--yellow-bg);
    color: var(--fg-strong);
    font-size: 14px;
    line-height: 1.4;
  }
  /* The questions scroll; the Answer under them stays in reach. */
  .questions {
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }
  .question {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .asks {
    display: flex;
    align-items: flex-start;
    flex-wrap: wrap;
    gap: 6px 8px;
    margin: 0;
    font-size: 15px;
  }
  .asks :global(.mark) {
    margin-top: 2px;
  }
  .chip {
    padding: 1px 8px;
    border-radius: 999px;
    background: var(--yellow);
    color: var(--on-accent);
    font-size: 12px;
    font-weight: 600;
  }
  .what {
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .needs-text {
    margin: 0;
    color: var(--muted);
    font-size: 13px;
    overflow-wrap: anywhere;
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .choice {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-height: 44px;
    padding: 9px 12px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: var(--bg-raised);
    color: var(--fg-strong);
    text-align: left;
  }
  .choice.on {
    border-color: var(--yellow);
    box-shadow: inset 0 0 0 1px var(--yellow);
  }
  .label {
    font-size: 15px;
    font-weight: 600;
  }
  .description {
    font-size: 13px;
    color: var(--muted);
  }
  .row {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 8px;
  }
  .go,
  .no {
    min-height: 44px;
    border-radius: 10px;
    font-size: 15px;
    font-weight: 600;
  }
  .go {
    border: 0;
    background: var(--yellow);
    color: var(--on-accent);
  }
  .no {
    border: 1px solid var(--line-strong);
    background: var(--bg-raised);
    color: var(--fg-strong);
  }
  .go:disabled {
    opacity: 0.5;
  }
  .hint {
    margin: 0;
    font-size: 12px;
    color: var(--muted);
  }
</style>
