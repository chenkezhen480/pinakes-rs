<!-- The hall drawn behind the console: three.js is loaded here, on demand, as its own chunk.
     Each station gets a label that floats over it (the dock is the keyboard's way to the same
     places); the sky follows the clock, or `?sky=dawn|day|rain|dusk|night` for a look. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { go, router } from "../lib/router.svelte";
  import { t } from "../lib/i18n.svelte";
  import { STATIONS, type Station } from "./stations";
  import { namedSky, skyNow, type Phase } from "./sky";
  import { canDraw } from "./webgl";
  import type { Hall } from "./scene";

  let {
    station,
    panel,
    covered,
    onFail,
    onPhase,
  }: {
    station: Station | null;
    panel: number;
    /** A page covers the whole window: the hall needn't draw. */
    covered: boolean;
    onFail: () => void;
    onPhase: (p: Phase) => void;
  } = $props();

  let canvas: HTMLCanvasElement | undefined = $state();
  let hall = $state.raw<Hall | null>(null);
  let ready = $state(false);
  let hovered = $state<Station | null>(null);
  const labels: Partial<Record<Station, HTMLElement>> = {};
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // A look asked for with `?sky=` stays until another is asked for (`?sky=now` for the clock),
  // though pages rewrite their own query.
  let forced: string | null = null;
  function sky() {
    const asked = router.query.get("sky");
    if (asked) forced = asked === "now" ? null : asked;
    return (forced ? namedSky(forced) : null) ?? skyNow();
  }

  onMount(() => {
    let disposed = false;
    let timer = 0;
    if (!canvas || !canDraw()) {
      onFail();
      return;
    }
    const target = canvas;
    import("./scene")
      .then((mod) => {
        if (disposed) return;
        const h = mod.createHall({
          canvas: target,
          motion: !reduced,
          onPick: (s) => go(`/${s}`),
          onHover: (s) => (hovered = s),
          onLost: () => onFail(),
        });
        h.onFrame((anchors) => {
          for (const a of anchors) {
            const el = labels[a.station];
            if (!el) continue;
            el.style.transform = `translate(${a.x.toFixed(1)}px, ${a.y.toFixed(1)}px)`;
            el.dataset.off = a.visible ? "0" : "1";
          }
        });
        const s = sky();
        h.setSky(s);
        onPhase(s.phase);
        timer = window.setInterval(() => {
          const next = sky();
          h.setSky(next);
          onPhase(next.phase);
        }, 60_000);
        hall = h;
        ready = true;
      })
      .catch(() => onFail());
    return () => {
      disposed = true;
      window.clearInterval(timer);
      hall?.dispose();
      hall = null;
    };
  });

  // A look asked for in the address.
  $effect(() => {
    void router.query;
    if (!hall) return;
    const s = sky();
    hall.setSky(s);
    onPhase(s.phase);
  });

  $effect(() => {
    hall?.focus(station, panel);
  });

  $effect(() => {
    hall?.highlight(hovered);
  });

  $effect(() => {
    hall?.cover(covered);
  });
</script>

<div class="stage" class:ready class:open={station !== null}>
  <canvas bind:this={canvas} aria-hidden="true"></canvas>
  <div class="labels" aria-hidden="true">
    {#each STATIONS as s (s)}
      <a
        class="label"
        data-off="1"
        class:hot={hovered === s}
        href={`#/${s}`}
        tabindex="-1"
        bind:this={labels[s]}
        onpointerenter={() => (hovered = s)}
        onpointerleave={() => (hovered = null)}
      >
        <span class="name">{t(`hall.station.${s}`)}</span>
        <span class="does">{t(`nav.${s}`)}</span>
      </a>
    {/each}
  </div>
</div>

<style>
  .stage {
    position: fixed;
    inset: 0;
    background: var(--hall-night);
  }
  canvas {
    display: block;
    width: 100%;
    height: 100%;
    opacity: 0;
    transition: opacity var(--slow) var(--ease);
  }
  .ready canvas {
    opacity: 1;
  }
  .labels {
    position: absolute;
    inset: 0;
    pointer-events: none;
    overflow: hidden;
  }
  .label {
    position: absolute;
    left: 0;
    top: 0;
    display: grid;
    justify-items: center;
    gap: 1px;
    translate: -50% -100%;
    padding: var(--space-1) var(--space-3);
    border: 1px solid var(--hall-gold-line);
    border-radius: var(--radius-sm);
    background: var(--hall-label);
    color: var(--hall-ink);
    text-decoration: none;
    white-space: nowrap;
    pointer-events: auto;
    backdrop-filter: blur(3px);
    transition:
      opacity var(--base) var(--ease),
      background var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
  }
  .label::after {
    content: "";
    position: absolute;
    left: 50%;
    top: 100%;
    width: 1px;
    height: 14px;
    background: var(--hall-gold-line);
  }
  .label.hot {
    background: var(--hall-label-hot);
    border-color: var(--hall-gold);
  }
  .name {
    font-family: var(--serif);
    font-size: var(--step-0);
    letter-spacing: 0.04em;
  }
  .does {
    font-size: var(--step--2);
    color: var(--hall-ink-2);
  }
  .label[data-off="1"],
  .open .label {
    opacity: 0;
    pointer-events: none;
  }
  @media (prefers-reduced-motion: reduce) {
    canvas,
    .label {
      transition: none;
    }
  }
</style>
