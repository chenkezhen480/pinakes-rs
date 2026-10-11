<!-- The tenant's jobs, in a tray at the corner: queued and running ones with what they do, the
     last finished ones with what they did (pages created and updated, what was held). -->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { flip } from "svelte/animate";
  import type { JobInfo } from "../api/types";
  import { jobs } from "../lib/jobs.svelte";
  import { isKey, t } from "../lib/i18n.svelte";
  import { ago } from "../lib/format";
  import { dur, ease } from "../lib/motion";
  import { href } from "../lib/router.svelte";
  import Icon from "./Icon.svelte";

  let open = $state(false);
  const recent = $derived(jobs.list.slice(0, 6));
  const active = $derived(jobs.active.length);

  function kindName(kind: string): string {
    const key = `job.kind.${kind}`;
    return isKey(key) ? t(key) : kind;
  }

  function outcome(j: JobInfo): string {
    if (j.status === "failed") return j.error ?? t("job.failed");
    if (j.status !== "done") return t(`job.status.${j.status}`);
    const r = j.result;
    if (!r) return t("job.status.done");
    const parts: string[] = [];
    if (r.pages_created.length) parts.push(t("job.created", { n: r.pages_created.length }));
    if (r.pages_updated.length) parts.push(t("job.updated", { n: r.pages_updated.length }));
    if (r.proposal) parts.push(t("job.held"));
    if (r.rejected.length) parts.push(t("job.rejected", { n: r.rejected.length }));
    return parts.length ? parts.join(" · ") : t("job.status.done");
  }
</script>

{#if recent.length > 0}
  <aside class="tray" class:open aria-label={t("job.tray")}>
    <button class="toggle" onclick={() => (open = !open)} aria-expanded={open}>
      {#if active > 0}
        <span class="pulse" aria-hidden="true"></span>
        <span>{t("job.running", { n: active })}</span>
      {:else}
        <span>{t("job.recent")}</span>
      {/if}
      <span class="count mono">{recent.length}</span>
    </button>
    {#if open}
      <ol class="list" aria-live="polite" transition:fly={{ y: 8, duration: dur("base"), easing: ease }}>
        {#each recent as j (j.job)}
          <li
            class={`job ${j.status}`}
            in:fly={{ x: 24, duration: dur("base"), easing: ease }}
            animate:flip={{ duration: dur("base") }}
          >
            <div class="top">
              <strong>{kindName(j.kind)}</strong>
              <span class="when">{ago(j.created_at)}</span>
            </div>
            <p class="what">{outcome(j)}</p>
            {#if j.result && j.status === "done"}
              <div class="links">
                {#each [...j.result.pages_created, ...j.result.pages_updated].slice(0, 4) as p (p)}
                  <a class="chip" href={href(`/wiki/${p}`)}>{p}</a>
                {/each}
                {#if j.result.proposal}
                  <a class="chip held" href={href("/review")}><Icon name="review" size={12} />{t("job.toReview")}</a>
                {/if}
              </div>
            {/if}
          </li>
        {/each}
      </ol>
    {/if}
  </aside>
{/if}

<style>
  .tray {
    position: fixed;
    right: var(--space-4);
    bottom: var(--space-4);
    z-index: 30;
    width: min(340px, calc(100vw - 32px));
    display: flex;
    flex-direction: column-reverse;
    gap: var(--space-2);
  }
  .toggle {
    align-self: flex-end;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-4);
    border: 1px solid var(--rule-2);
    border-radius: 999px;
    background: var(--card);
    color: var(--ink);
    font: inherit;
    font-size: var(--step--1);
    font-weight: 600;
    box-shadow: var(--shadow-lift);
    cursor: pointer;
  }
  .count {
    color: var(--muted);
  }
  .pulse {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    animation: pulse 1.4s var(--ease) infinite;
  }
  @keyframes pulse {
    50% {
      opacity: 0.3;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .pulse {
      animation: none;
    }
  }
  .list {
    list-style: none;
    margin: 0;
    padding: var(--space-2);
    display: grid;
    gap: var(--space-2);
    max-height: 60vh;
    overflow-y: auto;
    background: var(--paper);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lift);
  }
  .job {
    padding: var(--space-3);
    background: var(--card);
    border: 1px solid var(--rule);
    border-left: 3px solid var(--rule-2);
    border-radius: var(--radius-sm);
    display: grid;
    gap: var(--space-1);
  }
  .job.running,
  .job.queued {
    border-left-color: var(--accent);
  }
  .job.done {
    border-left-color: var(--ok);
  }
  .job.failed {
    border-left-color: var(--seal);
  }
  .top {
    display: flex;
    justify-content: space-between;
    gap: var(--space-2);
    font-size: var(--step--1);
  }
  .when {
    color: var(--muted);
    font-size: var(--step--2);
  }
  .what {
    font-size: var(--step--1);
    color: var(--ink-2);
    overflow-wrap: anywhere;
  }
  .failed .what {
    color: var(--seal);
  }
  .links {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
  }
  .chip.held {
    color: var(--seal);
  }
</style>
