<!-- A page's heading: its title, one sentence of what it is for, and its actions. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  let { title, lede = "", actions }: { title: string; lede?: string; actions?: Snippet } = $props();
</script>

<header class="head">
  <div class="text">
    <h1>{title}</h1>
    {#if lede}<p>{lede}</p>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>

<style>
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    justify-content: space-between;
    gap: var(--space-4);
    padding-bottom: var(--space-4);
    margin-bottom: var(--space-5);
    border-bottom: 1px solid var(--rule);
  }
  .text {
    display: grid;
    gap: var(--space-1);
    min-width: 0;
  }
  h1 {
    font-size: var(--step-3);
  }
  p {
    color: var(--ink-2);
    max-width: 70ch;
  }
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
  }
</style>
