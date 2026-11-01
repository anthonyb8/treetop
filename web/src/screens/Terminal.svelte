<script lang="ts">
  import { ApiError, sendKeys, terminalUrl, type TerminalScreen } from "../api";
  import { back, nav } from "../nav.svelte";

  let { name }: { name: string } = $props();

  /** The font the screen is drawn in, which with the width sets the columns. */
  const FONT_PX = 12.5;
  const CHAR_EM = 0.6;
  const LINE = 1.3;

  let screen = $state<TerminalScreen | null>(null);
  let isEnded = $state(false);
  let failure = $state<string | null>(null);
  let typed = $state("");
  let area: HTMLElement | undefined = $state();
  let size = $state<{ cols: number; rows: number } | null>(null);

  // The terminal is sized to the space it has on this phone, once, by the
  // width a character of its font really takes here.
  let ruler: HTMLElement | undefined = $state();
  $effect(() => {
    if (!area || !ruler || size) return;
    const charWidth = ruler.getBoundingClientRect().width / 10 || FONT_PX * CHAR_EM;
    const width = area.clientWidth - 16;
    const height = area.clientHeight - 16;
    size = {
      cols: Math.max(30, Math.min(120, Math.floor(width / charWidth))),
      rows: Math.max(12, Math.min(80, Math.floor(height / (FONT_PX * LINE)))),
    };
  });

  $effect(() => {
    if (!size) return;
    const source = new EventSource(terminalUrl(nav.workspace, name, size.cols, size.rows));
    source.addEventListener("screen", (event) => {
      screen = JSON.parse((event as MessageEvent<string>).data) as TerminalScreen;
      failure = null;
    });
    source.addEventListener("ended", () => {
      isEnded = true;
      source.close();
    });
    source.onerror = () => {
      if (source.readyState === EventSource.CLOSED && !isEnded) failure = "Can't open the agent's terminal";
    };
    return () => source.close();
  });

  // Presses go in the order they were made, each after the last is in.
  let queue: Promise<void> = Promise.resolve();
  function press(keys: string[], text?: string) {
    queue = queue.then(() =>
      sendKeys(nav.workspace, name, keys, text).catch((err: unknown) => {
        failure = err instanceof ApiError ? err.message : "Not sent";
      }),
    );
  }

  function type(event: SubmitEvent) {
    event.preventDefault();
    if (!typed) return;
    press([], typed);
    typed = "";
  }

  const rowOne = ["1", "2", "3", "4"];
  const rowTwo: [string, string][] = [
    ["Up", "↑"],
    ["Down", "↓"],
    ["Tab", "Tab"],
    ["Esc", "Esc"],
  ];
</script>

<div class="screen">
  <header>
    <button type="button" class="back" onclick={back} aria-label="Back">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
        stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>
    </button>
    <div class="who">
      <span class="title"><span class="mono number">{name}</span> terminal</span>
      <span class="sub">
        {#if isEnded}the terminal has closed{:else if failure}{failure}{:else if screen}the agent's own screen · live{:else}opening…{/if}
      </span>
    </div>
  </header>

  <main bind:this={area} aria-label="Terminal">
    <span class="ruler" bind:this={ruler} style:font-size="{FONT_PX}px" aria-hidden="true">MMMMMMMMMM</span>
    {#if screen}
      <pre class="term" style:font-size="{FONT_PX}px" style:line-height={LINE} aria-live="polite">{#each screen.lines as runs, row (row)}<span class="line">{#each runs as run, i (i)}<span
              style:color={run.fg}
              style:background={run.bg}
              class:b={run.b}
              class:d={run.d}
              class:i={run.i}
              class:u={run.u}>{run.t}</span
            >{/each}</span>{"\n"}{/each}</pre>
    {:else if !failure}
      <p class="note" aria-busy="true">Opening the agent's terminal…</p>
    {:else}
      <p class="note">{failure}.</p>
    {/if}
  </main>

  <footer>
    <div class="keys" role="group" aria-label="Keys">
      {#each rowOne as key (key)}
        <button type="button" class="key mono" onclick={() => press([key])}>{key}</button>
      {/each}
    </div>
    <div class="keys five" role="group" aria-label="More keys">
      {#each rowTwo as [key, label] (key)}
        <button type="button" class="key" aria-label={key} onclick={() => press([key])}>{label}</button>
      {/each}
      <button type="button" class="key enter" onclick={() => press(["Enter"])}>Enter</button>
    </div>
    <form onsubmit={type}>
      <label class="visually-hidden" for="keys-{name}">Type into the terminal</label>
      <input id="keys-{name}" type="text" placeholder="Type into the terminal…" bind:value={typed}
        autocomplete="off" autocapitalize="off" spellcheck="false" />
      <button type="submit" class="key wide" disabled={!typed}>Type</button>
    </form>
  </footer>
</div>

<style>
  .screen {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: #1d2021;
  }
  header {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: calc(var(--safe-top) + 6px) 12px 8px 4px;
    background: var(--bg-raised);
    border-bottom: 1px solid var(--bg-high);
  }
  .back {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    border: 0;
    background: none;
    color: var(--blue);
  }
  .who {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .title {
    font-size: 15px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .number {
    font-weight: 700;
  }
  .sub {
    font-size: 12px;
    color: var(--muted);
  }
  main {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 8px;
  }
  .ruler {
    position: absolute;
    visibility: hidden;
    font-family: var(--mono);
    white-space: pre;
  }
  .term {
    margin: 0;
    font-family: var(--mono);
    color: var(--fg);
    white-space: pre;
  }
  .b {
    font-weight: 700;
  }
  .d {
    opacity: 0.6;
  }
  .i {
    font-style: italic;
  }
  .u {
    text-decoration: underline;
  }
  .note {
    margin: 24px 8px;
    color: var(--muted);
    font-size: 14px;
  }
  footer {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 10px calc(var(--safe-bottom) + 10px);
    background: var(--bg);
    border-top: 1px solid var(--bg-high);
  }
  .keys {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 6px;
  }
  .keys.five {
    grid-template-columns: repeat(5, minmax(0, 1fr));
  }
  .key {
    min-height: 44px;
    border-radius: 10px;
    border: 1px solid #504945;
    background: var(--bg-high);
    color: var(--fg-strong);
    font-size: 15px;
    font-weight: 600;
  }
  .key:active {
    background: var(--line-strong);
  }
  .key.enter {
    border: 0;
    background: var(--green);
    color: var(--on-accent);
  }
  .key.wide {
    padding: 0 14px;
  }
  .key:disabled {
    color: var(--muted);
  }
  form {
    display: flex;
    gap: 8px;
    margin: 0;
  }
  input {
    flex: 1;
    min-width: 0;
    min-height: 44px;
    padding: 0 12px;
    border-radius: 10px;
    border: 1px solid var(--line-strong);
    background: #1d2021;
    color: var(--fg-strong);
    font-family: var(--mono);
    font-size: 16px;
  }
</style>
