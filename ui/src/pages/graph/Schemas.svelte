<!-- Schemas: each one's definition and the librarian's description of it; a new one created,
     or an existing one extended (additive only: what exists stays as it is). -->
<script lang="ts">
  import { slide } from "svelte/transition";
  import { api } from "../../api/client";
  import type { NodeTypeDef, PropertyType, RelationDef, SchemaDef, SchemaOut } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { href } from "../../lib/router.svelte";
  import { dur } from "../../lib/motion";
  import Loading from "../../components/Loading.svelte";
  import Empty from "../../components/Empty.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";
  import Icon from "../../components/Icon.svelte";

  const schemas = new Resource(() => api.schemas());
  const save = new Action();
  let saved = $state("");

  type Prop = { name: string; type: PropertyType; zh: string };
  type NodeRow = { name: string; key: string; zh: string; aliases: string; props: Prop[]; locked: boolean };
  type RelRow = { name: string; from: string; to: string; via: string; zh: string; aliases: string; locked: boolean };

  let editing = $state(false);
  let extending = $state(false);
  let name = $state("");
  let title = $state("");
  let summary = $state("");
  let aliases = $state("");
  let nodes = $state<NodeRow[]>([]);
  let rels = $state<RelRow[]>([]);

  const list = (s: string) =>
    s
      .split(/[,，]/)
      .map((x) => x.trim())
      .filter((x) => x !== "");

  function start(s?: SchemaOut) {
    editing = true;
    extending = !!s;
    saved = "";
    name = s?.schema ?? "";
    title = s?.title ?? "";
    summary = s?.summary ?? "";
    aliases = (s?.aliases ?? []).join(", ");
    nodes = s
      ? Object.entries(s.nodes).map(([n, d]) => ({
          name: n,
          key: d.key,
          zh: d.zh ?? "",
          aliases: (d.aliases ?? []).join(", "),
          props: Object.entries(d.properties ?? {}).map(([p, pd]) => ({ name: p, type: pd.type, zh: pd.zh ?? "" })),
          locked: true,
        }))
      : [{ name: "", key: "", zh: "", aliases: "", props: [], locked: false }];
    rels = s
      ? Object.entries(s.relations).map(([r, d]) => ({
          name: r,
          from: d.from,
          to: d.to,
          via: d.via ?? "",
          zh: d.zh ?? "",
          aliases: (d.aliases ?? []).join(", "),
          locked: true,
        }))
      : [];
  }

  function definition(): SchemaDef {
    const n: Record<string, NodeTypeDef> = {};
    for (const row of nodes) {
      if (!row.name.trim()) continue;
      const properties: NodeTypeDef["properties"] = {};
      for (const p of row.props) if (p.name.trim()) properties[p.name.trim()] = { type: p.type, zh: p.zh.trim() || null };
      n[row.name.trim()] = { key: row.key.trim(), zh: row.zh.trim() || null, aliases: list(row.aliases), properties };
    }
    const r: Record<string, RelationDef> = {};
    for (const row of rels) {
      if (!row.name.trim()) continue;
      r[row.name.trim()] = {
        from: row.from,
        to: row.to,
        via: row.via.trim() || null,
        zh: row.zh.trim() || null,
        aliases: list(row.aliases),
      };
    }
    return { schema: name.trim(), title: title.trim() || null, summary: summary.trim() || null, aliases: list(aliases), nodes: n, relations: r };
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const r = await save.run(() => api.putSchema(definition()));
    if (r) {
      saved = r.page;
      editing = false;
      void schemas.reload();
    }
  }

  const typeNames = $derived(nodes.map((n) => n.name.trim()).filter((n) => n !== ""));
</script>

<div class="bar">
  <p class="muted">{t("graph.schemas.lede")}</p>
  {#if !editing}<button class="btn primary" onclick={() => start()}><Icon name="plus" size={16} />{t("graph.schemas.new")}</button>{/if}
</div>

{#if saved}
  <p class="saved">{t("graph.schemas.saved")} <a href={href(`/wiki/${saved}`)}>{saved}</a></p>
{/if}

{#if editing}
  <form class="editor" onsubmit={submit} transition:slide={{ duration: dur("base") }}>
    <h2>{extending ? t("graph.schemas.extend", { name }) : t("graph.schemas.new")}</h2>
    {#if extending}<p class="hint">{t("graph.schemas.additive")}</p>{/if}
    <div class="grid3">
      <label class="field"><span>{t("graph.schemas.name")}</span><input type="text" class="mono" bind:value={name} required disabled={extending} pattern="[a-z0-9_]+" /></label>
      <label class="field"><span>{t("wiki.field.title")}</span><input type="text" bind:value={title} /></label>
      <label class="field"><span>{t("wiki.field.aliases")}</span><input type="text" bind:value={aliases} /></label>
    </div>
    <label class="field"><span>{t("wiki.field.summary")}</span><input type="text" bind:value={summary} /></label>

    <h3>{t("graph.schemas.nodeTypes")}</h3>
    {#each nodes as row, i (i)}
      <fieldset class="row" disabled={row.locked}>
        <div class="grid4">
          <label class="field"><span>{t("graph.schemas.type")}</span><input type="text" class="mono" bind:value={row.name} required pattern="[a-z0-9_]+" /></label>
          <label class="field"><span>{t("graph.schemas.key")}</span><input type="text" class="mono" bind:value={row.key} required /></label>
          <label class="field"><span>{t("graph.schemas.zh")}</span><input type="text" bind:value={row.zh} /></label>
          <label class="field"><span>{t("wiki.field.aliases")}</span><input type="text" bind:value={row.aliases} /></label>
        </div>
        {#each row.props as p, j (j)}
          <div class="prop">
            <input type="text" class="mono" bind:value={p.name} placeholder={t("graph.schemas.property")} aria-label={t("graph.schemas.property")} />
            <select bind:value={p.type} aria-label={t("graph.schemas.propertyType")}>
              <option value="text">text</option><option value="number">number</option><option value="date">date</option><option value="bool">bool</option>
            </select>
            <input type="text" bind:value={p.zh} placeholder={t("graph.schemas.zh")} aria-label={t("graph.schemas.zh")} />
          </div>
        {/each}
        {#if !row.locked}
          <div class="row-actions">
            <button type="button" class="btn quiet small" onclick={() => row.props.push({ name: "", type: "text", zh: "" })}><Icon name="plus" size={14} />{t("graph.schemas.addProperty")}</button>
            <button type="button" class="btn quiet small" onclick={() => nodes.splice(i, 1)}><Icon name="trash" size={14} />{t("action.remove")}</button>
          </div>
        {/if}
      </fieldset>
    {/each}
    <button type="button" class="btn small" onclick={() => nodes.push({ name: "", key: "", zh: "", aliases: "", props: [], locked: false })}><Icon name="plus" size={14} />{t("graph.schemas.addType")}</button>

    <h3>{t("graph.schemas.relations")}</h3>
    {#each rels as row, i (i)}
      <fieldset class="row" disabled={row.locked}>
        <div class="grid6">
          <label class="field"><span>{t("graph.schemas.relation")}</span><input type="text" class="mono" bind:value={row.name} required pattern="[a-z0-9_]+" /></label>
          <label class="field"><span>{t("graph.schemas.from")}</span><select bind:value={row.from} required>{#each typeNames as n (n)}<option value={n}>{n}</option>{/each}</select></label>
          <label class="field"><span>{t("graph.schemas.to")}</span><select bind:value={row.to} required>{#each typeNames as n (n)}<option value={n}>{n}</option>{/each}</select></label>
          <label class="field"><span>{t("graph.schemas.via")}</span><input type="text" class="mono" bind:value={row.via} /></label>
          <label class="field"><span>{t("graph.schemas.zh")}</span><input type="text" bind:value={row.zh} /></label>
          <label class="field"><span>{t("wiki.field.aliases")}</span><input type="text" bind:value={row.aliases} /></label>
        </div>
        {#if !row.locked}
          <div class="row-actions"><button type="button" class="btn quiet small" onclick={() => rels.splice(i, 1)}><Icon name="trash" size={14} />{t("action.remove")}</button></div>
        {/if}
      </fieldset>
    {/each}
    <button type="button" class="btn small" onclick={() => rels.push({ name: "", from: typeNames[0] ?? "", to: typeNames[0] ?? "", via: "", zh: "", aliases: "", locked: false })}><Icon name="plus" size={14} />{t("graph.schemas.addRelation")}</button>

    <p class="hint">{t("graph.schemas.describe")}</p>
    {#if save.error}<ErrorNote error={save.error} />{/if}
    <div class="buttons">
      <button type="button" class="btn quiet" onclick={() => (editing = false)}>{t("action.cancel")}</button>
      <button class="btn primary" disabled={save.running}>{save.running ? t("graph.schemas.describing") : t("action.save")}</button>
    </div>
  </form>
{/if}

{#if schemas.loading && !schemas.data}
  <Loading rows={2} />
{:else if schemas.error}
  <ErrorNote error={schemas.error} retry={() => schemas.reload()} />
{:else if schemas.data && schemas.data.schemas.length === 0 && !editing}
  <Empty title={t("graph.schemas.empty")} text={t("graph.schemas.emptyText")} />
{:else if schemas.data}
  <div class="cards">
    {#each schemas.data.schemas as s (s.schema)}
      <article class="schema">
        <header>
          <div>
            <h2>{s.title ?? s.schema}</h2>
            <p class="mono id">{s.schema}{#if s.aliases?.length} · {s.aliases.join(", ")}{/if}</p>
          </div>
          <div class="actions">
            <a class="btn small" href={href(`/wiki/${s.page}`)}>{t("graph.schemas.description")}</a>
            <button class="btn small" onclick={() => start(s)}>{t("graph.schemas.extendButton")}</button>
          </div>
        </header>
        {#if s.summary}<p class="summary">{s.summary}</p>{/if}
        <table>
          <thead><tr><th>{t("graph.schemas.type")}</th><th>{t("graph.schemas.key")}</th><th>{t("graph.schemas.properties")}</th></tr></thead>
          <tbody>
            {#each Object.entries(s.nodes) as [n, d] (n)}
              <tr><td class="mono">{n}{#if d.zh} <span class="muted">{d.zh}</span>{/if}</td><td class="mono">{d.key}</td><td class="mono">{Object.keys(d.properties ?? {}).join(", ")}</td></tr>
            {/each}
          </tbody>
        </table>
        {#if Object.keys(s.relations).length}
          <ul class="rels">
            {#each Object.entries(s.relations) as [r, d] (r)}
              <li><span class="mono">{d.from}</span> <span class="rel">—{r}→</span> <span class="mono">{d.to}</span>{#if d.zh} <span class="muted">{d.zh}</span>{/if}</li>
            {/each}
          </ul>
        {/if}
        <p class="spaces">{t("graph.schemas.spaces")}: {#each s.spaces as sp (sp)}<span class="chip">{sp}</span>{:else}<span class="muted">{t("graph.schemas.noSpaces")}</span>{/each}</p>
      </article>
    {/each}
  </div>
{/if}

<style>
  .bar {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-4);
    margin-bottom: var(--space-4);
  }
  .saved {
    margin-bottom: var(--space-4);
    padding: var(--space-3);
    background: var(--ok-wash);
    border-radius: var(--radius);
    font-size: var(--step--1);
  }
  .editor {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-5);
    margin-bottom: var(--space-5);
    background: var(--card);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lift);
  }
  .editor h2 {
    font-size: var(--step-2);
  }
  .editor h3 {
    font-size: var(--step--1);
    font-family: var(--sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
    margin-top: var(--space-3);
  }
  .grid3 {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--space-3);
  }
  .grid4 {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: var(--space-3);
  }
  .grid6 {
    display: grid;
    grid-template-columns: repeat(6, minmax(0, 1fr));
    gap: var(--space-2);
  }
  .row {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: var(--space-3);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    background: var(--paper);
  }
  .row:disabled {
    opacity: 0.7;
  }
  .prop {
    display: grid;
    grid-template-columns: 1fr 120px 1fr;
    gap: var(--space-2);
  }
  .row-actions,
  .buttons {
    display: flex;
    gap: var(--space-2);
    justify-content: flex-end;
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .cards {
    display: grid;
    gap: var(--space-4);
  }
  .schema {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  .schema header {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    align-items: flex-start;
  }
  .schema h2 {
    font-size: var(--step-2);
  }
  .id {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .actions {
    display: flex;
    gap: var(--space-2);
  }
  .summary {
    color: var(--ink-2);
  }
  table {
    border-collapse: collapse;
    font-size: var(--step--1);
    width: 100%;
  }
  th,
  td {
    text-align: left;
    padding: var(--space-1) var(--space-3);
    border-bottom: 1px solid var(--rule);
  }
  th {
    color: var(--muted);
    font-weight: 600;
  }
  .rels {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    font-size: var(--step--1);
  }
  .rel {
    color: var(--accent);
    font-family: var(--mono);
  }
  .spaces {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    align-items: center;
    font-size: var(--step--1);
    color: var(--muted);
  }
  @media (max-width: 860px) {
    .grid3,
    .grid4,
    .grid6 {
      grid-template-columns: 1fr 1fr;
    }
  }
</style>
