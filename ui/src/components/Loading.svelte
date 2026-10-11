<!-- The layout's skeleton while data loads: lines where cards will be, so nothing jumps. -->
<script lang="ts">
  import { t } from "../lib/i18n.svelte";
  let { rows = 3, card = true }: { rows?: number; card?: boolean } = $props();
</script>

<div class="skeleton" aria-busy="true" aria-live="polite">
  <span class="visually-hidden">{t("state.loading")}</span>
  {#each Array.from({ length: rows }, (_, i) => i) as i (i)}
    <div class:card class="row">
      <div class="bar short"></div>
      <div class="bar"></div>
      <div class="bar mid"></div>
    </div>
  {/each}
</div>

<style>
  .skeleton {
    display: grid;
    gap: var(--space-3);
  }
  .row {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-4);
  }
  .row.card {
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
  }
  .bar {
    height: 10px;
    border-radius: 3px;
    background: linear-gradient(90deg, var(--paper-2), var(--rule), var(--paper-2));
    background-size: 200% 100%;
    animation: shimmer 1.4s linear infinite;
  }
  .bar.short {
    width: 30%;
  }
  .bar.mid {
    width: 70%;
  }
  @keyframes shimmer {
    to {
      background-position: -200% 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .bar {
      animation: none;
    }
  }
</style>
