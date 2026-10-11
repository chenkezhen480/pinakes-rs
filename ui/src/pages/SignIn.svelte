<!-- Signing in as the service's admin: a user name and a password. The session it gives is kept
     in this browser, so the next visit is signed in. -->
<script lang="ts">
  import { session } from "../lib/session.svelte";
  import { t, i18n } from "../lib/i18n.svelte";
  import { api, ApiError } from "../api/client";
  import ErrorNote from "../components/ErrorNote.svelte";

  let user = $state("");
  let password = $state("");
  let error = $state<ApiError | null>(null);
  let busy = $state(false);
  const isDefault = $derived(session.info?.default_password === true);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    try {
      const done = await api.signIn(user.trim(), password);
      password = "";
      session.signIn(done.token);
    } catch (err) {
      const e = err instanceof ApiError ? err : new ApiError(0, "error", String(err));
      // The service's words, in the console's language where it has them.
      const words: Record<number, string> = {
        401: t("signin.wrong"),
        429: t("signin.tooMany"),
      };
      error = words[e.status] ? new ApiError(e.status, e.code, words[e.status] ?? "") : e;
    } finally {
      busy = false;
    }
  }
</script>

<div class="page">
  <section class="card">
    <div class="brand">
      <span class="glyph" aria-hidden="true">Π</span>
      <div>
        <h1>Pinakes</h1>
        <p class="tag">{t("signin.tagline")}</p>
      </div>
    </div>
    <form onsubmit={submit}>
      <label class="field">
        <span>{t("signin.user")}</span>
        <input type="text" bind:value={user} autocomplete="username" required spellcheck="false" placeholder={isDefault ? "root" : ""} />
      </label>
      <label class="field">
        <span>{t("signin.password")}</span>
        <input type="password" bind:value={password} autocomplete="current-password" required />
      </label>
      {#if isDefault}<p class="hint">{t("signin.defaultHint")}</p>{/if}
      {#if error}<ErrorNote {error} />{/if}
      <button class="btn primary" type="submit" disabled={busy || !user.trim() || !password}>{t("signin.submit")}</button>
    </form>
    <p class="foot">
      <button class="link" onclick={() => i18n.set(i18n.lang === "zh" ? "en" : "zh")}>{i18n.lang === "zh" ? "English" : "中文"}</button>
      <span>{t("signin.storage")}</span>
    </p>
  </section>
</div>

<style>
  .page {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: var(--space-5) var(--space-4);
    background:
      repeating-linear-gradient(0deg, transparent 0 27px, color-mix(in srgb, var(--rule) 55%, transparent) 27px 28px),
      var(--paper);
  }
  .card {
    width: min(460px, 100%);
    display: grid;
    gap: var(--space-5);
    padding: var(--space-6) var(--space-5) var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow-lift);
  }
  .brand {
    display: flex;
    gap: var(--space-3);
    align-items: center;
  }
  .glyph {
    display: grid;
    place-items: center;
    width: 44px;
    height: 44px;
    border-radius: var(--radius);
    background: var(--ink);
    color: var(--paper);
    font-family: var(--serif);
    font-size: 26px;
    font-weight: 700;
  }
  h1 {
    font-size: var(--step-3);
  }
  .tag {
    color: var(--ink-2);
    font-size: var(--step--1);
  }
  form {
    display: grid;
    gap: var(--space-4);
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .foot {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    font-size: var(--step--2);
    color: var(--muted);
  }
  .link {
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent);
    font: inherit;
    cursor: pointer;
  }
</style>
