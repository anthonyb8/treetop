// Pull down at the top of a list to refresh, as iOS lists do.

/** How far a pull at the top must go before it refreshes. */
export const PULL_PX = 70;

export class Pull {
  pulled = $state(0);
  #startY: number | null = null;
  #refresh: () => void;

  constructor(refresh: () => void) {
    this.#refresh = refresh;
  }

  start(event: TouchEvent, scroller: HTMLElement | undefined): void {
    this.#startY = scroller && scroller.scrollTop <= 0 ? (event.touches[0]?.clientY ?? null) : null;
  }

  move(event: TouchEvent): void {
    if (this.#startY === null) return;
    const dy = (event.touches[0]?.clientY ?? this.#startY) - this.#startY;
    this.pulled = Math.max(0, Math.min(dy * 0.5, PULL_PX * 1.4));
  }

  end(): void {
    if (this.pulled >= PULL_PX) this.#refresh();
    this.pulled = 0;
    this.#startY = null;
  }
}
