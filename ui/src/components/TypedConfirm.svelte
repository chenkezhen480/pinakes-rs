<!-- A destructive action asks for the name to be typed before it runs. -->
<script lang="ts">
  import { t } from "../lib/i18n.svelte";

  let {
    open = $bindable(false),
    title,
    text,
    name,
    action,
    onconfirm,
  }: {
    open?: boolean;
    title: string;
    text: string;
    name: string;
    action: string;
    onconfirm: () => void;
  } = $props();

  let dialog: HTMLDialogElement | undefined = $state();
  let typed = $state("");

  $effect(() => {
    if (!dialog) return;
    if (open && !dialog.open) {
      typed = "";
      dialog.showModal();
    } else if (!open && dialog.open) {
      dialog.close();
    }
  });
</script>

<dialog bind:this={dialog} onclose={() => (open = false)}>
  <form
    method="dialog"
    onsubmit={(e) => {
      e.preventDefault();
      if (typed === name) {
        open = false;
        onconfirm();
      }
    }}
  >
    <h2>{title}</h2>
    <p>{text}</p>
    <label class="field">
      <span>{t("confirm.type", { name })}</span>
      <input type="text" bind:value={typed} autocomplete="off" spellcheck="false" />
    </label>
    <div class="buttons">
      <button type="button" class="btn quiet" onclick={() => (open = false)}>{t("action.cancel")}</button>
      <button type="submit" class="btn danger solid" disabled={typed !== name}>{action}</button>
    </div>
  </form>
</dialog>

<style>
  dialog {
    border: 1px solid var(--rule);
    border-radius: var(--radius);
    background: var(--card);
    color: var(--ink);
    padding: var(--space-5);
    width: min(440px, calc(100vw - 32px));
    box-shadow: var(--shadow-lift);
  }
  dialog::backdrop {
    background: rgb(20 22 26 / 0.45);
  }
  form {
    display: grid;
    gap: var(--space-4);
  }
  h2 {
    font-size: var(--step-2);
    color: var(--seal);
  }
  p {
    color: var(--ink-2);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
</style>
