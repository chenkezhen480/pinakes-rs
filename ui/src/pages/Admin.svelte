<!-- Administration: tenants (created with their first key shown once, listed, deleted with the
     name typed), deleting a user or a group (hard: their text is gone from every file), and
     the audit logs, the service's and each tenant's. -->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { api } from "../api/client";
  import type { CreateTenantResponse } from "../api/types";
  import { Resource, Action } from "../lib/resource.svelte";
  import { t } from "../lib/i18n.svelte";
  import { when } from "../lib/format";
  import { dur, ease } from "../lib/motion";
  import PageHead from "../components/PageHead.svelte";
  import Loading from "../components/Loading.svelte";
  import ErrorNote from "../components/ErrorNote.svelte";
  import TypedConfirm from "../components/TypedConfirm.svelte";
  import Icon from "../components/Icon.svelte";

  const tenants = new Resource(() => api.tenants());
  const audit = new Resource(() => api.audit());

  const create = new Action();
  const remove = new Action();
  let newId = $state("");
  let created = $state<CreateTenantResponse | null>(null);
  let copied = $state(false);
  let confirm = $state<{ kind: "tenant" | "user" | "group"; name: string; tenant?: string } | null>(null);
  let confirmOpen = $state(false);
  let userId = $state("");
  let groupTenant = $state("");
  let groupId = $state("");
  let removed = $state("");
  let tenantAuditFor = $state("");
  const tenantAudit = new Action();
  let tenantEntries = $state<{ at: string; actor: string; action: string; detail: string }[]>([]);

  async function submitTenant(e: SubmitEvent) {
    e.preventDefault();
    const r = await create.run(() => api.createTenant(newId.trim()));
    if (r) {
      created = r;
      newId = "";
      copied = false;
      void tenants.reload();
      void audit.reload();
    }
  }

  async function copyKey() {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created.api_key);
      copied = true;
    } catch {
      copied = false;
    }
  }

  function ask(kind: "tenant" | "user" | "group", name: string, tenant?: string) {
    confirm = { kind, name, tenant };
    confirmOpen = true;
  }

  async function confirmed() {
    const c = confirm;
    if (!c) return;
    const done = await remove.run(async () => {
      if (c.kind === "tenant") await api.deleteTenant(c.name);
      else if (c.kind === "user") await api.deleteUser(c.name);
      else await api.deleteGroup(c.tenant ?? "", c.name);
      return true;
    });
    if (done) {
      removed = t(`admin.removed.${c.kind}`, { name: c.name });
      userId = groupId = "";
      void tenants.reload();
      void audit.reload();
    }
  }

  async function showTenantAudit(id: string) {
    tenantAuditFor = id;
    const r = await tenantAudit.run(() => api.tenantAudit(id));
    tenantEntries = r?.entries ?? [];
  }

  function detail(v: unknown): string {
    if (v === null || v === undefined) return "";
    return typeof v === "string" ? v : JSON.stringify(v);
  }
</script>

<PageHead title={t("nav.admin")} lede={t("admin.lede")} />

{#if removed}<p class="done" in:fly={{ y: -6, duration: dur("base"), easing: ease }}>{removed}</p>{/if}
  {#if remove.error}<ErrorNote error={remove.error} />{/if}
  <div class="layout">
    <section class="panel">
      <h2>{t("admin.tenants")}</h2>
      <form class="inline" onsubmit={submitTenant}>
        <input type="text" class="mono" bind:value={newId} placeholder={t("admin.tenantId")} aria-label={t("admin.tenantId")} required />
        <button class="btn primary" disabled={create.running}><Icon name="plus" size={16} />{t("admin.createTenant")}</button>
      </form>
      {#if create.error}<ErrorNote error={create.error} />{/if}
      {#if created}
        <div class="key" in:fly={{ y: 6, duration: dur("base"), easing: ease }}>
          <strong>{t("admin.keyOnce", { tenant: created.tenant_id })}</strong>
          <code>{created.api_key}</code>
          <div class="keyactions">
            <button class="btn small" onclick={copyKey}>{copied ? t("action.copied") : t("action.copy")}</button>
            <button class="btn quiet small" onclick={() => (created = null)}>{t("admin.keySaved")}</button>
          </div>
        </div>
      {/if}
      {#if tenants.loading && !tenants.data}
        <Loading rows={2} card={false} />
      {:else if tenants.error}
        <ErrorNote error={tenants.error} retry={() => tenants.reload()} />
      {:else if tenants.data}
        <table>
          <thead><tr><th>{t("admin.tenantId")}</th><th>{t("admin.folder")}</th><th>{t("admin.memory")}</th><th></th></tr></thead>
          <tbody>
            {#each tenants.data.tenants as tn (tn.tenant_id)}
              <tr>
                <td class="mono">{tn.tenant_id}</td>
                <td class="mono small">{tn.dir}</td>
                <td>{tn.in_memory ? t("admin.loaded") : "—"}</td>
                <td class="actions">
                  <button class="btn quiet small" onclick={() => showTenantAudit(tn.tenant_id)}>{t("admin.audit")}</button>
                  <button class="btn danger small" onclick={() => ask("tenant", tn.tenant_id)}><Icon name="trash" size={14} />{t("action.delete")}</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
      {#if tenantAuditFor}
        <div class="tenant-audit">
          <h3>{t("admin.tenantAudit", { tenant: tenantAuditFor })}</h3>
          {#if tenantAudit.error}<ErrorNote error={tenantAudit.error} />{/if}
          <ol class="log">
            {#each tenantEntries as e, i (i)}
              <li><span class="when">{when(e.at)}</span> <span class="mono action">{e.action}</span> <span class="actor">{e.actor}</span> <span class="mono small">{e.detail}</span></li>
            {/each}
          </ol>
        </div>
      {/if}
    </section>

    <section class="panel">
      <h2>{t("admin.deleteOwners")}</h2>
      <p class="hint">{t("admin.deleteText")}</p>
      <form class="inline" onsubmit={(e) => { e.preventDefault(); ask("user", userId.trim()); }}>
        <input type="text" class="mono" bind:value={userId} placeholder={t("admin.userId")} aria-label={t("admin.userId")} required />
        <button class="btn danger" disabled={!userId.trim()}>{t("admin.deleteUser")}</button>
      </form>
      <p class="hint small">{t("admin.userScope")}</p>
      <form class="inline" onsubmit={(e) => { e.preventDefault(); ask("group", groupId.trim(), groupTenant.trim()); }}>
        <input type="text" class="mono" bind:value={groupTenant} placeholder={t("admin.tenantId")} aria-label={t("admin.tenantId")} required />
        <input type="text" class="mono" bind:value={groupId} placeholder={t("admin.groupId")} aria-label={t("admin.groupId")} required />
        <button class="btn danger" disabled={!groupId.trim() || !groupTenant.trim()}>{t("admin.deleteGroup")}</button>
      </form>
    </section>
  </div>

  <section class="panel wide">
    <h2>{t("admin.serviceAudit")}</h2>
    {#if audit.loading && !audit.data}
      <Loading rows={2} card={false} />
    {:else if audit.error}
      <ErrorNote error={audit.error} retry={() => audit.reload()} />
    {:else if audit.data && audit.data.entries.length === 0}
      <p class="hint">{t("admin.auditEmpty")}</p>
    {:else if audit.data}
      <ol class="log">
        {#each audit.data.entries as e, i (i)}
          <li><span class="when">{when(e.at)}</span> <span class="mono action">{e.action}</span> <span class="actor">{e.actor}</span> <span class="mono small">{detail(e.detail)}</span></li>
        {/each}
      </ol>
    {/if}
  </section>

  {#if confirm}
    <TypedConfirm
      bind:open={confirmOpen}
      title={t(`admin.confirm.${confirm.kind}`, { name: confirm.name })}
      text={t("admin.confirmText")}
      name={confirm.name}
      action={t("action.delete")}
      onconfirm={confirmed}
    />
  {/if}

<style>
  .done {
    margin-bottom: var(--space-4);
    padding: var(--space-3);
    background: var(--ok-wash);
    border-radius: var(--radius);
    font-size: var(--step--1);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 1.5fr) minmax(280px, 1fr);
    gap: var(--space-5);
    align-items: start;
    margin-bottom: var(--space-5);
  }
  .panel {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-5);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    align-content: start;
    min-width: 0;
  }
  h2 {
    font-size: var(--step-1);
  }
  h3 {
    font-size: var(--step-0);
  }
  .inline {
    display: flex;
    gap: var(--space-2);
  }
  .key {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3);
    border: 1px solid var(--warn);
    border-radius: var(--radius);
    background: var(--warn-wash);
    font-size: var(--step--1);
  }
  .key code {
    overflow-wrap: anywhere;
    font-size: var(--step--1);
  }
  .keyactions {
    display: flex;
    gap: var(--space-2);
  }
  table {
    border-collapse: collapse;
    width: 100%;
    font-size: var(--step--1);
  }
  th,
  td {
    text-align: left;
    padding: var(--space-2);
    border-bottom: 1px solid var(--rule);
  }
  th {
    color: var(--muted);
    font-weight: 600;
  }
  .actions {
    text-align: right;
    white-space: nowrap;
  }
  .small {
    font-size: var(--step--2);
    color: var(--muted);
  }
  .hint {
    font-size: var(--step--1);
    color: var(--muted);
  }
  .log {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: var(--space-1);
    font-size: var(--step--1);
    max-height: 420px;
    overflow-y: auto;
  }
  .log li {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    padding: var(--space-1) 0;
    border-bottom: 1px dashed var(--rule);
  }
  .when {
    color: var(--muted);
  }
  .action {
    color: var(--accent);
  }
  .actor {
    color: var(--ink-2);
  }
  .tenant-audit {
    display: grid;
    gap: var(--space-2);
    padding-top: var(--space-3);
    border-top: 1px solid var(--rule);
  }
  @media (max-width: 900px) {
    .layout {
      grid-template-columns: 1fr;
    }
    .inline {
      flex-wrap: wrap;
    }
  }
</style>
