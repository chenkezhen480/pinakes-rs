<!-- An error as the service wrote it, and what to do next: never a bare status code. -->
<script lang="ts">
  import type { ApiError } from "../api/client";
  import { t } from "../lib/i18n.svelte";

  let { error, retry }: { error: ApiError; retry?: () => void } = $props();

  const hint = $derived(
    error.status === 0
      ? t("error.unreachable")
      : error.status === 401
        ? t("error.unauthorized")
        : error.status === 404
          ? t("error.notFound")
          : error.status === 503
            ? t("error.unavailable")
            : "",
  );
</script>

<div class="error" role="alert">
  <strong>{t("error.title", { status: error.status || "—" })}</strong>
  {#if error.message}<p class="message">{error.message}</p>{/if}
  {#if hint}<p class="hint">{hint}</p>{/if}
  {#if retry}<button class="btn small" onclick={retry}>{t("action.retry")}</button>{/if}
</div>

<style>
  .error {
    display: grid;
    gap: var(--space-2);
    justify-items: start;
    padding: var(--space-4);
    border: 1px solid color-mix(in srgb, var(--seal) 35%, var(--rule));
    border-left: 3px solid var(--seal);
    border-radius: var(--radius);
    background: var(--seal-wash);
  }
  strong {
    color: var(--seal);
    font-size: var(--step--1);
  }
  .message {
    font-size: var(--step-0);
    overflow-wrap: anywhere;
  }
  .hint {
    font-size: var(--step--1);
    color: var(--ink-2);
  }
</style>
