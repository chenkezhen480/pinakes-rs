<!-- A space's pending draft, drawn as a change on the graph: added in the accent, removed as
     ghosts, changed pulsing once; with the reasons the agents gave. Approved on Review. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { DraftView } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { href, router, setQuery } from "../../lib/router.svelte";
  import { when } from "../../lib/format";
  import { draftGraph, draftSize } from "../../lib/draft";
  import GraphCanvas from "../../components/GraphCanvas.svelte";
  import Loading from "../../components/Loading.svelte";
  import Empty from "../../components/Empty.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const spaces = new Resource(() => api.spaces());
  let space = $state(router.query.get("space") ?? "");
  let draft = $state<DraftView | null>(null);
  const load = new Action();

  $effect(() => {
    if (!space && spaces.data?.spaces[0]) space = spaces.data.spaces[0].space;
  });
  $effect(() => {
    const s = space;
    if (!s) return;
    setQuery({ space: s });
    void (async () => {
      draft = (await load.run(() => api.draft(s))) ?? null;
    })();
  });
  const picture = $derived(draft ? draftGraph(draft) : null);
</script>

{#if spaces.loading && !spaces.data}
  <Loading rows={2} />
{:else if spaces.error}
  <ErrorNote error={spaces.error} retry={() => spaces.reload()} />
{:else}
  <div class="bar">
    <label class="field">
      <span>{t("graph.space")}</span>
      <select bind:value={space}>{#each spaces.data?.spaces ?? [] as s (s.space)}<option value={s.space}>{s.name ?? s.space}</option>{/each}</select>
    </label>
    <p class="hint">{t("graph.drafts.lede")}</p>
  </div>
  {#if load.running && !draft}
    <Loading rows={1} />
  {:else if load.error}
    <ErrorNote error={load.error} />
  {:else if draft && picture && draftSize(draft) > 0}
    <div class="summary">
      <span class="add">+{draft.put_nodes.length} {t("graph.drafts.nodes")}</span>
      <span class="add">+{draft.add_edges.length} {t("graph.drafts.edges")}</span>
      <span class="del">−{draft.remove_nodes.length} {t("graph.drafts.nodes")}</span>
      <span class="del">−{draft.remove_edges.length} {t("graph.drafts.edges")}</span>
      {#if draft.proposal}<a class="btn small primary" href={href("/review")}>{t("graph.drafts.review")}</a>{/if}
    </div>
    <GraphCanvas nodes={picture.nodes} edges={picture.edges} height={440} />
    {#if draft.notes.length}
      <ol class="notes">
        {#each draft.notes as n, i (i)}
          <li><span class="when">{when(n.at)}</span>{#if n.by}<span class="chip">{n.by}</span>{/if} <span>{n.reason}</span> <span class="muted">({t("graph.drafts.changes", { n: n.changes })})</span></li>
        {/each}
      </ol>
    {/if}
  {:else}
    <Empty title={t("graph.drafts.empty")} text={t("graph.drafts.emptyText")} />
  {/if}
{/if}

<style>
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
    align-items: flex-end;
    margin-bottom: var(--space-4);
  }
  .bar .field {
    min-width: 220px;
  }
  .hint {
    color: var(--muted);
    font-size: var(--step--1);
    max-width: 60ch;
  }
  .summary {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
    align-items: center;
    margin-bottom: var(--space-3);
    font-weight: 600;
    font-size: var(--step--1);
  }
  .add {
    color: var(--accent);
  }
  .del {
    color: var(--seal);
  }
  .notes {
    margin: var(--space-4) 0 0;
    padding-left: var(--space-5);
    display: grid;
    gap: var(--space-2);
    font-size: var(--step--1);
  }
  .when {
    color: var(--muted);
    margin-right: var(--space-2);
  }
</style>
