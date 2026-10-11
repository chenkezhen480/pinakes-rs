<!-- The page of the current route, the same in the hall (on a folio) and in the list. In a
     multi-tenant deployment, the tenant's pages first ask which tenant to look at. Pages start
     again when the route or whom they are seen as changes. -->
<script lang="ts">
  import { session } from "../lib/session.svelte";
  import { router } from "../lib/router.svelte";
  import { t } from "../lib/i18n.svelte";
  import Viewer from "../components/Viewer.svelte";
  import Overview from "./Overview.svelte";
  import Search from "./Search.svelte";
  import Wiki from "./Wiki.svelte";
  import Knowledge from "./Knowledge.svelte";
  import Graph from "./Graph.svelte";
  import Review from "./Review.svelte";
  import Lint from "./Lint.svelte";
  import Acquisition from "./Acquisition.svelte";
  import Admin from "./Admin.svelte";
  import Settings from "./Settings.svelte";

  const adminIds: readonly string[] = ["admin", "settings"];
  const view = $derived(`${router.section}|${session.tenant}|${session.user}|${session.groups}`);
</script>

{#key view}
  {#if session.needsTenant && !adminIds.includes(router.section)}
    <section class="choose">
      <h1>{t("viewer.title")}</h1>
      <p>{t("viewer.text")}</p>
      <Viewer />
    </section>
  {:else if router.section === "overview"}<Overview />
  {:else if router.section === "search"}<Search />
  {:else if router.section === "wiki"}<Wiki />
  {:else if router.section === "knowledge"}<Knowledge />
  {:else if router.section === "graph"}<Graph />
  {:else if router.section === "review"}<Review />
  {:else if router.section === "lint"}<Lint />
  {:else if router.section === "acquisition"}<Acquisition />
  {:else if router.section === "admin"}<Admin />
  {:else if router.section === "settings"}<Settings />
  {:else}<Overview />
  {/if}
{/key}

<style>
  .choose {
    max-width: 520px;
    margin: var(--space-6) auto;
    padding: 0 var(--space-4);
    display: grid;
    gap: var(--space-4);
  }
  .choose p {
    color: var(--ink-2);
  }
</style>
