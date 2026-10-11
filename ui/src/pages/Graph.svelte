<!-- The business graph: browsing its spaces and nodes (drawn), its schemas, creating spaces,
     writing data as a host does, and the drafts agents propose. -->
<script lang="ts">
  import { router, href } from "../lib/router.svelte";
  import { t } from "../lib/i18n.svelte";
  import PageHead from "../components/PageHead.svelte";
  import Browse from "./graph/Browse.svelte";
  import Schemas from "./graph/Schemas.svelte";
  import Spaces from "./graph/Spaces.svelte";
  import DataForm from "./graph/DataForm.svelte";
  import Drafts from "./graph/Drafts.svelte";

  const tabs = ["browse", "schemas", "spaces", "data", "drafts"] as const;
  type Tab = (typeof tabs)[number];
  const first = $derived(router.parts[1] ?? "");
  // `#/graph/node/<id>` opens a node in the browser.
  const tab = $derived<Tab>(first === "node" ? "browse" : (tabs.find((x) => x === first) ?? "browse"));
  const node = $derived(first === "node" ? router.parts.slice(2).join("/") : "");
</script>

<PageHead title={t("nav.graph")} lede={t("graph.lede")} />

<nav class="tabs" aria-label={t("graph.tabs")}>
  {#each tabs as x (x)}
    <a href={href(`/graph/${x}`)} class:current={tab === x} aria-current={tab === x ? "page" : undefined}>{t(`graph.tab.${x}`)}</a>
  {/each}
</nav>

{#if tab === "browse"}<Browse {node} />
{:else if tab === "schemas"}<Schemas />
{:else if tab === "spaces"}<Spaces />
{:else if tab === "data"}<DataForm />
{:else}<Drafts />
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
  .tabs a.current {
    color: var(--accent);
    border-bottom-color: var(--accent);
  }
</style>
