// Durations and easing from the tokens, read once; every one is 0 under reduced motion, so
// nothing depends on animation to be understood.
import { cubicOut } from "svelte/easing";

const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");

function ms(name: string, fallback: number): number {
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  const n = Number.parseFloat(v);
  return Number.isFinite(n) ? n : fallback;
}

export function dur(which: "fast" | "base" | "slow" | "stagger"): number {
  if (reduced.matches) return 0;
  const fallback = { fast: 120, base: 200, slow: 320, stagger: 30 }[which];
  return ms(`--${which}`, fallback);
}

export const ease = cubicOut;

/** A rise and fade, for things dealt into place: the n-th waits n staggers. */
export function rise(_node: Element, { index = 0 }: { index?: number } = {}) {
  const d = dur("base");
  return {
    delay: Math.min(index, 12) * dur("stagger"),
    duration: d,
    easing: ease,
    css: (t: number) => `opacity: ${t}; transform: translateY(${(1 - t) * 6}px)`,
  };
}

/** Runs a DOM change as a view transition where the browser has them. */
export function viewTransition(change: () => void): void {
  const doc = document as Document & { startViewTransition?: (cb: () => void) => unknown };
  if (reduced.matches || typeof doc.startViewTransition !== "function") {
    change();
    return;
  }
  doc.startViewTransition(change);
}
