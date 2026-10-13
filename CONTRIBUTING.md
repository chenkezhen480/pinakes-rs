# Contributing to Pinakes

Thank you for helping. This page says how the project is built and checked, and the rules a
change has to keep. 中文读者：本页说明构建、测试与协作规范，欢迎用中文提 Issue 和 PR。

## Ways to help

- **Report a bug** or **ask for a feature** with the issue forms. For a security problem, don't
  open an issue: follow [SECURITY.md](SECURITY.md).
- **Improve the test set** (`testset/`): real questions from real use are what search is
  measured against.
- **Send a pull request.** For anything larger than a fix, open an issue first, so the change can
  be agreed on against the design before you write it.

## Design first

[docs/guide.md](docs/guide.md) describes what each part does and what each interface takes, and
the rules below are the ones the code keeps. A change that alters behavior updates the guide in
the same pull request; a change of configuration regenerates
[docs/configuration.md](docs/configuration.md).

## Setting up

- **Rust 1.98 or later** (edition 2024): `rustup update stable`.
- **Optional services**, each only for the tests that need it (they are skipped otherwise):
  - Neo4j, for the Neo4j graph backend:
    `NEO4J_PASSWORD=<pw> docker compose --profile neo4j up -d neo4j`
  - the optional MarkItDown converter (PDF, `.doc`, `.ppt`; the service reads the other
    formats itself): `docker compose --profile dev up -d --build converter-dev`
    (listens on 127.0.0.1:8090)
- **Node 24**, only to change the web console (`ui/`): `npm ci` there. Building and running the
  service never needs it; the built console in `ui/dist/` is committed.
- **The librarian's model**: copy `model.conf.example` to `model.conf` and fill it in. It holds
  a key, so it is git-ignored: never commit it.

## Checks before a pull request

```bash
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test -j 2 --no-fail-fast
```

With a change to the console (`ui/`), also:

```bash
cd ui
npm run check                 # svelte-check and TypeScript, no errors or warnings
npm run build                 # into dist/, within the size budget; commit dist/ as built
npx playwright test           # the browser tests (starts a seeded test service on 127.0.0.1:8097)
```

The console's API types are generated from the Rust structs: after changing a handler's request
or response, `PINAKES_WRITE_TYPES=1 cargo test --lib api::types`. The configuration reference
(`docs/configuration.md`) is generated too: `PINAKES_WRITE_REFERENCE=1 cargo test --lib settings`.

`-j 2` keeps the test build within a 16 GB machine's memory. The files of `tests/` are modules of
one test program, `integration` (listed in `tests/main.rs`; add a `mod` line for a new file), and
one file's tests run with its name as the filter: `cargo test --test integration lint::`. CI runs the same three. Tests that
need a service or cost money run only when asked:

| Variable | Runs |
|---|---|
| `PINAKES_TEST_NEO4J_URI=bolt://127.0.0.1:7687 PINAKES_TEST_NEO4J_PASSWORD=<pw>` | graph tests against Neo4j |
| `PINAKES_TEST_CONVERTER=http://127.0.0.1:8090` | a PDF through MarkItDown, and every fixture converted by both converters and compared |
| `PINAKES_TEST_EMBEDDING=1` | the real embedding model on the test set (downloads it once) |
| `PINAKES_TEST_LLM=1` | the librarian and acquisition against a real model (costs money) |
| `PINAKES_TEST_CRAWL=<domain>` | reading a real site |
| `PINAKES_BENCH_PAGES=1000,10000` with `--release --test integration bench:: -- --ignored` | the speed benchmark |

## Rules a change keeps

These are the project's invariants; reviews check them.

- **Isolation.** Every read and write takes a `Scope` (tenant, user, groups) from the
  credentials, never from a request body. Another user's, group's or tenant's data is reported
  as not found (404), never as forbidden.
- **The read path calls no LLM and queries no database**: `/search`, `/graph`, `/read` and
  `/pages` work on the in-memory snapshot. Every search stage has a tracing span with its timing.
- **The librarian writes the wiki; code writes the structure.** Every LLM reply is constrained
  by a strict JSON schema. Code, never the model, writes `kind`, `updated` and `sources`, and
  every wiki write runs on the tenant's write queue. The librarian never writes graph data.
- **Untrusted input stays data.** Uploaded files and fetched pages are shown to the model only
  as quoted data; fetches reach only allowlisted, public hosts.
- **Deletion is hard**: a deleted owner's text is gone from every file of the tenant (the
  deletion tests check byte by byte).
- **Storage sits behind traits** (`Store`, `GraphStore`); parallel work goes through the shared
  compute pool (`par`).
- **Languages are Chinese and English.** Code, comments and docs are in English; issues, pull
  requests and commit messages may be in Chinese or English.
- **Retries stop at three**, then fail with a message that says what to do.

## Pull requests

- One topic per pull request, with tests for what it changes and the guide updated.
- The description says what changed, why, and how it was checked (which of the optional tests
  ran, if any).
- Keep commits readable; they are squashed or kept as they are on merge, at the maintainers'
  choice.
- By contributing you agree that your contribution is licensed as the project is, under MIT OR
  Apache-2.0 (see [README.md](README.md#license)).

## Code of Conduct

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
