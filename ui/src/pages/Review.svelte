<!-- Review: every held proposal, rendered by what it is — edits held by the verifier (as a diff
     of the page), a source found outside (its gap, address, screening, license, and the text
     fetched), a graph draft (drawn) — approved, or rejected with a reason given in place. An
     approved card files itself away toward the count. -->
<script lang="ts">
  import { flip } from "svelte/animate";
  import { fade, scale } from "svelte/transition";
  import { api } from "../api/client";
  import type { HeldEdit, Proposal } from "../api/types";
  import { Resource, Action } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { href } from "../lib/router.svelte";
  import { jobs } from "../lib/jobs.svelte";
  import { when } from "../lib/format";
  import { dur, ease } from "../lib/motion";
  import { draftGraph, draftSize, spaceCounts } from "../lib/draft";
  import { diffLines, folded } from "../lib/diff";
  import PageHead from "../components/PageHead.svelte";
  import KindMark from "../components/KindMark.svelte";
  import Loading from "../components/Loading.svelte";
  import Empty from "../components/Empty.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import GraphCanvas from "../components/GraphCanvas.svelte";

  const proposals = new Resource(() => api.proposals());
  // What a draft deleting a space would remove: its counts, as the spaces list gives them.
  const spaces = new Resource(() => api.spaces());
  const acting = new Action();
  let rejecting = $state("");
  let reason = $state("");
  let filed = $state(0);
  let busy = $state("");
  // Current pages, for diffs of edits that change one.
  let current = $state<Record<string, string>>({});
  let fetched = $state<Record<string, string>>({});

  const list = $derived(proposals.data?.proposals ?? []);

  $effect(() => {
    for (const p of list) {
      for (const h of p.edits) {
        const page = h.edit.page;
        if (h.edit.op !== "create" && !(page in current)) {
          current[page] = "";
          void api.page(page).then(
            (r) => (current[page] = r.body),
            () => (current[page] = ""),
          );
        }
      }
    }
  });

  function kindOf(p: Proposal): "edits" | "source" | "draft" {
    if (p.graph) return "draft";
    if (p.candidate) return "source";
    return "edits";
  }

  async function approve(p: Proposal) {
    busy = p.proposal;
    const r = await acting.run(() => api.approve(p.proposal));
    busy = "";
    if (r) {
      jobs.track(r.job);
      filed++;
      if (proposals.data) proposals.data = { proposals: list.filter((x) => x.proposal !== p.proposal) };
    }
  }

  async function reject(e: SubmitEvent, p: Proposal) {
    e.preventDefault();
    busy = p.proposal;
    const r = await acting.run(() => api.reject(p.proposal, reason.trim()));
    busy = "";
    if (r) {
      rejecting = "";
      reason = "";
      if (proposals.data) proposals.data = { proposals: list.filter((x) => x.proposal !== p.proposal) };
    }
  }

  async function showFetched(p: Proposal) {
    if (!p.source || p.proposal in fetched) return;
    fetched[p.proposal] = t("state.loading");
    try {
      const r = await api.read([`sources/${p.source}`], { with_links: false });
      fetched[p.proposal] = r.pages[0]?.text ?? r.pages[0]?.error ?? "";
    } catch (err) {
      fetched[p.proposal] = String(err);
    }
  }

  function newText(h: HeldEdit): string {
    const e = h.edit;
    switch (e.op) {
      case "create":
        return e.body;
      case "replace":
        return e.body;
      case "update":
        return e.text ?? "";
      case "supersede":
        return e.replacement ?? "";
    }
  }

  // Filing away: the card shrinks toward the counter.
  function fileAway(node: Element) {
    return scale(node, { duration: dur("slow"), easing: ease, start: 0.6, opacity: 0 });
  }
</script>

<PageHead title={t("nav.review")} lede={t("review.lede")}>
  {#snippet actions()}
    {#if filed > 0}<span class="filed" in:fade>{t("review.filed", { n: filed })}</span>{/if}
    <button class="btn" onclick={() => proposals.reload()}>{t("action.refresh")}</button>
  {/snippet}
</PageHead>


{#if proposals.loading && !proposals.data}
  <Loading rows={3} />
{:else if proposals.error}
  <ErrorNote error={proposals.error} retry={() => proposals.reload()} />
{:else if list.length === 0}
  <Empty title={t("review.empty")} text={t("review.emptyText")} />
{:else}
  {#if acting.error}<ErrorNote error={acting.error} />{/if}
  <ol class="queue">
    {#each list as p (p.proposal)}
      {@const k = kindOf(p)}
      <li class="proposal" animate:flip={{ duration: dur("base") }} out:fileAway>
        <header>
          <div class="marks">
            <span class={`type ${k}`}>{t(`review.type.${k}`)}</span>
            <KindMark kind={p.kind} />
            <span class="audience mono">{p.audience}</span>
          </div>
          <span class="when">{when(p.created_at)}</span>
        </header>

        {#if k === "edits"}
          {#each p.edits as h, i (i)}
            <section class="edit">
              <h3>
                <span class="op mono">{h.edit.op}</span>
                <a href={href(`/wiki/${h.edit.page}`)}>{h.edit.page}</a>
              </h3>
              <p class="why">{t("review.heldBecause")} {h.reason}</p>
              {#if h.edit.op === "create"}
                <p class="newtitle">{h.edit.title}</p>
                <p class="muted">{h.edit.summary}</p>
              {:else if h.edit.op === "update" && h.edit.heading}
                <p class="muted">{t("review.section")} <strong>{h.edit.heading}</strong></p>
              {:else if h.edit.op === "supersede"}
                <p class="claim"><del>{h.edit.claim}</del></p>
                <p class="muted">{h.edit.reason}</p>
              {/if}
              {#if h.edit.op === "replace"}
                <pre class="diff">{#each folded(diffLines(current[h.edit.page] ?? "", h.edit.body)) as l, j (j)}{#if l.op === "fold"}<span class="fold">⋯ {t("review.unchanged", { n: l.count })}</span>{:else}<span class={l.op}>{l.op === "add" ? "+ " : l.op === "del" ? "− " : "  "}{l.text}</span>{/if}{/each}</pre>
              {:else if newText(h)}
                <pre class="diff">{#each newText(h).split("\n") as line, j (j)}<span class="add">+ {line}</span>{/each}</pre>
              {/if}
            </section>
          {/each}
        {:else if k === "source" && p.candidate}
          {@const c = p.candidate}
          <section class="source">
            <h3><a href={c.url} target="_blank" rel="noopener noreferrer">{c.title}</a></h3>
            <p class="url mono">{c.url}</p>
            <dl>
              <dt>{t("review.gap")}</dt>
              <dd><span class="mono">{c.gap.kind}</span> · {c.gap.target} — {c.gap.detail}</dd>
              <dt>{t("review.why")}</dt>
              <dd>{c.why}</dd>
              <dt>{t("review.screening")}</dt>
              <dd>{c.summary}</dd>
              <dt>{t("review.headings")}</dt>
              <dd class="pre">{c.snippet}</dd>
              <dt>{t("review.license")}</dt>
              <dd>{c.license ?? t("review.noLicense")} · {c.copy ? t("review.copyKept") : t("review.copyNot")}</dd>
              <dt>{t("review.fetchedAt")}</dt>
              <dd>{when(c.fetched_at)}</dd>
            </dl>
            {#if p.source}
              {#if p.proposal in fetched}
                <pre class="fetched">{fetched[p.proposal]}</pre>
              {:else}
                <button class="btn small" onclick={() => showFetched(p)}>{t("review.showFetched")}</button>
              {/if}
            {/if}
          </section>
        {:else if k === "draft" && p.graph && p.graph.delete_space}
          {@const graph = p.graph}
          {@const n = spaceCounts(spaces.data?.spaces.find((s) => s.space === graph.space))}
          <section class="draft">
            <p class="deletes">{t("review.deletesSpace", { space: graph.space, nodes: n?.nodes ?? "?", edges: n?.edges ?? "?" })}</p>
            {#if graph.notes.length}
              <ul class="notes">
                {#each graph.notes as note, i (i)}<li>{#if note.by}<span class="chip">{note.by}</span>{/if} {note.reason}</li>{/each}
              </ul>
            {/if}
          </section>
        {:else if k === "draft" && p.graph}
          {@const g = draftGraph(p.graph)}
          <section class="draft">
            <p>{t("review.draftLine", { space: p.graph.space, n: draftSize(p.graph) })}</p>
            <GraphCanvas nodes={g.nodes} edges={g.edges} height={340} />
            {#if p.graph.notes.length}
              <ul class="notes">
                {#each p.graph.notes as n, i (i)}<li>{#if n.by}<span class="chip">{n.by}</span>{/if} {n.reason}</li>{/each}
              </ul>
            {/if}
          </section>
        {/if}

        <footer>
          {#if rejecting === p.proposal}
            <form class="reason" onsubmit={(e) => reject(e, p)} transition:fade={{ duration: dur("fast") }}>
              <input type="text" bind:value={reason} required placeholder={t("review.reasonPlaceholder")} aria-label={t("review.reason")} />
              <button type="button" class="btn quiet" onclick={() => (rejecting = "")}>{t("action.cancel")}</button>
              <button class="btn danger solid" disabled={busy === p.proposal || !reason.trim()}>{t("review.reject")}</button>
            </form>
          {:else}
            <span class="mono id">{p.proposal}</span>
            <button class="btn danger" disabled={busy === p.proposal} onclick={() => { rejecting = p.proposal; reason = ""; }}>{t("review.reject")}</button>
            <button class="btn primary" disabled={busy === p.proposal} onclick={() => approve(p)}>{t("review.approve")}</button>
          {/if}
        </footer>
      </li>
    {/each}
  </ol>
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
  .filed {
    align-self: center;
    font-size: var(--step--1);
    color: var(--ok);
    font-weight: 600;
  }
  .queue {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-4);
  }
  /* Grid children may shrink below their content (a long diff line scrolls in its box). */
  .proposal > :global(*) {
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .proposal {
    position: relative;
    min-width: 0;
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  .proposal::after {
    content: "";
    position: absolute;
    top: -1px;
    right: -1px;
    width: 22px;
    height: 22px;
    background: linear-gradient(225deg, var(--seal) 50%, transparent 50%);
    border-top-right-radius: var(--radius);
  }
  header {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    align-items: center;
  }
  .marks {
    display: flex;
    gap: var(--space-3);
    align-items: center;
    flex-wrap: wrap;
  }
  .type {
    font-size: var(--step--2);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    padding: 1px var(--space-2);
    border-radius: var(--radius-sm);
    background: var(--seal-wash);
    color: var(--seal);
  }
  .type.source {
    background: var(--warn-wash);
    color: var(--warn);
  }
  .type.draft {
    background: var(--accent-wash);
    color: var(--accent);
  }
  .audience,
  .when,
  .id {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .when {
    margin-right: var(--space-5);
  }
  h3 {
    font-size: var(--step-1);
    display: flex;
    gap: var(--space-2);
    align-items: baseline;
    overflow-wrap: anywhere;
  }
  .op {
    font-size: var(--step--2);
    color: var(--muted);
    font-weight: 400;
  }
  .edit,
  .source,
  .draft {
    display: grid;
    gap: var(--space-2);
  }
  .why {
    font-size: var(--step--1);
    color: var(--seal);
  }
  .newtitle {
    font-family: var(--serif);
    font-weight: 600;
  }
  .claim del {
    color: var(--seal);
  }
  .diff,
  .fetched {
    margin: 0;
    padding: var(--space-3);
    max-height: 360px;
    overflow: auto;
    background: var(--paper-2);
    border-radius: var(--radius-sm);
    font-family: var(--mono);
    font-size: var(--step--2);
    line-height: 1.6;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .diff .add {
    display: block;
    background: var(--ok-wash);
    color: var(--ok);
  }
  .diff .del {
    display: block;
    background: var(--seal-wash);
    color: var(--seal);
    text-decoration: line-through;
  }
  .diff .same {
    display: block;
    color: var(--ink-2);
  }
  .diff .fold {
    display: block;
    color: var(--muted);
    font-style: italic;
  }
  .url {
    font-size: var(--step--2);
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: var(--space-1) var(--space-4);
    margin: 0;
    font-size: var(--step--1);
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .pre {
    white-space: pre-wrap;
  }
  .deletes {
    padding: var(--space-3) var(--space-4);
    border-left: 3px solid var(--seal);
    background: var(--warn-wash);
    border-radius: var(--radius);
    font-weight: 600;
  }
  .notes {
    margin: 0;
    padding-left: var(--space-5);
    font-size: var(--step--1);
  }
  footer {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    justify-content: flex-end;
    padding-top: var(--space-3);
    border-top: 1px solid var(--rule);
  }
  footer .id {
    margin-right: auto;
  }
  .reason {
    display: flex;
    gap: var(--space-2);
    width: 100%;
  }
  @media (max-width: 640px) {
    dl {
      grid-template-columns: 1fr;
    }
    .reason {
      flex-wrap: wrap;
    }
  }
</style>
