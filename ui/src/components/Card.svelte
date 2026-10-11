<!-- A catalogue card: a shelf mark, a title in the serif, a summary and small metadata in mono.
     A lower-trust card (`external`) has a dotted margin rule; a held one a seal-red corner. -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { Kind } from "../api/types";
  import KindMark from "./KindMark.svelte";
  import { t } from "../lib/i18n.svelte";
  import { plainLinks } from "../lib/format";

  let {
    title,
    kind = null,
    href = "",
    summary = "",
    external = false,
    held = false,
    selected = false,
    transitionName = "",
    meta,
    children,
    onclick,
  }: {
    title: string;
    kind?: Kind | null;
    href?: string;
    summary?: string;
    external?: boolean;
    held?: boolean;
    selected?: boolean;
    transitionName?: string;
    meta?: Snippet;
    children?: Snippet;
    onclick?: (e: MouseEvent) => void;
  } = $props();
</script>

<article class="card" class:external class:held class:selected style:view-transition-name={transitionName || undefined}>
  <header>
    <KindMark {kind} />
    {#if external}<span class="trust">{t("card.external")}</span>{/if}
  </header>
  <h3>
    {#if href}
      <a {href} {onclick}>{title}</a>
    {:else}
      {title}
    {/if}
  </h3>
  {#if summary}<p class="summary">{plainLinks(summary)}</p>{/if}
  {#if children}{@render children()}{/if}
  {#if meta}<footer>{@render meta()}</footer>{/if}
</article>

<style>
  .card {
    position: relative;
    display: grid;
    gap: var(--space-2);
    padding: var(--space-4) var(--space-4) var(--space-3);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
    transition:
      box-shadow var(--fast) var(--ease),
      transform var(--fast) var(--ease),
      border-color var(--fast) var(--ease);
    min-width: 0;
  }
  .card:hover {
    box-shadow: var(--shadow-lift);
  }
  .card.selected {
    border-color: var(--accent);
  }
  .card.external::before {
    content: "";
    position: absolute;
    left: 6px;
    top: 10px;
    bottom: 10px;
    border-left: 2px dotted var(--warn);
  }
  .card.held::after {
    content: "";
    position: absolute;
    top: -1px;
    right: -1px;
    width: 22px;
    height: 22px;
    background: linear-gradient(225deg, var(--seal) 50%, transparent 50%);
    border-top-right-radius: var(--radius);
  }
  header {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-height: 16px;
  }
  .trust {
    font-size: var(--step--2);
    color: var(--warn);
    font-weight: 600;
  }
  h3 {
    font-size: var(--step-1);
    overflow-wrap: anywhere;
  }
  h3 a {
    color: var(--ink);
    text-decoration: none;
  }
  h3 a:hover {
    color: var(--accent);
    text-decoration: underline;
  }
  h3 a::after {
    content: "";
    position: absolute;
    inset: 0;
  }
  .summary {
    color: var(--ink-2);
    font-size: var(--step--1);
    line-height: var(--leading);
  }
  footer {
    min-width: 0;
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2);
    align-items: center;
    position: relative;
    z-index: 1;
  }
</style>
