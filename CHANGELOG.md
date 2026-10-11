# Changelog

All notable changes to Pinakes are listed here, by milestone. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may break APIs).
Details, measurements and decisions for each milestone are in
[docs/design.md](docs/design.md) under "As built in Mx".

## [Unreleased]

### Added (M10, 2026-10-10)

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
  optional (compose profile `markitdown`), for PDF and `.doc`/`.xls`/`.ppt`; without it those are
  refused with what to do.
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

## [0.1.0] - 2026-10-10

The first version: milestones M1 to M9.

### Added

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
