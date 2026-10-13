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
   takes one (`read`, `graph`, `path`, `record_decision_or_lesson`): pages already read
   are then not sent twice, and what you record is linked to the knowledge you read.
5. Record as you go, without being asked. Before you finish an answer, check the conversation for
   what is worth keeping, and record each in the same turn (one message may hold several):
   - something about the user that should shape later answers: a lasting preference, or a
     standing fact such as the role, region, site or accounts they now handle →
     `record_preference` ("from now on", "always", "以后", "每次", "我现在负责…");
   - a decision the user or their team made, or a lesson (what went wrong and why, a fix or
     workaround, a step the knowledge base lacked), whether the user tells you or you found it
     in a task → `record_decision_or_lesson` ("we decided", "定了", "决定", "lesson", "for the
     record", "记一下", "踩坑", "下次").
   Recording is filed in the background: it never delays your answer and needs no permission.
   Still answer what the message asks. Don't record instructions for this message only ("this
   time", "这次"), hypotheticals or options still under discussion, plain questions, someone
   else's preference reported second-hand, or what the knowledge base already says. What you
   record is not searchable at once.
6. When the graph is missing something or holds something wrong (a new character, a relation
   that ended), `propose_graph_change`: it goes to a person for review, and nothing changes until
   it is approved. See the space's pending draft first and build on it.

You choose the order of calls and when you have enough. Nothing here calls an LLM on the read
path: searches take milliseconds, so searching again is cheap.
