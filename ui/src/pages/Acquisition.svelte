<!-- Acquisition: the sites this tenant trusts (edited by the admin), the last crawl of them and
     the last run (gaps, matches, proposals, why it stopped), today's budget, and what was turned
     back or rejected. Nothing found enters the wiki without approval. -->
<script lang="ts">
  import { api } from "../api/client";
  import type { AllowedSource } from "../api/types";
  import { Resource, Action } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { href } from "../lib/router.svelte";
  import { ago, when } from "../lib/format";
  import PageHead from "../components/PageHead.svelte";
  import Loading from "../components/Loading.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import Progress from "../components/Progress.svelte";
  import Icon from "../components/Icon.svelte";

  const status = new Resource(() => api.acquisition());
  const me = new Resource(() => api.status());
  const save = new Action();
  type Row = { domain: string; license: string; copy: boolean; urls: string };
  let enabled = $state(false);
  let rows = $state<Row[]>([]);
  let dirty = $state(false);

  $effect(() => {
    const s = status.data;
    if (s && !dirty) {
      enabled = s.settings.enabled;
      rows = s.settings.sources.map((x) => ({ domain: x.domain, license: x.license ?? "", copy: x.copy, urls: (x.urls ?? []).join("\n") }));
    }
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const tenant = me.data?.caller.tenant;
    if (!tenant) return;
    const sources: AllowedSource[] = rows
      .filter((r) => r.domain.trim())
      .map((r) => ({
        domain: r.domain.trim(),
        license: r.license.trim() || null,
        copy: r.copy,
        urls: r.urls.split("\n").map((u) => u.trim()).filter((u) => u),
      }));
    const r = await save.run(() => api.putAcquisition(tenant, { enabled, sources }));
    if (r) {
      dirty = false;
      void status.reload();
    }
  }
</script>

<PageHead title={t("nav.acquisition")} lede={t("acq.lede")} />

{#if status.loading && !status.data}
  <Loading rows={3} />
{:else if status.error}
  <ErrorNote error={status.error} retry={() => status.reload()} />
{:else if status.data}
  {@const s = status.data}
  {#if !s.available}<p class="banner">{t("acq.unavailable")}</p>{/if}
  <div class="layout">
    <form class="panel" onsubmit={submit} oninput={() => (dirty = true)}>
      <h2>{t("acq.sites")}</h2>
      <p class="hint">{t("acq.sitesText")}</p>
      <label class="check"><input type="checkbox" bind:checked={enabled} onchange={() => (dirty = true)} /> {t("acq.enabled")}</label>
      {#each rows as r, i (i)}
        <fieldset class="site">
          <div class="two">
            <label class="field"><span>{t("acq.domain")}</span><input type="text" class="mono" bind:value={r.domain} placeholder="help.example.com" /></label>
            <label class="field"><span>{t("review.license")}</span><input type="text" bind:value={r.license} placeholder="CC BY 4.0" /></label>
          </div>
          <label class="field"><span>{t("acq.starts")}</span><textarea class="mono" rows="2" bind:value={r.urls} placeholder="https://help.example.com/docs"></textarea></label>
          <div class="siteline">
            <label class="check"><input type="checkbox" bind:checked={r.copy} /> {t("acq.copy")}</label>
            <button type="button" class="btn quiet small" onclick={() => { rows.splice(i, 1); dirty = true; }}><Icon name="trash" size={14} />{t("action.remove")}</button>
          </div>
        </fieldset>
      {/each}
      <button type="button" class="btn small" onclick={() => { rows.push({ domain: "", license: "", copy: false, urls: "" }); dirty = true; }}><Icon name="plus" size={14} />{t("acq.addSite")}</button>
      {#if save.error}<ErrorNote error={save.error} />{/if}
      <div class="buttons"><button class="btn primary" disabled={!dirty || save.running}>{t("action.save")}</button></div>
    </form>

    <div class="side">
      <section class="panel">
        <h2>{t("acq.budget")}</h2>
        <Progress done={s.budget.used} total={s.budget.daily_tokens} label={t("acq.budgetLine", { used: s.budget.used, total: s.budget.daily_tokens })} />
      </section>
      <section class="panel">
        <h2>{t("acq.lastCrawl")}</h2>
        {#if s.last_crawl}
          {@const c = s.last_crawl}
          <p class="muted">{when(c.at)} · {ago(c.at)}</p>
          <dl class="stats">
            <dt>{t("acq.pages")}</dt><dd>{c.pages}</dd>
            <dt>{t("acq.failed")}</dt><dd>{c.failed}</dd>
            <dt>{t("acq.disallowed")}</dt><dd>{c.disallowed}</dd>
            <dt>{t("acq.removed")}</dt><dd>{c.removed}</dd>
          </dl>
          <p class="chips">{#each c.sites as site (site)}<span class="chip">{site}</span>{/each}</p>
        {:else}
          <p class="muted">{t("overview.never")}</p>
        {/if}
      </section>
      <section class="panel">
        <h2>{t("acq.lastRun")}</h2>
        {#if s.last_run}
          {@const r = s.last_run}
          <p class="muted">{when(r.at)} · {ago(r.at)}</p>
          <dl class="stats">
            <dt>{t("acq.gaps")}</dt><dd>{r.gaps_taken} / {r.gaps}</dd>
            <dt>{t("acq.matched")}</dt><dd>{r.matched.length}</dd>
            <dt>{t("acq.proposed")}</dt><dd>{r.proposals.length}</dd>
            <dt>{t("acq.refused")}</dt><dd>{r.refused}</dd>
            <dt>{t("acq.tokens")}</dt><dd>{r.tokens}</dd>
          </dl>
          {#if r.stopped}<p class="stopped">{t("acq.stopped", { why: r.stopped })}</p>{/if}
          {#if r.proposals.length}<a href={href("/review")}>{t("acq.toReview")}</a>{/if}
          {#if r.skipped.length}
            <details><summary>{t("acq.skipped", { n: r.skipped.length })}</summary>
              <ul>{#each r.skipped as k, i (i)}<li><strong>{k.gap}</strong> — {k.reason}</li>{/each}</ul>
            </details>
          {/if}
        {:else}
          <p class="muted">{t("overview.never")}</p>
        {/if}
      </section>
      {#if s.refused.length || s.rejected.length}
        <section class="panel">
          <h2>{t("acq.turnedBack")}</h2>
          <ul class="notes">
            {#each [...s.rejected.map((n) => ({ ...n, how: "rejected" })), ...s.refused.map((n) => ({ ...n, how: "refused" }))].slice(0, 30) as n, i (i)}
              <li><span class={`how ${n.how}`}>{t(`acq.${n.how === "rejected" ? "rejectedBy" : "refusedBy"}`)}</span> <span class="mono url">{n.url}</span> — {n.reason}</li>
            {/each}
          </ul>
        </section>
      {/if}
    </div>
  </div>
{/if}

<style>
  .banner {
    margin-bottom: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border-left: 3px solid var(--warn);
    background: var(--warn-wash);
    border-radius: var(--radius);
    font-size: var(--step--1);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1.3fr) minmax(280px, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  .side {
    display: grid;
    gap: var(--space-4);
  }
  .panel {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    align-content: start;
  }
  h2 {
    font-size: var(--step-1);
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .check {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    font-size: var(--step--1);
  }
  .site {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: var(--space-3);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    background: var(--paper);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-2);
  }
  .siteline {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
  }
  .stats {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--space-1) var(--space-3);
    margin: 0;
    font-size: var(--step--1);
  }
  .stats dt {
    color: var(--muted);
  }
  .stats dd {
    margin: 0;
    font-family: var(--mono);
    text-align: right;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .stopped {
    color: var(--warn);
    font-size: var(--step--1);
  }
  details,
  .notes {
    font-size: var(--step--1);
  }
  .notes {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: var(--space-2);
  }
  .how {
    font-size: var(--step--2);
    font-weight: 700;
    text-transform: uppercase;
    color: var(--warn);
  }
  .how.rejected {
    color: var(--seal);
  }
  .url {
    overflow-wrap: anywhere;
    font-size: var(--step--2);
  }
  @media (max-width: 900px) {
    .layout {
      grid-template-columns: 1fr;
    }
    .two {
      grid-template-columns: 1fr;
    }
  }
</style>
