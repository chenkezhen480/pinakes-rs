<!-- The embedding switch: off by default, keyword search only. Turning it on downloads the
     model (once) and embeds every page once, in the background, without a restart; the steps
     are shown as they happen. -->
<script lang="ts">
  import { onDestroy } from "svelte";
  import { api } from "../../api/client";
  import { Resource, Action } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { bytes } from "../../lib/format";
  import Loading from "../../components/Loading.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";
  import Progress from "../../components/Progress.svelte";
  import TypedConfirm from "../../components/TypedConfirm.svelte";

  const emb = new Resource(() => api.embedding());
  const act = new Action();
  let confirmDelete = $state(false);

  const busy = $derived(
    emb.data !== null && ["downloading", "loading", "embedding"].includes(emb.data.status.state),
  );
  const timer = window.setInterval(() => {
    if (busy) void emb.reload(true);
  }, 1000);
  onDestroy(() => window.clearInterval(timer));

  async function turn(on: boolean) {
    const next = await act.run(() => api.setEmbedding(on));
    if (next) emb.data = next;
  }

  async function deleteFiles() {
    const next = await act.run(() => api.deleteModelFiles());
    if (next) emb.data = next;
  }
</script>

{#if emb.loading && !emb.data}
  <Loading rows={2} />
{:else if emb.error}
  <ErrorNote error={emb.error} retry={() => emb.reload()} />
{:else if emb.data}
  {@const e = emb.data}
  <section class="panel">
    <div class="head">
      <div>
        <h2>{t("settings.emb.title")}</h2>
        <p class="model mono">{e.model ?? t("overview.embedding.unconfigured")}</p>
      </div>
      <button
        class="toggle"
        role="switch"
        aria-checked={e.enabled}
        disabled={!e.model || act.running}
        onclick={() => turn(!e.enabled)}
      >
        <span class="knob"></span>
        <span class="label">{e.enabled ? t("settings.on") : t("settings.off")}</span>
      </button>
    </div>

    <div class="state">
      {#if e.status.state === "off"}
        <p>{t("settings.emb.off")}</p>
      {:else if e.status.state === "downloading"}
        <Progress done={e.status.done_bytes} total={e.status.total_bytes} label={t("settings.emb.downloading", { file: e.status.file, n: e.status.files_done + 1, of: e.status.files })} />
      {:else if e.status.state === "loading"}
        <Progress done={0} total={0} label={t("overview.embedding.loading")} />
      {:else if e.status.state === "embedding"}
        <Progress done={e.status.done} total={e.status.total} label={t("settings.emb.embedding", { n: e.status.tenant, of: e.status.tenants, done: e.status.done, total: e.status.total })} />
      {:else if e.status.state === "on"}
        <p class="ok">{t("settings.emb.on")}</p>
      {:else if e.status.state === "failed"}
        <p class="bad">{t("settings.emb.failed", { error: e.status.error })}</p>
      {/if}
    </div>

    {#if !e.enabled}
      <aside class="note">
        <strong>{t("settings.emb.noteTitle")}</strong>
        <p>{t("settings.emb.note", { model: e.model ?? "", size: bytes(e.download_bytes), dir: e.files.dir, from: e.download_from })}</p>
      </aside>
    {/if}

    {#if act.error}<ErrorNote error={act.error} />{/if}

    <div class="files">
      <span>{t("settings.emb.files")}</span>
      <code>{e.files.dir}</code>
      <span>{e.files.bytes > 0 ? bytes(e.files.bytes) : t("settings.emb.noFiles")}</span>
      {#if !e.enabled && !busy && e.files.bytes > 0}
        <button class="btn danger small" onclick={() => (confirmDelete = true)}>{t("settings.emb.deleteFiles")}</button>
      {/if}
    </div>
  </section>
  <TypedConfirm
    bind:open={confirmDelete}
    title={t("settings.emb.deleteTitle")}
    text={t("settings.emb.deleteText")}
    name={e.model ?? "model"}
    action={t("action.delete")}
    onconfirm={deleteFiles}
  />
{/if}

<style>
  .panel {
    display: grid;
    gap: var(--space-4);
    max-width: 760px;
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  .head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: var(--space-4);
  }
  h2 {
    font-size: var(--step-2);
  }
  .model {
    color: var(--muted);
    font-size: var(--step--1);
  }
  .toggle {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: 4px var(--space-3) 4px 4px;
    border: 1px solid var(--rule-2);
    border-radius: 999px;
    background: var(--paper-2);
    color: var(--ink-2);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }
  .toggle:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  .knob {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--rule-2);
    transition:
      transform var(--base) var(--ease),
      background var(--base) var(--ease);
  }
  .toggle[aria-checked="true"] {
    background: var(--accent-wash);
    border-color: var(--accent);
    color: var(--accent);
  }
  .toggle[aria-checked="true"] .knob {
    background: var(--accent);
  }
  .state p {
    color: var(--ink-2);
  }
  .ok {
    color: var(--ok) !important;
    font-weight: 600;
  }
  .bad {
    color: var(--seal) !important;
    overflow-wrap: anywhere;
  }
  .note {
    padding: var(--space-3) var(--space-4);
    border-left: 3px solid var(--accent);
    background: var(--accent-wash);
    border-radius: var(--radius);
    display: grid;
    gap: var(--space-1);
    font-size: var(--step--1);
  }
  .files {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    font-size: var(--step--1);
    color: var(--muted);
    padding-top: var(--space-3);
    border-top: 1px solid var(--rule);
  }
  .files code {
    overflow-wrap: anywhere;
  }
  .files .btn {
    margin-left: auto;
  }
</style>
