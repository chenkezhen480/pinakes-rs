---
kind: experience
title: Resolving offline sync conflicts
aliases: [sync conflict, offline edits, 同步冲突, 离线编辑, conflicting copy]
keywords: [sync conflict, offline, conflicting copy, merge edits, 同步失败, 冲突副本]
summary: Conflicting copies appear when two people edit the same block offline; show the user the conflict banner and the version history instead of promising automatic merges.
about: [expertise/sync-engine]
sources: [2026-09-13-task-2788.json]
updated: 2026-09-13
---

# Resolving offline sync conflicts

## Steps that work

1. Ask which device was offline and for how long; conflicts need edits to the same block.
2. Show the user the conflict banner at the top of the note: it links both versions.
3. For anything lost, open version history on the note and restore the block from the other
   version.
4. Suggest turning on "keep notes available offline" only for the notes they actually travel with.

## Pitfalls

- Telling users "we merge everything automatically": text in the same block is not merged.
  Seen 4 times.
- Clearing the app cache before the device has synced deletes the offline edits. Seen 2 times.
