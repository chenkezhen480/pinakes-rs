<!-- A kind's shelf mark: its name in the kind's hue, small caps like a catalogue label. -->
<script lang="ts">
  import type { Kind } from "../api/types";
  import { t } from "../lib/i18n.svelte";

  let { kind }: { kind: Kind | null | undefined } = $props();
</script>

{#if kind}
  <span class="mark" style:--hue={`var(--k-${kind})`}>{t(`kind.${kind}`)}</span>
{/if}

<style>
  .mark {
    display: inline-flex;
    align-items: center;
    gap: var(--space-1);
    font-size: var(--step--2);
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
    color: var(--hue);
    white-space: nowrap;
  }
  .mark::before {
    content: "";
    width: 7px;
    height: 7px;
    border-radius: 2px;
    background: var(--hue);
  }
</style>
