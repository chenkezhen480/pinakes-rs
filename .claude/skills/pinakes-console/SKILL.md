---
name: pinakes-console
description: Design and build Pinakes' web console (M10): a Svelte 5 + TypeScript single-page app built by Vite, hand-designed and animated, with no component library and no CDN. Use for any work on the console under ui/ — a new page, a component, the graph view, styles, motion, copy in Chinese or English — and to check a page visually before calling it done.
---

# Designing the Pinakes console

The console is the face of a knowledge service named after the *Pinakes*, the catalogue
Callimachus wrote for the Library of Alexandria. Build it as a designer would, not as an admin
template: one clear visual idea, motion that explains what happened, and nothing the page
doesn't need. Read `docs/design.md` "M10: a web console, built-in conversion, vectors off by
default" first; it says what each page must do. This skill says how it should look, move and
be built.

## The idea: a catalogue you can walk through

- **Paper and ink.** Light mode is warm paper with dark ink and one lapis-blue accent; dark mode
  is a night reading room. Never pure white or pure black.
- **Cards and drawers.** Pages, records, nodes and proposals are catalogue cards: a title, a
  kind label like a shelf mark, a short summary, small metadata in monospace (ids, dates).
  The three search slots are three drawers of one cabinet.
- **Marks, not decoration.** Colour carries meaning only: the five kinds each have a hue, lower
  trust (`external`) is a dotted margin rule, a held proposal has a seal-red corner, coverage is
  how full a drawer's label is. No gradients for their own sake, no stock illustrations, no
  emoji as icons.
- **Typography does the work.** Titles in a serif, body and UI in a sans, ids and keys in mono.
  Reading views keep a 68-character measure (about 38 Chinese characters) and generous leading.

## How it is built

- **Svelte 5 with TypeScript, built by Vite** (decided with the user 2026-10-10: a framework, for
  a console of this many pages, the lightest one, and the one with motion built in). A plain
  single-page app, not SvelteKit: `ui/` holds `package.json` (with its lock file),
  `vite.config.ts`, `tsconfig.json` (strict) and `src/`; `npm run build` writes static files to
  `ui/dist/`, which the service embeds and serves at `/ui`.
- **Commit `ui/dist/` with the sources**, so `cargo build`, the Docker image and anyone not
  editing the console never run Node. The check that it is current
  (`npm ci && npm run build && git diff --exit-code ui/dist`) runs with the browser tests and
  before every commit that touches `ui/`.
- **Runes and typed props** (`$state`, `$derived`, `$props`, `$effect` only for real side
  effects); components small and named for what they show (`HitCard`, `Drawer`, `NodeChip`,
  `JobTray`). No `any`, no `as` casts of API data: the API's types are generated from the Rust
  structs into `ui/src/api/types.ts` (regenerate when a handler's shapes change; never edit by
  hand). `npm run check` (svelte-check and tsc) passes with no errors or warnings.
- **No component library.** Every component is drawn to this skill's language; a library would
  make it look like every other admin template.
- **Few dependencies, each with a reason**: `svelte`, `vite` and its Svelte plugin, `typescript`,
  `svelte-check`, `d3-force` (the graph's layout only, not all of d3) and `@playwright/test` for
  the browser tests. A routing library isn't needed: a small hash router of our own (`#/wiki/…`).
  Anything else is written, not added, unless a note next to it says why.
- **Budget.** The built JavaScript under 150 KB and CSS under 30 KB, gzipped (`npm run build`
  prints the sizes); a page that pushes past it is split or slimmed before it is done.
- **The service renders Markdown.** Pages arrive as sanitized HTML from the API (rendered and
  sanitized in Rust); the console never parses Markdown. `{@html}` appears in exactly one
  component (`SafeHtml.svelte`), fed only by those API fields; everything else is text.
- **The graph**: `d3-force` computes positions, Svelte draws the SVG, so nodes and edges are
  components with transitions like the rest (a few hundred nodes at most are ever shown).
- **Content-Security-Policy is strict** (`script-src 'self'`, no `unsafe-eval`, no inline
  scripts): Vite's build needs neither. Check that the transitions run under the policy, and
  allow only what they really need.

## Tokens

All colour, type, spacing and motion come from custom properties in `ui/src/styles/tokens.css`
(components use them in their scoped `<style>`); a value that isn't a token is a bug. Starting point (tune by eye, keep the roles):

```css
:root {
  --paper: #f6f3ec; --card: #fffdf8; --ink: #1f2328; --ink-2: #4a4f57; --muted: #6f747b;
  --rule: #e2dccf; --accent: #2f5d8a; --accent-wash: #e3ebf4; --seal: #b4432f;
  --ok: #3f7d4e; --warn: #a8701a;
  /* the five kinds */
  --k-experience: #c2703d; --k-decision: #7a4e9c; --k-expertise: #2f6f8a;
  --k-graph: #3f7d4e; --k-preference: #a3476b;
  --serif: "Songti SC", "Noto Serif CJK SC", "Source Han Serif SC", SimSun, Georgia, serif;
  --sans: "PingFang SC", "Microsoft YaHei UI", "Microsoft YaHei", "Noto Sans CJK SC",
          system-ui, -apple-system, "Segoe UI", sans-serif;
  --mono: ui-monospace, "Cascadia Mono", "SFMono-Regular", Consolas, monospace;
  --step--1: 0.8125rem; --step-0: 0.9375rem; --step-1: 1.125rem; --step-2: 1.4rem; --step-3: 1.85rem;
  --space-1: 4px; --space-2: 8px; --space-3: 12px; --space-4: 16px; --space-5: 24px; --space-6: 40px;
  --radius: 6px; --shadow: 0 1px 0 var(--rule), 0 6px 18px -12px rgb(31 35 40 / 0.35);
  --ease: cubic-bezier(.2, .8, .2, 1); --fast: 120ms; --base: 200ms; --slow: 320ms;
}
@media (prefers-color-scheme: dark) { :root:not([data-theme="light"]) {
  --paper: #14161a; --card: #1c1f24; --ink: #e8e6e1; --ink-2: #c4c2bc; --muted: #969ca4;
  --rule: #2c3038; --accent: #86acd8; --accent-wash: #1f2a37; --seal: #e07a62;
  --ok: #82c191; --warn: #e2b45c;
} }
```

System fonts only: no web fonts are downloaded.

## Motion that explains

Every animation answers "what just happened?" and none runs for show.

Svelte's own tools first: `transition:` and `in:`/`out:` for arrivals and departures,
`animate:flip` for lists that reorder, `svelte/motion` (`Spring`, `Tween`) for values that move
(counters, the graph's zoom). Durations and easing come from the tokens (read once into a small
`motion.ts`, which also returns 0 under reduced motion).

- **Arrivals:** search hits deal into their drawers one after another (30 ms stagger, 200 ms
  each, a 6 px rise and fade); a new job slides into the tray.
- **Following a route:** the card expands into the reader (the View Transitions API, with
  `view-transition-name` on the card and the reader's title); browsers without it just switch.
- **The graph settles:** the force layout runs visibly for about 600 ms, then freezes; dragging a
  node wakes only its neighbours. A walk deeper fades the new ring in from the node it grew from.
- **Drafts on the graph:** added nodes and edges draw in with the accent, removed ones stay as
  dashed ghosts in seal red, changed ones pulse once.
- **Review:** an approved card files itself away (shrinks toward the queue's counter); a rejected
  one asks for its reason in place before it goes.
- **Progress is honest:** job steps, embedding downloads and crawls show real counts from the
  API, never a fake spinner percentage.
- `prefers-reduced-motion: reduce` turns every movement into an instant change; nothing depends
  on animation to be understood.

## Every view has four states

Design each one before the happy path: **empty** (what this is and the one action to start),
**loading** (the layout's skeleton, no layout shift when data arrives), **full**, and **error**.
An error shows the API's own message (they are written to be read, such as the 415 that says how
to turn on MarkItDown) and what to do next, never a bare status code.

## Words

- Chinese and English, picked from the browser's language with a switch; strings live in
  `ui/src/i18n/zh.ts` and `ui/src/i18n/en.ts` (the English file's keys type the Chinese one, so
  a missing string fails `npm run check`), never written into components. Use the API's names for things (page,
  source, node, space, proposal, draft) consistently in both languages.
- Secrets are never shown: a key is "set" or "not set". Settings explain each field in one
  plain sentence, with its default and whether it applies now or after a restart.
- Destructive actions (deleting a tenant, user or group) ask for the name to be typed.

## Accessibility and size

Keyboard reachable everywhere with a visible focus ring; WCAG AA contrast in both themes (check
the kind hues on cards); `aria-live` for job progress; semantic elements before `div`s. Works
from a phone's width up (the local-app direction): at narrow widths the drawers stack and the
graph gets the full screen.

## Before calling a page done

1. Run the service bound to `127.0.0.1` (`run_in_background`), with a tenant that has data
   (the test fixtures or a quick ingest). While working, `npm run dev` (Vite, with `/api` calls
   proxied to the service) gives instant reloads; the check below is on the built files the
   service serves.
2. Open the page in a real browser (the `built-in-browser` or `chrome-browser` skill) and look:
   light and dark, a wide and a phone-sized window, `prefers-reduced-motion`, and each of the
   four states. Take screenshots and judge them as a designer: alignment, rhythm, contrast,
   whether the motion explains.
3. The browser console shows no errors and no CSP violations.
4. `npm run check` passes with no errors or warnings, `npm run build` stays within the budget,
   and `ui/dist/` is committed as built.
5. The API tests still pass (`cargo test -j 2`), since the console only calls the API.
