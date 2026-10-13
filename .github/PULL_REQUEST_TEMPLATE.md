## What and why / 改了什么、为什么

<!-- The change, the reason, and the issue it closes (Closes #…). -->

## How it was checked / 如何验证

- [ ] `cargo fmt`, `cargo clippy --all-targets -- -D warnings` and `cargo test -j 2` pass
- [ ] Tests cover the change
- [ ] Optional tests run, if they apply (Neo4j, converter, embedding, live model): which ones:
- [ ] `docs/guide.md` updated where behavior changed (and `CLAUDE.md` if a rule changed)
- [ ] No keys, `model.conf` or real tenant data in the change
