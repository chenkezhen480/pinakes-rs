<!-- The wiki's pages the caller may see, as catalogue cards: filtered by kind, audience and
     words in their titles and summaries; the service's index and log first. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { Kind, PageSummary } from "../../api/types";
  import { Resource } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { router, setQuery, goWithTransition } from "../../lib/router.svelte";
  import { ago } from "../../lib/format";
  import { rise } from "../../lib/motion";
  import PageHead from "../../components/PageHead.svelte";
  import Card from "../../components/Card.svelte";
  import Loading from "../../components/Loading.svelte";
  import Empty from "../../components/Empty.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const kinds: Kind[] = ["expertise", "experience", "decision", "preference", "graph"];
  type Audience = "all" | "tenant" | "group" | "user";
  const pages = new Resource(() => api.pages());

  let kind = $state<Kind | "all">(kinds.find((k) => k === router.query.get("kind")) ?? "all");
  let audience = $state<Audience>("all");
  let words = $state(router.query.get("q") ?? "");

  function audienceOf(p: string): Audience {
    if (p.startsWith("users/")) return "user";
    if (p.startsWith("groups/")) return "group";
    return "tenant";
  }

  const special = $derived((pages.data?.pages ?? []).filter((p) => p.kind === null));
  const shown = $derived.by(() => {
    const w = words.trim().toLowerCase();
    return (pages.data?.pages ?? [])
      .filter((p) => p.kind !== null)
      .filter((p) => kind === "all" || p.kind === kind)
      .filter((p) => audience === "all" || audienceOf(p.page) === audience)
      .filter(
        (p) =>
          !w ||
          (p.title ?? "").toLowerCase().includes(w) ||
          (p.summary ?? "").toLowerCase().includes(w) ||
          p.page.toLowerCase().includes(w),
      )
      .sort((a, b) => (b.updated ?? "").localeCompare(a.updated ?? ""));
  });
  const counts = $derived.by(() => {
    const c: Record<string, number> = {};
    for (const p of pages.data?.pages ?? []) if (p.kind) c[p.kind] = (c[p.kind] ?? 0) + 1;
    return c;
  });

  $effect(() => {
    setQuery({ kind: kind === "all" ? undefined : kind, q: words.trim() || undefined });
  });

  function open(e: MouseEvent, p: PageSummary) {
    if (e.metaKey || e.ctrlKey || e.shiftKey) return;
    e.preventDefault();
    goWithTransition(`/wiki/${p.page}`);
  }
</script>

<PageHead title={t("nav.wiki")} lede={t("wiki.lede")}>
  {#snippet actions()}
    <button class="btn" onclick={() => pages.reload()}>{t("action.refresh")}</button>
  {/snippet}
</PageHead>

<div class="filters">
  <div class="kinds" role="group" aria-label={t("wiki.byKind")}>
    <button class="pill" class:on={kind === "all"} onclick={() => (kind = "all")}>{t("wiki.all")}</button>
    {#each kinds as k (k)}
      <button class="pill" class:on={kind === k} style:--hue={`var(--k-${k})`} onclick={() => (kind = k)}>
        <span class="dot"></span>{t(`kind.${k}`)}<span class="n">{counts[k] ?? 0}</span>
      </button>
    {/each}
  </div>
  <div class="row">
    <select bind:value={audience} aria-label={t("wiki.byAudience")}>
      <option value="all">{t("wiki.audience.all")}</option>
      <option value="tenant">{t("wiki.audience.tenant")}</option>
      <option value="group">{t("wiki.audience.group")}</option>
      <option value="user">{t("wiki.audience.user")}</option>
    </select>
    <input type="search" bind:value={words} placeholder={t("wiki.filter")} aria-label={t("wiki.filter")} />
  </div>
</div>

{#if pages.loading && !pages.data}
  <Loading rows={6} />
{:else if pages.error}
  <ErrorNote error={pages.error} retry={() => pages.reload()} />
{:else if pages.data}
  {#if special.length}
    <p class="special">
      {#each special as p (p.page)}
        <a class="chip" href={`#/wiki/${p.page}`}>{p.page === "index" ? t("wiki.index") : p.page === "log" ? t("wiki.log") : p.page}</a>
      {/each}
    </p>
  {/if}
  {#if shown.length === 0}
    {#if (pages.data.pages ?? []).some((p) => p.kind !== null)}
      <Empty title={t("wiki.noMatch")} text={t("wiki.noMatchText")} />
    {:else}
      <Empty title={t("wiki.empty")} text={t("wiki.emptyText")}>
        {#snippet action()}<a class="btn primary" href="#/knowledge">{t("wiki.addFirst")}</a>{/snippet}
      </Empty>
    {/if}
  {:else}
    <div class="cards">
      {#each shown as p, i (p.page)}
        <div in:rise={{ index: i }}>
          <Card
            title={p.title ?? p.page}
            kind={p.kind}
            summary={p.summary ?? ""}
            href={`#/wiki/${p.page}`}
            transitionName={`page-${p.page.replace(/[^a-zA-Z0-9]/g, "-")}`}
            onclick={(e) => open(e, p)}
          >
            {#snippet meta()}
              <span class="chip">{p.page}</span>
              {#if p.updated}<span class="when">{ago(p.updated)}</span>{/if}
            {/snippet}
          </Card>
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .filters {
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-5);
  }
  .kinds {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px var(--space-3);
    border: 1px solid var(--rule-2);
    border-radius: 999px;
    background: var(--card);
    color: var(--ink-2);
    font: inherit;
    font-size: var(--step--1);
    font-weight: 600;
    cursor: pointer;
  }
  .pill.on {
    border-color: var(--hue, var(--ink));
    color: var(--hue, var(--ink));
    box-shadow: inset 0 0 0 1px var(--hue, var(--ink));
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    background: var(--hue);
  }
  .n {
    font-family: var(--mono);
    font-size: var(--step--2);
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: var(--space-2);
  }
  .row select {
    width: auto;
  }
  .special {
    display: flex;
    gap: var(--space-2);
    margin-bottom: var(--space-4);
  }
  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: var(--space-4);
  }
  .when {
    font-size: var(--step--2);
    color: var(--muted);
  }
</style>
