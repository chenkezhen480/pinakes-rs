Records a decision the user or their team made, or a lesson worth keeping (what went wrong and
why, a fix or workaround, a step the knowledge base didn't describe), so later tasks follow it.
One call may hold both: a lesson and the decision taken because of it. Call it without being
asked whenever the user makes or reports one ("we decided…", "定了：…", "the login loop was the
customer's clock being off", "记一下这次的教训"), even in a message that also asks something, and
after a task of your own that failed, hit a pitfall or needed a new approach. Not for your own
intermediate choices, options still under discussion, hypotheticals, routine successes, or what
the knowledge base already says.

- `topic`: what it is about, short and general ("refund policy", "partial refund after shipment").
- `content`: what was decided and what was learned, in the user's terms, with the exact numbers,
  names and conditions.
- `context`: optional: the situation that led to it, and what came of it.
- `entities`: optional ids of graph nodes or types it concerned, as `search` returned them.
- `session`: the task's session, so it is linked to the knowledge you read.

A librarian files it in the background: a decision for the user, a lesson for the team.
