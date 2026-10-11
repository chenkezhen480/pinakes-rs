<!-- Search, as an agent uses it: the query, keywords and mode; three drawers (memories,
     knowledge, relations) the hits are dealt into, with how each matched and how full each
     drawer is; a route followed opens the page, source or node and counts as read in this
     session. The developer chat is the second tab (with PINAKES_DEV=1). -->
<script lang="ts">
  import { onMount } from "svelte";
  import { api } from "../api/client";
  import type { Hit, Mode, ReadItem, SearchResponse } from "../api/types";
  import { Resource, Action } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { href, router, setQuery, goWithTransition, go } from "../lib/router.svelte";
  import { rise } from "../lib/motion";
  import { plainLinks } from "../lib/format";
  import PageHead from "../components/PageHead.svelte";
  import KindMark from "../components/KindMark.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import Empty from "../components/Empty.svelte";
  import Loading from "../components/Loading.svelte";
  import Icon from "../components/Icon.svelte";
  import Chat from "./search/Chat.svelte";

  const status = new Resource(() => api.status());
  let tab = $state<"search" | "chat">(router.query.get("tab") === "chat" ? "chat" : "search");
  let query = $state(router.query.get("q") ?? "");
  let keywords = $state("");
  let mode = $state<Mode>("keyword");
  let result = $state<SearchResponse | null>(null);
  let ran = $state(0);
  const searching = new Action();
  // One read-tracker session per visit, as an agent has per task.
  const session = `console-${crypto.randomUUID().slice(0, 8)}`;
  let source = $state<ReadItem | null>(null);

  const semanticOn = $derived(status.data?.embedding.status.state === "on");

  async function run(e?: SubmitEvent) {
    e?.preventDefault();
    if (!query.trim()) return;
    setQuery({ q: query.trim(), tab: tab === "chat" ? "chat" : undefined });
    const kw = keywords
      .split(/[,，\s]+/)
      .map((k) => k.trim())
      .filter((k) => k);
    const r = await searching.run(() => api.search(query.trim(), { keywords: kw, mode, session }));
    if (r) {
      result = r;
      ran++;
    }
  }

  // A search named in the address runs when the page opens.
  onMount(() => {
    if (query) void run();
  });

  async function follow(e: MouseEvent, hit: Hit) {
    e.preventDefault();
    // Opened from a search, as an agent would read it: it counts as read in this session.
    const read = await api.read([hit.route.read], { session, heading: hit.route.heading, with_links: false }).catch(() => null);
    if (hit.route.read.startsWith("sources/")) {
      source = read?.pages[0] ?? null;
      return;
    }
    goWithTransition(`/wiki/${hit.page}`);
  }

  const drawers = $derived(
    result
      ? [
          { id: "memories", label: t("search.memories"), slot: result.memories },
          { id: "knowledge", label: t("search.knowledge"), slot: result.knowledge },
        ]
      : [],
  );
  const readPages = $derived(
    result ? [...result.memories.hits, ...result.knowledge.hits].filter((h) => h.read) : [],
  );
</script>

<PageHead title={t("nav.search")} lede={t("search.lede")} />

<nav class="tabs" aria-label={t("search.tabs")}>
  <button class:current={tab === "search"} onclick={() => (tab = "search")}>{t("search.tab.search")}</button>
  <button class:current={tab === "chat"} onclick={() => (tab = "chat")}>{t("search.tab.chat")}</button>
</nav>

{#if tab === "chat"}
  {#if status.data && !status.data.dev}
    <Empty title={t("search.chat.off")} text={t("search.chat.offText")} />
  {:else}
    <Chat />
  {/if}
{:else}
  <form class="ask" onsubmit={run}>
    <input class="query" type="search" bind:value={query} placeholder={t("search.placeholder")} aria-label={t("search.query")} />
    <div class="row">
      <input type="text" class="mono" bind:value={keywords} placeholder={t("search.keywords")} aria-label={t("search.keywords")} />
      <div class="modes" role="radiogroup" aria-label={t("search.mode")}>
        {#each ["keyword", "auto", "semantic", "both"] as const as m (m)}
          <label class:on={mode === m} class:off={!semanticOn && (m === "semantic" || m === "both")} title={!semanticOn && (m === "semantic" || m === "both") ? t("search.semanticOff") : ""}>
            <input type="radio" bind:group={mode} value={m} />{t(`search.mode.${m}`)}
          </label>
        {/each}
      </div>
      <button class="btn primary" disabled={searching.running || !query.trim()}><Icon name="search" size={16} />{t("action.search")}</button>
    </div>
  </form>

  {#if searching.error}
    <ErrorNote error={searching.error} />
  {:else if searching.running && !result}
    <Loading rows={3} />
  {:else if !result}
    <Empty title={t("search.start")} text={t("search.startText")} />
  {:else}
    {#if result.note}<p class="note">{result.note}</p>{/if}
    <p class="facts">
      <span>{t("search.ranIn", { mode: t(`search.mode.${result.mode}`) })}</span>
      {#each result.entities as e (e)}<a class="chip" href={href(e.includes("/") ? `/graph/node/${e}` : "/graph/schemas")}>{e}</a>{/each}
      {#if result.unmatched_keywords.length}<span class="unmatched">{t("search.unmatched")} {result.unmatched_keywords.join(", ")}</span>{/if}
    </p>
    {#key ran}
      <div class="cabinet">
        {#each drawers as d (d.id)}
          <section class="drawer">
            <header class="label">
              <h2>{d.label}</h2>
              <span class={`coverage ${d.slot.coverage}`} title={t(`search.coverage.${d.slot.coverage}`)}><span></span>{t(`search.coverage.${d.slot.coverage}`)}</span>
            </header>
            {#if d.slot.hits.length === 0}
              <p class="none">{t("search.none")}</p>
            {:else}
              <ol>
                {#each d.slot.hits as h, i (h.page)}
                  <li class="hit" class:external={h.external} class:read={h.read} in:rise={{ index: i }} style:view-transition-name={`page-${h.page.replace(/[^a-zA-Z0-9]/g, "-")}`}>
                    <div class="top"><KindMark kind={h.kind} /><span class={`match ${h.match}`}>{t(`search.match.${h.match}`)}</span>{#if h.via}<span class="via">{t("search.viaMemory")}</span>{/if}{#if h.read}<span class="readmark">{t("search.read")}</span>{/if}</div>
                    <h3><a href={`#/wiki/${h.page}`} onclick={(e) => follow(e, h)}>{h.title}</a></h3>
                    <p class="summary">{plainLinks(h.summary)}</p>
                    {#each h.lines.slice(0, 2) as l, j (j)}
                      <p class="line">{#if l.heading}<span class="heading">{l.heading}</span>{/if}{plainLinks(l.text)}</p>
                    {/each}
                    <p class="meta">
                      <span class="chip">{h.route.read}{h.route.heading ? ` # ${h.route.heading}` : ""}</span>
                      {#each h.matched_keywords as k (k)}<span class="kw">{k}</span>{/each}
                      {#if h.external}<span class="trust">{t("card.external")}</span>{/if}
                    </p>
                  </li>
                {/each}
              </ol>
            {/if}
          </section>
        {/each}
        <section class="drawer">
          <header class="label">
            <h2>{t("search.relations")}</h2>
            <span class={`coverage ${result.relations.coverage}`}><span></span>{t(`search.coverage.${result.relations.coverage}`)}</span>
          </header>
          {#if result.relations.edges.length === 0}
            <p class="none">{t("search.none")}</p>
          {:else}
            <ol>
              {#each result.relations.edges as e, i (`${e.from}${e.type}${e.to}`)}
                <li class="edge" in:rise={{ index: i }}>
                  <a href={href(`/graph/node/${e.from}`)}>{e.from_name}</a>
                  <span class="rel">—{e.type}→</span>
                  <a href={href(`/graph/node/${e.to}`)}>{e.to_name}</a>
                  <span class="mono space">{e.space}</span>
                </li>
              {/each}
            </ol>
            {#if result.relations.route}
              {@const r = result.relations.route}
              <button class="btn small" onclick={() => go(`/graph/node/${r.graph}`)}><Icon name="graph" size={14} />{t("search.openGraph")}</button>
            {/if}
          {/if}
        </section>
      </div>
    {/key}
    {#if readPages.length}
      <p class="session">{t("search.sessionRead", { session })} {#each readPages as h (h.page)}<a class="chip" href={href(`/wiki/${h.page}`)}>{h.title}</a>{/each}</p>
    {/if}
  {/if}

  {#if source}
    <div class="sheet" role="dialog" aria-label={t("wiki.source")}>
      <header><h2>{t("wiki.source")}</h2><button class="btn quiet small" onclick={() => (source = null)} aria-label={t("action.close")}><Icon name="close" size={16} /></button></header>
      <p class="mono small">{source.id}</p>
      <pre>{source.text ?? source.error ?? ""}</pre>
    </div>
  {/if}
{/if}

<style>
  .tabs {
    display: flex;
    gap: var(--space-1);
    border-bottom: 1px solid var(--rule);
    margin: calc(-1 * var(--space-3)) 0 var(--space-5);
  }
  .tabs button {
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
  .ask {
    display: grid;
    gap: var(--space-3);
    margin-bottom: var(--space-5);
  }
  .query {
    font-size: var(--step-1);
    min-height: 48px;
    font-family: var(--serif);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
  }
  .row > input {
    flex: 1;
    min-width: 200px;
  }
  .modes {
    display: inline-flex;
    border: 1px solid var(--rule-2);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .modes label {
    padding: 6px var(--space-3);
    font-size: var(--step--1);
    font-weight: 600;
    color: var(--ink-2);
    cursor: pointer;
    background: var(--card);
  }
  .modes label.on {
    background: var(--ink);
    color: var(--paper);
  }
  .modes label.off {
    text-decoration: line-through;
    text-decoration-color: var(--muted);
  }
  .modes input {
    position: absolute;
    opacity: 0;
    pointer-events: none;
  }
  .note {
    margin-bottom: var(--space-3);
    padding: var(--space-2) var(--space-3);
    background: var(--warn-wash);
    border-radius: var(--radius);
    font-size: var(--step--1);
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
    font-size: var(--step--1);
    color: var(--muted);
    margin-bottom: var(--space-4);
  }
  .unmatched {
    color: var(--warn);
  }
  .cabinet {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: var(--space-4);
    align-items: start;
  }
  .drawer {
    background: var(--paper-2);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    padding: var(--space-3);
    display: grid;
    gap: var(--space-3);
    min-width: 0;
  }
  .label {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: var(--space-2) var(--space-3);
    background: var(--card);
    border: 1px solid var(--rule-2);
    border-radius: var(--radius-sm);
    box-shadow: inset 0 -2px 0 var(--rule);
  }
  .label h2 {
    font-size: var(--step-0);
  }
  .coverage {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--step--2);
    color: var(--muted);
  }
  .coverage span {
    width: 34px;
    height: 6px;
    border-radius: 3px;
    background: linear-gradient(90deg, var(--ok) var(--fill, 30%), var(--rule) 0);
  }
  .coverage.strong span {
    --fill: 100%;
  }
  .coverage.weak span {
    --fill: 30%;
    background: linear-gradient(90deg, var(--warn) 30%, var(--rule) 0);
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-2);
    min-width: 0;
  }
  .hit {
    min-width: 0;
    overflow-wrap: anywhere;
    position: relative;
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    box-shadow: var(--shadow);
  }
  .hit.external::before {
    content: "";
    position: absolute;
    left: 4px;
    top: 8px;
    bottom: 8px;
    border-left: 2px dotted var(--warn);
  }
  .top {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    flex-wrap: wrap;
  }
  .match,
  .via,
  .readmark {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .match.meaning {
    color: var(--accent);
  }
  .match.alias {
    color: var(--ok);
  }
  .readmark {
    margin-left: auto;
    color: var(--ok);
  }
  h3 {
    font-size: var(--step-0);
  }
  h3 a {
    color: var(--ink);
    text-decoration: none;
  }
  h3 a:hover {
    color: var(--accent);
    text-decoration: underline;
  }
  .summary {
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .line {
    font-size: var(--step--2);
    color: var(--muted);
    border-left: 2px solid var(--rule);
    padding-left: var(--space-2);
  }
  .heading {
    font-weight: 600;
    margin-right: var(--space-1);
  }
  .meta {
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    align-items: center;
  }
  .kw {
    font-size: var(--step--2);
    color: var(--accent);
    font-family: var(--mono);
  }
  .trust {
    font-size: var(--step--2);
    color: var(--warn);
  }
  .none {
    font-size: var(--step--1);
    color: var(--muted);
    padding: var(--space-2) var(--space-3);
  }
  .edge {
    padding: var(--space-2) var(--space-3);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    font-size: var(--step--1);
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-2);
    align-items: baseline;
  }
  .rel {
    font-family: var(--mono);
    color: var(--accent);
    font-size: var(--step--2);
  }
  .space {
    margin-left: auto;
    font-size: var(--step--2);
    color: var(--muted);
  }
  .session {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    align-items: center;
    margin-top: var(--space-4);
    font-size: var(--step--1);
    color: var(--muted);
  }
  .sheet {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    width: min(560px, 100vw);
    z-index: 40;
    padding: var(--space-5);
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    background: var(--card);
    border-left: 1px solid var(--rule);
    box-shadow: var(--shadow-lift);
    overflow-y: auto;
  }
  .sheet header {
    display: flex;
    justify-content: space-between;
  }
  .sheet pre {
    margin: 0;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--step--1);
    background: var(--paper-2);
    padding: var(--space-3);
    border-radius: var(--radius);
  }
  .small {
    font-size: var(--step--2);
    color: var(--muted);
  }
  @media (max-width: 1100px) {
    .cabinet {
      grid-template-columns: 1fr;
    }
  }
</style>
