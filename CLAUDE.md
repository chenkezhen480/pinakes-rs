# Pinakes (crate `pinakes`)

A self-hosted knowledge service in Rust. The full design is in `docs/design.md`
(the original is `docs/Design.docx`). Read the relevant section before starting a milestone.

## Working agreement

- Build one milestone at a time (M1…M10 in "Build milestones"). M10 (planned 2026-10-10, not
  built: a web console, built-in conversion, vectors off by default) is written up in design.md
  "M10: a web console, built-in conversion, vectors off by default"; settle its "(to confirm)"
  points with the user before building them. Each milestone ends with its
  "done when" check, backed by tests.
- Reply to the user in Chinese. Code, comments and docs stay in English; commit messages may be
  in Chinese (identifiers, test names and error strings as in the code).
- Before calling a task done, run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- On this Windows machine cargo lives in `%USERPROFILE%\.cargo\bin`; in Git Bash prefix commands
  with `export PATH="$HOME/.cargo/bin:$PATH";` if `cargo` is not found.
- The shell has `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` pointing at a local proxy (127.0.0.1:7897).
  When curling the local server, first `unset HTTP_PROXY HTTPS_PROXY ALL_PROXY; export NO_PROXY=127.0.0.1,localhost`.
- Graph data is kept in SQLite (`graph.sqlite` per tenant) by default; Neo4j is an optional
  backend (`[graph] backend = "neo4j"`, compose profile `neo4j`), kept working behind
  `GraphStore` (decided 2026-10-09: lighter, and a local mobile app may come). Each tenant
  records which store holds its graph data and won't open on the other; move it with
  `pinakes-migrate-graph --to sqlite|neo4j` (service stopped; it removes the old copy;
  `tests/graph_switch.rs`).
- Graph tests against Neo4j (M3 on): `NEO4J_PASSWORD=... docker compose --profile neo4j up -d neo4j`, then
  `PINAKES_TEST_NEO4J_URI=bolt://127.0.0.1:7687 PINAKES_TEST_NEO4J_PASSWORD=... cargo test --test graph_testset`.
  Without `PINAKES_TEST_NEO4J_URI` that test is skipped; the SQLite backend always runs.
- The speed benchmark (M6): `PINAKES_BENCH_PAGES=1000,10000,100000 PINAKES_BENCH_SETTLE=240 cargo
  test --release --test bench -- --ignored --nocapture` (100k pages take about 10 minutes; results
  in design.md "As built in M6"). Don't build or run other heavy work while it runs.
- Run the server bound to `127.0.0.1` locally; binding `0.0.0.0` pops a Windows Firewall dialog.
  Start it with `run_in_background`, never with `&` inside a foreground command (that hangs the tool).

## Rules

- Every read and write path takes a `Scope` (tenant + user + groups); never take tenant, user or
  group ids from request bodies. Groups arrive in the `X-User-Groups` header (API key mode);
  who an insert is for, in the `X-Audience` header (`tenant`, `user`, `group:<id>`).
- Schema descriptions come only from the librarian (`librarian::Librarian::describe`); code
  adds the structure (`src/graph/describe.rs`). No description, no schema: definition and page
  are stored together or not at all.
- Every LLM reply is constrained by the API with a strict JSON schema (closed objects, all
  properties required, optional values nullable): `output_config.format` on Anthropic itself, a
  forced `reply` tool on Anthropic-compatible endpoints (DeepSeek, the shipped config), strict
  `response_format` on OpenAI-compatible ones.
- The librarian's model and key live only in `model.conf` (git-ignored; template
  `model.conf.example`), never in `config.toml` or code. Never print or commit it. The live test
  reads it: `PINAKES_TEST_LLM=1 cargo test --test librarian_live -- --nocapture` (costs money).
- Every wiki write runs as a job on the tenant's write queue (`src/jobs.rs`) or holds the
  tenant's `wiki_writer` lock; code, never the LLM, writes `kind`, `updated` and `sources`.
- Librarian tests script the model (`tests/common/llm.rs`); only the live test calls a real one.
- Uploaded files go through the converter container (`converter/`, MarkItDown); the service
  never parses Word, PDF or HTML itself. In the compose stack it sits on an internal network with
  no egress; for the host, start `docker compose --profile dev up -d --build
  converter-dev` (127.0.0.1:8090); `PINAKES_TEST_CONVERTER=http://127.0.0.1:8090 cargo test --test
  convert` runs a real Word file through it (skipped without the variable).
- Expertise documents get ids from the service (`d_…`), never from the host. A new version
  finds the pages of earlier ones through the raw records they cite (`sources`), not by
  similarity. Code never interprets a document's structure; the model reads it from the top
  like a person, one concept per step (design.md "Reading like a person"); pages are the
  librarian's condensed account, never copies of the source.
- Write multi-line or backslash-containing edits with the Write/Edit tools or a script file:
  Bash heredocs here turn `\n` in Python strings into real line breaks.
- Every page has one audience — tenant, group or user — given by its folder (`groups/g_…`,
  `users/u_…`). `Kind::audiences()` and the path layout must agree. A page may anchor only to
  pages and nodes everyone who can read it can see.
- MCP tools (`src/mcp.rs`) call the HTTP handlers they stand for; their descriptions live in
  `prompts/tools/` and teach the retrieval strategy. Keep their parameters few; `session` comes
  back from `search` and is passed on (MCP 2026-07-28 has no protocol sessions; an older
  client's `Mcp-Session-Id` stands in). Trying them with a real agent: Pi (`pi`, with
  `pi-mcp-extension`, `.pi/mcp.json` naming `/mcp` and the key/user headers), run with
  `< /dev/null` in `-p` mode or it waits for stdin.
- `/search` is the only search entry. `/graph`, `/graph/path` and `/read` only take ids that `/search` returns.
- No LLM calls on the read path (`/search`, `/graph`, `/read`, `/pages`).
- Searches read only the in-memory copies; never query SQLite or Neo4j on the read path.
- Storage sits behind the `Store` and `GraphStore` traits, from M1 on.
- Every Neo4j node and edge carries the tenant id, and every Cypher query filters on it.
- Every search stage gets a tracing span with its timing.
- Languages are Chinese and English only. Embeddings use jina-embeddings-v2-base-zh (`src/embed.rs`,
  fixed revision, no prefixes), chosen on the test set (design.md "As built in M5"). Page vectors
  are built when the snapshot is (`search::vectors`) and kept in SQLite by page, heading and text
  hash. Tests use `common::embed::Fake`; the real model runs with
  `PINAKES_TEST_EMBEDDING=1 cargo test --test meaning_testset -- --nocapture` (downloads it into
  `target/models` the first time; skipped without the variable).
- The librarian never writes graph data, and every memory page has at least one anchor.
- A graph schema backs many graph spaces. Only creating or changing a schema writes the wiki (via
  the librarian); creating a space or writing nodes and edges never does. Structural schema fields
  come from the stored definition, never from the LLM. Node names and aliases reach the name
  matcher from the graph data.
- Entity references: `schema:type` for a node type, `space:type/key` for a node. Space names are
  unique in a tenant and never equal a schema name.
- Agents change graph data only by proposing (`POST /graph/draft`, MCP `propose_graph_change`):
  one pending draft per space, every change checked against the graph as the draft would leave
  it, applied whole only when the admin approves (`graph::draft`, `GraphStore::apply`). Graph
  data never touches the wiki.
- Graph spaces are shared (default), private (owner only) or restricted (member users and groups).
  Compute the caller's visible spaces once per request and drop everything from other spaces
  before responding; edges never cross spaces; hidden nodes are 404.
- Tenant, group and user folder names come from the id-mapping functions
  (`tenants::tenant_dir_name`, `group_dir_name`, `user_dir_name`), never from raw ids.
- A page another user or group owns, or another tenant's anything, is reported as not found
  (404), never as forbidden, so callers can't probe for existence.

- Lint never writes wiki pages (only its line in `log.md`): it reports findings and holds fixes
  as proposals (`librarian::lint`). A finding and its fix stay within the audience of the page
  (or the searches) it comes from.
  Deletion is hard (`deletion`, `Tenants::delete`): a deleted owner's text must be gone from
  every file of the tenant except the search traces (`tests/deletion.rs` checks byte by byte);
  writers take `Tenant::lock_wiki`/`lock_graph`, which refuse once the tenant is deleted.

- Parallel computing goes through `par` (one shared pool sized by `[server] threads`, 4 by
  default, fewer on a smaller machine); never spawn compute threads or size work by
  `available_parallelism` elsewhere. The embedding model and the async runtime use the same number.

- Acquisition (M9, `src/acquisition/`, the librarian's part in `librarian::acquire`) follows a
  lint run only when the deployment (`[acquisition]`) and the tenant (`PUT
  /admin/tenants/{id}/acquisition`) both turn it on. Only the tenant's allowlisted sites are
  ever read (a crawl into a local index: robots.txt, sitemaps, same-site links; no text kept);
  no search engine or other third party is asked anything, and gaps are matched on the
  service. Nothing it finds enters the wiki without an admin's approval: a candidate is a held
  proposal of a source, filed through the normal expertise ingest when approved (an upload with
  one review step more). Fetched text is shown to the model only between markers as data, and
  screened before anyone is asked; fetches reach only allowlisted, public hosts (redirects
  included). Tests use a local stand-in for the trusted site (`tests/acquisition.rs`); the real
  model's screening: `PINAKES_TEST_LLM=1 cargo test --test acquisition_live -- --nocapture`;
  reading a real site: `PINAKES_TEST_CRAWL=<domain> PINAKES_TEST_CONVERTER=http://127.0.0.1:8090
  cargo test --test crawl_live -- --nocapture`.
- Large tenants' page vectors get a graph index per search slot (`search::hnsw`, our own HNSW
  over the snapshot's int8 rows; `index/vectors-<slot>.hnsw` holds only lists and 64-bit keys),
  from `[search] vector_graph_rows` rows on; it follows every snapshot update, deletions
  included. `PINAKES_TEST_GRAPH_ROWS=1` forces it in `meaning_testset`.

- The whole stack: `docker compose up -d --build` (needs `model.conf`;
  the image's config is `docker/config.toml`, kept in step with `config.toml`). The test chat
  (`/dev`) exists only with `PINAKES_DEV=1`.

## Layout

Single package: `src/lib.rs` holds the modules, `src/main.rs` only reads config and starts the
server, `src/bin/pinakes-replay.rs` replays search traces, and `tests/` holds HTTP-level integration
tests that drive the axum `Router` directly (shared helpers in `tests/common/`).
