<!-- Writing nodes and edges into a space as a host does (`POST /ingest/graph/data`): each is
     checked against the space's schema; graph data never touches the wiki. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { DataResponse, NodeIn, EdgeIn } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { href } from "../../lib/router.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";
  import Loading from "../../components/Loading.svelte";
  import Icon from "../../components/Icon.svelte";

  const spaces = new Resource(() => api.spaces());
  const schemas = new Resource(() => api.schemas());
  const write = new Action();
  let space = $state("");
  type NodeRow = { type: string; key: string; name: string; aliases: string };
  type EdgeRow = { from: string; type: string; to: string };
  let nodes = $state<NodeRow[]>([{ type: "", key: "", name: "", aliases: "" }]);
  let edges = $state<EdgeRow[]>([]);
  let done = $state<DataResponse | null>(null);

  $effect(() => {
    if (!space && spaces.data?.spaces[0]) space = spaces.data.spaces[0].space;
  });
  const schema = $derived.by(() => {
    const s = spaces.data?.spaces.find((x) => x.space === space);
    return schemas.data?.schemas.find((x) => x.schema === s?.schema);
  });
  const types = $derived(Object.keys(schema?.nodes ?? {}));
  const relations = $derived(Object.keys(schema?.relations ?? {}));

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const n: NodeIn[] = nodes
      .filter((r) => r.type && r.key.trim())
      .map((r) => ({
        type: r.type,
        key: r.key.trim(),
        name: r.name.trim() || null,
        aliases: r.aliases.split(/[,，]/).map((x) => x.trim()).filter((x) => x),
        properties: {},
      }));
    const ed: EdgeIn[] = edges.filter((r) => r.from.trim() && r.to.trim() && r.type).map((r) => ({ from: r.from.trim(), type: r.type, to: r.to.trim() }));
    const r = await write.run(() => api.putData({ space, schema: null, nodes: n, edges: ed }));
    if (r) {
      done = r;
      nodes = [{ type: types[0] ?? "", key: "", name: "", aliases: "" }];
      edges = [];
    }
  }
</script>

{#if spaces.loading && !spaces.data}
  <Loading rows={2} />
{:else if spaces.error}
  <ErrorNote error={spaces.error} retry={() => spaces.reload()} />
{:else}
  <form class="form" onsubmit={submit}>
    <p class="hint">{t("graph.data.lede")}</p>
    <label class="field">
      <span>{t("graph.space")}</span>
      <select bind:value={space}>{#each spaces.data?.spaces ?? [] as s (s.space)}<option value={s.space}>{s.name ?? s.space}</option>{/each}</select>
    </label>
    <h3>{t("graph.data.nodes")}</h3>
    {#each nodes as row, i (i)}
      <div class="row">
        <select bind:value={row.type} aria-label={t("graph.schemas.type")}><option value="" disabled>{t("graph.schemas.type")}</option>{#each types as ty (ty)}<option value={ty}>{ty}</option>{/each}</select>
        <input type="text" class="mono" bind:value={row.key} placeholder={t("graph.schemas.key")} aria-label={t("graph.schemas.key")} />
        <input type="text" bind:value={row.name} placeholder={t("graph.data.name")} aria-label={t("graph.data.name")} />
        <input type="text" bind:value={row.aliases} placeholder={t("wiki.field.aliases")} aria-label={t("wiki.field.aliases")} />
        <button type="button" class="btn quiet small" onclick={() => nodes.splice(i, 1)} aria-label={t("action.remove")}><Icon name="trash" size={14} /></button>
      </div>
    {/each}
    <button type="button" class="btn small" onclick={() => nodes.push({ type: types[0] ?? "", key: "", name: "", aliases: "" })}><Icon name="plus" size={14} />{t("graph.data.addNode")}</button>
    <h3>{t("graph.data.edges")}</h3>
    {#each edges as row, i (i)}
      <div class="row edge">
        <input type="text" class="mono" bind:value={row.from} placeholder="order/1042" aria-label={t("graph.schemas.from")} />
        <select bind:value={row.type} aria-label={t("graph.schemas.relation")}><option value="" disabled>{t("graph.schemas.relation")}</option>{#each relations as r (r)}<option value={r}>{r}</option>{/each}</select>
        <input type="text" class="mono" bind:value={row.to} placeholder="customer/c1" aria-label={t("graph.schemas.to")} />
        <button type="button" class="btn quiet small" onclick={() => edges.splice(i, 1)} aria-label={t("action.remove")}><Icon name="trash" size={14} /></button>
      </div>
    {/each}
    <button type="button" class="btn small" onclick={() => edges.push({ from: "", type: relations[0] ?? "", to: "" })}><Icon name="plus" size={14} />{t("graph.data.addEdge")}</button>
    {#if write.error}<ErrorNote error={write.error} />{/if}
    {#if done}<p class="done">{t("graph.data.done", { nodes: done.nodes_written, edges: done.edges_written })} <a href={href("/graph/browse")}>{t("graph.spaces.browse")}</a></p>{/if}
    <div class="buttons"><button class="btn primary" disabled={write.running || !space}>{t("graph.data.write")}</button></div>
  </form>
{/if}

<style>
  .form {
    display: grid;
    gap: var(--space-3);
    max-width: 900px;
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  h3 {
    font-size: var(--step--1);
    font-family: var(--sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
    margin-top: var(--space-2);
  }
  .row {
    display: grid;
    grid-template-columns: 140px 1fr 1.2fr 1.2fr auto;
    gap: var(--space-2);
  }
  .row.edge {
    grid-template-columns: 1fr 160px 1fr auto;
  }
  .hint {
    color: var(--muted);
    font-size: var(--step--1);
  }
  .done {
    padding: var(--space-3);
    background: var(--ok-wash);
    border-radius: var(--radius);
    font-size: var(--step--1);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
  }
  @media (max-width: 760px) {
    .row,
    .row.edge {
      grid-template-columns: 1fr 1fr;
    }
  }
</style>
