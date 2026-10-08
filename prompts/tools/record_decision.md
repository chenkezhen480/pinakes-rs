Records a decision the user made, so later tasks follow it. Call it when the user decides how
something should be done ("refund shipping on partial refunds for repeat customers"), not for
your own intermediate choices.

- `topic`: what the decision is about, short ("refund policy").
- `decision`: what was decided, in the user's terms, with exact numbers and conditions.
- `context`: optional: the situation that led to it.
- `outcome`: optional: what came of it, if known.
- `entities`: optional ids of graph nodes or types it concerned, as `search` returned them.
- `session`: the task's session, so the decision is linked to the knowledge you read.

It is filed for the user by default, in the background.
