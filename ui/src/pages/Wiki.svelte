<!-- The wiki: its pages listed by kind and audience, or one page read. -->
<script lang="ts">
  import { router } from "../lib/router.svelte";
  import WikiList from "./wiki/WikiList.svelte";
  import Reader from "./wiki/Reader.svelte";
</script>

{#if router.rest}
  {#key router.rest}<Reader path={router.rest} />{/key}
{:else}
  <WikiList />
{/if}
