<!-- The console's frame. Signed out, the sign-in page. Signed in, the Library's hall (where the
     browser draws WebGL and the person hasn't chosen otherwise), or the list: the catalogue rail
     on the left (a bar on top on a phone), the page, and the job tray. -->
<script lang="ts">
  import { onMount } from "svelte";
  import { session } from "./lib/session.svelte";
  import { router, href } from "./lib/router.svelte";
  import { i18n, t } from "./lib/i18n.svelte";
  import { jobs } from "./lib/jobs.svelte";
  import { api, ApiError } from "./api/client";
  import ErrorNote from "./components/ErrorNote.svelte";
  import Icon from "./components/Icon.svelte";
  import JobTray from "./components/JobTray.svelte";
  import Viewer from "./components/Viewer.svelte";
  import Pages from "./pages/Pages.svelte";
  import HallFrame from "./hall/HallFrame.svelte";
  import { layout } from "./lib/layout.svelte";
  import SignIn from "./pages/SignIn.svelte";

  type Theme = "auto" | "light" | "dark";
  const THEME = "pinakes.theme";

  function savedTheme(): Theme {
    try {
      const v = localStorage.getItem(THEME);
      if (v === "light" || v === "dark") return v;
    } catch {
      // auto
    }
    return "auto";
  }

  let theme = $state<Theme>(savedTheme());
  $effect(() => {
    const root = document.documentElement;
    if (theme === "auto") root.removeAttribute("data-theme");
    else root.setAttribute("data-theme", theme);
    try {
      localStorage.setItem(THEME, theme);
    } catch {
      // not remembered
    }
  });

  const sections = [
    { id: "overview", icon: "overview" },
    { id: "search", icon: "search" },
    { id: "wiki", icon: "wiki" },
    { id: "knowledge", icon: "knowledge" },
    { id: "graph", icon: "graph" },
    { id: "review", icon: "review" },
    { id: "lint", icon: "lint" },
    { id: "acquisition", icon: "acquisition" },
  ] as const;
  const adminSections = [
    { id: "admin", icon: "admin" },
    { id: "settings", icon: "settings" },
  ] as const;

  // How the service wants to be called (its header names): asked first, and the pages wait
  // for it, so no call goes out with the wrong headers.
  let unreachable = $state<ApiError | null>(null);
  function describe() {
    unreachable = null;
    void api
      .consoleSession()
      .then((info) => {
        session.info = info;
      })
      .catch((e: unknown) => {
        unreachable = e instanceof ApiError ? e : new ApiError(0, "unreachable", "");
      });
  }
  onMount(describe);

  // The tray follows whom the knowledge is looked at as.
  $effect(() => {
    void [session.tenant, session.user, session.groups];
    jobs.stop();
    if (session.signedIn && !session.needsTenant) jobs.start();
  });

  let viewerDialog: HTMLDialogElement | undefined = $state();

  async function signOut() {
    try {
      await api.signOut();
    } catch {
      // the session is forgotten here either way
    }
    session.signOut();
  }

  // Every page starts at the top.
  $effect(() => {
    void router.section;
    window.scrollTo(0, 0);
  });
</script>

{#snippet tools()}
  <div class="seg" role="group" aria-label={t("nav.language")}>
    <button class:on={i18n.lang === "zh"} onclick={() => i18n.set("zh")}>中</button>
    <button class:on={i18n.lang === "en"} onclick={() => i18n.set("en")}>EN</button>
  </div>
  <button
    class="btn quiet small icon"
    onclick={() => (theme = theme === "auto" ? "dark" : theme === "dark" ? "light" : "auto")}
    title={t(`nav.theme.${theme}`)}
    aria-label={t(`nav.theme.${theme}`)}
  >
    <Icon name={theme === "light" ? "sun" : "moon"} size={16} />
    <span class="theme-name">{t(`nav.theme.${theme}`)}</span>
  </button>
  <button
    class="btn quiet small icon"
    onclick={() => layout.set(layout.hall ? "list" : "hall")}
    title={layout.hall ? t("nav.layout.list") : t("nav.layout.hall")}
    aria-label={layout.hall ? t("nav.layout.list") : t("nav.layout.hall")}
  >
    <Icon name={layout.hall ? "list" : "columns"} size={16} />
    <span class="tool-name">{layout.hall ? t("nav.layout.list") : t("nav.layout.hall")}</span>
  </button>
  <button class="btn quiet small" onclick={signOut} aria-label={t("nav.signOut")}><Icon name="out" size={16} /><span class="tool-name">{t("nav.signOut")}</span></button>
{/snippet}

{#if !session.signedIn}
  <SignIn />
{:else if !session.info}
  <div class="booting">
    {#if unreachable}<ErrorNote error={unreachable} retry={describe} />{/if}
  </div>
{:else if layout.hall}
  <HallFrame {tools} onViewer={() => viewerDialog?.showModal()} />
{:else}
  <div class="frame">
    <nav class="rail" aria-label={t("nav.label")}>
      <a class="brand" href={href("/overview")}>
        <span class="glyph" aria-hidden="true">Π</span>
        <span class="name">Pinakes</span>
      </a>
      <button class="who mono" title={t("nav.viewingAs")} onclick={() => viewerDialog?.showModal()}>
        <Icon name="admin" size={14} />
        <span>
          {session.personal ? t("nav.personal") : session.tenant ? `${session.tenant} ·` : t("nav.noTenant")}
          {session.user || (session.personal ? "admin" : t("nav.noUser"))}
        </span>
      </button>
      <div class="nav">
      <ul class="drawers">
        {#each sections as s (s.id)}
          <li>
            <a href={href(`/${s.id}`)} class:current={router.section === s.id} aria-current={router.section === s.id ? "page" : undefined}>
              <Icon name={s.icon} /><span>{t(`nav.${s.id}`)}</span>
            </a>
          </li>
        {/each}
      </ul>
      <ul class="drawers admin" aria-label={t("nav.adminLabel")}>
        {#each adminSections as s (s.id)}
          <li>
            <a href={href(`/${s.id}`)} class:current={router.section === s.id} aria-current={router.section === s.id ? "page" : undefined}>
              <Icon name={s.icon} /><span>{t(`nav.${s.id}`)}</span>
            </a>
          </li>
        {/each}
      </ul>
      </div>
      <div class="tools">
        {@render tools()}
      </div>
    </nav>
    <main id="main">
      <Pages />
    </main>
    <JobTray />
  </div>
{/if}

{#if session.signedIn && session.info}
  <dialog class="viewer-dialog" bind:this={viewerDialog}>
    <h2>{t("viewer.title")}</h2>
    <Viewer done={() => viewerDialog?.close()} />
    <button class="btn quiet small close" onclick={() => viewerDialog?.close()}>{t("action.cancel")}</button>
  </dialog>
{/if}

<style>
  .booting {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: var(--space-5);
  }
  .frame {
    display: grid;
    grid-template-columns: var(--rail) minmax(0, 1fr);
    min-height: 100vh;
  }
  .rail {
    position: sticky;
    top: 0;
    height: 100vh;
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-5) var(--space-3) var(--space-4);
    border-right: 1px solid var(--rule);
    background: var(--paper-2);
    overflow-y: auto;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: 0 var(--space-2);
    color: var(--ink);
    text-decoration: none;
  }
  .glyph {
    display: grid;
    place-items: center;
    width: 30px;
    height: 30px;
    border-radius: var(--radius-sm);
    background: var(--ink);
    color: var(--paper);
    font-family: var(--serif);
    font-size: 18px;
    font-weight: 700;
  }
  .name {
    font-family: var(--serif);
    font-size: var(--step-2);
    font-weight: 700;
    letter-spacing: 0.02em;
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
    font-size: var(--step--2);
    text-align: left;
    cursor: pointer;
    min-width: 0;
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
  .viewer-dialog {
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--card);
    color: var(--ink);
    padding: var(--space-5);
    width: min(480px, calc(100vw - 32px));
    box-shadow: var(--shadow-lift);
  }
  .viewer-dialog::backdrop {
    background: rgb(20 22 26 / 0.45);
  }
  .viewer-dialog h2 {
    font-size: var(--step-2);
    margin-bottom: var(--space-4);
  }
  .viewer-dialog .close {
    margin-top: var(--space-3);
  }
  .nav {
    display: grid;
    gap: var(--space-3);
  }
  .drawers {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 2px;
  }
  .drawers.admin {
    padding-top: var(--space-3);
    border-top: 1px solid var(--rule);
  }
  .drawers a {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius);
    color: var(--ink-2);
    text-decoration: none;
    font-weight: 600;
    font-size: var(--step--1);
    transition:
      background var(--fast) var(--ease),
      color var(--fast) var(--ease);
  }
  .drawers a:hover {
    background: var(--card);
    color: var(--ink);
  }
  .drawers a.current {
    background: var(--card);
    color: var(--accent);
    box-shadow: var(--shadow);
  }
  .drawers a.current::before {
    content: "";
    position: absolute;
    left: -12px;
    top: 8px;
    bottom: 8px;
    width: 3px;
    border-radius: 2px;
    background: var(--accent);
  }
  .tools {
    margin-top: auto;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1);
    padding-top: var(--space-3);
    border-top: 1px solid var(--rule);
  }
  .seg {
    display: inline-flex;
    border: 1px solid var(--rule-2);
    border-radius: var(--radius);
    overflow: hidden;
  }
  .seg button {
    border: 0;
    background: transparent;
    color: var(--ink-2);
    font: inherit;
    font-size: var(--step--1);
    padding: 3px var(--space-3);
    cursor: pointer;
  }
  .seg button.on {
    background: var(--ink);
    color: var(--paper);
  }
  main {
    min-width: 0;
    padding: var(--space-6) clamp(var(--space-4), 4vw, var(--space-7)) var(--space-7);
    max-width: 1280px;
  }
  @media (max-width: 860px) {
    .frame {
      grid-template-columns: minmax(0, 1fr);
    }
    .rail {
      position: sticky;
      z-index: 20;
      height: auto;
      flex-direction: row;
      flex-wrap: wrap;
      align-items: center;
      gap: var(--space-2);
      padding: var(--space-2) var(--space-3);
      border-right: 0;
      border-bottom: 1px solid var(--rule);
    }
    .who span,
    .name,
    .theme-name,
    .tool-name {
      display: none;
    }
    .nav {
      display: flex;
      order: 3;
      width: 100%;
      overflow-x: auto;
      gap: var(--space-1);
      scrollbar-width: none;
    }
    .drawers,
    .drawers.admin {
      display: contents;
    }
    .drawers a span {
      white-space: nowrap;
    }
    .drawers a.current::before {
      display: none;
    }
    .tools {
      margin: 0 0 0 auto;
      padding: 0;
      border: 0;
      flex-wrap: nowrap;
    }
    main {
      padding: var(--space-5) var(--space-4) var(--space-7);
    }
  }
</style>
