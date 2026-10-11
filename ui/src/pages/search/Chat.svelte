<!-- The developer chat (`POST /dev/chat`, PINAKES_DEV=1): the librarian's model answers with
     the read tools an MCP agent has, and every step streams in as it happens — the call it
     chose and why, what the tool returned, then the answer. -->
<script lang="ts">
  import { fly } from "svelte/transition";
  import { callerHeaders } from "../../api/client";
  import { t } from "../../lib/i18n.svelte";
  import { dur, ease } from "../../lib/motion";
  import Icon from "../../components/Icon.svelte";

  type Entry =
    | { kind: "step"; thought: string; tool: string; args: string }
    | { kind: "result"; tool: string; text: string }
    | { kind: "answer"; thought: string; text: string }
    | { kind: "error"; text: string };

  let question = $state("");
  let entries = $state<Entry[]>([]);
  let running = $state(false);
  let controller: AbortController | null = null;

  function str(v: unknown): string {
    return typeof v === "string" ? v : JSON.stringify(v, null, 2);
  }

  function field(o: unknown, k: string): unknown {
    return o && typeof o === "object" && k in o ? (o as Record<string, unknown>)[k] : undefined;
  }

  function add(event: string, data: unknown) {
    switch (event) {
      case "step":
        entries.push({ kind: "step", thought: str(field(data, "thought") ?? ""), tool: str(field(data, "tool") ?? ""), args: str(field(data, "arguments") ?? "") });
        break;
      case "result":
        entries.push({ kind: "result", tool: str(field(data, "tool") ?? ""), text: str(field(data, "result") ?? "") });
        break;
      case "answer":
        entries.push({ kind: "answer", thought: str(field(data, "thought") ?? ""), text: str(field(data, "text") ?? "") });
        break;
      case "error":
        entries.push({ kind: "error", text: str(field(data, "message") ?? data) });
        break;
    }
  }

  async function ask(e: SubmitEvent) {
    e.preventDefault();
    if (!question.trim()) return;
    entries = [];
    running = true;
    controller = new AbortController();
    try {
      const resp = await fetch("/dev/chat", {
        method: "POST",
        headers: callerHeaders(),
        body: JSON.stringify({ question: question.trim() }),
        signal: controller.signal,
      });
      if (!resp.ok || !resp.body) {
        const text = await resp.text();
        add("error", { message: text || String(resp.status) });
        return;
      }
      const reader = resp.body.pipeThrough(new TextDecoderStream()).getReader();
      let buffer = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += value;
        let cut: number;
        while ((cut = buffer.indexOf("\n\n")) >= 0) {
          const block = buffer.slice(0, cut);
          buffer = buffer.slice(cut + 2);
          let event = "message";
          const data: string[] = [];
          for (const line of block.split("\n")) {
            if (line.startsWith("event:")) event = line.slice(6).trim();
            else if (line.startsWith("data:")) data.push(line.slice(5).trimStart());
          }
          if (data.length) {
            try {
              add(event, JSON.parse(data.join("\n")));
            } catch {
              add(event, data.join("\n"));
            }
          }
        }
      }
    } catch (err) {
      if (!(err instanceof DOMException && err.name === "AbortError")) add("error", { message: String(err) });
    } finally {
      running = false;
      controller = null;
    }
  }
</script>

<form class="ask" onsubmit={ask}>
  <textarea bind:value={question} rows="2" placeholder={t("search.chat.placeholder")} aria-label={t("search.chat.question")}></textarea>
  <div class="buttons">
    {#if running}<button type="button" class="btn" onclick={() => controller?.abort()}>{t("search.chat.stop")}</button>{/if}
    <button class="btn primary" disabled={running || !question.trim()}><Icon name="arrow" size={16} />{t("search.chat.ask")}</button>
  </div>
  <p class="hint">{t("search.chat.lede")}</p>
</form>

<ol class="log" aria-live="polite">
  {#each entries as e, i (i)}
    <li class={e.kind} in:fly={{ y: 6, duration: dur("base"), easing: ease }}>
      {#if e.kind === "step"}
        <p class="thought">{e.thought}</p>
        <p class="call"><span class="tool mono">{e.tool}</span> <code>{e.args}</code></p>
      {:else if e.kind === "result"}
        <details>
          <summary>{t("search.chat.result", { tool: e.tool })}</summary>
          <pre>{e.text}</pre>
        </details>
      {:else if e.kind === "answer"}
        <p class="thought">{e.thought}</p>
        <div class="answer">{e.text}</div>
      {:else}
        <p class="error">{e.text}</p>
      {/if}
    </li>
  {/each}
  {#if running}<li class="pending">{t("search.chat.thinking")}</li>{/if}
</ol>

<style>
  .ask {
    display: grid;
    gap: var(--space-2);
    margin-bottom: var(--space-5);
    max-width: 860px;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
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
    gap: var(--space-2);
    max-width: 860px;
  }
  li {
    padding: var(--space-3) var(--space-4);
    background: var(--card);
    border: 1px solid var(--rule);
    border-radius: var(--radius-sm);
    font-size: var(--step--1);
    display: grid;
    gap: var(--space-1);
  }
  li.step {
    border-left: 3px solid var(--accent);
  }
  li.result {
    background: var(--paper-2);
  }
  li.answer {
    border-left: 3px solid var(--ok);
  }
  .thought {
    color: var(--ink-2);
    font-style: italic;
  }
  .tool {
    font-weight: 700;
    color: var(--accent);
  }
  code {
    overflow-wrap: anywhere;
  }
  pre {
    margin: var(--space-2) 0 0;
    max-height: 320px;
    overflow: auto;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: var(--step--2);
  }
  .answer {
    white-space: pre-wrap;
    font-size: var(--step-0);
    line-height: var(--leading-read);
  }
  .error {
    color: var(--seal);
  }
  .pending {
    color: var(--muted);
  }
</style>
