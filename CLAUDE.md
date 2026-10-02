# Pinakes (crate `pinakes`)

A self-hosted knowledge service in Rust. The full design is in `docs/design.md`
(the original is `docs/Design.docx`). Read the relevant section before starting a milestone.

## Working agreement

- Build one milestone at a time (M1…M9 in "Build milestones"). Each milestone ends with its
  "done when" check, backed by tests.
- Reply to the user in Chinese. Code, comments, commit messages and docs stay in English.
- Before calling a task done, run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- On this Windows machine cargo lives in `%USERPROFILE%\.cargo\bin`; in Git Bash prefix commands
  with `export PATH="$HOME/.cargo/bin:$PATH";` if `cargo` is not found.
- The shell has `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` pointing at a local proxy (127.0.0.1:7897).
  When curling the local server, first `unset HTTP_PROXY HTTPS_PROXY ALL_PROXY; export NO_PROXY=127.0.0.1,localhost`.
- Graph tests against Neo4j (M3 on): `NEO4J_PASSWORD=... docker compose up -d neo4j`, then
  `PINAKES_TEST_NEO4J_URI=bolt://127.0.0.1:7687 PINAKES_TEST_NEO4J_PASSWORD=... cargo test --test graph_testset`.
  Without `PINAKES_TEST_NEO4J_URI` that test is skipped; the SQLite backend always runs.
- Run the server bound to `127.0.0.1` locally; binding `0.0.0.0` pops a Windows Firewall dialog.
  Start it with `run_in_background`, never with `&` inside a foreground command (that hangs the tool).

## Rules

- Every read and write path takes a `Scope` (tenant + user + groups); never take tenant, user or
  group ids from request bodies. Groups arrive in the `X-User-Groups` header (API key mode).
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
- `/search` is the only search entry. `/graph`, `/graph/path` and `/read` only take ids that `/search` returns.
- No LLM calls on the read path (`/search`, `/graph`, `/read`, `/pages`).
- Searches read only the in-memory copies; never query SQLite or Neo4j on the read path.
- Storage sits behind the `Store` and `GraphStore` traits, from M1 on.
- Every Neo4j node and edge carries the tenant id, and every Cypher query filters on it.
- Every search stage gets a tracing span with its timing.
- Embeddings use the e5 prefixes: `"query: "` for queries, `"passage: "` for stored text.
- The librarian never writes graph data, and every memory page has at least one anchor.
- A graph schema backs many graph spaces. Only creating or changing a schema writes the wiki (via
  the librarian); creating a space or writing nodes and edges never does. Structural schema fields
  come from the stored definition, never from the LLM. Node names and aliases reach the name
  matcher from the graph data.
- Entity references: `schema:type` for a node type, `space:type/key` for a node. Space names are
  unique in a tenant and never equal a schema name.
- Graph spaces are shared (default), private (owner only) or restricted (member users and groups).
  Compute the caller's visible spaces once per request and drop everything from other spaces
  before responding; edges never cross spaces; hidden nodes are 404.
- Tenant, group and user folder names come from the id-mapping functions
  (`tenants::tenant_dir_name`, `group_dir_name`, `user_dir_name`), never from raw ids.
- A page another user or group owns, or another tenant's anything, is reported as not found
  (404), never as forbidden, so callers can't probe for existence.

## Layout

Single package: `src/lib.rs` holds the modules, `src/main.rs` only reads config and starts the
server, `src/bin/pinakes-replay.rs` replays search traces, and `tests/` holds HTTP-level integration
tests that drive the axum `Router` directly (shared helpers in `tests/common/`).
