<script lang="ts">
  import { markdown } from "../markdown";

  /**
   * A reply from Claude, written out word by word when it is new, as a
   * chat with Claude reads. Claude Code writes a reply to its transcript
   * once it is finished, so this paces the finished reply rather than
   * following it as it is made; it takes at most about a second however
   * long the reply, and none at all for a reader who asked for less motion.
   */
  let { text, isNew, ongrow }: { text: string; isNew: boolean; ongrow?: () => void } = $props();

  const LONGEST_MS = 1000;
  const WORDS_PER_SECOND = 60;

  let shown = $state("");

  $effect(() => {
    const full = text;
    const isStill = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    if (!isNew || isStill) {
      shown = full;
      return;
    }
    // Words with the spaces after them, so the text builds as it will end.
    const words = full.match(/\S+\s*/g) ?? [full];
    const perSecond = Math.max(WORDS_PER_SECOND, (words.length * 1000) / LONGEST_MS);
    const started = performance.now();
    let frame = 0;
    const step = (now: number) => {
      const count = Math.min(words.length, Math.ceil(((now - started) / 1000) * perSecond));
      shown = words.slice(0, count).join("");
      ongrow?.();
      if (count < words.length) frame = requestAnimationFrame(step);
      else shown = full;
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  });
</script>

<div class="prose" class:writing={shown.length < text.length}>{@html markdown(shown)}</div>

<style>
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
  /* A caret at the end while the reply is still being written out. */
  .writing :global(> :last-child)::after {
    content: "";
    display: inline-block;
    width: 0.5em;
    height: 1em;
    margin-left: 2px;
    vertical-align: -0.15em;
    border-radius: 2px;
    background: var(--orange);
    opacity: 0.8;
  }
</style>
