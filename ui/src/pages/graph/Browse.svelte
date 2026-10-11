<!-- Browsing the graph: a space's nodes listed and searched by type and name, one node drawn
     with its neighbours (depth adjustable; a deeper ring grows from the node it came from), its
     properties and the pages anchored to it, and the path between two nodes. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { GraphResponse, NodeOut, PathResponse } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { go, href } from "../../lib/router.svelte";
  import { rise } from "../../lib/motion";
  import GraphCanvas, { type GEdge, type GNode } from "../../components/GraphCanvas.svelte";
  import KindMark from "../../components/KindMark.svelte";
  import Loading from "../../components/Loading.svelte";
  import Empty from "../../components/Empty.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  let { node = "" }: { node?: string } = $props();

  const spaces = new Resource(() => api.spaces());
  let space = $state("");
  // A node opened by its id brings its space along.
  $effect(() => {
    const s = node.split(":")[0];
    if (node && s) space = s;
  });
  let type = $state("");
  let q = $state("");
  let offset = $state(0);
  const LIMIT = 50;
  let list = $state<NodeOut[]>([]);
  let total = $state(0);
  const listing = new Action();

  $effect(() => {
    if (!space && spaces.data?.spaces[0]) space = spaces.data.spaces[0].space;
  });

  let timer = 0;
  $effect(() => {
    const [s, ty, query] = [space, type, q];
    if (!s) return;
    window.clearTimeout(timer);
    timer = window.setTimeout(async () => {
      offset = 0;
      const r = await listing.run(() => api.nodes(s, { type: ty || undefined, q: query.trim() || undefined, limit: LIMIT }));
      if (r) {
        list = r.nodes;
        total = r.total;
      }
    }, 200);
  });

  async function more() {
    offset += LIMIT;
    const r = await listing.run(() => api.nodes(space, { type: type || undefined, q: q.trim() || undefined, offset, limit: LIMIT }));
    if (r) list = [...list, ...r.nodes];
  }

  const current = $derived(spaces.data?.spaces.find((s) => s.space === space));

  // The node drawn
  let depth = $state(1);
  let walk = $state<GraphResponse | null>(null);
  let grewFrom = $state("");
  const walking = new Action();
  let path = $state<PathResponse | null>(null);
  let pathTo = $state("");
  const pathing = new Action();

  $effect(() => {
    const [id, d] = [node, depth];
    if (!id) {
      walk = null;
      return;
    }
    void (async () => {
      const r = await walking.run(() => api.graph(id, { depth: d, max_nodes: 80 }));
      if (r) walk = r;
    })();
  });

  function select(id: string) {
    grewFrom = node;
    path = null;
    go(`/graph/node/${id}`);
  }

  const drawn = $derived.by((): { nodes: GNode[]; edges: GEdge[] } => {
    if (!walk) return { nodes: [], edges: [] };
    const onPath = new Set(path?.edges.map((e) => `${e.from}>${e.type}>${e.to}`) ?? []);
    const nodes: GNode[] = walk.nodes.map((n) => ({
      id: n.id,
      label: n.name,
      type: n.type,
      depth: n.depth,
      from: grewFrom || walk?.node.id,
    }));
    if (path) {
      for (const n of path.nodes) {
        if (!nodes.some((m) => m.id === n.id)) nodes.push({ id: n.id, label: n.name, type: n.type, from: walk.node.id });
      }
    }
    const edges: GEdge[] = walk.edges.map((e) => ({
      from: e.from,
      to: e.to,
      type: e.type,
      mark: onPath.has(`${e.from}>${e.type}>${e.to}`) ? "added" : "normal",
    }));
    for (const e of path?.edges ?? []) {
      if (!walk.edges.some((w) => w.from === e.from && w.to === e.to && w.type === e.type)) {
        edges.push({ from: e.from, to: e.to, type: e.type, mark: "added" });
      }
    }
    return { nodes, edges };
  });

  async function findPath(e: SubmitEvent) {
    e.preventDefault();
    if (!walk || !pathTo.trim()) return;
    const r = await pathing.run(() => api.graphPath(walk!.node.id, pathTo.trim()));
    path = r ?? null;
  }

  function value(v: unknown): string {
    return typeof v === "string" ? v : JSON.stringify(v);
  }
</script>

{#if spaces.loading && !spaces.data}
  <Loading rows={3} />
{:else if spaces.error}
  <ErrorNote error={spaces.error} retry={() => spaces.reload()} />
{:else if spaces.data && spaces.data.spaces.length === 0}
  <Empty title={t("graph.noSpaces")} text={t("graph.noSpacesText")}>
    {#snippet action()}<a class="btn primary" href={href("/graph/schemas")}>{t("graph.startSchema")}</a>{/snippet}
  </Empty>
{:else if spaces.data}
  <div class="layout">
    <aside class="side">
      <label class="field">
        <span>{t("graph.space")}</span>
        <select bind:value={space}>
          {#each spaces.data.spaces as s (s.space)}
            <option value={s.space}>{s.name ? `${s.name} · ${s.space}` : s.space}</option>
          {/each}
        </select>
      </label>
      {#if current}
        <div class="types">
          <button class="pill" class:on={type === ""} onclick={() => (type = "")}>{t("wiki.all")}</button>
          {#each Object.entries(current.nodes) as [ty, n] (ty)}
            <button class="pill" class:on={type === ty} onclick={() => (type = ty)}>{ty} <span class="n">{n}</span></button>
          {/each}
        </div>
      {/if}
      <input type="search" bind:value={q} placeholder={t("graph.searchNodes")} aria-label={t("graph.searchNodes")} />
      {#if listing.error}<ErrorNote error={listing.error} />{/if}
      <p class="count">{t("graph.nodesFound", { n: total })}</p>
      <ul class="nodes">
        {#each list as n, i (n.id)}
          <li in:rise={{ index: i }}>
            <button class:current={n.id === node} onclick={() => select(n.id)}>
              <span class="name">{n.name}</span>
              <span class="mono id">{n.type}/{n.key}</span>
            </button>
          </li>
        {/each}
      </ul>
      {#if list.length < total}<button class="btn small" disabled={listing.running} onclick={more}>{t("action.more")}</button>{/if}
    </aside>

    <section class="stage">
      {#if !node}
        <Empty title={t("graph.pickNode")} text={t("graph.pickNodeText")} />
      {:else if walking.error && !walk}
        <ErrorNote error={walking.error} />
      {:else if !walk}
        <Loading rows={1} />
      {:else}
        <div class="toolbar">
          <label class="depth">
            {t("graph.depth")}
            <input type="range" min="1" max="3" bind:value={depth} />
            <span class="mono">{depth}</span>
          </label>
          {#if walk.truncated}<span class="note">{t("graph.truncated")}</span>{/if}
          <form class="path" onsubmit={findPath}>
            <input type="text" class="mono" bind:value={pathTo} placeholder={t("graph.pathTo")} list="graph-ids" />
            <datalist id="graph-ids">{#each list as n (n.id)}<option value={n.id}>{n.name}</option>{/each}</datalist>
            <button class="btn small" disabled={pathing.running}>{t("graph.findPath")}</button>
          </form>
        </div>
        <GraphCanvas nodes={drawn.nodes} edges={drawn.edges} selected={node} onselect={select} />
        {#if pathing.error}<ErrorNote error={pathing.error} />{/if}
        {#if path}
          <p class="pathline">
            {#if !path.found}
              {t("graph.noPath")}
            {:else}
              {#each path.edges as e, i (i)}<span class="step">{e.from_name} <span class="rel">—{e.type}→</span> {e.to_name}</span>{/each}
            {/if}
          </p>
        {/if}
        <div class="detail">
          <div class="card">
            <h2>{walk.node.name}</h2>
            <p class="mono id">{walk.node.id}</p>
            {#if walk.node.aliases.length}
              <p class="chips">{#each walk.node.aliases as a (a)}<span class="chip">{a}</span>{/each}</p>
            {/if}
            {#if Object.keys(walk.node.properties ?? {}).length}
              <dl>
                {#each Object.entries(walk.node.properties ?? {}) as [k, v] (k)}
                  <dt>{k}</dt>
                  <dd>{value(v)}</dd>
                {/each}
              </dl>
            {/if}
          </div>
          <div class="pages">
            <h3>{t("graph.anchored")}</h3>
            {#if walk.pages.length === 0}
              <p class="muted">{t("graph.noAnchored")}</p>
            {:else}
              <ul>
                {#each walk.pages as p (p.page)}
                  <li>
                    <KindMark kind={p.kind} />
                    <a href={href(`/wiki/${p.page}`)}>{p.title}</a>
                    <span class="mono via">{p.anchored_to}</span>
                  </li>
                {/each}
              </ul>
            {/if}
          </div>
        </div>
      {/if}
    </section>
  </div>
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: 280px minmax(0, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  .side {
    display: grid;
    gap: var(--space-3);
    position: sticky;
    top: var(--space-4);
  }
  .types {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .pill {
    padding: 2px var(--space-3);
    border: 1px solid var(--rule-2);
    border-radius: 999px;
    background: var(--card);
    color: var(--ink-2);
    font: inherit;
    font-size: var(--step--1);
    font-family: var(--mono);
    cursor: pointer;
  }
  .pill.on {
    border-color: var(--accent);
    color: var(--accent);
  }
  .n {
    color: var(--muted);
  }
  .count {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .nodes {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
    max-height: 52vh;
    overflow-y: auto;
  }
  .nodes button {
    display: grid;
    width: 100%;
    text-align: left;
    padding: var(--space-2) var(--space-3);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--ink);
    font: inherit;
    cursor: pointer;
  }
  .nodes button:hover {
    background: var(--card);
    border-color: var(--rule);
  }
  .nodes button.current {
    background: var(--card);
    border-color: var(--accent);
  }
  .name {
    font-weight: 600;
    font-size: var(--step--1);
  }
  .id {
    font-size: var(--step--2);
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .stage {
    display: grid;
    gap: var(--space-3);
    min-width: 0;
  }
  .toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-4);
  }
  .depth {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--step--1);
    font-weight: 600;
    color: var(--ink-2);
  }
  .note {
    font-size: var(--step--1);
    color: var(--warn);
  }
  .path {
    display: flex;
    gap: var(--space-2);
    margin-left: auto;
    min-width: 260px;
  }
  .pathline {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    font-size: var(--step--1);
  }
  .step {
    padding: 2px var(--space-2);
    background: var(--accent-wash);
    border-radius: var(--radius-sm);
  }
  .rel {
    font-family: var(--mono);
    color: var(--accent);
  }
  .detail {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: var(--space-4);
  }
  .card,
  .pages {
    padding: var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    display: grid;
    gap: var(--space-2);
    align-content: start;
  }
  h2 {
    font-size: var(--step-2);
  }
  h3 {
    font-size: var(--step--1);
    font-family: var(--sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  dl {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: var(--space-1) var(--space-3);
    margin: 0;
    font-size: var(--step--1);
  }
  dt {
    font-family: var(--mono);
    color: var(--muted);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .pages ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-2);
  }
  .pages li {
    display: grid;
    gap: 2px;
  }
  .via {
    font-size: var(--step--2);
    color: var(--muted);
  }
  @media (max-width: 960px) {
    .layout {
      grid-template-columns: 1fr;
    }
    .side {
      position: static;
    }
    .detail {
      grid-template-columns: 1fr;
    }
  }
</style>
