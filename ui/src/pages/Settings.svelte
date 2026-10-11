<!-- Settings, made clear: the configuration field by field (what it does, its default, whether
     it applies now or after a restart), the environment variables, the librarian's model, the
     embedding switch and the converters. Secrets are never shown: only set or not set. -->
<script lang="ts">
  import PageHead from "../components/PageHead.svelte";
  import { router, href } from "../lib/router.svelte";
  import { t } from "../lib/i18n.svelte";
  import ConfigTab from "./settings/ConfigTab.svelte";
  import EnvironmentTab from "./settings/EnvironmentTab.svelte";
  import ModelTab from "./settings/ModelTab.svelte";
  import EmbeddingTab from "./settings/EmbeddingTab.svelte";
  import ConverterTab from "./settings/ConverterTab.svelte";

  const tabs = ["config", "environment", "model", "embedding", "converter"] as const;
  const tab = $derived(tabs.find((x) => x === router.rest) ?? "config");
</script>

<PageHead title={t("nav.settings")} lede={t("settings.lede")} />

<nav class="tabs" aria-label={t("settings.tabs")}>
    {#each tabs as x (x)}
      <a href={href(`/settings/${x}`)} class:current={tab === x} aria-current={tab === x ? "page" : undefined}>{t(`settings.tab.${x}`)}</a>
    {/each}
  </nav>
  {#if tab === "config"}<ConfigTab />
  {:else if tab === "environment"}<EnvironmentTab />
  {:else if tab === "model"}<ModelTab />
  {:else if tab === "embedding"}<EmbeddingTab />
  {:else}<ConverterTab />
  {/if}

<style>
  .tabs {
    display: flex;
    gap: var(--space-1);
    margin: calc(-1 * var(--space-3)) 0 var(--space-5);
    overflow-x: auto;
    border-bottom: 1px solid var(--rule);
  }
  .tabs a {
    padding: var(--space-2) var(--space-4);
    color: var(--ink-2);
    font-weight: 600;
    font-size: var(--step--1);
    text-decoration: none;
    border-bottom: 2px solid transparent;
    margin-bottom: -1px;
    white-space: nowrap;
  }
  .tabs a:hover {
    color: var(--ink);
  }
  .tabs a.current {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
</style>
