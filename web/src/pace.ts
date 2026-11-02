// How a finished reply is written out. Claude Code writes a reply to its
// transcript whole, so the rhythm is made here, from the words themselves:
// longer words take longer, and a comma, a full stop and the end of a
// paragraph each take a breath, as someone writing does.

/** How long a word takes to come up. */
export const FADE_MS = 260;

/** The most of the next step of an eased motion one frame may take, so a
 * frame that ran long carries on from where it was rather than jumping. */
export function eased(ms: number, over: number): number {
  return 1 - Math.exp(-Math.min(ms, 34) / over);
}

export interface Paced {
  /** The reply's HTML with each word wrapped and given its moment. */
  html: string;
  /** When its last word starts to come up. */
  ms: number;
}

/** The pause after a word, at full pace. */
function beat(word: Element): number {
  const text = (word.textContent ?? "").trimEnd();
  let ms = 20 + 2.5 * Math.min(text.length, 12);
  if (/[,;:]$/.test(text)) ms += 45;
  else if (/[.!?]["')]*$/.test(text)) ms += 110;
  const endsBlock = !word.nextSibling && word.parentElement?.matches("p, li, blockquote");
  return endsBlock ? ms + 140 : ms;
}

/** Wraps each word under `node` in a span, gathering them in reading order. */
function wrap(node: Node, words: Element[]) {
  for (const child of [...node.childNodes]) {
    if (child instanceof Element) {
      // Inline code carries a background, which comes up with its words.
      if (child.matches("code") && !child.parentElement?.matches("pre")) words.push(child);
      else wrap(child, words);
    } else if (child instanceof Text) {
      const pieces = child.data.match(/\s+|\S+\s*/g) ?? [];
      child.replaceWith(
        ...pieces.map((piece) => {
          if (!piece.trim()) return piece;
          const word = document.createElement("span");
          word.textContent = piece;
          words.push(word);
          return word;
        }),
      );
    }
  }
}

/**
 * The reply as `markdown` made it, its words timed to come up one after
 * another, taking no longer than `longest` however long the reply.
 */
export function pace(html: string, longest: number): Paced {
  const page = document.createElement("template");
  page.innerHTML = html;
  const words: Element[] = [];
  wrap(page.content, words);
  const beats = words.map(beat);
  const whole = beats.reduce((sum, ms) => sum + ms, 0);
  const scale = Math.min(1, longest / whole);
  let at = 0;
  words.forEach((word, i) => {
    word.classList.add("word");
    word.setAttribute("style", `--at:${Math.round(at)}ms`);
    at += (beats[i] ?? 0) * scale;
  });
  return { html: page.innerHTML, ms: at };
}
