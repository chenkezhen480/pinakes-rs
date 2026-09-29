# cyrene-memory (crate `kb`)

A self-hosted knowledge service in Rust. The full design is in `docs/design.md`
(the original is `docs/Design.docx`). Read the relevant section before starting a milestone.

## Working agreement

- Build one milestone at a time (M1…M9 in "Build milestones"). Each milestone ends with its
  "done when" check, backed by tests.
- Reply to the user in Chinese. Code, comments, commit messages and docs stay in English.
- Before calling a task done, run: `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `cargo test`.
- On this Windows machine cargo lives in `%USERPROFILE%\.cargo\bin`; in Git Bash prefix commands
  with `export PATH="$HOME/.cargo/bin:$PATH";` if `cargo` is not found.

## Rules

- Every read and write path takes a `Scope` (tenant + user); never take tenant or user ids from request bodies.
- `/search` is the only search entry. `/graph`, `/graph/path` and `/read` only take ids that `/search` returns.
- No LLM calls on the read path (`/search`, `/graph`, `/read`, `/pages`).
- Searches read only the in-memory copies; never query SQLite or Neo4j on the read path.
- Storage sits behind the `Store` and `GraphStore` traits, from M1 on.
- Every Neo4j node and edge carries the tenant id, and every Cypher query filters on it.
- Every search stage gets a tracing span with its timing.
- Embeddings use the e5 prefixes: `"query: "` for queries, `"passage: "` for stored text.
- The librarian never writes graph data, and every memory page has at least one anchor.
- Tenant and user folder names come from the id-mapping function (`tenants::tenant_dir_name`,
  `tenants::user_dir_name`), never from raw ids.
- A page another user owns, or another tenant's anything, is reported as not found (404), never
  as forbidden, so callers can't probe for existence.

## Layout

Single package: `src/lib.rs` holds the modules, `src/main.rs` only reads config and starts the
server, and `tests/` holds HTTP-level integration tests that drive the axum `Router` directly.
