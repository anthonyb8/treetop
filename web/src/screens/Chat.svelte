<script lang="ts">
  import { tick } from "svelte";
  import { cubicOut } from "svelte/easing";
  import { SvelteSet } from "svelte/reactivity";
  import { fly, slide } from "svelte/transition";
  import {
    answer,
    ApiError,
    chatUrl,
    fetchPrompt,
    fetchTree,
    sendLine,
    type Answer,
    type ChatMessage,
    type Question,
    type ToolCall,
    type Tree,
  } from "../api";
  import Mark from "../components/Mark.svelte";
  import PortButton from "../components/PortButton.svelte";
  import PromptCard from "../components/PromptCard.svelte";
  import Reveal from "../components/Reveal.svelte";
  import Working from "../components/Working.svelte";
  import { back, nav } from "../nav.svelte";
  import { eased } from "../pace";
  import { recall, remember } from "../store";

  let { name }: { name: string } = $props();

  /** A line you sent that the transcript has not shown yet. */
  interface Pending {
    id: number;
    text: string;
    state: "sending" | "sent" | "failed";
    why?: string;
    /** The prompt these words answer, when one was waiting. */
    answers?: string;
  }

  let messages = $state<ChatMessage[]>([]);
  /** Messages that came in while the chat was open, which ease in; those
   * shown on opening are just there. */
  const fresh = new SvelteSet<string>();
  /** Replies being written out. What came after one waits for it, so a
   * burst from the transcript reads in the order it happened. */
  const writing = new SvelteSet<string>();
  /** The time a reply may take, shared when several are waiting. */
  const LONGEST_MS = 4000;
  const isStill = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  function arrive(id: string) {
    return { y: 10, duration: fresh.has(id) && !isStill ? 240 : 0, easing: cubicOut };
  }
  let pending = $state<Pending[]>([]);
  let tree = $state<Tree | null>(null);
  let isLive = $state(false);
  let isGone = $state(false);
  let draft = $state("");
  let opened = $state<Set<string>>(new Set());
  let unseen = $state(0);
  let scroller: HTMLElement | undefined = $state();
  let thread: HTMLElement | undefined = $state();
  let footerHeight = $state(76);
  let isAtEnd = true;
  let nextId = 0;

  let shown = $derived.by(() => {
    const at = messages.findIndex((m) => writing.has(m.id));
    return at < 0 ? messages : messages.slice(0, at + 1);
  });

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
        void stickToEnd(true);
      });
      source.addEventListener("message", (event) => {
        const message = JSON.parse((event as MessageEvent<string>).data) as ChatMessage;
        const isYours = isPending(message);
        const isNew = take(message);
        // Your line is on screen already; it settles in place.
        if (isNew && !isYours) fresh.add(message.id);
        if (isNew && message.kind === "claude" && !isStill) writing.add(message.id);
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

  function isPending(message: ChatMessage): boolean {
    return message.kind === "you" && pending.some((p) => p.text.trim() === message.text?.trim());
  }

  // Lines the conversation now shows are no longer pending.
  $effect(() => {
    const said = shown.filter((m) => m.kind === "you").map((m) => m.text?.trim());
    const left = pending.filter((p) => p.state === "failed" || !said.includes(p.text.trim()));
    if (left.length !== pending.length) pending = left;
  });

  async function stickToEnd(always: boolean, isNew = true) {
    const wasAtEnd = isAtEnd;
    await tick();
    if (!scroller) return;
    if (always) {
      scroller.scrollTop = scroller.scrollHeight;
      unseen = 0;
    } else if (wasAtEnd) {
      unseen = 0;
    } else if (isNew) {
      unseen += 1;
    }
  }

  /** How long the chat takes to close most of the way to its end. */
  const FOLLOW_MS = 110;
  let following = 0;
  let isTouching = false;

  // A chat at its end glides after what grows under it, rather than jumping
  // a line at a time. A finger on the screen has the scroll to itself.
  function follow() {
    if (following || !scroller) return;
    let last = performance.now();
    const step = (now: number) => {
      following = 0;
      if (!scroller || !isAtEnd || isTouching) return;
      const gap = scroller.scrollHeight - scroller.clientHeight - scroller.scrollTop;
      if (gap < 1) return;
      scroller.scrollTop += Math.max(1, gap * eased(now - last, FOLLOW_MS));
      last = now;
      following = requestAnimationFrame(step);
    };
    following = requestAnimationFrame(step);
  }

  $effect(() => {
    if (!thread) return;
    const observer = new ResizeObserver(() => follow());
    observer.observe(thread);
    return () => {
      observer.disconnect();
      cancelAnimationFrame(following);
      following = 0;
    };
  });

  function onTouchEnd() {
    isTouching = false;
    follow();
  }

  // The conversation's box shrinks when a question comes in under it: a
  // chat that was at its end stays there rather than the newest message
  // slipping out of sight.
  $effect(() => {
    if (!scroller) return;
    const box = scroller;
    const observer = new ResizeObserver(() => {
      if (isAtEnd) box.scrollTop = box.scrollHeight;
    });
    observer.observe(box);
    return () => observer.disconnect();
  });

  // Whether the chat follows new messages is what you did, not where the
  // layout left it: scrolling up yourself stops it, reaching the end again
  // starts it, and a box that shrank under it changes nothing.
  let lastTop = 0;
  function onScroll() {
    if (!scroller) return;
    const isNearEnd = scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 48;
    const wentUp = scroller.scrollTop < lastTop;
    lastTop = scroller.scrollTop;
    if (isNearEnd) isAtEnd = true;
    else if (wentUp) isAtEnd = false;
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
      // While a prompt has the keys a message would not land, so the words
      // answer it: the prompt is put away and they go in as a message.
      if (line.answers) {
        await answer(nav.workspace, name, { action: "say", text: line.text });
        answeredId = line.answers;
        answeredNeeds = needs;
      } else {
        await sendLine(nav.workspace, name, line.text);
      }
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
    // While the agent waits on you, words answer whatever it waits on, found
    // or not: a prompt it shows would swallow a plain message.
    const answers = promptTool?.id ?? (isWaiting ? (spell ?? undefined) : undefined);
    const line: Pending = { id: nextId++, text, state: "sending", answers };
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
  // The agent's own status is read every few seconds and its report only
  // when it writes one, so a report can still say it waits on you after it
  // has been answered and gone back to work: busy wins.
  let isBusy = $derived(tree?.agents[0]?.status === "busy");
  let isWaiting = $derived(!!card?.needs_you && !isBusy);
  let needs = $derived(tree?.report?.state === "blocked" && !isBusy ? tree.report.says : null);

  /** The prompt just answered, and what the agent said it needed then, kept
   * out of sight until the transcript and the agent's state catch up. */
  let answeredId = $state<string | null>(null);
  let answeredNeeds = $state<string | null>(null);
  let shownNeeds = $derived(needs !== answeredNeeds ? needs : null);
  // The agent's last words, when they are what it waits on you for: the
  // question is marked where it was asked rather than said again below.
  let askedId = $derived.by(() => {
    const last = shown.at(-1);
    if (!shownNeeds || last?.kind !== "claude" || !last.text) return null;
    return last.text.includes(shownNeeds.trim()) ? last.id : null;
  });

  // What the agent waits on: its newest tool call, when it has no result
  // yet and the agent waits on you. A question carries its choices; any
  // other is a permission to give or refuse.
  let promptMessage = $derived.by(() => {
    if (!isWaiting) return null;
    let last: ChatMessage | undefined;
    for (let i = shown.length - 1; i >= 0 && !last; i--) if (shown[i]?.kind === "tool") last = shown[i];
    return last?.tool?.status === "running" && last.id !== answeredId ? last : null;
  });

  // Claude Code writes a question to the transcript only once it has been
  // answered, so while the agent waits with nothing running in its chat,
  // the question is read off its screen, once for each time it waits.
  let screenAsk = $state<{ spell: string; ask: Question[] } | null>(null);
  let spell = $derived(isWaiting ? `screen:${tree?.report?.says ?? ""}` : null);
  $effect(() => {
    if (!spell || promptMessage || screenAsk?.spell === spell) return;
    const asking = spell;
    screenAsk = { spell: asking, ask: [] };
    fetchPrompt(nav.workspace, name)
      .then(({ ask }) => {
        if (screenAsk?.spell === asking) screenAsk = { spell: asking, ask };
      })
      .catch(() => {});
  });
  let promptTool = $derived.by((): (ToolCall & { id: string }) | null => {
    if (promptMessage?.tool) return { ...promptMessage.tool, id: promptMessage.id };
    const ask = screenAsk?.spell === spell ? screenAsk?.ask : undefined;
    if (!spell || !ask?.length || answeredId === spell) return null;
    return {
      id: spell,
      verb: "Asked you",
      what: ask[0]?.question ?? null,
      added: null,
      removed: null,
      status: "running",
      output: null,
      ask,
    };
  });
  let answerFailure = $state<string | null>(null);

  // What a running tool call is doing, as the dots say it: a command's own
  // description, else the verb as it is happening.
  const now: Record<string, string> = {
    Edited: "Editing",
    Wrote: "Writing",
    Read: "Reading",
    Searched: "Searching for",
    Fetched: "Fetching",
    "Searched the web": "Searching the web for",
    "Asked an agent": "Asking an agent to",
    "Used skill": "Using the skill",
  };
  function doing(verb: string, what: string | null): string {
    if (verb === "Ran") return what ?? "Running a command";
    return [now[verb] ?? verb, what].filter(Boolean).join(" ");
  }

  // The agent at work, and what on: its running tool call, else what its
  // report says it is doing. Nothing while it waits on you.
  let working = $derived.by(() => {
    if (!card || isWaiting || promptTool) return null;
    if (!isBusy && tree?.report?.state !== "working") return null;
    const last = shown.at(-1);
    if (last?.kind === "tool" && last.tool?.status === "running") {
      return { step: doing(last.tool.verb, last.tool.what) };
    }
    return { step: tree?.report?.state === "working" ? tree.report.says || null : null };
  });

  async function answerPrompt(given: Answer) {
    if (given.action === "say") {
      const line: Pending = { id: nextId++, text: given.text, state: "sending", answers: promptTool?.id };
      pending.push(line);
      isAtEnd = true;
      void stickToEnd(true);
      await deliver(pending[pending.length - 1]!);
      return;
    }
    answerFailure = null;
    try {
      await answer(nav.workspace, name, given);
      answeredId = promptTool?.id ?? null;
      answeredNeeds = needs;
    } catch (err) {
      answerFailure = err instanceof ApiError ? err.message : "The answer did not go through";
    }
  }
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
        {#if isLive && isWaiting}<span class="waiting">· waiting on you</span>{/if}
      </div>
    </div>
    {#if tree?.ports.length}
      <div class="ports">
        {#each tree.ports as port (port.port)}<PortButton port={port.port} />{/each}
      </div>
    {/if}
  </header>

  <main
    bind:this={scroller}
    onscroll={onScroll}
    ontouchstart={() => (isTouching = true)}
    ontouchend={onTouchEnd}
    ontouchcancel={onTouchEnd}
    aria-label="Conversation"
    aria-live="polite"
  >
    {#if isGone}
      <p class="empty">This agent has no conversation treetop can read.</p>
    {:else if !isLive && messages.length === 0}
      <div class="empty" aria-busy="true">Opening the conversation…</div>
    {/if}
    <div class="thread" bind:this={thread}>
    {#each shown as message (message.id)}
      {#if message.kind === "you"}
        <div class="you" in:fly={arrive(message.id)}>
          <div class="bubble">{message.text}</div>
          <span class="at">{time(message.at)}</span>
        </div>
      {:else if message.kind === "claude"}
        <div class="claude" class:asks={message.id === askedId}>
          {#if message.id === askedId}<span class="asks-label">Needs you</span>{/if}
          <Reveal
            text={message.text ?? ""}
            isNew={writing.has(message.id)}
            longest={LONGEST_MS / Math.max(1, writing.size)}
            ondone={() => writing.delete(message.id)}
          />
        </div>
      {:else if message.kind === "tool" && message.tool}
        {@const tool = message.tool}
        <div class="tool" class:failed={tool.status === "error"} in:fly={arrive(message.id)}>
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
      <div class="you" class:sending={line.state !== "failed"} in:fly={{ y: 10, duration: isStill ? 0 : 240, easing: cubicOut }}>
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
    {#if working}
      <div transition:slide={{ duration: isStill ? 0 : 180 }}><Working step={working.step} /></div>
    {/if}
    </div>
  </main>

  {#if unseen > 0}
    <button type="button" class="pill" style:--footer="{footerHeight}px" onclick={toEnd}>
      {unseen} new message{unseen === 1 ? "" : "s"}
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"
        stroke-linecap="round" aria-hidden="true"><path d="M12 5v14M6 13l6 6 6-6" /></svg>
    </button>
  {/if}

  <footer bind:clientHeight={footerHeight}>
    {#if promptTool || (shownNeeds && !askedId)}
      <PromptCard tool={promptTool} needs={shownNeeds} onanswer={answerPrompt} />
    {/if}
    {#if answerFailure}<p class="failure" role="alert">{answerFailure}</p>{/if}
    <form onsubmit={send}>
      <label class="visually-hidden" for="line-{name}">Message {name}</label>
      <textarea id="line-{name}" rows="1" placeholder={promptTool ? "Answer in your own words…" : isWaiting ? `Answer ${name}…` : `Message ${name}…`}
        bind:value={draft} onkeydown={onKey}
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
    padding: 14px 14px 12px;
  }
  main > :global(*) {
    flex-shrink: 0;
  }
  .thread {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .ports {
    flex: none;
    display: flex;
    gap: 6px;
    max-width: 45%;
    overflow-x: auto;
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
  .claude.asks {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 14px;
    border-radius: 16px;
    background: var(--yellow-bg);
  }
  .asks-label {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--yellow);
  }
  .waiting {
    color: var(--yellow);
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
    padding: 8px 12px var(--bottom);
    border-top: 1px solid var(--bg-high);
    background: var(--bg);
  }
  .failure {
    margin: 0;
    padding: 8px 12px;
    border-radius: 10px;
    background: var(--red-bg);
    color: var(--red);
    font-size: 13px;
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
