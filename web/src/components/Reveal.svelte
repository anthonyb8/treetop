<script lang="ts">
  import { untrack } from "svelte";
  import { markdown } from "../markdown";
  import { eased, FADE_MS, pace } from "../pace";

  /**
   * A reply from Claude, written out when it is new, as a chat with Claude
   * reads. Claude Code writes a reply to its transcript once it is finished,
   * so this paces the finished reply: the whole of it is laid out from the
   * start, each word fades up in turn, and the box opens as the words reach
   * each line. Nothing reflows, half-written Markdown never shows, and the
   * browser runs the fades itself, so a busy page does not stutter them.
   * None of it for a reader who asked for less motion.
   */
  let {
    text,
    isNew,
    longest,
    ondone,
  }: { text: string; isNew: boolean; longest: number; ondone?: () => void } = $props();

  /** How long the box takes to close most of the way to the next line. */
  const OPEN_MS = 90;

  const paced = untrack(() => (isNew ? pace(markdown(text), longest) : null));
  let isWriting = $state(paced !== null);
  let box: HTMLElement | undefined = $state();

  $effect(() => {
    if (!paced || !box) return;
    const prose = box;
    const top = prose.getBoundingClientRect().top;
    // Where the text ends as each word comes up.
    const marks = [...prose.querySelectorAll<HTMLElement>(".word")].map((word) => ({
      at: parseFloat(word.style.getPropertyValue("--at")),
      bottom: word.getBoundingClientRect().bottom - top,
    }));
    const full = prose.scrollHeight;
    const started = performance.now();
    let last = started;
    let height = 0;
    let target = 0;
    let next = 0;
    let frame = 0;
    const step = (now: number) => {
      const elapsed = now - started;
      while (next < marks.length && (marks[next]?.at ?? 0) <= elapsed) target = marks[next++]?.bottom ?? target;
      if (next === marks.length) target = full;
      height += (target - height) * eased(now - last, OPEN_MS);
      last = now;
      prose.style.height = `${height}px`;
      if (elapsed >= paced.ms + FADE_MS && target - height < 0.5) {
        isWriting = false;
        ondone?.();
      } else {
        frame = requestAnimationFrame(step);
      }
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  });
</script>

<div class="prose" class:writing={isWriting} style:height={isWriting ? "0" : null} style:--fade="{FADE_MS}ms" bind:this={box}>
  {@html isWriting && paced ? paced.html : markdown(text)}
</div>

<style>
  .writing {
    overflow: hidden;
  }
  .writing :global(.word) {
    opacity: 0;
    animation: word var(--fade) ease-out var(--at) forwards;
  }
  @keyframes -global-word {
    to {
      opacity: 1;
    }
  }
</style>
