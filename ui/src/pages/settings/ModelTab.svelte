<!-- The librarian's model (model.conf): provider, address, model and where the key comes from.
     The key is write-only. A test makes one tiny call; saving applies at once, no restart. -->
<script lang="ts">
  import { api } from "../../api/client";
  import type { LlmProvider, ModelChange, ModelTest, ModelView, Preset } from "../../api/types";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import Loading from "../../components/Loading.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const model = new Resource(() => api.model());
  const save = new Action();
  const test = new Action();

  let provider = $state<LlmProvider>("anthropic");
  let baseUrl = $state("");
  let name = $state("");
  let keyEnv = $state("");
  let apiKey = $state("");
  let clearKey = $state(false);
  let result = $state<ModelTest | null>(null);
  let savedNote = $state(false);

  function fill(m: ModelView) {
    provider = m.provider;
    baseUrl = m.base_url ?? "";
    name = m.model ?? "";
    keyEnv = m.key_env;
    apiKey = "";
    clearKey = false;
  }

  $effect(() => {
    if (model.data) fill(model.data);
  });

  function usePreset(p: Preset) {
    provider = p.provider;
    baseUrl = p.base_url ?? "";
    name = p.model;
    keyEnv = p.key_env;
    result = null;
  }

  function change(): ModelChange {
    const c: ModelChange = {
      provider,
      base_url: baseUrl.trim() || null,
      model: name.trim() || null,
      key_env: keyEnv.trim() || null,
    };
    if (apiKey.trim()) c.api_key = apiKey.trim();
    else if (clearKey) c.api_key = null;
    return c;
  }

  async function runTest() {
    result = null;
    const r = await test.run(() => api.testModel(change()));
    if (r) result = r;
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    const next = await save.run(() => api.putModel(change()));
    if (next) {
      model.data = next;
      savedNote = true;
      window.setTimeout(() => (savedNote = false), 2500);
    }
  }
</script>

{#if model.loading && !model.data}
  <Loading rows={2} />
{:else if model.error}
  <ErrorNote error={model.error} retry={() => model.reload()} />
{:else if model.data}
  {@const m = model.data}
  <div class="layout">
    <section class="status">
      <h2>{t("settings.model.status")}</h2>
      <p class="big" class:ok={m.librarian} class:bad={!m.librarian}>{m.librarian ? t("overview.on") : t("overview.off")}</p>
      {#if m.off_because}<p class="why">{m.off_because}</p>{/if}
      <dl>
        <dt>{t("settings.model.file")}</dt>
        <dd><code>{m.file}</code></dd>
        <dt>{t("settings.model.key")}</dt>
        <dd>{m.key ? t(`settings.model.keyFrom.${m.key}`) : t("settings.env.notSet")}</dd>
      </dl>
      <h3>{t("settings.model.presets")}</h3>
      <div class="presets">
        {#each m.presets as p (p.name)}
          <button class="btn small" type="button" onclick={() => usePreset(p)}>{p.name}</button>
        {/each}
      </div>
    </section>

    <form class="form" onsubmit={submit}>
      <label class="field">
        <span>{t("settings.model.provider")}</span>
        <select bind:value={provider}>
          <option value="anthropic">{t("settings.model.anthropic")}</option>
          <option value="openai_compatible">{t("settings.model.openai")}</option>
        </select>
      </label>
      <label class="field">
        <span>{t("settings.model.baseUrl")}</span>
        <input type="url" class="mono" bind:value={baseUrl} placeholder={provider === "anthropic" ? "https://api.anthropic.com" : "http://127.0.0.1:11434/v1"} />
        <span class="hint">{provider === "anthropic" ? t("settings.model.baseUrlAnthropic") : t("settings.model.baseUrlOpenai")}</span>
      </label>
      <label class="field">
        <span>{t("settings.model.name")}</span>
        <input type="text" class="mono" bind:value={name} required />
      </label>
      <label class="field">
        <span>{t("settings.model.apiKey")}</span>
        <input type="password" bind:value={apiKey} autocomplete="new-password" placeholder={m.key === "file" ? t("settings.model.keepKey") : ""} />
        <span class="hint">{t("settings.model.apiKeyHint")}</span>
      </label>
      {#if m.key === "file" && !apiKey.trim() && (provider !== m.provider || (baseUrl.trim() || null) !== (m.base_url ?? null))}
        <p class="hint warnline">{t("settings.model.keyStays")}</p>
      {/if}
      {#if m.key === "file"}
        <label class="check"><input type="checkbox" bind:checked={clearKey} disabled={apiKey !== ""} /> {t("settings.model.clearKey")}</label>
      {/if}
      <label class="field">
        <span>{t("settings.model.keyEnv")}</span>
        <input type="text" class="mono" bind:value={keyEnv} />
        <span class="hint">{t("settings.model.keyEnvHint")}</span>
      </label>

      {#if result}
        <p class="result" class:ok={result.ok} class:bad={!result.ok} role="status">
          {result.ok ? t("settings.model.testOk", { ms: result.ms }) : t("settings.model.testFailed", { error: result.error ?? "" })}
        </p>
      {/if}
      {#if test.error}<ErrorNote error={test.error} />{/if}
      {#if save.error}<ErrorNote error={save.error} />{/if}
      <div class="buttons">
        <button class="btn" type="button" disabled={test.running} onclick={runTest}>{test.running ? t("settings.model.testing") : t("settings.model.test")}</button>
        <button class="btn primary" type="submit" disabled={save.running}>{save.running ? t("state.saving") : savedNote ? t("state.saved") : t("settings.model.save")}</button>
      </div>
      <p class="hint">{t("settings.model.applies")}</p>
    </form>
  </div>
{/if}

<style>
  .layout {
    display: grid;
    grid-template-columns: minmax(240px, 0.8fr) minmax(0, 1.4fr);
    gap: var(--space-5);
    align-items: start;
  }
  .status,
  .form {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  h2,
  h3 {
    font-size: var(--step--1);
    font-family: var(--sans);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--muted);
  }
  .big {
    font-family: var(--serif);
    font-size: var(--step-2);
    font-weight: 600;
  }
  .ok {
    color: var(--ok);
  }
  .bad {
    color: var(--seal);
  }
  .why {
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  dl {
    display: grid;
    gap: var(--space-1);
    margin: 0;
    font-size: var(--step--1);
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0 0 var(--space-2);
    overflow-wrap: anywhere;
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
  .check {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    font-size: var(--step--1);
  }
  .result {
    padding: var(--space-3);
    border-radius: var(--radius);
    font-size: var(--step--1);
    background: var(--paper-2);
    overflow-wrap: anywhere;
  }
  .result.ok {
    background: var(--ok-wash);
  }
  .result.bad {
    background: var(--seal-wash);
  }
  .buttons {
    display: flex;
    gap: var(--space-2);
    justify-content: flex-end;
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .warnline {
    color: var(--warn);
  }
  @media (max-width: 860px) {
    .layout {
      grid-template-columns: 1fr;
    }
  }
</style>
