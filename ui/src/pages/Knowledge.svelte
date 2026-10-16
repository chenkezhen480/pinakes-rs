<!-- Adding knowledge: documents uploaded (several at once, dragged in), a document's next
     version, and experiences, decisions and preferences recorded, each for the audience the
     kind allows; every job followed to what it did. -->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { flip } from "svelte/animate";
  import { api, ApiError } from "../api/client";
  import type { JobInfo, Kind } from "../api/types";
  import { Action } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { href, router, setQuery } from "../lib/router.svelte";
  import { session } from "../lib/session.svelte";
  import { jobs } from "../lib/jobs.svelte";
  import { bytes } from "../lib/format";
  import { dur, ease } from "../lib/motion";
  import PageHead from "../components/PageHead.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import Icon from "../components/Icon.svelte";
  import Progress from "../components/Progress.svelte";

  type Tab = "upload" | "experience" | "decision" | "preference" | "text";
  const tabs: Tab[] = ["upload", "experience", "decision", "preference", "text"];
  let tab = $state<Tab>(tabs.find((x) => x === router.query.get("tab")) ?? "upload");
  $effect(() => setQuery({ tab: tab === "upload" ? undefined : tab }));

  const groups = $derived(
    session.groups
      .split(",")
      .map((g) => g.trim())
      .filter((g) => g !== ""),
  );

  /** The audiences a kind may be filed for (as the service allows), its default first. */
  function audiences(kind: Kind): string[] {
    const g = groups.map((x) => `group:${x}`);
    switch (kind) {
      case "experience":
      case "expertise":
        return ["tenant", ...g];
      case "decision":
        return ["user", ...g];
      default:
        return ["user"];
    }
  }

  function audienceName(a: string): string {
    if (a === "tenant") return t("know.audience.tenant");
    if (a === "user") return session.user ? t("know.audience.me", { user: session.user }) : t("know.audience.user");
    return t("know.audience.group", { group: a.slice(6) });
  }

  // Jobs sent from this page, followed in place.
  type Sent = { id: string; label: string; job: JobInfo | null; error: ApiError | null; converter?: string };
  let sent = $state<Sent[]>([]);

  function follow(entry: Sent, job: JobInfo) {
    entry.job = job;
    jobs.track(job);
    const off = jobs.onFinish((j) => {
      if (j.job !== job.job) return;
      const i = sent.findIndex((s) => s.id === entry.id);
      if (i >= 0) sent[i] = { ...sent[i]!, job: j };
      off();
    });
  }

  /** The job as last polled: its status and, while a document is read, how far it has come. */
  function live(s: Sent): JobInfo | null {
    const mine = s.job;
    if (!mine) return null;
    return jobs.list.find((j) => j.job === mine.job) ?? mine;
  }

  function newEntry(label: string, converter?: string): Sent {
    const e: Sent = { id: crypto.randomUUID(), label, job: null, error: null, converter };
    sent = [e, ...sent];
    return sent[0]!;
  }

  // Upload
  let dragging = $state(false);
  let uploadAudience = $state("tenant");
  let documentId = $state("");
  let sourceUrl = $state("");
  let fileInput: HTMLInputElement | undefined = $state();
  const builtin = ["md", "markdown", "txt", "html", "htm", "xhtml", "docx", "xlsx", "xlsm", "csv", "pptx", "pdf"];

  async function upload(files: FileList | File[]) {
    for (const file of Array.from(files)) {
      const ext = file.name.split(".").pop()?.toLowerCase() ?? "";
      const entry = newEntry(`${file.name} · ${bytes(file.size)}`, builtin.includes(ext) ? t("know.byService") : t("know.byMarkitdown"));
      const form = new FormData();
      form.append("file", file, file.name);
      if (documentId.trim()) form.append("document", documentId.trim());
      if (sourceUrl.trim()) form.append("source_url", sourceUrl.trim());
      try {
        const job = await api.upload(form, uploadAudience);
        follow(entry, job);
      } catch (e) {
        entry.error = e instanceof ApiError ? e : new ApiError(0, "error", String(e));
        sent = [...sent];
      }
    }
    documentId = "";
  }

  // Records
  const recordAction = new Action();
  let audience = $state("");
  let entities = $state("");
  let date = $state("");
  let f1 = $state("");
  let f2 = $state("");
  let f3 = $state("");
  let f4 = $state("");
  let flag = $state(false);

  $effect(() => {
    if (tab !== "upload") {
      const kind: Kind = tab === "text" ? "expertise" : tab;
      if (!audiences(kind).includes(audience)) audience = audiences(kind)[0] ?? "";
    }
  });

  const list = (s: string) =>
    s
      .split(/[,，\n]/)
      .map((x) => x.trim())
      .filter((x) => x !== "");
  const opt = (s: string) => (s.trim() ? s.trim() : null);

  async function submitRecord(e: SubmitEvent) {
    e.preventDefault();
    const label = f1.trim().slice(0, 60);
    const job = await recordAction.run(() => {
      switch (tab) {
        case "experience":
          return api.experience(
            { task: f1, outcome: f2, details: opt(f3), succeeded: flag, entities: list(entities), date: opt(date), session: null },
            audience,
          );
        case "decision":
          return api.decision(
            { topic: f1, decision: f2, context: opt(f3), outcome: opt(f4), entities: list(entities), date: opt(date), session: null },
            audience,
          );
        case "preference":
          return api.preference({ statement: f1, observed: flag, entities: list(entities), date: opt(date) }, audience);
        default:
          return api.expertise(
            { title: f1, content: f2, document: opt(f3), source_url: opt(f4), entities: list(entities), date: opt(date) },
            audience,
          );
      }
    });
    if (job) {
      follow(newEntry(`${t(`know.tab.${tab}`)} · ${label}`), job);
      f1 = f2 = f3 = f4 = entities = date = "";
      flag = false;
    }
  }

  function outcome(s: Sent): { tone: string; text: string } {
    if (s.error) return { tone: "bad", text: s.error.message };
    const j = s.job;
    if (!j) return { tone: "wait", text: t("know.sending") };
    if (j.status === "failed") return { tone: "bad", text: j.error ?? t("job.failed") };
    if (j.status !== "done") return { tone: "wait", text: t(`job.status.${live(s)?.status ?? j.status}`) };
    const r = j.result;
    const parts: string[] = [];
    if (r?.pages_created.length) parts.push(t("job.created", { n: r.pages_created.length }));
    if (r?.pages_updated.length) parts.push(t("job.updated", { n: r.pages_updated.length }));
    if (r?.proposal) parts.push(t("job.held"));
    if (r?.approved?.length) parts.push(t("job.approved", { n: r.approved.length }));
    if (!parts.length) parts.push(t("know.nothingChanged"));
    return { tone: r?.proposal ? "held" : "ok", text: parts.join(" · ") };
  }
</script>

<PageHead title={t("nav.knowledge")} lede={t("know.lede")} />

<nav class="tabs" aria-label={t("know.tabs")}>
  {#each tabs as x (x)}
    <button class:current={tab === x} aria-pressed={tab === x} onclick={() => (tab = x)}>{t(`know.tab.${x}`)}</button>
  {/each}
</nav>

<div class="layout">
  <section class="work">
    {#if tab === "upload"}
      <div
        class="drop"
        class:dragging
        role="button"
        tabindex="0"
        aria-label={t("know.drop")}
        ondragover={(e) => { e.preventDefault(); dragging = true; }}
        ondragleave={() => (dragging = false)}
        ondrop={(e) => { e.preventDefault(); dragging = false; if (e.dataTransfer?.files) void upload(e.dataTransfer.files); }}
        onclick={() => fileInput?.click()}
        onkeydown={(e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); fileInput?.click(); } }}
      >
        <Icon name="upload" size={28} />
        <strong>{t("know.drop")}</strong>
        <span>{t("know.formats")}</span>
        <input bind:this={fileInput} type="file" multiple hidden onchange={(e) => { if (e.currentTarget.files) void upload(e.currentTarget.files); e.currentTarget.value = ""; }} />
      </div>
      <div class="options">
        <label class="field">
          <span>{t("know.for")}</span>
          <select bind:value={uploadAudience}>
            {#each audiences("expertise") as a (a)}<option value={a}>{audienceName(a)}</option>{/each}
          </select>
        </label>
        <label class="field">
          <span>{t("know.document")}</span>
          <input type="text" class="mono" bind:value={documentId} placeholder="d_…" />
          <span class="hint">{t("know.documentHint")}</span>
        </label>
        <label class="field">
          <span>{t("know.sourceUrl")}</span>
          <input type="url" bind:value={sourceUrl} placeholder="https://" />
        </label>
      </div>
    {:else}
      {@const kind: Kind = tab === "text" ? "expertise" : tab}
      <form class="record" onsubmit={submitRecord}>
        <p class="hint">{t(`know.lede.${tab}`)}</p>
        {#if tab === "experience"}
          <label class="field"><span>{t("know.f.task")}</span><input type="text" bind:value={f1} required placeholder={t("know.f.taskExample")} /></label>
          <label class="field"><span>{t("know.f.outcome")}</span><textarea bind:value={f2} required rows="3"></textarea></label>
          <label class="field"><span>{t("know.f.details")}</span><textarea bind:value={f3} rows="3"></textarea></label>
          <label class="check"><input type="checkbox" bind:checked={flag} /> {t("know.f.succeeded")}</label>
        {:else if tab === "decision"}
          <label class="field"><span>{t("know.f.topic")}</span><input type="text" bind:value={f1} required /></label>
          <label class="field"><span>{t("know.f.decision")}</span><textarea bind:value={f2} required rows="3"></textarea></label>
          <label class="field"><span>{t("know.f.context")}</span><textarea bind:value={f3} rows="2"></textarea></label>
          <label class="field"><span>{t("know.f.decisionOutcome")}</span><input type="text" bind:value={f4} /></label>
        {:else if tab === "preference"}
          <label class="field"><span>{t("know.f.statement")}</span><textarea bind:value={f1} required rows="3"></textarea></label>
          <label class="check"><input type="checkbox" bind:checked={flag} /> {t("know.f.observed")}</label>
        {:else}
          <label class="field"><span>{t("know.f.title")}</span><input type="text" bind:value={f1} required /></label>
          <label class="field"><span>{t("know.f.content")}</span><textarea class="mono" bind:value={f2} required rows="12"></textarea></label>
          <div class="two">
            <label class="field"><span>{t("know.document")}</span><input type="text" class="mono" bind:value={f3} placeholder="d_…" /></label>
            <label class="field"><span>{t("know.sourceUrl")}</span><input type="url" bind:value={f4} placeholder="https://" /></label>
          </div>
        {/if}
        <div class="two">
          <label class="field"><span>{t("know.f.entities")}</span><input type="text" class="mono" bind:value={entities} placeholder="commerce:order, mainland:customer/c1" /></label>
          <label class="field"><span>{t("know.f.date")}</span><input type="date" bind:value={date} /></label>
        </div>
        <label class="field">
          <span>{t("know.for")}</span>
          <select bind:value={audience}>
            {#each audiences(kind) as a (a)}<option value={a}>{audienceName(a)}</option>{/each}
          </select>
        </label>
        {#if recordAction.error}<ErrorNote error={recordAction.error} />{/if}
        <div class="buttons"><button class="btn primary" disabled={recordAction.running}>{t("know.submit")}</button></div>
      </form>
    {/if}
  </section>

  <aside class="sent" aria-live="polite">
    <h2>{t("know.sent")}</h2>
    {#if sent.length === 0}
      <p class="hint">{t("know.sentEmpty")}</p>
    {:else}
      <ol>
        {#each sent as s (s.id)}
          {@const o = outcome(s)}
          <li class={`item ${o.tone}`} in:fly={{ y: -8, duration: dur("base"), easing: ease }} animate:flip={{ duration: dur("base") }}>
            <strong>{s.label}</strong>
            {#if s.converter && !s.error}<span class="by">{s.converter}</span>{/if}
            <p>{o.text}</p>
            {#if s.job?.status !== "done" && live(s)?.progress}
              {@const p = live(s)!.progress!}
              <Progress done={p.lines_read} total={p.lines_total} label={t("job.reading", { read: p.lines_read, total: p.lines_total, n: p.concepts_filed })} />
            {/if}
            {#if s.job?.status === "done" && s.job.result}
              {@const r = s.job.result}
              <div class="links">
                {#each [...r.pages_created, ...r.pages_updated] as p (p)}<a class="chip" href={href(`/wiki/${p}`)}>{p}</a>{/each}
                {#if r.proposal}<a class="chip held" href={href("/review")}>{t("job.toReview")}</a>{/if}
              </div>
              {#if r.document}
                <p class="doc">{t("know.docId")} <code>{r.document}</code>{#if r.version} · {t("know.version", { n: r.version })}{/if}</p>
              {/if}
            {/if}
          </li>
        {/each}
      </ol>
    {/if}
  </aside>
</div>

<style>
  .tabs {
    display: flex;
    gap: var(--space-1);
    border-bottom: 1px solid var(--rule);
    margin: calc(-1 * var(--space-3)) 0 var(--space-5);
    overflow-x: auto;
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
    white-space: nowrap;
  }
  .tabs button.current {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1.6fr) minmax(260px, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  .work {
    display: grid;
    gap: var(--space-4);
  }
  .drop {
    display: grid;
    justify-items: center;
    gap: var(--space-2);
    padding: var(--space-6) var(--space-4);
    border: 2px dashed var(--rule-2);
    border-radius: var(--radius);
    background: var(--card);
    color: var(--ink-2);
    text-align: center;
    cursor: pointer;
    transition:
      border-color var(--fast) var(--ease),
      background var(--fast) var(--ease),
      transform var(--fast) var(--ease);
  }
  .drop:hover,
  .drop.dragging {
    border-color: var(--accent);
    background: var(--accent-wash);
    color: var(--accent);
  }
  .drop.dragging {
    transform: scale(1.01);
  }
  .drop span {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .options,
  .record {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  .options {
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
    align-items: start;
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
  }
  .check {
    display: flex;
    gap: var(--space-2);
    align-items: center;
    font-size: var(--step--1);
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
  }
  .sent {
    position: sticky;
    top: var(--space-5);
    display: grid;
    gap: var(--space-3);
  }
  .sent h2 {
    font-size: var(--step--1);
    font-family: var(--sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-2);
  }
  .item {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3);
    background: var(--card);
    border: 1px solid var(--rule);
    border-left: 3px solid var(--accent);
    border-radius: var(--radius-sm);
    font-size: var(--step--1);
    min-width: 0;
  }
  .item strong {
    overflow-wrap: anywhere;
  }
  .item.ok {
    border-left-color: var(--ok);
  }
  .item.bad {
    border-left-color: var(--seal);
  }
  .item.bad p {
    color: var(--seal);
  }
  .item.held {
    border-left-color: var(--seal);
  }
  .item p {
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }
  .by {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .links {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .chip.held {
    color: var(--seal);
  }
  .doc {
    font-size: var(--step--2) !important;
  }
  @media (max-width: 900px) {
    .layout {
      grid-template-columns: 1fr;
    }
    .sent {
      position: static;
    }
    .two {
      grid-template-columns: 1fr;
    }
  }
</style>
