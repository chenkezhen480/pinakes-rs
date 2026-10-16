# Changelog

All notable changes to Pinakes are listed here, by milestone. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may break APIs).
How each part works is in [docs/guide.md](docs/guide.md).

## [Unreleased]

### Added

- **Reading progress**: while a document is read, its job shows how far the reading has come
  (`progress` on `/jobs`: lines read of the total, concepts filed so far), updated after every
  window; the console draws it as a bar on the upload page and in the jobs tray.
- **Auto-approve** (`[librarian] auto_approve`, off by default, on the console's Settings page):
  every proposal is approved at once through the admin's approval path, instead of waiting on
  the Review page: edits the verifier held (filing, lint's fixes, hand edits; a job lists them in
  `result.approved`), candidate sources from acquisition (`approved` in the run's report) and
  agents' graph drafts (the draft's answer says what it `applied`).

### Fixed

- The console marked uploaded PDFs as needing MarkItDown, though the service reads them itself.
- A document's reading could fail on numbers the text never lacked: the check that every number
  of an extract comes from the text counted the extract's own list numbering ("1. ", "2) ",
  "3、") and, when a new version was read, the version numbers the librarian is told ("since
  version 2 …"), and refused the reading four times. A refusal now quotes where the number is.
- A record could be lost without a trace: sent back with the verifier's reasons, the model
  could answer with no edits for it, which counted as filed. It is now asked again, and if it
  keeps dropping the record, the edits it was last refused for are held (or refused) as for any
  record that keeps failing. A record that files nothing says so in `log.md`.

## [0.1.0] - 2026-10-13

The first version: milestones M1 to M10, and the work after them.

### Added after M10 (2026-10-11 to 2026-10-13)

- **One tool for decisions and lessons**: MCP `record_decision_or_lesson` (`POST /ingest/note`)
  replaces `record_decision` and `record_experience`. The agent only judges that something is
  worth keeping; the librarian splits the note, filing decisions for the user and lessons for the
  team, and keeps what it can't tell apart as the user's decision. The HTTP inserts
  `/ingest/decision` and `/ingest/experience` stay for host code that knows the kind.
- **Recording unprompted**: the MCP server's instructions and the record tools ask the agent to
  record lasting preferences, standing facts about the user (role, region, accounts), decisions
  and lessons in the same turn, without being asked, and not to record one-off instructions,
  hypotheticals or questions.
- **PDF read by the service**: the text layer is read by `pinakes-pdf`, a second binary that
  reads each PDF in a process of its own, its memory capped and killed past a deadline. A scan
  (no text layer) is drawn page by page and transcribed by the librarian's model. MarkItDown is
  now needed only for `.doc`, `.xls`, `.ppt` and PDFs the service can't read.
- **An 8-bit embedding model**: after the download the service quantizes
  jina-embeddings-v2-base-zh to 8-bit weights itself (checked against a pinned SHA-256) and
  removes the full one: same answers on the test set at half the query time and memory. In `both`
  mode the query is embedded alongside the keyword search.
- **Graph spaces changed and deleted**: `PUT /graph/spaces/{space}` sets a space's visibility and
  members (its owner or the admin); `DELETE /graph/spaces/{space}` removes a space with its data
  (the admin), and agents can propose it in a draft (`delete_space`), reviewed like any draft.
- **Job status kept across restarts** in the tenant's SQLite; a job a restart cut off answers
  failed with "send it again".
- `/graph` and `/graph/path` write search trace lines too.
- The console's Overview tests the model with one button; turning embeddings off drops the page
  vectors at once.

### Fixed

- A tenant's search snapshot and graph are built in a task of their own that later requests
  wait for, so a client that gives up no longer throws the build away; new page vectors are stored
  as they are made.

### Added in M10 (2026-10-10)

- **A web console** at `/ui` for every operation: an overview, search (the three slots, with the
  developer chat as a tab), the wiki rendered with its history and hand edits, adding knowledge
  (documents dragged in, records by kind and audience, every job followed), the graph browsed and
  drawn (schemas, spaces, data, drafts), review (held edits as diffs, sources, drafts drawn),
  lint, acquisition, tenants and audit logs, and settings (the configuration explained field by
  field and written back with its comments, the environment, the model with a test call, the
  embedding switch, the converters). Svelte 5 and TypeScript, built into `ui/dist/` and embedded
  in the binary, under a strict Content-Security-Policy; Chinese and English, light and dark,
  phone-sized too. Its API types are generated from the Rust structs.
- **Built-in conversion** of HTML, Word, Excel (`.xlsx`, CSV) and PowerPoint in pure
  Rust, every file bounded (size, zip bombs, entities, nesting, cells, time). MarkItDown is
  optional (compose profile `markitdown`) for the formats the service doesn't read; without it
  those are refused with what to do.
- **The Library's hall**: the console opens as a 3D hall of the Library of Alexandria (three.js,
  loaded on demand), its sections standing in it as the catalogue table, the scroll shelves, the
  scriptorium, the armillary sphere, the sealing lectern, the water clock and the rest; through
  the arches a sea of clouds under a sky that follows the clock (morning or rain by day, sunset,
  stars). The list layout is a click away.
- **Signing in to the console** with one admin account: `root` / `1234` (change it before
  exposing the service) or `PINAKES_ADMIN_USER` / `PINAKES_ADMIN_PASSWORD`,
  changed in Settings; the browser stays signed in. The signed-in admin looks at any tenant as
  any of its users, so the console needs no tenant keys and no admin key.
- **Vector search off by default**, turned on and off while the service runs
  (`/admin/embedding`): the model downloads and loads in the background, with its progress shown.
- New APIs for the console: `/status`, `/jobs`, `/admin/tenants` (list), graph browsing
  (`/graph/schemas`, `/graph/spaces`, `/graph/nodes`), `/pages/{path}?html=true`,
  `/history/{path}`, `/admin/settings`, `/admin/environment`, `/admin/model` (and `/test`),
  `/admin/audit`, `/admin/tenants/{id}/audit`. `docs/configuration.md` is generated from the
  settings' descriptions.

### Added in M1 to M9 (2026-09-29 to 2026-10-10)

- **M9, acquisition and scale** (2026-10-10): gaps found by lint are matched against a local
  index of the sites a tenant trusts (read politely: `robots.txt`, `Crawl-delay`, sitemaps), and
  pages are proposed as sources after the librarian screens them for prompt injection; a person
  approves before anything is filed. An HNSW index of our own over page vectors for large
  tenants. Agents propose graph changes as one reviewable draft per space
  (`POST /graph/draft`, MCP `propose_graph_change`). Node properties declared by the schema.
  `POST /proposals/{id}/reject`.
- **M8, lint and deletion** (2026-10-09): contradictions, stale claims, duplicates, broken links,
  unanchored memories, frequent weak searches and missing aliases; hard deletion of users,
  groups and tenants, checked byte by byte. Graph data in SQLite by default, Neo4j optional,
  with `pinakes-migrate-graph` to move between them. A compute budget (`[server] threads`).
- **M7, MCP and packaging** (2026-10-08): the MCP server and its tools, JWT mode, the `/dev` test
  chat, the Docker image and Compose stack.
- **M6, speed** (2026-10-08): incremental snapshot updates, the tantivy index on disk, int8
  vectors, a benchmark at 1k, 10k and 100k pages.
- **M5, hop and meaning** (2026-10-04): the memory-to-knowledge hop; semantic, both and auto
  search modes with a local Chinese–English embedding model.
- **M4, the librarian** (2026-10-02): the five insert entry points, the plan-write-verify loop,
  the per-tenant write queue and jobs, held proposals, reading documents like a person, uploads
  through a converter container.
- **M3, the business graph** (2026-09-30): schemas, spaces with visibility, graph data, the
  relations slot, walks and paths.
- **M2, search** (2026-09-30): one search entry in keyword mode for Chinese and English, the read
  tracker, search traces and `pinakes-replay`; the starter test set of six scenarios.
- **M1, skeleton** (2026-09-29): configuration, API-key identity, tenant folders, per-tenant
  SQLite, pages as files, tenant isolation.

[Unreleased]: https://github.com/chenkezhen480/pinakes-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/chenkezhen480/pinakes-rs/releases/tag/v0.1.0
