---
kind: expertise
title: How sync works
aliases: [sync engine, 同步机制, offline mode, 离线模式, CRDT]
keywords: [CRDT, block-level merge, conflicting copy, offline cache, 块级合并, 冲突]
summary: Notes sync per block with a CRDT; edits to different blocks always merge, but concurrent offline edits to the same text block produce a conflicting copy.
about: [accounts:workspace]
sources: [engineering-blog-sync-2026.md]
updated: 2026-05-12
---

# How sync works

Each note is split into blocks (paragraphs, list items, tables). Blocks sync independently using
a CRDT, so two people editing different blocks never conflict.

## Offline

Notes marked "available offline" are cached on the device. Edits made offline are queued and sent
when the device reconnects.

## Conflicts

When two devices edit the **same text block** while at least one is offline, both versions are
kept: the note shows a conflict banner linking the conflicting copy. Lists and tables merge
item by item; plain text inside one block does not.
