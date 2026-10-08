Records the outcome of a task you finished, so the next agent doing this kind of task finds it.
Call it after a task, above all when it failed, hit a pitfall, or needed an approach the knowledge
base didn't already describe. Don't record routine successes that taught nothing new.

- `task`: the kind of task, short and general ("partial refund after shipment"), not this one
  instance.
- `outcome`: what happened and what worked or didn't, with the exact numbers, names and conditions.
- `details`: optional steps, pitfalls and the reasons behind them.
- `entities`: optional ids of graph nodes or types it concerned, as `search` returned them.
- `session`: the task's session, so the experience is linked to the knowledge you read for it.

A librarian files it into the wiki in the background (it may merge it into an existing page);
it becomes searchable when that job finishes.
