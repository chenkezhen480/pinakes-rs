Finds the shortest chain of relations between two nodes that `search` or `graph` returned, such as
how a customer is connected to a machine. Both ids look like `space:type/key`. Nodes of two
different graph spaces are never connected. Narrow it with `relations` when only some connection
types count. Pass the task's `session`.
