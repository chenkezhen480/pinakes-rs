<!-- Honest progress: real counts from the API, and a bar only when there is a total. -->
<script lang="ts">
  import { Tween } from "svelte/motion";
  import { dur, ease } from "../lib/motion";

  let { done, total, label }: { done: number; total: number; label: string } = $props();
  const pct = $derived(total > 0 ? Math.min(100, (done / total) * 100) : 0);
  const shown = new Tween(0, { duration: dur("slow"), easing: ease });
  $effect(() => {
    shown.target = pct;
  });
</script>

<div class="progress" role="progressbar" aria-valuemin="0" aria-valuemax={total || undefined} aria-valuenow={total ? done : undefined} aria-label={label}>
  <div class="label"><span>{label}</span>{#if total > 0}<span class="mono">{Math.round(pct)}%</span>{/if}</div>
  <div class="track" class:indeterminate={total <= 0}>
    <div class="fill" style:width={total > 0 ? `${shown.current}%` : undefined}></div>
  </div>
</div>

<style>
  .progress {
    display: grid;
    gap: var(--space-1);
  }
  .label {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .track {
    height: 6px;
    border-radius: 3px;
    background: var(--paper-2);
    border: 1px solid var(--rule);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
  }
  .indeterminate .fill {
    width: 30%;
    animation: slide 1.2s var(--ease) infinite;
  }
  @keyframes slide {
    from {
      transform: translateX(-100%);
    }
    to {
      transform: translateX(340%);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .indeterminate .fill {
      animation: none;
      width: 100%;
      opacity: 0.4;
    }
  }
</style>
