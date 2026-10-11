<!-- Lint: a run started, and its findings grouped by kind, each linking the pages, nodes and
     records it names; the fixes it proposes are held on the Review page. Lint never writes a
     page itself. -->
<script lang="ts">
  import { api } from "../api/client";
  import type { Finding, FindingKind } from "../api/types";
  import { Resource, Action } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { href } from "../lib/router.svelte";
  import { jobs } from "../lib/jobs.svelte";
  import { ago, when } from "../lib/format";
  import { rise } from "../lib/motion";
  import PageHead from "../components/PageHead.svelte";
  import Loading from "../components/Loading.svelte";
  import Empty from "../components/Empty.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import Icon from "../components/Icon.svelte";

  const report = new Resource(() => api.lint());
  const run = new Action();
  let runningJob = $state("");

  const order: FindingKind[] = [
    "contradiction", "stale", "duplicate", "broken_link", "bad_anchor", "unanchored",
    "missing_source", "orphan_record", "unfinished_reading", "frequent_miss", "partial_name",
  ];
  const groups = $derived.by(() => {
    const by = new Map<FindingKind, Finding[]>();
    for (const f of report.data?.findings ?? []) by.set(f.kind, [...(by.get(f.kind) ?? []), f]);
    return order.filter((k) => by.has(k)).map((k) => ({ kind: k, findings: by.get(k) ?? [] }));
  });

  async function start() {
    const job = await run.run(() => api.runLint());
    if (job) {
      runningJob = job.job;
      jobs.track(job);
      const off = jobs.onFinish((j) => {
        if (j.job !== job.job) return;
        runningJob = "";
        off();
        void report.reload();
      });
    }
  }

  function pageHref(p: string): string {
    return p.startsWith("sources/") ? href("/wiki") : href(`/wiki/${p.split("#")[0]}`);
  }
</script>

<PageHead title={t("nav.lint")} lede={t("lint.lede")}>
  {#snippet actions()}
    <button class="btn primary" disabled={run.running || runningJob !== ""} onclick={start}><Icon name="lint" size={16} />{runningJob ? t("lint.running") : t("lint.run")}</button>
  {/snippet}
</PageHead>

{#if run.error}<ErrorNote error={run.error} />{/if}

{#if report.loading && !report.data}
  <Loading rows={3} />
{:else if report.error}
  <ErrorNote error={report.error} retry={() => report.reload()} />
{:else if report.data}
  {#if report.data.run}
    {@const r = report.data.run}
    <p class="run">
      <span>{t("lint.lastRun", { when: when(r.at), ago: ago(r.at) })}</span>
      <span><strong>{r.findings}</strong> {t("lint.findings")}</span>
      <span><strong>{r.pages_reviewed}</strong> {t("lint.reviewed")}</span>
      {#if r.pages_waiting}<span><strong>{r.pages_waiting}</strong> {t("lint.waiting")}</span>{/if}
      {#if r.proposals}<a href={href("/review")}><strong>{r.proposals}</strong> {t("lint.proposals")}</a>{/if}
    </p>
  {/if}
  {#if !report.data.run}
    <Empty title={t("lint.never")} text={t("lint.neverText")} />
  {:else if groups.length === 0}
    <Empty title={t("lint.clean")} text={t("lint.cleanText")} />
  {:else}
    {#each groups as g (g.kind)}
      <section class="group">
        <h2>{t(`lint.kind.${g.kind}`)} <span class="n">{g.findings.length}</span></h2>
        <p class="what">{t(`lint.kindText.${g.kind}`)}</p>
        <ul>
          {#each g.findings as f, i (i)}
            <li class="finding" in:rise={{ index: i }}>
              <p class="detail">{f.detail}</p>
              {#if f.claims.length}
                <ul class="claims">
                  {#each f.claims as c, j (j)}<li><a href={pageHref(c.page)}>{c.page}</a>: <q>{c.text}</q></li>{/each}
                </ul>
              {/if}
              <p class="refs">
                {#each f.pages as p (p)}<a class="chip" href={pageHref(p)}>{p}</a>{/each}
                {#each f.nodes as n (n)}<a class="chip" href={href(`/graph/node/${n}`)}>{n}</a>{/each}
                {#if f.target}<span class="chip target">{f.target}</span>{/if}
                {#if f.count}<span class="count">{t("lint.count", { n: f.count })}</span>{/if}
                {#if f.proposal}<a class="fix" href={href("/review")}>{t("lint.fixHeld")}</a>{/if}
              </p>
            </li>
          {/each}
        </ul>
      </section>
    {/each}
  {/if}
{/if}

<style>
  .run {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
    align-items: baseline;
    margin-bottom: var(--space-5);
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .run strong {
    font-family: var(--serif);
    font-size: var(--step-1);
    color: var(--ink);
  }
  .group {
    margin-bottom: var(--space-6);
  }
  h2 {
    font-size: var(--step-1);
    display: flex;
    gap: var(--space-2);
    align-items: baseline;
  }
  .n {
    font-family: var(--mono);
    font-size: var(--step--1);
    color: var(--muted);
  }
  .what {
    color: var(--muted);
    font-size: var(--step--1);
    margin: var(--space-1) 0 var(--space-3);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-2);
  }
  .finding {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  .detail {
    font-size: var(--step-0);
  }
  .claims {
    font-size: var(--step--1);
    color: var(--ink-2);
    gap: var(--space-1);
  }
  .refs {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    align-items: center;
  }
  .target {
    color: var(--seal);
  }
  .count {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .fix {
    margin-left: auto;
    font-size: var(--step--1);
    font-weight: 600;
    color: var(--seal);
  }
</style>
