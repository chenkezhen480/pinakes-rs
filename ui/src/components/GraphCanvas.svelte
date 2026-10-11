<!-- A graph drawn: d3-force computes the positions, Svelte draws the SVG. The layout settles
     visibly for about 600 ms, then freezes; dragging a node wakes only its neighbours. A
     draft's changes are marks: added in the accent, removed as dashed ghosts in seal red,
     changed pulsing once. Wheel or pinch zooms, dragging the background pans. -->
<script lang="ts" module>
  export type Mark = "normal" | "added" | "removed" | "changed";
  export type GNode = { id: string; label: string; type: string; mark?: Mark; depth?: number; from?: string };
  export type GEdge = { from: string; to: string; type: string; mark?: Mark };
</script>

<script lang="ts">
  import { onDestroy, untrack } from "svelte";
  import {
    forceSimulation,
    forceLink,
    forceManyBody,
    forceCenter,
    forceCollide,
    type Simulation,
    type SimulationNodeDatum,
    type SimulationLinkDatum,
  } from "d3-force";
  import { dur } from "../lib/motion";

  let {
    nodes,
    edges,
    selected = "",
    height = 520,
    onselect,
  }: {
    nodes: GNode[];
    edges: GEdge[];
    selected?: string;
    height?: number;
    onselect?: (id: string) => void;
  } = $props();

  type SimNode = SimulationNodeDatum & GNode;
  type SimLink = SimulationLinkDatum<SimNode> & { type: string; mark?: Mark; key: string };

  let width = $state(800);
  // The simulation's own objects, moved by d3 in place; what is drawn is derived per frame.
  let simNodes = $state.raw<SimNode[]>([]);
  let simLinks = $state.raw<SimLink[]>([]);
  let tick = $state(0);
  let sim: Simulation<SimNode, SimLink> | null = null;
  let frame = 0;
  let view = $state({ x: 0, y: 0, k: 1 });

  const palette = ["var(--accent)", "var(--k-experience)", "var(--k-decision)", "var(--k-graph)", "var(--k-preference)", "var(--k-expertise)"];
  const types = $derived([...new Set(nodes.map((n) => n.type))]);
  function hue(type: string): string {
    return palette[types.indexOf(type) % palette.length] ?? "var(--accent)";
  }

  function run(ms: number) {
    cancelAnimationFrame(frame);
    if (!sim) return;
    if (ms <= 0) {
      sim.tick(300);
      sim.stop();
      // Called from the effect that builds the layout: counted without reading `tick` there.
      untrack(() => (tick += 1));
      return;
    }
    const until = performance.now() + ms;
    const step = () => {
      if (!sim) return;
      sim.tick(3);
      tick++;
      if (performance.now() < until && sim.alpha() > sim.alphaMin()) frame = requestAnimationFrame(step);
      else sim.stop();
    };
    frame = requestAnimationFrame(step);
  }

  // A new graph: nodes kept from the last one keep their places; a new node starts where the
  // node it grew from is.
  $effect(() => {
    const [w, h] = [untrack(() => width), height];
    const old = new Map(untrack(() => simNodes).map((n) => [n.id, n]));
    const next: SimNode[] = nodes.map((n) => {
      const was = old.get(n.id);
      const origin = n.from ? old.get(n.from) : undefined;
      return {
        ...n,
        x: was?.x ?? origin?.x ?? w / 2 + (Math.random() - 0.5) * 40,
        y: was?.y ?? origin?.y ?? h / 2 + (Math.random() - 0.5) * 40,
        vx: was?.vx,
        vy: was?.vy,
      };
    });
    const ids = new Set(next.map((n) => n.id));
    const links: SimLink[] = edges
      .filter((e) => ids.has(e.from) && ids.has(e.to))
      .map((e, i) => ({ source: e.from, target: e.to, type: e.type, mark: e.mark, key: `${e.from}>${e.type}>${e.to}#${i}` }));
    simNodes = next;
    simLinks = links;
    sim?.stop();
    sim = forceSimulation<SimNode, SimLink>(next)
      .force("link", forceLink<SimNode, SimLink>(links).id((d) => d.id).distance(110).strength(0.6))
      .force("charge", forceManyBody().strength(-420))
      .force("center", forceCenter(w / 2, h / 2))
      .force("collide", forceCollide(38))
      .stop();
    run(dur("slow") > 0 ? 600 : 0);
  });

  onDestroy(() => {
    cancelAnimationFrame(frame);
    sim?.stop();
  });

  function pos(n: SimNode | string | number | undefined): { x: number; y: number } {
    if (n && typeof n === "object") return { x: n.x ?? 0, y: n.y ?? 0 };
    return { x: 0, y: 0 };
  }

  const drawnNodes = $derived.by(() => {
    void tick;
    return simNodes.map((n) => ({ node: n, x: n.x ?? 0, y: n.y ?? 0 }));
  });
  const drawnLinks = $derived.by(() => {
    void tick;
    return simLinks.map((l) => ({ link: l, a: pos(l.source), b: pos(l.target) }));
  });

  // Dragging a node: it follows the pointer; only it and its neighbours move.
  let dragging: SimNode | null = null;
  let panning: { x: number; y: number; vx: number; vy: number } | null = null;
  let svg: SVGSVGElement | undefined = $state();

  function toGraph(e: PointerEvent): { x: number; y: number } {
    const r = svg?.getBoundingClientRect();
    const px = e.clientX - (r?.left ?? 0);
    const py = e.clientY - (r?.top ?? 0);
    return { x: (px - view.x) / view.k, y: (py - view.y) / view.k };
  }

  function grab(e: PointerEvent, n: SimNode) {
    e.stopPropagation();
    (e.currentTarget as Element).setPointerCapture(e.pointerId);
    dragging = n;
    const neighbours = new Set<string>([n.id]);
    for (const l of simLinks) {
      const s = typeof l.source === "object" ? l.source.id : String(l.source);
      const t = typeof l.target === "object" ? l.target.id : String(l.target);
      if (s === n.id) neighbours.add(t);
      if (t === n.id) neighbours.add(s);
    }
    for (const m of simNodes) {
      if (!neighbours.has(m.id)) {
        m.fx = m.x;
        m.fy = m.y;
      }
    }
  }

  function move(e: PointerEvent) {
    if (dragging) {
      const p = toGraph(e);
      dragging.fx = p.x;
      dragging.fy = p.y;
      sim?.alpha(0.3);
      run(120);
    } else if (panning) {
      view = { ...view, x: panning.vx + e.clientX - panning.x, y: panning.vy + e.clientY - panning.y };
    }
  }

  function release() {
    if (dragging) {
      for (const m of simNodes) {
        m.fx = undefined;
        m.fy = undefined;
      }
      dragging = null;
    }
    panning = null;
  }

  function startPan(e: PointerEvent) {
    panning = { x: e.clientX, y: e.clientY, vx: view.x, vy: view.y };
  }

  function wheel(e: WheelEvent) {
    e.preventDefault();
    const r = svg?.getBoundingClientRect();
    const px = e.clientX - (r?.left ?? 0);
    const py = e.clientY - (r?.top ?? 0);
    const k = Math.min(3, Math.max(0.3, view.k * (e.deltaY < 0 ? 1.12 : 1 / 1.12)));
    view = { k, x: px - ((px - view.x) / view.k) * k, y: py - ((py - view.y) / view.k) * k };
  }

  function short(s: string, n = 14): string {
    return s.length > n ? `${s.slice(0, n - 1)}…` : s;
  }
</script>

<div class="canvas" bind:clientWidth={width} style:height={`${height}px`}>
  <svg
    bind:this={svg}
    {width}
    {height}
    role="img"
    aria-label="graph"
    onpointerdown={startPan}
    onpointermove={move}
    onpointerup={release}
    onpointerleave={release}
    onwheel={wheel}
  >
    <defs>
      <marker id="arrow" viewBox="0 0 10 10" refX="22" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
        <path d="M 0 0 L 10 5 L 0 10 z" fill="var(--muted)" />
      </marker>
    </defs>
    <g transform={`translate(${view.x},${view.y}) scale(${view.k})`}>
      {#each drawnLinks as { link: l, a, b } (l.key)}
        <g class={`edge ${l.mark ?? "normal"}`}>
          <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} marker-end="url(#arrow)" />
          <text x={(a.x + b.x) / 2} y={(a.y + b.y) / 2 - 4}>{l.type}</text>
        </g>
      {/each}
      {#each drawnNodes as { node: n, x, y } (n.id)}
        <g
          class={`node ${n.mark ?? "normal"}`}
          class:selected={n.id === selected}
          transform={`translate(${x},${y})`}
          style:--hue={hue(n.type)}
          role="button"
          tabindex="0"
          aria-label={`${n.label} (${n.type})`}
          onpointerdown={(e) => grab(e, n)}
          onclick={() => onselect?.(n.id)}
          onkeydown={(e) => { if (e.key === "Enter") onselect?.(n.id); }}
        >
          <rect class="hit" x="-34" y="-20" width="68" height="66" rx="6" />
          <circle r={n.depth === 0 ? 17 : 13} />
          <text class="label" y={30}>{short(n.label)}</text>
          <text class="type" y={42}>{n.type}</text>
        </g>
      {/each}
    </g>
  </svg>
  {#if types.length}
    <ul class="legend">
      {#each types as ty (ty)}<li style:--hue={hue(ty)}><span></span>{ty}</li>{/each}
    </ul>
  {/if}
</div>

<style>
  .canvas {
    position: relative;
    width: 100%;
    background:
      radial-gradient(circle at 1px 1px, var(--rule) 1px, transparent 0) 0 0 / 22px 22px,
      var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    overflow: hidden;
    touch-action: none;
  }
  svg {
    display: block;
    cursor: grab;
    user-select: none;
  }
  .edge line {
    stroke: var(--rule-2);
    stroke-width: 1.4;
  }
  .edge text {
    font-size: 10px;
    fill: var(--muted);
    text-anchor: middle;
    font-family: var(--mono);
  }
  .edge.added line {
    stroke: var(--accent);
    stroke-width: 2.2;
  }
  .edge.removed line {
    stroke: var(--seal);
    stroke-dasharray: 5 4;
  }
  .node {
    cursor: pointer;
  }
  /* The whole node, label included, answers a click. */
  .node .hit {
    fill: transparent;
  }
  .node circle {
    fill: var(--card);
    stroke: var(--hue);
    stroke-width: 2.5;
    transition: r var(--fast) var(--ease);
  }
  .node:hover circle,
  .node:focus-visible circle {
    fill: color-mix(in srgb, var(--hue) 18%, var(--card));
  }
  .node.selected circle {
    fill: var(--hue);
  }
  .node.added circle {
    stroke: var(--accent);
    stroke-width: 3.5;
    filter: drop-shadow(0 0 6px color-mix(in srgb, var(--accent) 60%, transparent));
  }
  .node.removed circle {
    stroke: var(--seal);
    stroke-dasharray: 4 3;
    fill: transparent;
  }
  .node.removed text {
    fill: var(--seal);
    text-decoration: line-through;
  }
  .node.changed circle {
    animation: pulse 900ms var(--ease) 1;
  }
  @keyframes pulse {
    40% {
      stroke-width: 7;
    }
  }
  .label {
    font-size: 12px;
    font-weight: 600;
    fill: var(--ink);
    text-anchor: middle;
    font-family: var(--sans);
  }
  .type {
    font-size: 9.5px;
    fill: var(--muted);
    text-anchor: middle;
    font-family: var(--mono);
  }
  .legend {
    position: absolute;
    left: var(--space-3);
    bottom: var(--space-3);
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    margin: 0;
    padding: var(--space-1) var(--space-3);
    list-style: none;
    background: color-mix(in srgb, var(--card) 85%, transparent);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    font-size: var(--step--2);
    font-family: var(--mono);
    color: var(--ink-2);
  }
  .legend li {
    display: flex;
    align-items: center;
    gap: var(--space-1);
  }
  .legend span {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    border: 2px solid var(--hue);
  }
  @media (prefers-reduced-motion: reduce) {
    .node.changed circle {
      animation: none;
      stroke-width: 4;
    }
  }
</style>
