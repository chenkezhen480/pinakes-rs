Proposes changes to the data of a graph space (its nodes and edges) for a person to review.
Nothing changes in the graph until the change is approved; `search`, `graph` and `path` keep
showing the graph as it is. Where the deployment approves everything at once, the answer says
what was applied (`applied`) and the graph has changed already.

Each space has at most one pending draft. Your changes are added to it, and are checked against
the space as the draft would leave it: you may link, rename or take back a node the draft adds,
even one another call added. Call it with only `space` (no `changes`) to see the pending draft
before you change it, and build on what is already there instead of proposing it again. The
answer is always the whole draft: the nodes it puts, the nodes it removes (with their edges), the
edges it adds and removes, and why each call changed it.

Changes, applied in order:

- `put_node`: adds a node, or changes one. Only what you give changes: `name`, `aliases` (the
  whole list), `properties` (merged in by name; `null` takes one out). A new node's name is its
  key unless you give one. Properties are the ones its type declares in the schema.
- `remove_node`: removes a node and every edge of it.
- `add_edge` / `remove_edge`: `from` and `to` as `type/key` within the space, `type` the
  relation; both ends must exist once the draft is applied.
- `delete_space` (no other fields): deletes the whole space, with every node and edge in it, once
  approved. The draft then holds only that; other changes wait until it is approved or rejected.

Node and relation types, and which types a relation joins, are the space's schema's (`search`
shows them, and `graph` shows a node's type and key). Give a `reason` that says, for the reviewer,
where the change comes from. Use node ids that `search` or `graph` returned; don't guess keys of
existing nodes. Schemas themselves can't be changed here.
