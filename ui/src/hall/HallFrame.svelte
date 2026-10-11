<!-- The console as the Library's hall: the scene fills the window, a bar runs along the top, the
     dock along the bottom names every station, and choosing one walks the camera there and opens
     its page on a folio from the right (the scene stays in view beside it on a wide screen).
     Escape or the close mark walks back to the entrance. -->
<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import { fly } from "svelte/transition";
  import { router, go, href } from "../lib/router.svelte";
  import { session } from "../lib/session.svelte";
  import { t, i18n } from "../lib/i18n.svelte";
  import { dur, ease } from "../lib/motion";
  import Icon from "../components/Icon.svelte";
  import JobTray from "../components/JobTray.svelte";
  import Pages from "../pages/Pages.svelte";
  import HallCanvas from "./HallCanvas.svelte";
  import { STATIONS, isStation, type Station } from "./stations";
  import type { Phase } from "./sky";
  import { layout } from "../lib/layout.svelte";

  let { tools, onViewer }: { tools: Snippet; onViewer: () => void } = $props();

  let innerWidth = $state(window.innerWidth);
  let phase = $state<Phase>("day");

  // The local time, as the clock on the wall would say it.
  let now = $state(new Date());
  onMount(() => {
    const timer = window.setInterval(() => (now = new Date()), 10_000);
    return () => window.clearInterval(timer);
  });
  const locale = $derived(i18n.lang === "zh" ? "zh-CN" : "en-GB");
  const clock = $derived(new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit" }).format(now));
  const day = $derived(new Intl.DateTimeFormat(locale, { dateStyle: "full" }).format(now));

  /** The station whose page is open, or none at the entrance (`#/`). */
  const open = $derived<Station | null>(
    router.parts.length === 0 ? null : isStation(router.section) ? router.section : "overview",
  );
  const wide = $derived(innerWidth >= 900);
  const panel = $derived(open && wide ? Math.min(1080, Math.round(innerWidth * 0.62)) : 0);

  function onKey(e: KeyboardEvent) {
    if (e.key !== "Escape" || !open) return;
    const el = e.target;
    if (el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement) return;
    if (document.querySelector("dialog[open]")) return;
    go("/");
  }
</script>

<svelte:window bind:innerWidth onkeydown={onKey} />

<div class="hall" class:open={open !== null}>
  <HallCanvas station={open} {panel} covered={open !== null && !wide} onFail={() => layout.fail()} onPhase={(p) => (phase = p)} />

  <header class="bar">
    <a class="brand" href={href("/")} aria-label={t("hall.entrance")}>
      <span class="glyph" aria-hidden="true">Π</span>
      <span class="name">Pinakes</span>
      <span class="sky"><time datetime={now.toISOString()} title={day}>{clock}</time> · {t(`hall.sky.${phase}`)}</span>
    </a>
    <div class="right">
      <button class="who" title={t("nav.viewingAs")} onclick={onViewer}>
        <Icon name="admin" size={14} />
        <span>
          {session.personal ? t("nav.personal") : session.tenant ? `${session.tenant} ·` : t("nav.noTenant")}
          {session.user || (session.personal ? "admin" : t("nav.noUser"))}
        </span>
      </button>
      {@render tools()}
    </div>
  </header>

  {#if open === null}
    <nav class="dock" aria-label={t("nav.label")} transition:fly={{ y: 16, duration: dur("base"), easing: ease }}>
      {#each STATIONS as s (s)}
        <a href={href(`/${s}`)} title={t(`hall.station.${s}`)}>
          <Icon name={s} size={18} />
          <span>{t(`nav.${s}`)}</span>
        </a>
      {/each}
    </nav>
  {:else}
    <main id="main" class="folio" class:wide transition:fly={{ x: 48, duration: dur("slow"), easing: ease }}>
      <div class="folio-head">
        <p class="where">{t(`hall.station.${open}`)}</p>
        <a class="close" href={href("/")} aria-label={t("hall.back")} title={t("hall.back")}>
          <Icon name="close" size={18} />
          <span>{t("hall.back")}</span>
        </a>
      </div>
      <div class="folio-body">
        <Pages />
      </div>
    </main>
  {/if}
  <JobTray />
</div>

<style>
  .hall {
    min-height: 100vh;
  }
  .bar {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: linear-gradient(to bottom, var(--hall-veil), transparent);
    color: var(--hall-ink);
    pointer-events: none;
  }
  .bar > * {
    pointer-events: auto;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--hall-ink);
    text-decoration: none;
  }
  .glyph {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border: 1px solid var(--hall-gold-line);
    border-radius: var(--radius-sm);
    background: var(--hall-label);
    font-family: var(--serif);
    font-weight: 700;
    color: var(--hall-gold);
  }
  .name {
    font-family: var(--serif);
    font-size: var(--step-1);
    font-weight: 700;
    letter-spacing: 0.06em;
  }
  .sky {
    font-variant-numeric: tabular-nums;
    font-size: var(--step--1);
    color: var(--hall-ink-2);
    padding-left: var(--space-2);
    border-left: 1px solid var(--hall-gold-line);
  }
  .right {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    flex-wrap: wrap;
    justify-content: flex-end;
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius);
    background: var(--card);
    box-shadow: var(--shadow);
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-1) var(--space-2);
    border: 1px solid transparent;
    border-radius: var(--radius-sm);
    background: none;
    color: var(--muted);
    font: inherit;
    font-family: var(--mono);
    font-size: var(--step--2);
    cursor: pointer;
    max-width: 220px;
  }
  .who span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .who:hover {
    border-color: var(--rule);
    color: var(--ink);
  }
  .dock {
    position: fixed;
    left: 50%;
    bottom: var(--space-5);
    translate: -50% 0;
    z-index: 5;
    display: flex;
    gap: var(--space-1);
    padding: var(--space-2);
    border: 1px solid var(--hall-gold-line);
    border-radius: var(--radius);
    background: var(--hall-label);
    backdrop-filter: blur(6px);
    max-width: calc(100vw - 2 * var(--space-4));
    overflow-x: auto;
  }
  .dock a {
    display: grid;
    justify-items: center;
    gap: 2px;
    min-width: 64px;
    padding: var(--space-2) var(--space-2);
    border-radius: var(--radius-sm);
    color: var(--hall-ink);
    text-decoration: none;
    font-size: var(--step--2);
    white-space: nowrap;
  }
  .dock a:hover,
  .dock a:focus-visible {
    background: var(--hall-label-hot);
    color: var(--hall-gold);
  }
  .folio {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    z-index: 4;
    width: 100%;
    overflow-y: auto;
    overflow-x: hidden;
    background: var(--paper);
    color: var(--ink);
    box-shadow: var(--hall-shadow);
  }
  .folio.wide {
    width: min(1080px, 62vw);
    border-left: 1px solid var(--hall-gold-line);
  }
  .folio-head {
    position: sticky;
    top: 0;
    z-index: 3;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) clamp(var(--space-4), 3vw, var(--space-6));
    padding-top: 64px;
    background: linear-gradient(to bottom, var(--paper) 80%, transparent);
  }
  .where {
    font-family: var(--serif);
    font-size: var(--step--1);
    letter-spacing: 0.08em;
    color: var(--muted);
  }
  .close {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-sm);
    color: var(--ink-2);
    text-decoration: none;
    font-size: var(--step--1);
  }
  .close:hover {
    background: var(--paper-2);
    color: var(--ink);
  }
  .folio-body {
    min-width: 0;
    padding: 0 clamp(var(--space-4), 3vw, var(--space-6)) var(--space-7);
  }
  .open .bar {
    background: none;
  }
  @media (max-width: 640px) {
    .sky,
    .name {
      display: none;
    }
    /* The bar wraps to two rows here. */
    .folio-head {
      padding-top: 104px;
    }
    .dock a {
      min-width: 52px;
    }
  }
</style>
