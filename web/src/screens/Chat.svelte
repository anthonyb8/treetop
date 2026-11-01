<script lang="ts">
  import { tick } from "svelte";
  import { ApiError, chatUrl, fetchTree, sendLine, type ChatMessage, type Tree } from "../api";
  import Mark from "../components/Mark.svelte";
  import { markdown } from "../markdown";
  import { back, link, nav } from "../nav.svelte";
  import { recall, remember } from "../store";

  let { name }: { name: string } = $props();

  /** A line you sent that the transcript has not shown yet. */
  interface Pending {
    id: number;
    text: string;
    state: "sending" | "sent" | "failed";
    why?: string;
  }

  let messages = $state<ChatMessage[]>([]);
  let pending = $state<Pending[]>([]);
  let tree = $state<Tree | null>(null);
  let isLive = $state(false);
  let isGone = $state(false);
  let draft = $state("");
  let opened = $state<Set<string>>(new Set());
  let unseen = $state(0);
  let scroller: HTMLElement | undefined = $state();
  let footerHeight = $state(76);
  let isAtEnd = true;
  let nextId = 0;

  // Following the conversation: the transcript's last messages first, then
  // each new one as it is written. A dropped connection comes back by
  // itself and starts again from the backlog. One the server turned down,
  // as it does until it has read the pool's agents, is tried again after a
  // pause, and after a few tries the screen says there is nothing to read.
  $effect(() => {
    let source: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let refusals = 0;
    // A connection that drops and comes straight back is not worth saying;
    // one still down after a few seconds is.
    let quiet: ReturnType<typeof setTimeout> | undefined;
    const open = () => {
      source = new EventSource(chatUrl(nav.workspace, name));
      source.addEventListener("backlog", (event) => {
        messages = JSON.parse((event as MessageEvent<string>).data) as ChatMessage[];
        clearTimeout(quiet);
        isLive = true;
        isGone = false;
        refusals = 0;
        settle();
        void stickToEnd(true);
      });
      source.addEventListener("message", (event) => {
        const isNew = take(JSON.parse((event as MessageEvent<string>).data) as ChatMessage);
        settle();
        void stickToEnd(false, isNew);
      });
      source.onopen = () => {
        clearTimeout(quiet);
        isLive = true;
      };
      source.onerror = () => {
        clearTimeout(quiet);
        quiet = setTimeout(() => (isLive = false), 3000);
        if (source?.readyState !== EventSource.CLOSED) return;
        refusals += 1;
        if (refusals >= 3) isGone = true;
        retry = setTimeout(open, refusals >= 3 ? 10000 : 2000);
      };
    };
    open();
    return () => {
      clearTimeout(retry);
      clearTimeout(quiet);
      source?.close();
    };
  });

  $effect(() => {
    const load = () =>
      fetchTree(nav.workspace, name)
        .then((t) => (tree = t))
        .catch(() => {});
    void load();
    const timer = setInterval(load, 5000);
    return () => clearInterval(timer);
  });

  // What you were typing stays, through leaving the chat and reopening it.
  $effect.pre(() => {
    draft = recall<string>(`draft:${name}`) ?? "";
  });
  $effect(() => {
    remember(`draft:${name}`, draft);
  });

  /** Takes a message from the stream; whether it is one not seen before. */
  function take(message: ChatMessage): boolean {
    if (message.kind === "result") {
      let call: ChatMessage | undefined;
      for (let i = messages.length - 1; i >= 0 && !call; i--) if (messages[i]?.id === message.id) call = messages[i];
      if (call?.tool) {
        call.tool.status = message.status ?? "ok";
        call.tool.output = message.output ?? null;
      }
      return false;
    }
    const at = messages.findIndex((m) => m.id === message.id);
    if (at >= 0) {
      messages[at] = message;
      return false;
    }
    messages.push(message);
    return true;
  }

  /** Lines the transcript now shows are no longer pending. */
  function settle() {
    const said = messages.filter((m) => m.kind === "you").map((m) => m.text?.trim());
    pending = pending.filter((p) => p.state === "failed" || !said.includes(p.text.trim()));
  }

  async function stickToEnd(always: boolean, isNew = true) {
    const wasAtEnd = isAtEnd;
    await tick();
    if (!scroller) return;
    if (always || wasAtEnd) {
      scroller.scrollTop = scroller.scrollHeight;
      unseen = 0;
    } else if (isNew) {
      unseen += 1;
    }
  }

  // The footer grows when a question comes in: a chat that was at its end
  // stays there rather than the newest message slipping under it.
  $effect(() => {
    void footerHeight;
    if (isAtEnd && scroller) scroller.scrollTop = scroller.scrollHeight;
  });

  function onScroll() {
    if (!scroller) return;
    isAtEnd = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48;
    if (isAtEnd) unseen = 0;
  }

  function toEnd() {
    scroller?.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
    unseen = 0;
  }

  async function deliver(line: Pending) {
    line.state = "sending";
    line.why = undefined;
    try {
      await sendLine(nav.workspace, name, line.text);
      line.state = "sent";
    } catch (err) {
      line.state = "failed";
      line.why = err instanceof ApiError ? err.message : "Not sent";
    }
  }

  function send(event: SubmitEvent) {
    event.preventDefault();
    const text = draft.trim();
    if (!text) return;
    const line: Pending = { id: nextId++, text, state: "sending" };
    pending.push(line);
    draft = "";
    isAtEnd = true;
    void stickToEnd(true);
    void deliver(pending[pending.length - 1]!);
  }

  function onKey(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey && !event.isComposing && window.matchMedia("(pointer: fine)").matches) {
      event.preventDefault();
      (event.currentTarget as HTMLTextAreaElement).form?.requestSubmit();
    }
  }

  function toggle(id: string) {
    const next = new Set(opened);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    opened = next;
  }

  function time(at: string | undefined): string {
    if (!at) return "";
    const date = new Date(at);
    return Number.isNaN(date.getTime())
      ? ""
      : date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  }

  let card = $derived(tree?.card);
  let question = $derived(tree?.report?.state === "blocked" ? tree.report.says : null);
</script>

<div class="screen">
  <header>
    <button type="button" class="back" onclick={back} aria-label="Back to tree {name}">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
        stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6" /></svg>
    </button>
    <div class="who">
      <div class="title">
        <span class="mono number">{name}</span>
        <span class="name">{card?.title ?? card?.branch ?? ""}</span>
      </div>
      <div class="status">
        {#if card}<Mark mark={card.mark} />{/if}
        <span>{tree?.agents[0]?.name ?? "agent"}{isLive ? "" : " · connecting…"}</span>
      </div>
    </div>
    <a class="icon" href="/trees/{encodeURIComponent(name)}/terminal" onclick={link} aria-label="Open the terminal">
      <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"
        stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 17l6-5-6-5M12 19h8" /></svg>
    </a>
  </header>

  <main bind:this={scroller} onscroll={onScroll} aria-label="Conversation" aria-live="polite">
    {#if isGone}
      <p class="empty">This agent has no conversation treetop can read.</p>
    {:else if !isLive && messages.length === 0}
      <div class="empty" aria-busy="true">Opening the conversation…</div>
    {/if}
    {#each messages as message (message.id)}
      {#if message.kind === "you"}
        <div class="you">
          <div class="bubble">{message.text}</div>
          <span class="at">{time(message.at)}</span>
        </div>
      {:else if message.kind === "claude"}
        <div class="claude">
          <div class="prose">{@html markdown(message.text ?? "")}</div>
        </div>
      {:else if message.kind === "tool" && message.tool}
        {@const tool = message.tool}
        <div class="tool" class:failed={tool.status === "error"}>
          <button type="button" class="tool-row" aria-expanded={opened.has(message.id)} onclick={() => toggle(message.id)}>
            <svg class="chevron" class:open={opened.has(message.id)} width="12" height="12" viewBox="0 0 24 24"
              fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true"
              ><path d="M9 18l6-6-6-6" /></svg>
            <span class="verb">{tool.verb}</span>
            <span class="what mono">{tool.what ?? ""}</span>
            {#if tool.added !== null && tool.added !== undefined}<span class="added mono">+{tool.added}</span>{/if}
            {#if tool.removed}<span class="removed mono">−{tool.removed}</span>{/if}
            {#if tool.status === "running"}<span class="spinner" role="img" aria-label="running"></span>{/if}
            {#if tool.status === "error"}<span class="removed" role="img" aria-label="failed">✗</span>{/if}
          </button>
          {#if opened.has(message.id)}
            <pre class="output">{tool.output || (tool.status === "running" ? "Running…" : "No output")}</pre>
          {/if}
        </div>
      {/if}
    {/each}
    {#each pending as line (line.id)}
      <div class="you" class:sending={line.state !== "failed"}>
        <div class="bubble" class:failed={line.state === "failed"}>{line.text}</div>
        {#if line.state === "failed"}
          <span class="at error">
            {line.why ?? "Not sent"} ·
            <button type="button" class="retry" onclick={() => deliver(line)}>Retry</button>
          </span>
        {:else}
          <span class="at">{line.state === "sending" ? "Sending…" : "Sent"}</span>
        {/if}
      </div>
    {/each}
  </main>

  {#if unseen > 0}
    <button type="button" class="pill" style:--footer="{footerHeight}px" onclick={toEnd}>
      {unseen} new message{unseen === 1 ? "" : "s"}
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
        stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M6 13l6 6 6-6" /></svg>
    </button>
  {/if}

  <footer bind:clientHeight={footerHeight}>
    {#if question}
      <div class="needs" role="status">
        <Mark mark="waiting" />
        <span class="ask"><strong>Needs you:</strong> {question}</span>
        <a class="answer" href="/trees/{encodeURIComponent(name)}/terminal" onclick={link}>Answer</a>
      </div>
    {/if}
    <form onsubmit={send}>
      <label class="visually-hidden" for="line-{name}">Message {name}</label>
      <textarea id="line-{name}" rows="1" placeholder="Message {name}…" bind:value={draft} onkeydown={onKey}
        enterkeyhint="send"></textarea>
      <button type="submit" class="send" aria-label="Send" disabled={!draft.trim()}>
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
          stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 19V5M5 12l7-7 7 7" /></svg>
      </button>
    </form>
  </footer>
</div>

<style>
  .screen {
    position: relative;
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg);
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
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .number {
    font-size: 16px;
    font-weight: 700;
    color: var(--fg-strong);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 15px;
    font-weight: 600;
    color: var(--fg-strong);
  }
  .status {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    font-size: 12px;
    color: var(--muted);
  }
  .status span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  main {
    flex: 1;
    overflow-y: auto;
    -webkit-overflow-scrolling: touch;
    overscroll-behavior: contain;
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 14px 14px 12px;
  }
  main > :global(*) {
    flex-shrink: 0;
  }
  .empty {
    margin: auto;
    color: var(--muted);
    font-size: 14px;
    text-align: center;
  }
  .you {
    align-self: flex-end;
    max-width: 84%;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 4px;
  }
  .bubble {
    padding: 10px 13px;
    border-radius: 16px 16px 4px 16px;
    background: #3c4a3f;
    color: var(--fg-strong);
    font-size: 15px;
    line-height: 1.4;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .sending .bubble {
    opacity: 0.7;
  }
  .bubble.failed {
    background: var(--red-bg);
  }
  .at {
    font-size: 11px;
    color: var(--muted);
  }
  .at.error {
    color: var(--red);
  }
  .retry {
    border: 0;
    padding: 4px;
    background: none;
    color: var(--blue);
    font-size: 11px;
    font-weight: 600;
  }
  .claude {
    max-width: 96%;
  }
  .prose {
    font-size: 15px;
    line-height: 1.5;
    color: var(--fg);
    overflow-wrap: anywhere;
  }
  .prose :global(p) {
    margin: 0 0 8px;
  }
  .prose :global(p:last-child) {
    margin-bottom: 0;
  }
  .prose :global(.h) {
    font-weight: 700;
    color: var(--fg-strong);
  }
  .prose :global(ul),
  .prose :global(ol) {
    margin: 0 0 8px;
    padding-left: 20px;
  }
  .prose :global(li + li) {
    margin-top: 2px;
  }
  .prose :global(code) {
    font-family: var(--mono);
    font-size: 13px;
    padding: 1px 4px;
    border-radius: 4px;
    background: var(--bg-high);
    color: var(--fg-strong);
  }
  .prose :global(pre) {
    margin: 0 0 8px;
    padding: 10px;
    border-radius: 8px;
    background: #1d2021;
    overflow-x: auto;
  }
  .prose :global(pre code) {
    padding: 0;
    background: none;
  }
  .prose :global(blockquote) {
    margin: 0 0 8px;
    padding-left: 10px;
    border-left: 2px solid var(--line-strong);
    color: var(--muted);
  }
  .prose :global(strong) {
    color: var(--fg-strong);
  }
  .tool {
    display: flex;
    flex-direction: column;
    border-radius: 8px;
    border: 1px solid var(--line);
    background: var(--bg-raised);
    overflow: hidden;
  }
  .tool.failed {
    border-color: #6b3a36;
  }
  .tool-row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 36px;
    padding: 0 10px;
    border: 0;
    background: none;
    color: var(--muted);
    font-size: 13px;
    text-align: left;
  }
  .chevron {
    flex: none;
    transition: transform 0.15s;
  }
  .chevron.open {
    transform: rotate(90deg);
  }
  .verb {
    flex: none;
    color: var(--fg);
  }
  .what {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .added {
    flex: none;
    font-size: 12px;
    color: var(--green);
  }
  .removed {
    flex: none;
    font-size: 12px;
    color: var(--red);
  }
  .spinner {
    flex: none;
    width: 12px;
    height: 12px;
    border-radius: 50%;
    border: 2px solid var(--line-strong);
    border-top-color: var(--blue);
    animation: spin 0.8s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  .output {
    margin: 0;
    max-height: 240px;
    overflow: auto;
    padding: 10px;
    border-top: 1px solid var(--line);
    background: #1d2021;
    font-family: var(--mono);
    font-size: 12px;
    line-height: 1.45;
    color: var(--fg);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .pill {
    position: absolute;
    left: 50%;
    bottom: calc(var(--footer) + 12px);
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 6px;
    min-height: 36px;
    padding: 0 14px;
    border-radius: 18px;
    border: 0;
    background: var(--blue);
    color: var(--on-accent);
    font-size: 13px;
    font-weight: 600;
    box-shadow: 0 4px 12px rgb(0 0 0 / 0.4);
  }
  footer {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 12px calc(var(--safe-bottom) + 10px);
    border-top: 1px solid var(--bg-high);
    background: var(--bg);
  }
  .needs {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 10px 12px;
    border-radius: 12px;
    border: 1px solid var(--yellow);
    background: var(--yellow-bg);
    color: var(--fg-strong);
    font-size: 14px;
    line-height: 1.4;
  }
  .needs :global(.mark) {
    margin-top: 2px;
  }
  .ask {
    flex: 1;
    min-width: 0;
  }
  .answer {
    flex: none;
    align-self: center;
    display: flex;
    align-items: center;
    min-height: 36px;
    padding: 0 12px;
    border-radius: 8px;
    background: var(--yellow);
    color: var(--on-accent);
    font-size: 14px;
    font-weight: 600;
  }
  .icon {
    flex: none;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 44px;
    height: 44px;
    color: var(--muted);
  }
  form {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    margin: 0;
  }
  textarea {
    flex: 1;
    min-width: 0;
    min-height: 44px;
    max-height: 140px;
    padding: 11px 14px;
    border-radius: 22px;
    border: 1px solid var(--line-strong);
    background: var(--bg-raised);
    color: var(--fg-strong);
    font-size: 16px;
    line-height: 1.3;
    resize: none;
    field-sizing: content;
  }
  .send {
    flex: none;
    width: 44px;
    height: 44px;
    border-radius: 22px;
    border: 0;
    background: var(--green);
    color: var(--on-accent);
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .send:disabled {
    background: var(--bg-high);
    color: var(--muted);
  }
</style>
