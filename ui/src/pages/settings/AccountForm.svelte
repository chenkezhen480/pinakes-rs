<!-- The console's account: its user name and password, changed here (kept by the service as a
     hash). Every other session ends; this one goes on. -->
<script lang="ts">
  import { fade } from "svelte/transition";
  import { api, ApiError } from "../../api/client";
  import { session } from "../../lib/session.svelte";
  import { Resource } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";

  const account = new Resource(() => api.account());

  let current = $state("");
  let user = $state("");
  let password = $state("");
  let again = $state("");
  let busy = $state(false);
  let error = $state<ApiError | null>(null);
  let saved = $state(false);

  // The current name, filled in once when it arrives (clearing the field leaves it clear).
  let filled = false;
  $effect(() => {
    if (account.data && !filled) {
      filled = true;
      user = account.data.user;
    }
  });

  const mismatch = $derived(again !== "" && again !== password);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    busy = true;
    error = null;
    saved = false;
    try {
      const done = await api.changeAccount({ current_password: current, user: user.trim(), password });
      session.signIn(done.token);
      account.data = done.account;
      current = password = again = "";
      saved = true;
    } catch (err) {
      const e = err instanceof ApiError ? err : new ApiError(0, "error", String(err));
      error = e.status === 403 ? new ApiError(403, e.code, t("account.wrongCurrent")) : e;
    } finally {
      busy = false;
    }
  }
</script>

<section class="account">
  <h2>{t("account.title")}</h2>
  {#if account.data}
    <p class="state">
      {t("account.signedInAs", { user: account.data.user })}
      · {t(`account.source.${account.data.source === "console" ? "console" : account.data.source === "environment" ? "environment" : "default"}`)}
    </p>
    {#if account.data.default_password}<p class="warn">{t("account.defaultWarn")}</p>{/if}
  {:else if account.error}
    <ErrorNote error={account.error} retry={() => account.reload()} />
  {/if}
  <form onsubmit={submit}>
    <label class="field">
      <span>{t("account.current")}</span>
      <input type="password" bind:value={current} autocomplete="current-password" required />
    </label>
    <label class="field">
      <span>{t("account.user")}</span>
      <input type="text" bind:value={user} autocomplete="username" required spellcheck="false" />
    </label>
    <div class="two">
      <label class="field">
        <span>{t("account.password")}</span>
        <input type="password" bind:value={password} autocomplete="new-password" required />
      </label>
      <label class="field">
        <span>{t("account.again")}</span>
        <input type="password" bind:value={again} autocomplete="new-password" required />
      </label>
    </div>
    {#if mismatch}<p class="warn">{t("account.mismatch")}</p>{/if}
    <p class="hint">{t("account.hint")}</p>
    {#if error}<ErrorNote {error} />{/if}
    <div class="buttons">
      {#if saved}<span class="saved" in:fade>{t("account.saved")}</span>{/if}
      <button class="btn primary" disabled={busy || !current || !user.trim() || !password || again !== password}>{t("account.change")}</button>
    </div>
  </form>
</section>

<style>
  .account {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4);
    margin-bottom: var(--space-5);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--card);
    max-width: 640px;
  }
  h2 {
    font-size: var(--step-1);
  }
  .state,
  .hint {
    color: var(--muted);
    font-size: var(--step--1);
  }
  .warn {
    color: var(--seal);
    font-size: var(--step--1);
  }
  form {
    display: grid;
    gap: var(--space-3);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: var(--space-3);
  }
  .saved {
    color: var(--ok, var(--accent));
    font-size: var(--step--1);
  }
  @media (max-width: 480px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
</style>
