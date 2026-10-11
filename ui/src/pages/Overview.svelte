<!-- The overview: what runs (librarian, embeddings, converters, graph store), the tenant's
     work in progress (jobs, proposals) and the last lint and acquisition runs. -->
<script lang="ts">
  import { onDestroy } from "svelte";
  import { api } from "../api/client";
  import type { EmbeddingState } from "../api/types";
  import { Resource } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { href } from "../lib/router.svelte";
  import { ago, bytes } from "../lib/format";
  import { rise } from "../lib/motion";
  import PageHead from "../components/PageHead.svelte";
  import Loading from "../components/Loading.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import Progress from "../components/Progress.svelte";

  const status = new Resource(() => api.status());
  const lint = new Resource(() => api.lint());
  const acquisition = new Resource(() => api.acquisition());

  // While the embedding model is on its way, its progress is followed.
  const timer = window.setInterval(() => {
    const s = status.data?.embedding.status.state;
    if (s === "downloading" || s === "loading" || s === "embedding") void status.reload(true);
  }, 1500);
  onDestroy(() => window.clearInterval(timer));

  function embeddingLine(s: EmbeddingState): string {
    switch (s.state) {
      case "off":
        return t("overview.embedding.off");
      case "on":
        return t("overview.embedding.on");
      case "loading":
        return t("overview.embedding.loading");
      case "downloading":
        return t("overview.embedding.downloading", { done: bytes(s.done_bytes), total: bytes(s.total_bytes) });
      case "embedding":
        return t("overview.embedding.embedding", { done: s.done, total: s.total });
      case "failed":
        return t("overview.embedding.failed");
    }
  }
</script>

<PageHead title={t("overview.title")} lede={t("overview.lede")}>
  {#snippet actions()}
    <button class="btn" onclick={() => { void status.reload(); void lint.reload(); void acquisition.reload(); }}>{t("action.refresh")}</button>
  {/snippet}
</PageHead>

{#if status.loading && !status.data}
  <Loading rows={4} />
{:else if status.error}
  <ErrorNote error={status.error} retry={() => status.reload()} />
{:else if status.data}
  {@const s = status.data}
  {#if !s.local_only}
    <div class="banner" role="note">{t("overview.exposed")}</div>
  {/if}
  <p class="caller">
    <span class="label">{t("overview.tenant")}</span><span class="chip">{s.caller.tenant}</span>
    <span class="label">{t("overview.user")}</span><span class="chip">{s.caller.user ?? t("nav.noUser")}</span>
    {#if s.caller.groups.length}<span class="label">{t("overview.groups")}</span>{#each s.caller.groups as g (g)}<span class="chip">{g}</span>{/each}{/if}
    <span class="service mono">{s.service}</span>
  </p>

  <div class="grid">
    <section class="tile" in:rise={{ index: 0 }}>
      <h2>{t("overview.librarian")}</h2>
      <p class="state" class:ok={s.librarian.on} class:bad={!s.librarian.on}>
        {s.librarian.on ? t("overview.on") : t("overview.off")}
      </p>
      {#if s.librarian.model}<p class="mono small">{s.librarian.model}</p>{/if}
      {#if s.librarian.off_because}<p class="small">{s.librarian.off_because}</p>{/if}
      <a class="more" href={href("/settings/model")}>{t("overview.configureModel")}</a>
    </section>

    <section class="tile" in:rise={{ index: 1 }}>
      <h2>{t("overview.embedding")}</h2>
      <p class="state" class:ok={s.embedding.status.state === "on"} class:bad={s.embedding.status.state === "failed"}>
        {embeddingLine(s.embedding.status)}
      </p>
      {#if s.embedding.status.state === "downloading"}
        <Progress done={s.embedding.status.done_bytes} total={s.embedding.status.total_bytes} label={s.embedding.status.file} />
      {:else if s.embedding.status.state === "embedding"}
        <Progress done={s.embedding.status.done} total={s.embedding.status.total} label={t("overview.embedding.tenant", { n: s.embedding.status.tenant, of: s.embedding.status.tenants })} />
      {:else if s.embedding.status.state === "failed"}
        <p class="small">{s.embedding.status.error}</p>
      {/if}
      {#if !s.embedding.configured}<p class="small">{t("overview.embedding.unconfigured")}</p>{/if}
      <a class="more" href={href("/settings/embedding")}>{t("overview.configureEmbedding")}</a>
    </section>

    <section class="tile" in:rise={{ index: 2 }}>
      <h2>{t("overview.converters")}</h2>
      <p class="formats">
        {#each s.converters.builtin as f (f)}<span class="chip">.{f}</span>{/each}
      </p>
      <p class="small">
        {#if !s.converters.markitdown}
          {t("overview.markitdown.off")}
        {:else if s.converters.markitdown_version}
          {t("overview.markitdown.on", { version: s.converters.markitdown_version })}
        {:else}
          <span class="bad">{t("overview.markitdown.down", { why: s.converters.markitdown_problem ?? "" })}</span>
        {/if}
      </p>
    </section>

    <section class="tile" in:rise={{ index: 3 }}>
      <h2>{t("overview.graphStore")}</h2>
      <p class="state">{s.graph === "neo4j" ? "Neo4j" : "SQLite"}</p>
      <a class="more" href={href("/graph")}>{t("overview.openGraph")}</a>
    </section>

    <section class="tile" in:rise={{ index: 4 }}>
      <h2>{t("overview.work")}</h2>
      <p class="numbers">
        <span><strong>{s.jobs.running}</strong> {t("overview.running")}</span>
        <span><strong>{s.jobs.queued}</strong> {t("overview.queued")}</span>
      </p>
      <p class="numbers">
        <span class:seal={s.proposals > 0}><strong>{s.proposals}</strong> {t("overview.proposals")}</span>
      </p>
      <a class="more" href={href("/review")}>{t("overview.openReview")}</a>
    </section>

    <section class="tile" in:rise={{ index: 5 }}>
      <h2>{t("overview.lastLint")}</h2>
      {#if lint.data?.run}
        <p class="state">{ago(lint.data.run.at)}</p>
        <p class="small">{t("overview.lintLine", { findings: lint.data.run.findings, proposals: lint.data.run.proposals })}</p>
      {:else if lint.error}
        <p class="small">{lint.error.message}</p>
      {:else if !lint.loading}
        <p class="small">{t("overview.never")}</p>
      {/if}
      <a class="more" href={href("/lint")}>{t("overview.openLint")}</a>
    </section>

    <section class="tile" in:rise={{ index: 6 }}>
      <h2>{t("overview.lastAcquisition")}</h2>
      {#if acquisition.data}
        {@const a = acquisition.data}
        {#if !a.available}
          <p class="small">{t("overview.acquisition.unavailable")}</p>
        {:else if !a.settings.enabled}
          <p class="small">{t("overview.acquisition.off")}</p>
        {:else if a.last_run}
          <p class="state">{ago(a.last_run.at)}</p>
          <p class="small">{t("overview.acquisitionLine", { gaps: a.last_run.gaps, proposals: a.last_run.proposals.length })}</p>
        {:else}
          <p class="small">{t("overview.never")}</p>
        {/if}
        {#if a.available && a.settings.enabled}
          <Progress done={a.budget.used} total={a.budget.daily_tokens} label={t("overview.budget")} />
        {/if}
      {:else if acquisition.error}
        <p class="small">{acquisition.error.message}</p>
      {/if}
      <a class="more" href={href("/acquisition")}>{t("overview.openAcquisition")}</a>
    </section>
  </div>
{/if}

<style>
  .banner {
    margin-bottom: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border: 1px solid color-mix(in srgb, var(--warn) 40%, var(--rule));
    border-left: 3px solid var(--warn);
    border-radius: var(--radius);
    background: var(--warn-wash);
    font-size: var(--step--1);
  }
  .caller {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    margin-bottom: var(--space-5);
  }
  .caller .label {
    font-size: var(--step--2);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .service {
    margin-left: auto;
    font-size: var(--step--2);
    color: var(--muted);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(250px, 1fr));
    gap: var(--space-4);
  }
  .tile {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    padding: var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    min-width: 0;
  }
  h2 {
    font-size: var(--step--1);
    font-family: var(--sans);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .state {
    font-family: var(--serif);
    font-size: var(--step-1);
    font-weight: 600;
  }
  .ok {
    color: var(--ok);
  }
  .bad {
    color: var(--seal);
  }
  .seal strong {
    color: var(--seal);
  }
  .small {
    font-size: var(--step--1);
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }
  .formats {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .numbers {
    display: flex;
    gap: var(--space-4);
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .numbers strong {
    font-family: var(--serif);
    font-size: var(--step-2);
    color: var(--ink);
  }
  .more {
    margin-top: auto;
    padding-top: var(--space-2);
    font-size: var(--step--1);
    font-weight: 600;
  }
</style>
