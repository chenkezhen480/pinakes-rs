<!-- The converters: the formats the service reads itself, and the optional MarkItDown, with a
     check that it answers. -->
<script lang="ts">
  import { api } from "../../api/client";
  import { Resource } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import { href } from "../../lib/router.svelte";
  import Loading from "../../components/Loading.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const status = new Resource(() => api.status());
</script>

{#if status.loading && !status.data}
  <Loading rows={2} />
{:else if status.error}
  <ErrorNote error={status.error} retry={() => status.reload()} />
{:else if status.data}
  {@const c = status.data.converters}
  <div class="grid">
    <section class="panel">
      <h2>{t("settings.conv.builtin")}</h2>
      <p>{t("settings.conv.builtinText")}</p>
      <p class="formats">
        <span class="chip">.md</span><span class="chip">.txt</span>
        {#each c.builtin as f (f)}<span class="chip">.{f}</span>{/each}
      </p>
      <p class="hint">{t("settings.conv.limits")}</p>
    </section>
    <section class="panel">
      <h2>MarkItDown</h2>
      {#if !c.markitdown}
        <p>{t("settings.conv.off")}</p>
        <pre class="mono">docker compose --profile markitdown up -d converter</pre>
        <p class="hint">{t("settings.conv.offHint")} <a href={href("/settings/config")}>converter.url</a></p>
      {:else if c.markitdown_version}
        <p class="ok">{t("overview.markitdown.on", { version: c.markitdown_version })}</p>
      {:else}
        <p class="bad">{t("overview.markitdown.down", { why: c.markitdown_problem ?? "" })}</p>
      {/if}
      <button class="btn small" onclick={() => status.reload()}>{t("settings.conv.check")}</button>
    </section>
  </div>
{/if}

<style>
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(300px, 1fr));
    gap: var(--space-4);
  }
  .panel {
    display: grid;
    gap: var(--space-3);
    align-content: start;
    justify-items: start;
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }
  h2 {
    font-size: var(--step-1);
  }
  p {
    color: var(--ink-2);
  }
  .formats {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  pre {
    margin: 0;
    padding: var(--space-2) var(--space-3);
    background: var(--paper-2);
    border-radius: var(--radius);
    font-size: var(--step--1);
    white-space: pre-wrap;
    max-width: 100%;
  }
  .ok {
    color: var(--ok);
  }
  .bad {
    color: var(--seal);
  }
</style>
