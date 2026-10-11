<!-- An empty view: what belongs here, and the one action that starts it. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  let { title, text = "", action }: { title: string; text?: string; action?: Snippet } = $props();
</script>

<div class="empty">
  <div class="drawer" aria-hidden="true"><span></span></div>
  <h3>{title}</h3>
  {#if text}<p>{text}</p>{/if}
  {#if action}<div class="action">{@render action()}</div>{/if}
</div>

<style>
  .empty {
    display: grid;
    justify-items: center;
    gap: var(--space-2);
    padding: var(--space-6) var(--space-4);
    text-align: center;
    border: 1px dashed var(--rule-2);
    border-radius: var(--radius);
    color: var(--ink-2);
  }
  h3 {
    font-size: var(--step-1);
    color: var(--ink);
  }
  p {
    max-width: 46ch;
    font-size: var(--step--1);
  }
  .action {
    margin-top: var(--space-2);
  }
  .drawer {
    width: 54px;
    height: 34px;
    border: 1.5px solid var(--rule-2);
    border-radius: 4px;
    display: grid;
    place-items: center;
    margin-bottom: var(--space-2);
  }
  .drawer span {
    width: 16px;
    height: 5px;
    border: 1.5px solid var(--rule-2);
    border-radius: 3px;
  }
</style>
