<!-- The environment variables the service and its compose file read: what each does, whether
     it is set and where to set it. A secret's value never leaves the service. -->
<script lang="ts">
  import { api } from "../../api/client";
  import { Resource } from "../../lib/resource.svelte";
  import { t } from "../../lib/i18n.svelte";
  import Loading from "../../components/Loading.svelte";
  import ErrorNote from "../../components/ErrorNote.svelte";
  import AccountForm from "./AccountForm.svelte";

  const env = new Resource(() => api.environment());
</script>

<AccountForm />

<p class="lede">{t("settings.env.lede")}</p>
{#if env.loading && !env.data}
  <Loading rows={3} />
{:else if env.error}
  <ErrorNote error={env.error} retry={() => env.reload()} />
{:else if env.data}
  <div class="list">
    {#each env.data.variables as v (v.name)}
      <div class="var">
        <div class="name">
          <code>{v.name}</code>
          {#if v.secret}<span class="secret">{t("settings.env.secret")}</span>{/if}
        </div>
        <p class="what">{v.description}</p>
        <p class="state">
          <span class={v.set ? "set" : "unset"}>{v.set ? t("settings.env.set") : t("settings.env.notSet")}</span>
          {#if v.value}<code class="value">{v.value}</code>{/if}
        </p>
        <p class="where">{t("settings.env.where", { where: v.set_in })}</p>
      </div>
    {/each}
  </div>
{/if}

<style>
  .lede {
    color: var(--ink-2);
    margin-bottom: var(--space-4);
    max-width: 75ch;
  }
  .list {
    display: grid;
    gap: var(--space-2);
  }
  .var {
    display: grid;
    grid-template-columns: minmax(180px, 0.8fr) minmax(0, 2fr) auto;
    grid-template-areas: "name what state" "name where state";
    gap: var(--space-1) var(--space-4);
    padding: var(--space-3) var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
  }
  .name {
    grid-area: name;
    display: grid;
    align-content: start;
    gap: var(--space-1);
  }
  .name code {
    font-weight: 700;
    overflow-wrap: anywhere;
  }
  .secret {
    font-size: var(--step--2);
    color: var(--seal);
    font-weight: 700;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .what {
    grid-area: what;
    font-size: var(--step--1);
    color: var(--ink-2);
  }
  .where {
    grid-area: where;
    font-size: var(--step--2);
    color: var(--muted);
  }
  .state {
    grid-area: state;
    display: grid;
    justify-items: end;
    gap: var(--space-1);
    font-size: var(--step--1);
  }
  .set {
    color: var(--ok);
    font-weight: 700;
  }
  .unset {
    color: var(--muted);
  }
  .value {
    max-width: 220px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  @media (max-width: 720px) {
    .var {
      grid-template-columns: 1fr;
      grid-template-areas: "name" "what" "where" "state";
    }
    .state {
      justify-items: start;
    }
  }
</style>
