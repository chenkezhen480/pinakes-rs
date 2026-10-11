<!-- Whom the admin looks at the knowledge as: a tenant (multi-tenant deployments; a personal one
     has its own), a user and groups, sent as the headers a host would send. The pages show what
     that user would see. -->
<script lang="ts">
  import { session } from "../lib/session.svelte";
  import { t } from "../lib/i18n.svelte";
  import { api, ApiError } from "../api/client";
  import type { TenantOut } from "../api/types";
  import ErrorNote from "./ErrorNote.svelte";

  let { done }: { done?: () => void } = $props();

  let tenant = $state(session.tenant);
  let user = $state(session.user);
  let groups = $state(session.groups);
  let tenants = $state<TenantOut[]>([]);
  let error = $state<ApiError | null>(null);

  $effect(() => {
    if (session.personal) return;
    api
      .tenants()
      .then((r) => {
        tenants = r.tenants;
        if (!tenant && r.tenants.length === 1) tenant = r.tenants[0]?.tenant_id ?? "";
      })
      .catch((e: unknown) => {
        error = e instanceof ApiError ? e : new ApiError(0, "error", String(e));
      });
  });

  // What the service reads from a header: printable ASCII.
  const sendable = (s: string) => /^[\x20-\x7e]*$/.test(s);
  const unsendable = $derived(!sendable(user) || !sendable(groups));

  function apply(e: SubmitEvent) {
    e.preventDefault();
    if (unsendable) return;
    session.view(session.personal ? "" : tenant, user, groups);
    done?.();
  }
</script>

<form class="viewer" onsubmit={apply}>
  {#if !session.personal}
    <label class="field">
      <span>{t("viewer.tenant")}</span>
      {#if tenants.length > 0}
        <select bind:value={tenant} required>
          <option value="" disabled>{t("viewer.chooseTenant")}</option>
          {#each tenants as x (x.tenant_id)}<option value={x.tenant_id}>{x.tenant_id}</option>{/each}
        </select>
      {:else}
        <input type="text" bind:value={tenant} required spellcheck="false" placeholder="acme" />
      {/if}
    </label>
  {/if}
  <div class="two">
    <label class="field">
      <span>{t("viewer.user")}</span>
      <input type="text" bind:value={user} spellcheck="false" placeholder={session.personal ? "admin" : ""} />
    </label>
    <label class="field">
      <span>{t("viewer.groups")}</span>
      <input type="text" bind:value={groups} spellcheck="false" placeholder="risk,ops" />
    </label>
  </div>
  <p class="hint">{t("viewer.hint")}</p>
  {#if unsendable}<p class="warn">{t("viewer.unsendable")}</p>{/if}
  {#if error}<ErrorNote {error} />{/if}
  <button class="btn primary" type="submit" disabled={unsendable || (!session.personal && !tenant.trim())}>{t("viewer.apply")}</button>
</form>

<style>
  .viewer {
    display: grid;
    gap: var(--space-4);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--space-3);
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .warn {
    font-size: var(--step--1);
    color: var(--seal);
  }
  select {
    width: 100%;
  }
  @media (max-width: 480px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
</style>
