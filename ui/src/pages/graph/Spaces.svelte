<!-- Spaces: the ones the caller may see, with what they hold and who sees them, which the
     admin may change; a new one created under a schema, shared, private or restricted to
     members. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { SpaceOut, Visibility } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { spaceCounts } from "../../lib/draft";
  import { t } from "../../lib/i18n.svelte";
  import { href } from "../../lib/router.svelte";
  import { rise } from "../../lib/motion";
  import Loading from "../../components/Loading.svelte";
  import Empty from "../../components/Empty.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const spaces = new Resource(() => api.spaces());
  const schemas = new Resource(() => api.schemas());
  const create = new Action();
  let space = $state("");
  let schema = $state("");
  let name = $state("");
  let visibility = $state<Visibility>("shared");
  let users = $state("");
  let groups = $state("");
  // The space whose access is being changed, and the change.
  const change = new Action();
  let editing = $state("");
  let editVis = $state<Visibility>("shared");
  let editUsers = $state("");
  let editGroups = $state("");
  // The space whose deletion waits for a second click.
  const removal = new Action();
  let deleting = $state("");

  async function remove(space: string) {
    const r = await removal.run(() => api.deleteSpace(space));
    if (r) {
      deleting = "";
      void spaces.reload();
    }
  }

  $effect(() => {
    if (!schema && schemas.data?.schemas[0]) schema = schemas.data.schemas[0].schema;
  });

  const list = (s: string) =>
    s
      .split(/[,，]/)
      .map((x) => x.trim())
      .filter((x) => x !== "");

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const r = await create.run(() =>
      api.putSpace({
        space: space.trim(),
        schema,
        name: name.trim() || null,
        aliases: [],
        visibility,
        members: visibility === "restricted" ? { users: list(users), groups: list(groups) } : {},
      }),
    );
    if (r) {
      space = name = users = groups = "";
      void spaces.reload();
    }
  }

  function edit(s: SpaceOut) {
    editing = s.space;
    editVis = s.visibility;
    editUsers = (s.members?.users ?? []).join(", ");
    editGroups = (s.members?.groups ?? []).join(", ");
    change.error = null;
  }

  async function saveAccess(e: SubmitEvent) {
    e.preventDefault();
    const r = await change.run(() =>
      api.putSpaceAccess(editing, {
        visibility: editVis,
        members: editVis === "restricted" ? { users: list(editUsers), groups: list(editGroups) } : {},
      }),
    );
    if (r) {
      editing = "";
      void spaces.reload();
    }
  }
</script>

{#snippet visChoice(name: string, value: Visibility, set: (v: Visibility) => void)}
  <fieldset class="vis-choice">
    <legend>{t("graph.spaces.visibility")}</legend>
    {#each ["shared", "private", "restricted"] as const as v (v)}
      <label><input type="radio" {name} checked={value === v} onchange={() => set(v)} /> <span><strong>{t(`graph.vis.${v}`)}</strong> — {t(`graph.vis.${v}.hint`)}</span></label>
    {/each}
  </fieldset>
{/snippet}

<div class="layout">
  <section>
    {#if spaces.loading && !spaces.data}
      <Loading rows={2} />
    {:else if spaces.error}
      <ErrorNote error={spaces.error} retry={() => spaces.reload()} />
    {:else if spaces.data && spaces.data.spaces.length === 0}
      <Empty title={t("graph.noSpaces")} text={t("graph.spaces.emptyText")} />
    {:else if spaces.data}
      <div class="cards">
        {#each spaces.data.spaces as s, i (s.space)}
          <article class="space" in:rise={{ index: i }}>
            <header>
              <h2>{s.name ?? s.space}</h2>
              <span class={`vis ${s.visibility}`}>{t(`graph.vis.${s.visibility}`)}</span>
            </header>
            <p class="mono id">{s.space} · {s.schema}{#if s.mine} · {t("graph.spaces.mine")}{/if}</p>
            <p class="counts">
              {#each Object.entries(s.nodes) as [ty, n] (ty)}<span><strong>{n}</strong> {ty}</span>{/each}
              <span><strong>{s.edges}</strong> {t("graph.spaces.edges")}</span>
            </p>
            {#if s.members}
              <p class="members">
                {#each s.members.users ?? [] as u (u)}<span class="chip">{u}</span>{/each}
                {#each s.members.groups ?? [] as g (g)}<span class="chip">group:{g}</span>{/each}
              </p>
            {/if}
            {#if editing === s.space}
              <form class="access" onsubmit={saveAccess}>
                {@render visChoice(`vis-${s.space}`, editVis, (v) => (editVis = v))}
                {#if editVis === "restricted"}
                  <label class="field"><span>{t("graph.spaces.users")}</span><input type="text" bind:value={editUsers} /></label>
                  <label class="field"><span>{t("graph.spaces.groups")}</span><input type="text" bind:value={editGroups} /></label>
                {/if}
                {#if change.error}<ErrorNote error={change.error} />{/if}
                <div class="row">
                  <button class="btn primary small" disabled={change.running}>{change.running ? t("state.saving") : t("action.save")}</button>
                  <button class="btn quiet small" type="button" onclick={() => (editing = "")}>{t("action.cancel")}</button>
                </div>
              </form>
            {:else if deleting === s.space}
              {@const n = spaceCounts(s)}
              <div class="confirm">
                <p>{t("graph.spaces.deleteConfirm", { space: s.name ?? s.space, nodes: n?.nodes ?? 0, edges: n?.edges ?? 0 })}</p>
                {#if removal.error}<ErrorNote error={removal.error} />{/if}
                <div class="row">
                  <button class="btn danger solid small" disabled={removal.running} onclick={() => remove(s.space)}>{t("graph.spaces.deleteYes")}</button>
                  <button class="btn quiet small" type="button" onclick={() => (deleting = "")}>{t("action.cancel")}</button>
                </div>
              </div>
            {:else}
              <div class="links">
                <a href={href("/graph/browse")}>{t("graph.spaces.browse")}</a>
                <a href={href("/graph/drafts", { space: s.space })}>{t("graph.spaces.drafts")}</a>
                <button class="linkish" type="button" onclick={() => edit(s)}>{t("graph.spaces.access")}</button>
                <button class="linkish danger" type="button" onclick={() => { deleting = s.space; removal.error = null; }}>{t("graph.spaces.delete")}</button>
              </div>
            {/if}
          </article>
        {/each}
      </div>
    {/if}
  </section>

  <form class="create" onsubmit={submit}>
    <h2>{t("graph.spaces.new")}</h2>
    {#if schemas.data && schemas.data.schemas.length === 0}
      <p class="hint">{t("graph.spaces.needSchema")} <a href={href("/graph/schemas")}>{t("graph.tab.schemas")}</a></p>
    {:else}
      <label class="field"><span>{t("graph.spaces.id")}</span><input type="text" class="mono" bind:value={space} required pattern="[a-z0-9_]+" /><span class="hint">{t("graph.spaces.idHint")}</span></label>
      <label class="field">
        <span>{t("graph.spaces.schema")}</span>
        <select bind:value={schema}>{#each schemas.data?.schemas ?? [] as s (s.schema)}<option value={s.schema}>{s.title ?? s.schema}</option>{/each}</select>
      </label>
      <label class="field"><span>{t("graph.spaces.name")}</span><input type="text" bind:value={name} /></label>
      {@render visChoice("vis-new", visibility, (v) => (visibility = v))}
      {#if visibility === "restricted"}
        <label class="field"><span>{t("graph.spaces.users")}</span><input type="text" bind:value={users} /></label>
        <label class="field"><span>{t("graph.spaces.groups")}</span><input type="text" bind:value={groups} /></label>
      {/if}
      {#if create.error}<ErrorNote error={create.error} />{/if}
      <button class="btn primary" disabled={create.running}>{t("action.create")}</button>
    {/if}
  </form>
</div>

<style>
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1.6fr) minmax(280px, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(260px, 1fr));
    gap: var(--space-4);
  }
  .space,
  .create {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    align-content: start;
  }
  .create {
    gap: var(--space-3);
    padding: var(--space-5);
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: var(--space-2);
    align-items: baseline;
  }
  h2 {
    font-size: var(--step-1);
  }
  .vis {
    font-size: var(--step--2);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ok);
  }
  .vis.private {
    color: var(--seal);
  }
  .vis.restricted {
    color: var(--warn);
  }
  .id {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .counts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .counts strong {
    font-family: var(--serif);
    font-size: var(--step-1);
    color: var(--ink);
  }
  .members {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .links {
    display: flex;
    gap: var(--space-4);
    font-size: var(--step--1);
    font-weight: 600;
  }
  .access {
    display: grid;
    gap: var(--space-2);
    padding-top: var(--space-2);
    border-top: 1px solid var(--rule);
  }
  .access .row {
    display: flex;
    gap: var(--space-2);
  }
  .linkish {
    background: none;
    border: 0;
    padding: 0;
    font: inherit;
    color: var(--accent);
    cursor: pointer;
  }
  .linkish:hover {
    text-decoration: underline;
  }
  .linkish.danger {
    color: var(--seal);
  }
  .confirm {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3);
    border-left: 3px solid var(--seal);
    background: var(--card);
    font-size: var(--step--1);
  }
  .confirm .row {
    display: flex;
    gap: var(--space-2);
  }
  .vis-choice {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    border: 0;
    font-size: var(--step--1);
  }
  .vis-choice legend {
    font-weight: 600;
    color: var(--ink-2);
    margin-bottom: var(--space-1);
  }
  .vis-choice label {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  @media (max-width: 900px) {
    .layout {
      grid-template-columns: 1fr;
    }
  }
</style>
