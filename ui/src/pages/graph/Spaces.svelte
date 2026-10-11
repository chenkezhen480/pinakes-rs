<!-- Spaces: the ones the caller may see, with what they hold and who sees them; a new one
     created under a schema, shared, private or restricted to members. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { Visibility } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
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
</script>

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
            <div class="links">
              <a href={href("/graph/browse")}>{t("graph.spaces.browse")}</a>
              <a href={href("/graph/drafts", { space: s.space })}>{t("graph.spaces.drafts")}</a>
            </div>
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
      <fieldset class="vis-choice">
        <legend>{t("graph.spaces.visibility")}</legend>
        {#each ["shared", "private", "restricted"] as const as v (v)}
          <label><input type="radio" bind:group={visibility} value={v} /> <span><strong>{t(`graph.vis.${v}`)}</strong> — {t(`graph.vis.${v}.hint`)}</span></label>
        {/each}
      </fieldset>
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
