<script lang="ts">
  import type { Snippet } from "svelte";

  /**
   * A row a thumb can swipe left, as Mail's are: far enough and letting go
   * does `onswipe`, sending the row off the screen while it happens; short of
   * that it springs back. Up and down stay the page's.
   */
  let {
    label,
    isRounded = false,
    onswipe,
    children,
  }: { label: string; isRounded?: boolean; onswipe: () => Promise<void>; children: Snippet } = $props();

  /** How far a swipe goes before letting go does anything. */
  const ARMED_PX = 96;
  /** How far a finger moves before it counts as a swipe or a scroll. */
  const SLOP_PX = 10;

  let dragged = $state(0);
  let isDragging = $state(false);
  let isLeaving = $state(false);
  let start: { x: number; y: number } | null = null;
  let didSwipe = false;

  function onDown(event: PointerEvent) {
    if (event.pointerType !== "touch" || isLeaving) return;
    start = { x: event.clientX, y: event.clientY };
  }

  function onMove(event: PointerEvent) {
    if (!start) return;
    const left = start.x - event.clientX;
    if (!isDragging) {
      if (Math.abs(event.clientY - start.y) > SLOP_PX) start = null;
      else if (left > SLOP_PX) {
        isDragging = true;
        (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
      }
      return;
    }
    dragged = Math.max(0, left);
  }

  async function onUp() {
    start = null;
    if (!isDragging) return;
    isDragging = false;
    didSwipe = true;
    if (dragged < ARMED_PX) {
      dragged = 0;
      return;
    }
    isLeaving = true;
    await onswipe();
    // Still here means it did not go: the row comes back.
    isLeaving = false;
    dragged = 0;
  }

  function onCancel() {
    start = null;
    isDragging = false;
    dragged = 0;
  }

  /** The tap that ends a swipe is not a tap on what is under it. */
  function onClick(event: MouseEvent) {
    if (!didSwipe) return;
    didSwipe = false;
    event.preventDefault();
    event.stopPropagation();
  }
</script>

<div class="swipe" class:rounded={isRounded}>
  <div class="behind" class:armed={isLeaving || dragged >= ARMED_PX} aria-hidden="true">{label}</div>
  <div
    class="front"
    class:settling={!isDragging}
    role="presentation"
    style:transform={isLeaving ? "translateX(-100%)" : dragged ? `translateX(${-dragged}px)` : null}
    onpointerdown={onDown}
    onpointermove={onMove}
    onpointerup={onUp}
    onpointercancel={onCancel}
    onclickcapture={onClick}
  >
    {@render children()}
  </div>
</div>

<style>
  .swipe {
    position: relative;
    overflow: hidden;
  }
  .rounded {
    border-radius: 14px;
  }
  .behind {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    padding-right: 22px;
    background: var(--bg-high);
    color: var(--muted);
    font-size: 14px;
    font-weight: 600;
    transition:
      background 0.12s,
      color 0.12s;
  }
  .behind.armed {
    background: var(--blue);
    color: var(--on-accent);
  }
  .front {
    position: relative;
    background: var(--bg-raised);
    touch-action: pan-y;
    user-select: none;
    -webkit-user-select: none;
  }
  .rounded .front {
    background: var(--bg);
  }
  .front.settling {
    transition: transform 0.22s cubic-bezier(0.2, 0.8, 0.2, 1);
  }
</style>
