Pinakes is this team's knowledge base: a wiki kept by a librarian, plus a business graph. It holds
memories (experience from past tasks, users' decisions and preferences), knowledge (expertise and
the graph's schema descriptions) and relations (live graph data: orders, customers, machines…).

How to use it:

1. Start every lookup with `search`. One call returns memories, knowledge and relations in separate
   slots, each hit with a short summary and a route. Search before answering anything about this
   team's work, its domain, its data or the user's own preferences.
2. Read the slots first. Open in full only the hits that look promising: follow each hit's route,
   `read` for a page, `graph` or `path` for a node. Never call those with ids you made up; they only
   take ids that `search` (or an earlier follow-up) returned.
3. A slot marked `weak`, or keywords listed in `unmatched_keywords`, mean the search missed. Retry
   with other terms (synonyms, the other language, an identifier), or with `mode: "semantic"`.
4. Every `search` result carries a `session`. Pass it to every later call of the same task that
   takes one (`read`, `graph`, `path`, `record_experience`, `record_decision`): pages already read
   are then not sent twice, and what you record is linked to the knowledge you read.
5. Record what was learned: `record_experience` after finishing a task (especially one that failed,
   or that needed a new approach), `record_decision` when the user makes a decision,
   `record_preference` when the user states a preference. Recording is filed in the background; it
   is not searchable at once.

You choose the order of calls and when you have enough. Nothing here calls an LLM on the read
path: searches take milliseconds, so searching again is cheap.
