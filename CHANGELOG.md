# Changelog

All notable changes to Pinakes are listed here, by milestone. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/) (pre-1.0: minor versions may break APIs).
Details, measurements and decisions for each milestone are in
[docs/design.md](docs/design.md) under "As built in Mx".

## [Unreleased]

### Planned (M10)

- A web console for every operation (search, wiki, knowledge, graph, review, lint, acquisition,
  administration, settings), in Svelte 5 and TypeScript.
- Built-in conversion of HTML, Word, Excel and PowerPoint files; the MarkItDown container becomes
  optional, with clear errors for formats only it reads.
- Vector search off by default, turned on from the console (the embedding model downloads then).

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
