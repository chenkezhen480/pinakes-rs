<!-- config.toml, field by field: what each does, its default, the value the service runs with
     and the one its file holds, written back with the file's comments kept. -->
<script lang="ts">
  import { fade } from "svelte/transition";
  import { api } from "../../api/client";
  import type { FieldView, JsonValue, SettingsView } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { i18n, t } from "../../lib/i18n.svelte";
  import { dur } from "../../lib/motion";
  import Loading from "../../components/Loading.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const view = new Resource(() => api.settings());
  const save = new Action();
  let edits = $state<Record<string, JsonValue>>({});
  let savedNote = $state(false);
  const count = $derived(Object.keys(edits).length);

  function shown(f: FieldView): JsonValue {
    if (f.path in edits) {
      const e = edits[f.path];
      return e === null || e === undefined ? f.default : e;
    }
    return f.saved ?? f.value;
  }

  function set(f: FieldView, v: JsonValue) {
    const original = f.saved ?? f.value;
    if (JSON.stringify(v) === JSON.stringify(original)) {
      const { [f.path]: _, ...rest } = edits;
      edits = rest;
    } else {
      edits = { ...edits, [f.path]: v };
    }
  }

  function text(v: JsonValue): string {
    if (v === null) return "";
    return typeof v === "string" ? v : JSON.stringify(v);
  }

  function shownDefault(v: JsonValue): string {
    return v === null ? t("settings.unset") : text(v);
  }

  async function submit() {
    const next = await save.run(() => api.putSettings(edits));
    if (next) {
      view.data = next;
      edits = {};
      savedNote = true;
      window.setTimeout(() => (savedNote = false), 2500);
    }
  }

  function sections(v: SettingsView) {
    return v.sections.map((s) => ({
      ...s,
      fields: v.fields.filter((f) => f.section === s.name && !f.advanced),
      advanced: v.fields.filter((f) => f.section === s.name && f.advanced),
    }));
  }
</script>

{#snippet control(f: FieldView)}
  {@const v = shown(f)}
  {#if f.kind === "boolean"}
    <label class="switch">
      <input type="checkbox" checked={v === true} onchange={(e) => set(f, e.currentTarget.checked)} />
      <span>{v === true ? t("settings.on") : t("settings.off")}</span>
    </label>
  {:else if f.kind === "integer"}
    <input type="number" min="0" value={text(v)} oninput={(e) => set(f, e.currentTarget.value === "" ? null : Number(e.currentTarget.value))} />
  {:else if f.kind === "choice"}
    <select value={text(v)} onchange={(e) => set(f, e.currentTarget.value)}>
      {#each f.options as o (o)}<option value={o}>{o}</option>{/each}
    </select>
  {:else}
    <input
      type="text"
      class="mono"
      value={text(v)}
      placeholder={f.kind === "optional_text" ? t("settings.unset") : ""}
      oninput={(e) => set(f, e.currentTarget.value === "" && f.kind === "optional_text" ? null : e.currentTarget.value)}
      spellcheck="false"
    />
  {/if}
{/snippet}

{#snippet field(f: FieldView)}
  <div class="field-row" class:dirty={f.path in edits}>
    <div class="about">
      <code class="key">{f.path.slice(f.section.length + 1)}</code>
      <p>{i18n.lang === "zh" && f.description_zh ? f.description_zh : f.description}</p>
      <p class="facts">
        <span>{t("settings.default")}: <code>{shownDefault(f.default)}</code></span>
        <span class={`applies ${f.applies}`}>{f.applies === "now" ? t("settings.appliesNow") : t("settings.appliesRestart")}</span>
        {#if f.saved !== null && f.saved !== undefined}
          <span class="pending">{t("settings.pending", { value: text(f.value) })}</span>
        {/if}
      </p>
    </div>
    <div class="input">
      {@render control(f)}
      {#if JSON.stringify(shown(f)) !== JSON.stringify(f.default)}
        <button class="btn quiet small" onclick={() => set(f, null)}>{t("settings.toDefault")}</button>
      {/if}
    </div>
  </div>
{/snippet}

{#if view.loading && !view.data}
  <Loading rows={4} />
{:else if view.error}
  <ErrorNote error={view.error} retry={() => view.reload()} />
{:else if view.data}
  {@const v = view.data}
  {#if v.file}
    <p class="file">{t("settings.file")} <code>{v.file}</code></p>
  {:else}
    <p class="banner warn">{t("settings.noFile")}</p>
  {/if}
  {#if !["127.0.0.1", "localhost", "::1"].includes(String(v.fields.find((f) => f.path === "server.host")?.value ?? "")) && location.protocol === "http:"}
    <p class="banner warn">{t("settings.exposed")}</p>
  {/if}
  {#if v.restart_needed}
    <p class="banner">{t("settings.restartNeeded")}</p>
  {/if}
  {#each sections(v) as s (s.name)}
    <section class="section">
      <header>
        <h2><code>[{s.name}]</code></h2>
        <p>{i18n.lang === "zh" && s.description_zh ? s.description_zh : s.description}</p>
      </header>
      {#each s.fields as f (f.path)}{@render field(f)}{/each}
      {#if s.advanced.length}
        <details>
          <summary>{t("settings.advanced", { n: s.advanced.length })}</summary>
          {#each s.advanced as f (f.path)}{@render field(f)}{/each}
        </details>
      {/if}
    </section>
  {/each}

  {#if count > 0 || save.error || savedNote}
    <div class="savebar" transition:fade={{ duration: dur("fast") }}>
      {#if save.error}<ErrorNote error={save.error} />{/if}
      <div class="row">
        {#if savedNote && count === 0}
          <span>{t("state.saved")}</span>
        {:else}
          <span>{t("settings.changes", { n: count })}</span>
          <button class="btn quiet" onclick={() => (edits = {})}>{t("action.cancel")}</button>
          <button class="btn primary" disabled={!v.file || save.running} onclick={submit}>{save.running ? t("state.saving") : t("action.save")}</button>
        {/if}
      </div>
    </div>
  {/if}
{/if}

<style>
  .file {
    color: var(--muted);
    font-size: var(--step--1);
    margin-bottom: var(--space-4);
    overflow-wrap: anywhere;
  }
  .banner {
    margin-bottom: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border-left: 3px solid var(--accent);
    background: var(--accent-wash);
    border-radius: var(--radius);
    font-size: var(--step--1);
  }
  .banner.warn {
    border-left-color: var(--warn);
    background: var(--warn-wash);
  }
  .section {
    margin-bottom: var(--space-6);
  }
  .section header {
    display: grid;
    gap: var(--space-1);
    margin-bottom: var(--space-3);
  }
  h2 {
    font-size: var(--step-1);
  }
  h2 code {
    font-family: var(--mono);
    font-size: var(--step-0);
    color: var(--accent);
  }
  .section header p {
    color: var(--ink-2);
    font-size: var(--step--1);
    max-width: 75ch;
  }
  .field-row {
    display: grid;
    grid-template-columns: minmax(0, 1.5fr) minmax(200px, 1fr);
    gap: var(--space-4);
    align-items: start;
    padding: var(--space-3) var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    margin-bottom: var(--space-2);
    transition: border-color var(--fast) var(--ease);
  }
  .field-row.dirty {
    border-color: var(--accent);
    box-shadow: inset 3px 0 0 var(--accent);
  }
  .about {
    display: grid;
    gap: var(--space-1);
    min-width: 0;
  }
  .key {
    font-weight: 700;
    color: var(--ink);
  }
  .about p {
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-3);
    color: var(--muted) !important;
    font-size: var(--step--2) !important;
  }
  .applies.now {
    color: var(--ok);
    font-weight: 600;
  }
  .pending {
    color: var(--warn);
    font-weight: 600;
  }
  .input {
    display: grid;
    gap: var(--space-1);
    justify-items: start;
  }
  .input :global(input[type="text"]),
  .input :global(input[type="number"]),
  .input :global(select) {
    width: 100%;
  }
  .switch {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-height: 36px;
    font-size: var(--step--1);
  }
  details summary {
    cursor: pointer;
    color: var(--muted);
    font-size: var(--step--1);
    padding: var(--space-2) 0;
  }
  .savebar {
    position: sticky;
    bottom: var(--space-4);
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    background: var(--card);
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lift);
    z-index: 5;
  }
  .savebar .row {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .savebar .row span {
    margin-right: auto;
    font-weight: 600;
  }
  @media (max-width: 720px) {
    .field-row {
      grid-template-columns: 1fr;
    }
  }
</style>
