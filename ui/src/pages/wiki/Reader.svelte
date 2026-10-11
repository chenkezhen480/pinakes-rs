<!-- One page, read: its front matter as the back of its card (kind, summary, aliases, anchors,
     sources, updated), the body as the service rendered it, the sources it cites openable, its
     history, and a hand edit that goes through the librarian's check. -->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { api } from "../../api/client";
  import type { JobInfo, PageResponse, ReadItem } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { href, router, setQuery } from "../../lib/router.svelte";
  import { jobs } from "../../lib/jobs.svelte";
  import { ago, when } from "../../lib/format";
  import { dur, ease } from "../../lib/motion";
  import KindMark from "../../components/KindMark.svelte";
  import SafeHtml from "../../components/SafeHtml.svelte";
  import Loading from "../../components/Loading.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";
  import Empty from "../../components/Empty.svelte";
  import Icon from "../../components/Icon.svelte";

  let { path }: { path: string } = $props();

  const page = new Resource(() => api.page(path));
  const history = new Resource(() => api.history(path), false);
  type Tab = "read" | "history" | "edit";
  const tabs: Tab[] = ["read", "history", "edit"];
  let tab = $state<Tab>(tabs.find((x) => x === router.query.get("tab")) ?? "read");
  let old = $state<PageResponse | null>(null);
  let source = $state<ReadItem | null>(null);
  const sourceAction = new Action();
  const oldAction = new Action();

  $effect(() => {
    setQuery({ tab: tab === "read" ? undefined : tab });
    if (tab === "history" && !history.data && !history.loading) void history.reload();
  });

  const transitionName = $derived(`page-${path.replace(/[^a-zA-Z0-9]/g, "-")}`);
  const audience = $derived(
    path.startsWith("users/") ? t("wiki.audience.user") : path.startsWith("groups/") ? t("wiki.audience.group") : t("wiki.audience.tenant"),
  );

  /** The body without its first heading when that only repeats the page's title. */
  function withoutTitle(html: string, title: string): string {
    const m = /^<h1>(.*?)<\/h1>\s*/.exec(html);
    if (!m) return html;
    const text = (m[1] ?? "").replace(/<[^>]+>/g, "").replaceAll("&amp;", "&").trim();
    return text === title.trim() ? html.slice(m[0].length) : html;
  }

  function anchorHref(a: string): string {
    return a.includes(":") ? href(`/graph/node/${a}`) : href(`/wiki/${a}`);
  }

  async function openSource(s: string) {
    const r = await sourceAction.run(() => api.read([`sources/${s}`], { with_links: false }));
    source = r?.pages[0] ?? null;
  }

  async function openOld(version: string) {
    const r = await oldAction.run(() => api.historyVersion(path, version));
    old = r ?? null;
  }

  // The hand edit
  let title = $state("");
  let summary = $state("");
  let aliases = $state("");
  let keywords = $state("");
  let about = $state("");
  let body = $state("");
  const edit = new Action();
  let editJob = $state<JobInfo | null>(null);

  function startEdit(p: PageResponse) {
    title = p.front_matter.title ?? "";
    summary = p.front_matter.summary ?? "";
    aliases = (p.front_matter.aliases ?? []).join(", ");
    keywords = (p.front_matter.keywords ?? []).join(", ");
    about = (p.front_matter.about ?? []).join(", ");
    body = p.body;
    editJob = null;
  }

  $effect(() => {
    if (tab === "edit" && page.data && editJob === null && body === "") startEdit(page.data);
  });

  const list = (s: string) =>
    s
      .split(/[,，\n]/)
      .map((x) => x.trim())
      .filter((x) => x !== "");

  async function submitEdit(e: SubmitEvent, p: PageResponse) {
    e.preventDefault();
    const job = await edit.run(() =>
      api.putPage(path, {
        version: p.version,
        page: null,
        kind: null,
        front_matter: {
          ...p.front_matter,
          title: title.trim() || null,
          summary: summary.trim() || null,
          aliases: list(aliases),
          keywords: list(keywords),
          about: list(about),
        },
        body,
      }),
    );
    if (job) {
      editJob = job;
      jobs.track(job);
      const off = jobs.onFinish((j) => {
        if (j.job !== job.job) return;
        editJob = j;
        off();
        void page.reload(true);
        history.data = null;
      });
    }
  }

  const verdict = $derived.by(() => {
    const j = editJob;
    if (!j) return null;
    if (j.status === "failed") return { tone: "bad", text: j.error ?? t("job.failed") };
    if (j.status !== "done") return { tone: "wait", text: t("wiki.edit.checking") };
    if (j.result?.proposal) return { tone: "held", text: t("wiki.edit.held") };
    if (j.result?.rejected.length) return { tone: "bad", text: j.result.rejected.map((r) => r.reason).join("; ") };
    return { tone: "ok", text: t("wiki.edit.saved") };
  });
</script>

<p class="crumbs"><a href={href("/wiki")}>← {t("nav.wiki")}</a> <span class="mono">{path}</span></p>

{#if page.loading && !page.data}
  <Loading rows={1} />
{:else if page.error}
  <ErrorNote error={page.error} retry={() => page.reload()} />
{:else if page.data}
  {@const p = page.data}
  <article class="reader">
    <header class="title" style:view-transition-name={transitionName}>
      <div class="marks">
        <KindMark kind={p.kind} />
        <span class="audience">{audience}</span>
        {#if p.external}<span class="trust">{t("card.external")}</span>{/if}
      </div>
      <h1>{p.front_matter.title ?? path}</h1>
      {#if p.front_matter.summary}<p class="summary">{p.front_matter.summary}</p>{/if}
    </header>

    <nav class="tabs" aria-label={t("wiki.tabs")}>
      {#each tabs as x (x)}
        <button class:current={tab === x} onclick={() => (tab = x)} aria-pressed={tab === x}>
          <Icon name={x === "read" ? "wiki" : x === "history" ? "history" : "edit"} size={15} />{t(`wiki.tab.${x}`)}
        </button>
      {/each}
    </nav>

    <div class="layout">
      <div class="main">
        {#if tab === "read"}
          {#if p.html}<SafeHtml html={withoutTitle(p.html, p.front_matter.title ?? "")} />{/if}
        {:else if tab === "history"}
          {#if history.loading}
            <Loading rows={2} card={false} />
          {:else if history.error}
            <ErrorNote error={history.error} retry={() => history.reload()} />
          {:else if history.data && history.data.versions.length === 0}
            <Empty title={t("wiki.history.empty")} text={t("wiki.history.emptyText")} />
          {:else if history.data}
            <p class="hint">{t("wiki.history.lede")}</p>
            <ol class="versions">
              {#each [...history.data.versions].reverse() as v, i (v.version)}
                <li>
                  <button class="version" onclick={() => openOld(v.version)}>
                    <span class="mono">{v.version}</span>
                    <span>{when(v.saved_at)}</span>
                    {#if i === 0}<span class="latest">{t("wiki.history.latest")}</span>{/if}
                  </button>
                </li>
              {/each}
            </ol>
            {#if oldAction.error}<ErrorNote error={oldAction.error} />{/if}
            {#if old?.html}
              <section class="old" in:fly={{ y: 8, duration: dur("base"), easing: ease }}>
                <h2>{old.front_matter.title ?? path}</h2>
                <SafeHtml html={old.html} />
              </section>
            {/if}
          {/if}
        {:else}
          {#if p.kind === "graph" || p.kind === null}
            <p class="hint">{t("wiki.edit.notHere")}</p>
          {:else}
            <form class="edit" onsubmit={(e) => submitEdit(e, p)}>
              <p class="hint">{t("wiki.edit.lede")}</p>
              <label class="field"><span>{t("wiki.field.title")}</span><input type="text" bind:value={title} /></label>
              <label class="field"><span>{t("wiki.field.summary")}</span><input type="text" bind:value={summary} /></label>
              <div class="two">
                <label class="field"><span>{t("wiki.field.aliases")}</span><input type="text" bind:value={aliases} /></label>
                <label class="field"><span>{t("wiki.field.keywords")}</span><input type="text" bind:value={keywords} /></label>
              </div>
              <label class="field"><span>{t("wiki.field.about")}</span><input type="text" class="mono" bind:value={about} /><span class="hint">{t("wiki.field.aboutHint")}</span></label>
              <label class="field"><span>{t("wiki.field.body")}</span><textarea class="mono body" bind:value={body} rows="18"></textarea></label>
              {#if edit.error}<ErrorNote error={edit.error} />{/if}
              {#if verdict}<p class={`verdict ${verdict.tone}`} role="status">{verdict.text}</p>{/if}
              <div class="buttons">
                <button type="button" class="btn quiet" onclick={() => startEdit(p)}>{t("wiki.edit.reset")}</button>
                <button type="submit" class="btn primary" disabled={edit.running || editJob?.status === "queued" || editJob?.status === "running"}>{t("wiki.edit.submit")}</button>
              </div>
            </form>
          {/if}
        {/if}
      </div>

      <aside class="back" aria-label={t("wiki.fields")}>
        <dl>
          {#if p.front_matter.aliases?.length}
            <dt>{t("wiki.field.aliases")}</dt>
            <dd class="chips">{#each p.front_matter.aliases as a (a)}<span class="chip">{a}</span>{/each}</dd>
          {/if}
          {#if p.front_matter.keywords?.length}
            <dt>{t("wiki.field.keywords")}</dt>
            <dd class="chips">{#each p.front_matter.keywords as k (k)}<span class="chip">{k}</span>{/each}</dd>
          {/if}
          {#if p.front_matter.about?.length}
            <dt>{t("wiki.field.about")}</dt>
            <dd class="chips">{#each p.front_matter.about as a (a)}<a class="chip" href={anchorHref(a)}>{a}</a>{/each}</dd>
          {/if}
          {#if p.front_matter.sources?.length}
            <dt>{t("wiki.field.sources")}</dt>
            <dd class="chips">
              {#each p.front_matter.sources as s (s)}
                <button class="chip source" onclick={() => openSource(s)}>{s}</button>
              {/each}
            </dd>
          {/if}
          {#if p.front_matter.updated}
            <dt>{t("wiki.field.updated")}</dt>
            <dd>{when(p.front_matter.updated)}{#if ago(p.front_matter.updated)} · {ago(p.front_matter.updated)}{/if}</dd>
          {/if}
          <dt>{t("wiki.field.version")}</dt>
          <dd class="mono small">{p.version}</dd>
        </dl>
      </aside>
    </div>
  </article>

  {#if source || sourceAction.error}
    <div class="drawer" role="dialog" aria-label={t("wiki.source")} transition:fly={{ x: 40, duration: dur("base"), easing: ease }}>
      <header>
        <h2>{t("wiki.source")}</h2>
        <button class="btn quiet small" onclick={() => { source = null; sourceAction.error = null; }} aria-label={t("action.close")}><Icon name="close" size={16} /></button>
      </header>
      {#if sourceAction.error}
        <ErrorNote error={sourceAction.error} />
      {:else if source}
        <p class="mono small">{source.id}</p>
        {#if source.external}<p class="trust">{t("card.external")}</p>{/if}
        {#if source.error}
          <p>{source.error}</p>
        {:else}
          <pre class="text">{source.text}</pre>
        {/if}
      {/if}
    </div>
  {/if}
{/if}

<style>
  .crumbs {
    display: flex;
    gap: var(--space-3);
    align-items: center;
    font-size: var(--step--1);
    color: var(--muted);
    margin-bottom: var(--space-4);
    overflow-wrap: anywhere;
  }
  .crumbs a {
    font-weight: 600;
    text-decoration: none;
  }
  .title {
    display: grid;
    gap: var(--space-2);
    padding-bottom: var(--space-4);
  }
  .marks {
    display: flex;
    gap: var(--space-3);
    align-items: center;
  }
  .audience {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .trust {
    font-size: var(--step--2);
    color: var(--warn);
    font-weight: 600;
  }
  h1 {
    font-size: var(--step-3);
    max-width: 30ch;
  }
  .summary {
    color: var(--ink-2);
    font-size: var(--step-1);
    max-width: 60ch;
    font-family: var(--serif);
  }
  .tabs {
    display: flex;
    gap: var(--space-1);
    border-bottom: 1px solid var(--rule);
    margin-bottom: var(--space-5);
  }
  .tabs button {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    border: 0;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    background: none;
    color: var(--ink-2);
    font: inherit;
    font-weight: 600;
    font-size: var(--step--1);
    cursor: pointer;
  }
  .tabs button.current {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 280px;
    gap: var(--space-6);
    align-items: start;
  }
  .back {
    position: sticky;
    top: var(--space-5);
    padding: var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  dl {
    margin: 0;
    display: grid;
    gap: var(--space-1);
  }
  dt {
    font-size: var(--step--2);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
    margin-top: var(--space-2);
  }
  dd {
    margin: 0;
    font-size: var(--step--1);
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .source {
    display: block;
    max-width: 100%;
    cursor: pointer;
    font: inherit;
    font-family: var(--mono);
    font-size: var(--step--2);
  }
  .source:hover {
    border-color: var(--accent);
    color: var(--accent);
  }
  .small {
    font-size: var(--step--2);
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .hint {
    color: var(--muted);
    font-size: var(--step--1);
    margin-bottom: var(--space-3);
  }
  .versions {
    list-style: none;
    margin: 0 0 var(--space-4);
    padding: 0;
    display: grid;
    gap: var(--space-1);
  }
  .version {
    display: flex;
    gap: var(--space-3);
    align-items: center;
    width: 100%;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    background: var(--card);
    color: var(--ink);
    font: inherit;
    font-size: var(--step--1);
    cursor: pointer;
    text-align: left;
  }
  .version:hover {
    border-color: var(--accent);
  }
  .latest {
    margin-left: auto;
    color: var(--ok);
    font-weight: 600;
  }
  .old {
    padding: var(--space-4);
    border: 1px dashed var(--rule-2);
    border-radius: var(--radius);
    display: grid;
    gap: var(--space-3);
  }
  .old h2 {
    font-size: var(--step-1);
  }
  .edit {
    display: grid;
    gap: var(--space-3);
    max-width: 760px;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
  }
  .body {
    font-size: var(--step--1);
    line-height: 1.6;
  }
  .verdict {
    padding: var(--space-3);
    border-radius: var(--radius);
    font-size: var(--step--1);
    background: var(--paper-2);
  }
  .verdict.ok {
    background: var(--ok-wash);
  }
  .verdict.held {
    background: var(--seal-wash);
  }
  .verdict.bad {
    background: var(--seal-wash);
    color: var(--seal);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
  .drawer {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(560px, 100vw);
    z-index: 40;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-5);
    background: var(--card);
    border-left: 1px solid var(--rule);
    box-shadow: var(--shadow-lift);
    overflow-y: auto;
  }
  .drawer header {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .drawer h2 {
    font-size: var(--step-2);
  }
  .text {
    margin: 0;
    padding: var(--space-3);
    background: var(--paper-2);
    border-radius: var(--radius);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--step--1);
    line-height: 1.6;
  }
  @media (max-width: 960px) {
    .layout {
      grid-template-columns: 1fr;
    }
    .back {
      position: static;
      order: -1;
    }
    .two {
      grid-template-columns: 1fr;
    }
  }
</style>
