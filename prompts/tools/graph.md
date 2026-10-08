Walks the business graph from a node that `search` returned (a node id such as
`commerce:order/1042`, from `entities`, an edge's `from` or `to`, or a hit's `route.graph`).
Returns the nodes around it with typed edges, and the wiki pages anchored to them.

Start with `depth` 1 (the default); go deeper only when the answer is further away (3 at most,
unless the service allows more). Narrow the walk with `relations` (relation types such as
`placed_by`) when you know which connection you need. Pass the task's `session`.
