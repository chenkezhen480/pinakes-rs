# Knowledge Base Service in Rust — Design

Sep 29, 2026 · @Banks Jorge

## What we're building

We're building an open-source, self-hosted knowledge service in Rust. It works like a library: the wiki is the collection, and an LLM acts as the librarian that files and summarizes everything that comes in. Agents search it in milliseconds, and no LLM is involved when they read.

- **One way in: the wiki.** Every search goes through a single entry, and that entry is the wiki. One query brings back related memories, related knowledge and entity relations together, plus further knowledge reached through those memories.
- **Five ways to add knowledge.** There is one insert entry point per kind of knowledge, each with its own rules. Searching stays in one place; adding is split by kind.
- **The librarian keeps it current.** When new content arrives, the librarian finds the related pages and updates them, instead of just adding another document. An LLM verifies every change to the wiki.
- **Five kinds of knowledge.** Agent experience, user decision history, vertical expertise, the business graph and user preferences. Each has its own scope and page shape.
- **A graph described by its schema.** A schema defines node and relation types; any number of graph spaces are built from one schema. The wiki holds one description per schema. Spaces and their nodes and edges live in Neo4j as data, never in the wiki, so relationship questions get real edges, not summaries.
- **The agent decides how to search.** Following A-RAG, the agent chooses keyword or meaning-based search, what to open and what to follow. The service returns signals, not decisions.
- **Isolated by tenant, group and user.** A tenant can never see another tenant's knowledge; group-level knowledge stays within the group, and user-level knowledge with that user. The same rule covers wiki pages and graph spaces.
- **Identity comes from outside.** The service doesn't manage accounts. The system that embeds it passes tenant, user and group ids in, so developers can plug it into their own stack.
- **Ultra-fast reads.** Reads are lookups, keyword search, graph walks and, when the agent asks, a vector search, all over data in memory, with no LLM. The target is a few milliseconds of service time and around 20 ms end to end.
- **English and Chinese from day one.**
- **Three interfaces.** There's an HTTP API to manage the knowledge, an MCP server for agents, and a streaming chat endpoint for testing.

The service returns knowledge, not answers; writing the answer is the calling agent's job. Version 1 leaves out a hosted offering, user account management, spreading one tenant across several machines, and every search entry point other than the wiki. Those other entry points come later, as experiments measured against the wiki-only design.

The thesis research built on this design is in the Research plan tab.

## The core idea: a library with a librarian

The expensive thinking happens once, when content is added, so reads only have to look things up. This is the LLM wiki pattern Andrej Karpathy described in an April 2026 [gist](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f).

The pattern has three layers, and this service adds a fourth for graph data:

| Layer | In this service | Who writes it |
|---|---|---|
| Raw sources | Task records, decision records, documents, schema definitions and preference notes; never modified | The systems that call the service, through the five insert entry points |
| Wiki | Markdown pages for each tenant and user, including one description per graph schema, plus `index.md` (a catalog) and `log.md` (a history) | The librarian |
| Graph data | Graph spaces and their nodes and edges, checked against the space's schema | The host system; no LLM involved |
| Rules | One rules file per knowledge kind, covering page shape, what to merge and what to flag | Shipped with the project; tenants can override them |

It also has three operations:

- **Ingest.** The librarian folds new content into existing pages, or creates new pages. A new or changed graph schema is described by the librarian; graph spaces and graph data skip the librarian and are checked by code against their schema.
- **Query.** Agents search through the wiki, the only entry, then open the pages, memories and graph edges it leads to, with no LLM involved.
- **Lint.** The librarian periodically checks for contradictions, stale claims, duplicate pages, broken links and memories that aren't anchored to anything.

**How this differs from classic RAG.** Classic RAG searches raw text chunks and asks an LLM to assemble an answer on every question. Here, the synthesis is already written into the pages, so a search returns an answer-shaped page instead of fragments. That's what makes a fast read path with no LLM possible. Vectors still have a place, but as one way into the wiki that the agent chooses, not as the whole method.

## Five kinds of knowledge

Each kind has its own insert entry point, scope and page shape. All five are searched through the same single entry, the wiki, and come back in one of three slots: memories, knowledge or relations.

| Kind | Insert entry point | Scope | Written from | Page shape | Comes back in |
|---|---|---|---|---|---|
| Agent experience | `/ingest/experience` | Tenant or group | Agent task outcomes, including problems agents hit while serving users | One playbook per task type: steps that work, pitfalls and their fixes, how often each was seen | Memories |
| User decision history | `/ingest/decision` | User or group | Records of what the user decided and what happened as a result | One page per decision topic: the decision, its context, the outcome, the date | Memories, filtered to the caller's user and groups |
| Vertical expertise | `/ingest/expertise` | Tenant or group | Domain documents and manuals | Linked concept and entity pages, as in a classic wiki | Knowledge |
| Business graph | `/ingest/graph/schema`, `/space`, `/data` | Tenant; many graph spaces per schema | Schema definitions, plus spaces, nodes and edges as data | One description per schema; spaces, nodes and edges are data, not pages | Knowledge (the description) and relations (the edges) |
| Preferences | `/ingest/preference` | User | Explicit statements and observed habits | One short profile page per user | Memories, filtered to the user |

Problems users run into are recorded as agent experience, because they are lessons about how agents should operate. User decision history records choices and their outcomes, not problems.

Preferences are scoped per tenant, so the same person in two tenants has two separate profiles.

**Audiences.** Every page is for one audience: the whole tenant, one group, or one user. A group page (a team's playbook, a decision the risk team made together) is visible to every caller the host sends with that group. Each kind allows only some audiences: experience and expertise are for the tenant or a group, decisions for a group or a user, preferences only for a user. The page's folder says its audience (`groups/g_…/`, `users/u_…/`), and the path layout enforces the rule. On insert, the caller picks the audience from what its own credentials allow: the tenant, one of its groups, or its user; it can never write for a group it isn't in.

## One way in: the wiki as the only search entry

Every read starts with one `search` call, and the wiki is where it enters. The upstream agent wraps the user's request into a query with meaning, and the service turns that one query into memories, knowledge and relations together. Version 1 has no other search entry on purpose: running the wiki as the only way in is the primary experiment of the thesis.

**Why one entry.** A single query can bring back related memories and related knowledge at once, then more knowledge reached through those memories. Separate entry points per kind would make the agent guess where the answer lives, and would lose the links between kinds.

**The catch: prose can hide the graph.** If the entry only ranks page text, a relationship question returns knowledge pages that happen to share its keywords, and the actual edges never appear. Knowledge pages are long and full of keywords, while relationships live in graph data that a text index barely sees. So the design keeps one entry but stops any single signal from deciding everything.

**How one search runs**

1. **Match names.** Query terms are matched against every alias in the wiki's catalog, in English and Chinese, with a multi-pattern matcher. This gives the starting entities. It's exact and cheap, and prose can't outrank it.
2. **Search three channels in parallel.**
   - Memories: experience, decisions and preferences, filtered to the user.
   - Knowledge: expertise pages and schema descriptions.
   - Relations: relation types matched in the schema descriptions, then the graph edges among the starting entities; with two or more entities, the paths between them too.
3. **Lift text hits to entities.** Each memory or knowledge hit adds the entities it's anchored to. Even when a prose page wins the ranking, its entities still reach the graph.
4. **Hop from memories to knowledge.** The entities anchored by the top 3 memories bring in their one-hop knowledge pages and edges. These score lower and are marked `via: memory`, so the hop can't drift far.
5. **Assemble by slot.** Each channel keeps its own ranked slot in the response, with its own `top_k` and coverage flag.

![search pipeline · one entry, three channels, three slots](images/image1.png)

search pipeline · one entry, three channels, three slots

Names and text both lead into the three channels, so a relationship question reaches the graph even when prose shares its keywords.

**Slots, not one ranked list.** Scores from different channels are never compared, so prose can't crowd out the graph. A slot with nothing good stays empty rather than padded. Web search engines do the same thing: one search box, with separate blocks for news, images and maps.

**Intent changes the budget, not the entry.** Relationship cue words, or two or more matched entities, give the relations slot more room and turn on the path search. Cue words include "relationship", "depends on", "between", "upstream", 关系, 关联, 依赖, 上下游 and 影响. The agent can also pass `entities` or `intent` hints in the same call.

**Every hit carries a route.** A hit says where to go next: a Markdown file to `read` (a page, one heading of it, or a raw source it cites), or a node to open with `graph` or `path`. These follow-up calls only take ids that `search` returned, so they are steps after the entry, not other ways in.

**Later entry points.** Direct graph queries, search over raw chunks and per-kind search tools are left out of version 1. Once the wiki-only design is measured, they are opened one at a time and compared against it on the same test set.

## The business graph: a schema description plus live data

A **schema** defines a set of node types and relation types. A **graph space** is one body of data built from a schema: the same `commerce` schema can back a `mainland` space and a `crossborder` space, with the same structure and separate data. The wiki holds one description per schema and nothing about individual spaces, nodes or changes. Spaces, nodes and edges are stored in Neo4j, checked against their schema, and copied into memory so searches can walk them.

**Schemas and graph spaces.** Three separate operations, and only the first touches the wiki:

| Operation | Endpoint | What happens | LLM | Wiki |
|---|---|---|---|---|
| Create or change a schema | `POST /ingest/graph/schema` | Code validates the definition and stores it in SQLite as the authoritative copy; the librarian writes the schema's wiki description, which is verified like any other edit | Yes (background job) | Writes `graph/<schema>.md` |
| Create a graph space | `POST /ingest/graph/space` | Code registers the space under its schema in SQLite and creates the Neo4j constraints and indexes it needs | No | Nothing |
| Write graph data | `POST /ingest/graph/data` | Code checks nodes and edges against the space's schema, writes them to Neo4j, and updates the in-memory graph and the name matcher | No | Nothing |

Code, not the librarian, owns the structural fields of a description (node types, relation types, their ends and keys), copied from the stored definition; the librarian writes what each type means in both languages, the aliases and the summary. So an LLM edit can never change what data is accepted. In version 1 a schema change may only add types, relations and aliases: removing a type or relation that a space still uses, or changing a key, is rejected.

Space names are unique within a tenant and never equal a schema name, so an entity reference is unambiguous: `schema:type` names a node type (`commerce:order`), and `space:type/key` names one node (`mainland:order/1042`). Two spaces of one schema can each hold an order 1042. A space is registered with a display name and aliases (`跨境站`, "cross-border store"), which go into the name matcher like node names, so "跨境站的订单1042" narrows the search to that space; without a space named, searches cover every space the caller can see and each edge says which space it came from.

**Who can see a graph space.** Each space has a visibility, set when it is created:

| Visibility | Who can see it | Typical use |
|---|---|---|
| `shared` (default) | Everyone in the tenant | A product catalog, an org chart, a plant's equipment |
| `private` | Only its owner, the user who created it | A salesperson's own map of prospects |
| `restricted` | Its owner, plus the users and groups on its member list | A fraud investigation only the risk team may see |

- The owner of a private space comes from the credentials of the call that creates it, never from the body. A restricted space lists member user ids and group ids. Groups come from the caller's credentials like the user id (see "Tenants, users and identity"), because the service manages neither accounts nor groups.
- Every request works out once, from its `Scope`, which spaces the caller can see. Name-matcher hits, relation edges, walks, paths and anchors in any other space are dropped before anything is returned, so a hidden space's node names never appear, not even among the matched entities or in `unmatched_keywords` feedback. `/graph` and `/graph/path` on a node in a hidden space return 404.
- Edges never cross spaces, so a shared space can't lead into a private or restricted one.
- Tenant-level pages may anchor only to node types and to nodes in shared spaces; a user's own pages may also anchor to nodes in spaces that user can see. The librarian is only shown what the page's scope can see, and lint flags anchors that break this.
- Writing data to a space requires seeing it. Changing a space's visibility or members needs its owner (private) or the admin key.
- Schemas are always tenant-wide: a description holds types, never data, so any space can use any schema.
- Deleting a user deletes their private spaces and removes them from member lists.

**What the description holds.** The node types and relation types of the schema, what each one means in English and Chinese, a few aliases, and the key fields. It changes only when the schema itself changes.

```yaml
---
kind: graph
schema: commerce
summary: Orders and the customers who place them.
nodes:
  order:    { en: "A customer order", zh: "客户订单", aliases: [orders, 订单], key: order_id }
  customer: { en: "A buyer account",  zh: "买家账户", aliases: [customers, 客户], key: customer_id }
relations:
  placed_by:
    from: order
    to: customer
    via: customer_id
    en: "The customer who placed the order"
    zh: "下单的客户"
    aliases: [ordered by, buyer, 下单人, 所属客户]
updated: 2026-09-29
---
```

**Relation types are searchable; edges are not summarized.** Each relation type in a description becomes one entry in a small index of its own: its name, both descriptions, its aliases and the node types it connects. A schema with 40 relation types has 40 entries, shared by all its spaces, and they rarely change. A matched relation type is looked up in every space built from that schema, and each returned edge names its space. For a question like "which customer placed order 1042" (订单1042的下单客户), "placed" or 下单 matches `placed_by`, the name matcher finds the order, and the graph returns the real edge. The answer always comes from the graph itself.

**Graph spaces and data skip the librarian.** Spaces are created through `/ingest/graph/space`, and nodes and edges arrive through `/ingest/graph/data`. Code checks them against the space's schema (known types, required keys, both ends present in the same space) and writes them to Neo4j. No LLM is involved, and no page is written: the wiki holds only schema descriptions, never spaces or individual nodes. Each node's name and aliases are added to the name matcher as they are written; the same name in two spaces gives two entities.

**Anchors join the wiki to the graph.** Memory and knowledge pages list the entities they're about in their front matter. The lift and the memory-to-knowledge hop follow these anchors, so the librarian must anchor every memory, and lint flags any memory without one.

**In memory.** When a tenant opens, its nodes and edges are read from Neo4j and its page anchors from the wiki, into adjacency lists in both directions built with petgraph. Searches walk that copy and never query Neo4j, so a walk takes microseconds. Walks are breadth-first, with caps on depth (3) and nodes (100).

**As built in M3 (2026-09-30).**

- **Descriptions before the librarian.** Until M4, `/ingest/graph/schema` answers synchronously (200, not a job id), stores the definition first and then has code write `graph/<schema>.md` (if the page write fails, sending the same schema again rewrites it): structural fields from the stored definition, descriptive fields (title, summary, aliases, `en`/`zh`, type aliases) from the request, or else from the page as it stands, so hand-written prose survives a schema change. M4 puts the librarian's description step behind the same endpoint.
- **Stores.** Definitions and the space registry live in the tenant's `kb.sqlite`. Each tenant gets its own `GraphStore` with its tenant id built in. The SQLite backend is `graph.sqlite` in the tenant folder (the default when `[graph]` is absent; the shipped `config.toml` selects Neo4j). Creating a tenant first clears any graph data a deleted tenant of the same id left in Neo4j; if that can't be done, the tenant isn't created. The graph loads on the tenant's first search, like its wiki index; if its stores can't be read (Neo4j down), that search answers from the wiki with an empty relations slot, and for the next 10 seconds requests don't wait on the store again before a new attempt. Startup with the Neo4j backend retries while Neo4j comes up, but a missing password fails at once.
- **Two in-memory copies.** The graph snapshot (petgraph, the graph's own name matcher, the space registry) is separate from the wiki snapshot, so writing nodes never rebuilds the tantivy index; a write swaps in a new graph snapshot and searches finish on the old one. A search merges the hits of both name matchers.
- **How nodes are named.** Node names and aliases always, except a name that is only the key (the default when a node is sent without one), which follows the key rules; keys that look like identifiers (with a digit, `_`, `-` or `.`, such as `SKU-AIR-01`), never plain-word keys like `team`; numeric keys only right next to a name of their type ("order 1042", "order #1042", 订单1042, "1042 order"), which finds every such node in the visible spaces; and an agent keyword that names nothing whole may be part of a node name (冲压线 for "冲压线 L1") when it is specific (three or more Chinese characters, or four or more other characters on word boundaries) and fits at most three nodes. A space's display name and aliases narrow the named nodes to that space. Entity ids the agent passes (`space:type/key`, in `entities`, `keywords` or the query, also inside Chinese text without spaces) are looked up exactly and taken out of the text before names are matched; a `schema:type`-shaped word counts as an id only when that node type exists, so "status:active" stays text; so an id never also names the same key in another space; a named space never drops them; a hidden id behaves exactly like an unknown one. Partial matching applies to graph data only, never to wiki names: pages already get partial overlap from BM25, and a partial name pinning a page would push unrelated pages to the top. Graph nodes have no BM25, and the librarian never writes graph data, so no lint pass can add their aliases; the partial match is the fallback, and each search trace records its keywords and nodes (`partial_names`) so M8's lint can report them to the host as missing aliases (decided with the user 2026-09-30).
- **Ranking in the relations slot.** Edges come in tiers: the shortest path between two named nodes; edges of a named relation type at a named node, and one-hop edges to a node of a named type; two- and three-hop chains to a named type ("which plan is Lumen on": workspace → subscription → plan); other edges of named nodes; and last, edges of lifted nodes, only when they have a named relation type or lead to a named type. The slot is strong when an edge touches a named node, and stays empty rather than padded when nothing is named. A relation intent or two named nodes doubles the slot, unless the caller set its size. `relation_types` lists the relation types the request names; each edge carries its space, both ends' names, the schema's `via` and how it was found.
- **Follow-ups.** `/graph` returns the walked nodes, their edges and the visible pages anchored to them or to the start node's type; `/graph/path` looks for at most 6 hops and never joins two spaces. Unknown and hidden nodes are both 404.
- **Visibility details.** A restricted space is visible to its owner (the user who created it) as well as its members (decided with the user 2026-09-30). `GET /pages/{path}` filters a page's `about` anchors by the same rules as search hits: node anchors only in visible spaces, type anchors only for node types that exist. Space names are unique across the tenant, hidden spaces included, so creating a taken name is a 409; this tells a caller that a name is taken, even by a space they can't see. It can't be avoided while names are unique in a tenant, which `space:type/key` references need. Visibility and members are fixed at creation until an update endpoint comes (not in M3).
- **Neo4j model.** Each node is labelled `KbNode` plus its type, with `tenant`, `space`, `type`, `key`, `name` and `aliases`; each edge is a `KB_REL` relationship with `tenant`, `space` and `type`. Each deployment has its own Neo4j (confirmed with the user 2026-09-30), so `tenant` is the tenant id. Checked on Neo4j 5.26.28 Community: the composite uniqueness constraint on `(tenant, space, type, key)` works there (node key and existence constraints are Enterprise only), plus indexes on the tenant id of nodes and relationships.
- **Traces.** Each search trace records the graph snapshot's version. Graph data has no history, so `kb-replay` always runs on today's graph and reports which replays saw a different one.

**Why a graph still fits A-RAG.** A-RAG's critique of graph-based RAG, such as GraphRAG and HippoRAG2, is that those systems "rely on predefined retrieval algorithms rather than model-driven decisions." Here the graph is reached through the same agent-driven search, and the agent decides whether to follow a graph route.

## Tenants, users and identity

The service never creates or authenticates end users, and never manages groups. The host system passes tenant, user and group ids in, and the service enforces isolation around them. Group membership changes take effect on the next request: when the host stops sending a group, that caller loses access to its pages and spaces at once.

**Where the ids come from.** There are two modes, chosen in config:

| Mode | Tenant id | User id | Fits |
|---|---|---|---|
| API key (default) | Bound to the key; each key belongs to one tenant | The `X-User-Id` header, set by the calling backend | A backend or agent platform calling the service directly |
| JWT | A claim in a token the host signs | A claim in the same token | Setups where end-user tokens reach the service |

Group ids are optional and only matter for restricted graph spaces: in API key mode they come from the `X-User-Groups` header (comma-separated), in JWT mode from a claim in the same token. Ids are opaque strings: the service never reads meaning into them. Header names, claim names and key lists are all set in config, so developers can adapt the service without changing code.

**Personal deployments.** The service targets both shared platforms and one person running it for themselves. `deployment = "personal"` in `[identity]` (decided 2026-09-30) keeps the same model with defaults filled in:

- On first start the service creates the default tenant `000000`, generates its API key, writes the key to `personal-api-key.txt` in the data folder and prints it once. Calls still carry that key; nothing is open without one.
- A request without the user header acts as the default user `admin`. `admin` is an ordinary user id: it grants no admin rights, which still need the admin key.
- There is no default group; a single person needs none.
- Both defaults can be changed in config (`default_tenant`, `default_user`). In the default `multi_tenant` deployment none of this applies: a request without the user header sees tenant-level pages only.

**How isolation is enforced.**

- **One folder per tenant.** Each tenant gets its own folder, holding its wiki files, its SQLite file and its search index.
- **Safe folder names.** Folder names are derived from the ids (a hash), never the raw strings, so an id like `../other` can't escape its folder.
- **Filters the caller can't remove.** The service adds the tenant, group and user filters to every search, graph walk and read, and request bodies can't widen them. Anchors that lead into another user's or group's pages are dropped before a walk starts.
- **One graph database, filtered.** Neo4j Community edition runs a single database, so every node and edge carries the tenant id, with an index on it, and every Cypher query the service sends filters on it.
- **Separate ranking statistics.** Separate indexes mean one tenant's content never shifts another tenant's search results. Inside a tenant, one index holds every audience's pages behind a filter, so another user's or group's pages do count in the term statistics and can nudge scores slightly; the name matcher and the tokenizer's term list, by contrast, only use what the caller may see. Responses never carry scores, so this can't practically be used to probe for hidden content. Separate indexes per group and user would remove it at a large cost; decided 2026-09-30 to accept it.
- **Graph spaces with a visibility.** Shared spaces are open to the whole tenant; private and restricted spaces are filtered out of every search, walk and anchor for callers who may not see them (see "Who can see a graph space").
- **A scoped librarian.** The librarian only ever sees pages the new content's audience may see: for a group page, the tenant's pages and that group's; for a user page, also that user's and their groups'. Another tenant's content never enters a prompt, and content is never merged into a page with a wider audience: a group decision can't end up on a tenant page. A page may anchor only to pages and nodes that everyone who can read it can also see.
- **Clean deletion.** Deleting a tenant deletes its folder and its nodes and edges in Neo4j. Deleting a user removes their decision history and preferences, with the anchors and vectors that came from them. Groups live outside the service, so a group's pages stay until an admin deletes them. Their pages also leave the wiki history kept for replays (see "Search traces and replay").
- **Loaded on demand.** A tenant's index, vectors and graph load on first use, from its files, its SQLite file and Neo4j. Busy tenants stay in memory, and idle ones cost only disk space.

## Search signals: names, keywords and meaning, in English and Chinese

Inside the one entry, three signals find things: names, keywords and meaning. Names and keywords run on every search; meaning runs when the agent asks for it. The semantic work is shared by two LLMs that already exist, one at each end.

- **At write time, the librarian enriches every page.** It adds aliases, keywords in both languages, and the questions the page answers. Matching different phrasings is then solved once per page, not on every query.
- **At read time, the calling agent writes the query.** It wraps the user's request into a query with meaning, plus keywords: canonical terms, exact identifiers, and both English and Chinese forms. If a slot comes back weak, it retries with other terms or switches to meaning-based search.

The next section lists the exact tools and parameters agents use for this.

**Tokenizing Chinese.** Chinese has no spaces between words, so a plain tokenizer finds almost nothing. Each text field is therefore indexed twice, and searches use both versions, with whole words weighted higher. Aliases, node names and domain terms from the wiki are kept whole, so jieba doesn't split them wrongly. All tenants share one base jieba dictionary; each tenant's own terms are found first by its name matcher and emitted as whole words, and only the text between them goes to jieba. So one tenant's terms never change another tenant's tokens, and no tenant needs its own copy of the dictionary:

| Version | How the text is split | Why |
|---|---|---|
| Words | The tenant's known terms kept whole, jieba segmentation (via `jieba-rs`) for the rest of the Chinese text, English words lowercased and stemmed | Precise matches on whole words |
| Bigrams | Overlapping 2-character pieces of Chinese text, English words as-is | Catches domain terms that jieba's dictionary splits wrongly |

**The name matcher.** Every alias and title in the tenant's wiki, and the name and aliases of every graph node, go into an Aho-Corasick automaton, rebuilt when pages or graph data change. It finds all known names in a query in one pass, in either language, in microseconds. Page and type aliases come from page front matter and schema descriptions; instance names (a customer called 林晓, machine ROB-11) come from the graph data, since the wiki holds only schemas.

**Meaning, when the agent asks for it.** Vectors are part of version 1, but only as a mode the agent chooses. Following A-RAG, the service doesn't decide when to use them. It returns coverage per slot and the keywords that matched nothing, and the agent decides whether to retry with `mode: semantic`. In A-RAG's ablation, removing semantic search hurt most on the hardest multi-hop benchmark. Vectors help most with:

- cross-language questions, such as an English query over Chinese pages, without the agent translating its keywords
- "have we seen something like this?" searches over experience and decision memories, where the wording rarely matches but the situation does
- relation types asked about in words no alias covers, such as "who bought this" for `placed_by`

Only wiki content is embedded: page sections, memory pages and relation-type descriptions, never raw sources. The embeddings are computed at write time with a small multilingual model, such as multilingual-e5-small through fastembed, and stored in the tenant's SQLite file (see "Storage layout"). e5 models expect a `query: ` prefix on queries and `passage: ` on stored text; leaving them off quietly hurts results.

The only read-time cost is embedding the query, roughly 5–15 ms on a CPU, to be confirmed by the benchmark. The vector search itself is brute force in memory: about 100,000 vectors take a few milliseconds, and an in-process index such as usearch is added only if one tenant nears a million. For callers that don't decide well, such as weaker models or code, `mode: auto` tries keywords first and falls back to meaning when coverage is weak.

**The test set.** Before any tuning, collect 30–50 real questions per knowledge kind. Tag each with the page, memory or edge that answers it and the slot it should come back in. Include relationship questions whose keywords also appear in knowledge pages, and questions that need a memory plus the knowledge it leads to. Every change to search is measured against this set.

## Retrieval for agents: open parameters, A-RAG style

The service doesn't run a fixed retrieval pipeline. It exposes one entry, `search`, and three follow-ups, and leaves their parameters open, so the calling agent decides how to search, what to open, which connections to follow, and when it has enough.

This follows [A-RAG](https://arxiv.org/abs/2602.03442) (Du et al., February 2026). The paper gives the model keyword search, semantic search and full-text reading as separate tools. It found that this beat fixed pipelines and graph-based RAG, while retrieving a similar or smaller number of tokens.

**One entry, three follow-ups**

| Tool | Role | Returns | Open parameters |
|---|---|---|---|
| `search` | The only entry | Memories, knowledge and relations in separate slots; each hit has a summary, matching lines and a route, never full text | `query`, `keywords`, `mode`, `entities`, `intent`, `slots`, `hop`, `session` |
| `graph` | Follow-up | The nodes and pages around a node, with typed edges | `node`, `depth`, `direction`, `relations`, `max_nodes`, `session` |
| `path` | Follow-up | The shortest chain of relations between two nodes | `from`, `to`, `relations`, `session` |
| `read` | Follow-up | A whole page, one heading of it, or a raw source it cites | `ids`, `heading`, `with_links`, `session` |

`search` also replaces the old `get_context` warm start, since one call already returns the user's preferences, related memories, knowledge and relations. The follow-ups take the ids that `search` returns; they are never a way to start.

**Parameters the agent controls**

| Parameter | Meaning | Default |
|---|---|---|
| `query` | The request, wrapped by the agent into a sentence with meaning | required |
| `keywords` | Short terms of 1–3 words: names, identifiers, and both English and Chinese forms | taken from `query` |
| `mode` | `keyword`, `semantic`, `both`, or `auto` (keywords first, meaning when coverage is weak) | `keyword` |
| `entities` | Names the agent already knows are involved | none |
| `intent` | A hint such as `relation`, `procedure` or `history`, which shifts the slot budget | inferred from cue words |
| `slots` | Hits per slot, such as `{memories: 3, knowledge: 3, relations: 5}` | those values, maximum 20 each |
| `hop` | Follow the top memories to the knowledge they're anchored to | on |
| `depth`, `direction`, `relations`, `max_nodes` | How far a `graph` walk goes, which way, along which relation types, and how many nodes it returns | 1 (maximum 3), both ways, all types, 25 (maximum 100) |
| `heading` | Read only one heading of a page | whole page |
| `with_links` | Also return the summaries of pages linked from the one being read | false |
| `session` | An id for the agent's current task, used by the read tracker | none |

Tenant and user are never parameters; they always come from the credentials.

**How keyword ranking works inside a channel**

1. **Exact routing.** A name found by the name matcher pins its page, node or relation type to the top of its slot.
2. **Ranking.** BM25 runs over wiki pages. Title and aliases count most, then summary and keywords, then body text. Multi-word keywords get a phrase-match boost, longer and more specific keywords weigh more, and identifiers such as `order_status` also match exactly.
3. **Snippets.** Each hit returns its summary, the lines that contain the keywords, its anchors and the sources it cites.
4. **Coverage.** Each slot is marked `strong` or `weak` from its best score and its number of matches.
5. **Feedback.** Each hit lists the keywords it matched, and the response lists the keywords that matched nothing, so the agent knows what to rephrase or whether to switch mode.

A-RAG matches keywords by scanning the text at query time. We keep an index instead, because it stays fast as tenants grow.

Here is an example search, a question that needs a memory, knowledge and a relation at once:

```json
POST /search
{ "query": "which customer placed order 1042, and how do we give them a partial refund after shipment?",
  "keywords": ["order 1042", "partial refund", "部分退款"],
  "mode": "keyword" }

200 OK
{ "entities": ["commerce:order/1042"],
  "intent": "relation",
  "memories": { "coverage": "strong", "hits": [
    { "page": "experience/refund-handling", "match": "alias",
      "matched_keywords": ["partial refund", "部分退款"],
      "summary": "Check shipment status first; partial refunds after shipment need a manual ledger entry.",
      "about": ["commerce:order", "expertise/refund-regulations"],
      "route": { "read": "experience/refund-handling", "heading": "Pitfalls" }, "read": false },
    { "page": "users/u_7b21e0d4/decisions/refund-policy", "summary": "...",
      "about": ["commerce:order"],
      "route": { "read": "users/u_7b21e0d4/decisions/refund-policy" }, "read": false } ] },
  "knowledge": { "coverage": "strong", "hits": [
    { "page": "expertise/refund-regulations", "summary": "...", "via": "memory",
      "route": { "read": "expertise/refund-regulations" }, "read": false } ] },
  "relations": { "coverage": "strong", "relation_types": ["commerce:placed_by"],
    "edges": [ { "from": "commerce:order/1042", "type": "placed_by",
                 "to": "commerce:customer/c_5521", "via": "customer_id" } ],
    "route": { "graph": "commerce:order/1042" } },
  "unmatched_keywords": [] }
```

**Read tracker.** When calls carry a `session` id, the service remembers which pages and sources that session has already read. Reading one again returns a short "already read" notice instead of the full text, and search marks those hits `read: true`. A-RAG uses the same idea to save tokens and to push the agent toward material it hasn't seen. Sessions live in memory, are scoped to the tenant and user, and expire after an idle timeout (30 minutes by default).

**Tool descriptions teach the strategy.** As in A-RAG, the guidance lives in the MCP tool descriptions, not in a fixed workflow:

- wrap the request into a query with meaning, plus short, specific keywords, with good and bad examples
- abstract first-person questions into the topic they are about: "what language should replies to me use?" becomes keywords like `preferences`, `reply language`, `偏好`, `回复语言`. The service does no such rewriting itself (decided 2026-09-30); it only reports weak slots and unmatched keywords, so the agent can retry
- read the slots first, then open only the promising hits in full
- follow each hit's route: `read` for a file, `graph` or `path` for a node
- on a weak slot, retry with other terms, pass `entities` or `intent`, or switch `mode` to `semantic`

The service never forces an order of calls.

**Misses improve the wiki.** Every weak slot is logged with its keywords. During lint, the librarian reviews the most frequent misses and adds the missing pages, aliases, anchors or relation-type aliases, so weak slots become rarer.

## How it stays fast

Reads never wait on an LLM. They are lookups, keyword search, graph walks and, in semantic mode, one small embedding model, all over data already in memory. The target is a few milliseconds of service time in keyword mode and around 20 ms end to end, to be confirmed by the benchmark in milestone M6.

| Read step | What happens | Rough cost |
|---|---|---|
| Resolve identity | Check the API key or JWT, and pick the tenant | microseconds |
| Match names | One pass of the name matcher over the query | microseconds |
| Keyword search | BM25 over the tenant's wiki index for memories and knowledge, and over the relation-type index | around a millisecond |
| Meaning search, only when asked | Embed the query with a small local model, then compare it with the stored vectors | roughly 5–15 ms, mostly the embedding |
| Graph walk, lift and hop | Breadth-first over in-memory adjacency lists, capped by depth and node count | microseconds |
| Build the response | Slots of summaries and routes, capped at a size budget | microseconds |

These costs are orders of magnitude, not measurements.

Five rules keep it fast:

1. **No LLM on the read path.** Search, follow-ups and page reads never call an LLM. The only LLM calls are the librarian's, in background jobs, plus the optional test chat.
2. **Return summaries, not whole pages.** Every page has a 1–3 sentence summary written by the librarian. Search returns summaries, and agents call `read` for the full text. Small responses keep the agent fast too.
3. **One round trip per task.** One `search` returns memories, knowledge and relations together, so an agent often needs no second call.
4. **Keep hot data in memory.** Busy tenants' indexes, vectors, name matchers and graphs stay loaded, in a size-limited cache. They're loaded from the files, SQLite and Neo4j when a tenant opens, and a search never queries SQLite or Neo4j.
5. **Writes never block reads.** Each tenant has one writer. Searches keep using the last committed snapshot while an ingest runs.

## Search traces and replay

Every `/search` and `/read` that is answered leaves one line in the tenant's `logs/search-<date>.jsonl` (decided 2026-09-30), so real traffic can be studied and replayed against later versions of the search:

- **What a line holds.** Time, a request id, the endpoint, the tenant folder and the caller's hashed user and group folders (never raw ids), the full request, what came back per slot (page ids, how each matched, coverage), the entities, the intent, the unmatched keywords, the time of each step, the version of the search snapshot (a hash of the wiki it was built from), the code that answered (crate version and git commit, `-dirty` for uncommitted changes) and a fingerprint of the search settings (slot sizes and ablation switches).
- **Off the read path.** Handlers hand the line to a background writer and never wait for it; if the writer falls behind, lines are dropped and counted rather than slowing a search. Nothing on the read path touches SQLite.
- **Wiki history.** Every wiki version a search snapshot is built from is kept in the tenant's `history/`, content-addressed so an unchanged page is stored once (`history/objects/`, plus one manifest per version in `history/versions/`). No git is needed, and a wiki the host keeps in its own git is left alone. Next to each day's traces, `logs/versions-<date>.txt` lists the versions that day's lines ran on.
- **Retention.** Trace files and version lists older than `retention_days` (30 by default, in `[trace]`) are removed, then the wiki versions no remaining list names and nothing built recently, with the page objects only they used. Deleting a user does not touch traces; they expire with the retention period. It does remove that user's pages from every kept wiki version (decided 2026-09-30), since those copies hold the pages' full text; searches by that user can then no longer be replayed on their recorded wiki.
- **Weak slots.** A weak slot is visible in its line, which is the miss log that lint reviews (see "Misses improve the wiki").
- **Replay.** `kb-replay` reads trace files and runs each recorded search again with the current code and config, on the wiki version it ran on (rebuilt from the history, the default) or on today's wiki (`--wiki current`). On the recorded wiki a difference comes from the code or the settings alone, which is what an experiment on search needs; on today's wiki it shows what users would get now. The report lists which results stayed the same and which changed (pages added, dropped or reordered, coverage flips), which code and settings answered at the time and now, and which searches had to fall back to today's wiki because their version had expired. By default it replays the last 7 days (`--days N`, 0 for all of the 30 kept); it streams the files line by line, so its memory stays flat however much it replays. Replays never write traces, never mark pages as read, and never consume or delete what they read.

## Architecture

Writes and reads are two separate paths through one program. They share the per-tenant state in memory, one folder of files per tenant, and Neo4j for graph data.

![architecture · five insert paths, one search entry, shared state](images/image2.png)

architecture · five insert paths, one search entry, shared state

Only step 3 of the write path calls an LLM, and graph data skips it entirely. The read path is lookups, keyword search, graph walks and an optional query embedding, so its speed doesn't depend on any LLM.

## The librarian: how new content is filed

Content arrives through five insert entry points, one per kind. Every write is a background job: the API returns a job id right away, and the librarian updates the wiki shortly after. Graph spaces and graph data are the exception; code checks them and applies them without the librarian. Graph schemas do go through the librarian, which writes their descriptions.

1. **Receive.** Content arrives at the insert entry point for its kind. Its tenant, user and groups come from the credentials, and the caller picks the audience among those the credentials and the kind allow. The raw record is saved unchanged.
2. **Find related pages.** Without an LLM, the service finds pages in the same scope that share names, identifiers, aliases or keywords with the new content, and keeps the best 5–10. A cheap first LLM call can extract keywords to sharpen this.
3. **Plan the edits (LLM).** The librarian's main LLM call gets the new content, those pages and the rules for this kind. It returns a structured edit plan: create a page, update part of a page, add an anchor, or mark a claim as superseded.
4. **Verify the edits (LLM).** A second LLM call checks each edit against the new content and the cited sources. Every changed claim and every anchor must be supported, must not contradict another page, and must follow the rules for its kind. A failed edit goes back to step 3 once with the reasons; if it fails again, it's held as a proposal for review. This check can use a cheaper model.
5. **Apply.** The service checks every edit for the same scope, allowed paths and required fields, and every memory for at least one anchor. Code, not the model, writes ids, dates and front matter.
6. **Record.** The changed pages are re-indexed and re-embedded, the name matcher and anchors are updated, `index.md` gets their new summaries, and `log.md` gets one line for the job.

**Manual edits get the same check.** An edit through `PUT /pages` goes through step 4 before it's saved. If the verifier objects, the edit is held as a proposal with its reasons, and an admin can approve it anyway.

Each kind has its own merge rule:

| Kind | How new content is merged |
|---|---|
| Agent experience | Into the playbook for that task type; repeats are counted instead of duplicated. Anchored to the entities and concepts the task touched |
| User decision history | Appended to the topic page as a new decision and outcome; past entries are never overwritten. Anchored to what the decision was about |
| Vertical expertise | Integrated into concept pages; contradictions are flagged, not silently resolved |
| Business graph | A schema change (additive only) updates the stored definition, and the librarian rewrites the schema's description, which is verified. Spaces, nodes and edges are checked by code against their schema and applied without the librarian; no change summary is written |
| Preferences | The latest statement wins; the profile stays short |

**Volume and cost.** Agents can produce many task records. The service batches them per tenant, and can be configured to file only notable ones, such as failures and new approaches. Superseded claims are marked, not deleted, so the history stays readable.

## Filling gaps from outside, with approval

The wiki can grow on its own: the service finds what it's missing, searches external sources, and proposes new material. Nothing enters the wiki until a person approves it. The loop is off by default, runs in the background after each lint pass, and never touches the read path.

1. **Find gaps.** Frequent weak slots, lint findings and stale pages mark what's missing.
2. **Search outside.** External sources are pluggable connectors, like the LLM backends: a web search API, documentation sites, vendor APIs. Each tenant opts in and keeps an allowlist of the sources it trusts.
3. **Propose.** The librarian ranks the candidates by relevance, by novelty compared with existing pages, and by source credibility. Each proposal says which gap it fills.
4. **Approve.** Proposals land in the same queue as held edits (`/proposals`). Approved sources go through normal ingest, including the verify step. Rejections are stored with their reasons, to improve later proposals.
5. **Keep provenance.** The URL, fetch date and license are stored with the raw record, and pages built from external content are marked as lower trust.

Four risks are designed for from the start:

| Risk | What could happen | Safeguard |
|---|---|---|
| Prompt injection | A fetched page contains instructions aimed at the librarian | Fetched text is treated as data only; the verify step and human approval are backstops |
| Privacy | Search queries built from a tenant's gaps reveal its business to an outside provider | Opt-in per tenant, allowlisted providers, and queries shown in the proposal |
| Licensing | Copying content the tenant has no right to store | When the license is unclear, keep a summary and a link, not a copy |
| Cost | Unbounded searching and LLM calls | A daily token budget per tenant |

## Storage layout

Wiki pages and raw records are plain files, one folder per tenant, so developers can inspect them, back them up or keep them in git. Each tenant also has one SQLite file for operational data and embeddings, and graph data lives in Neo4j. The search index and the in-memory copies can be rebuilt from these at any time.

```text
data/
├── rules/                     default rules per kind, shipped with the project
│   ├── experience.md
│   ├── decision.md
│   ├── expertise.md
│   ├── graph.md
│   └── preference.md
└── tenants/
    └── t_3f9a1c05/            folder name derived from the tenant id
        ├── rules/             optional overrides of the default rules
        ├── sources/           raw records, never modified
        ├── wiki/
        │   ├── index.md       catalog: every page with its summary
        │   ├── log.md         one line per ingest, edit and lint run
        │   ├── experience/    e.g. refund-handling.md
        │   ├── expertise/     e.g. refund-regulations.md
        │   ├── graph/         one description per schema, e.g. commerce.md
        │   ├── groups/
        │   │   └── g_5c0e91a7/        folder name derived from the group id
        │   │       ├── experience/    a team's own playbooks
        │   │       ├── expertise/     a team's internal notes
        │   │       └── decisions/     decisions the team made together
        │   └── users/
        │       └── u_7b21e0d4/        folder name derived from the user id
        │           ├── preferences.md
        │           └── decisions/     e.g. refund-policy.md
        ├── kb.sqlite          jobs, proposals, API keys, audit log, graph schemas and spaces, embeddings
        ├── graph.sqlite       nodes and edges, only with the SQLite graph backend
        ├── logs/              search traces, one JSONL file per day (search-2026-09-30.jsonl)
        ├── history/           past wiki versions for replays: objects/ and versions/
        └── index/             derived, safe to delete
            └── tantivy/       wiki pages and relation types
```

Each page starts with front matter that the service writes and reads. Memory pages carry `about` anchors to the entities and pages they concern:

```markdown
---
kind: experience
title: Handling partial refunds
aliases: [partial refund, 部分退款, refund after shipment]
summary: Check shipment status first; partial refunds after shipment need a manual ledger entry.
about: [commerce:order, expertise/refund-regulations]
sources: [2026-09-28-task-4812.json]
updated: 2026-09-28
---
```

Pages link to each other as `[[page-name]]`. Entities are named `schema:type` for a node type, such as `commerce:order`, and `space:type/key` for a single node, such as `mainland:order/1042`. On load, page anchors and graph data go into one in-memory graph per tenant. In version 1, job status lives in memory, and finished jobs are dropped after a configurable time.

**What SQLite holds.** One file per tenant, opened in WAL mode so reads carry on while the tenant's single writer works. It holds the job queue, proposals, API keys, the audit log, the graph schema definitions, the registry of graph spaces, and the embeddings:

```sql
CREATE TABLE embeddings (
  page     TEXT NOT NULL,   -- e.g. experience/refund-handling
  heading  TEXT NOT NULL,   -- the section this vector covers
  model    TEXT NOT NULL,   -- e.g. multilingual-e5-small
  hash     TEXT NOT NULL,   -- hash of the section text it was built from
  vector   BLOB NOT NULL,   -- 384 float32 values, about 1.5 KB
  PRIMARY KEY (page, heading, model)
);
```

Vectors stay out of the Markdown files: they are derived data, like the search index. The hash catches hand edits, so a section whose text no longer matches is re-embedded, and changing the embedding model rebuilds this table without touching a single page.

**What Neo4j holds.** The graph nodes and edges of every tenant, in one database, since the Community edition has only one. Each node is labelled with its type and carries the tenant id, its space and its key; an index on the tenant id keeps every query scoped, and a uniqueness constraint keeps a key unique within its type and space (Neo4j Community supports this composite constraint, checked in M3; code validates the rest). The service writes to Neo4j from `/ingest/graph/space` and `/ingest/graph/data` and reads a tenant's graph into memory when the tenant opens; searches never query it. A `GraphStore` trait keeps an SQLite backend beside Neo4j, for tests and for trying the service without it.

## Interfaces

One axum server exposes three interfaces, and all of them call the same core functions. Every request carries credentials: an API key (plus `X-User-Id` for user-scoped calls), or a JWT.

### HTTP API

| Method | Path | What it does |
|---|---|---|
| POST | `/ingest/experience` | Add an agent task outcome; returns a job id |
| POST | `/ingest/decision` | Add a user's decision and its outcome; returns a job id |
| POST | `/ingest/expertise` | Add a domain document or manual; returns a job id |
| POST | `/ingest/graph/schema` | Create or change a graph schema (additive only); the librarian writes its description; returns a job id |
| POST | `/ingest/graph/space` | Create a graph space under a schema, with its visibility (shared, private or restricted with members); no LLM, no wiki page |
| POST | `/ingest/graph/data` | Add nodes and edges to a space, checked by code against its schema; no LLM, no wiki page |
| POST | `/ingest/preference` | Add a user's stated preference or observed habit; returns a job id |
| GET | `/jobs/{id}` | Status and result of an ingest or lint job |
| POST | `/search` | The only search entry: memories, knowledge and relations in slots, with a route per hit |
| POST | `/graph` | Follow-up: walk the graph from a node that `/search` returned |
| POST | `/graph/path` | Follow-up: the shortest chain of relations between two nodes |
| POST | `/read` | Follow-up: full text of pages or sources, whole or one heading, with the read tracker |
| GET | `/pages` | List pages, filtered by kind |
| GET | `/pages/{path}` | Read one page (for browsing) |
| PUT | `/pages/{path}` | Edit a page by hand; the librarian verifies it before it's saved |
| GET | `/proposals` | Held edits and external-source candidates awaiting approval, with reasons |
| POST | `/proposals/{id}/approve` | Approve a held edit or an external source (admin key only) |
| POST | `/lint` | Queue a lint pass for the tenant |
| DELETE | `/users/{user_id}` | Delete a user's decision history and preferences in this tenant |
| POST | `/admin/tenants` | Create a tenant and its first API key (admin key only) |
| DELETE | `/admin/tenants/{id}` | Delete a tenant and all its data (admin key only) |
| GET | `/health` | Liveness check |

**Response formats.** Decided 2026-09-30, so every endpoint reads the same way:

- A wiki page is named `page` everywhere: search hits, `/graph` pages, `/pages` and `/pages/{path}`. Only `/read` items use `id`, because from M4 they can also be raw sources.
- Entities are named one way: `space:type/key` for a node, `schema:type` for a node type, `schema:relation` for a relation type. Inside something that already carries its `space` (a node, an edge, a graph data request), `type` is the bare name and a data request's edge ends are `type/key`; a reference standing on its own is always qualified (`entities`, `relation_types`, `about`, `anchored_to`).
- A node is `{id, space, type, key, name, aliases, depth}`; an edge is `{space, from, from_name, type, to, to_name, via, match}`.
- Lists are always present, possibly empty. A single value that doesn't apply is left out, never `null`. Counts never reuse a list's name (`nodes_written`, not `nodes`).
- `match` says how something was found: `alias` or `keyword` for page hits; `path`, `relation`, `node_type`, `neighbor` or `anchor` for edges.
- Keys are snake_case; errors are `{"error": {"code", "message"}}` everywhere.
- Stored forms use the same field names as the API: a schema definition is the same in the request, in SQLite and in its description's front matter; a node or edge is the same in a data request, in SQLite and as Neo4j properties. The one exception is `front_matter` in `GET /pages/{path}`: it is the page file as stored, so empty lists are left out as in the file.
- Search traces use the response's names, with edges as `{from, type, to}` objects (`kb-replay` also reads the `[from, type, to]` arrays of earlier traces).

Here is an example insert, a decision record. The search example is in "Retrieval for agents":

```json
POST /ingest/decision
{ "topic": "refund policy",
  "decision": "Refund shipping costs on partial refunds for repeat customers",
  "context": "The customer asked for a partial refund on order 1042 after shipment",
  "outcome": "The customer kept the rest of the order",
  "entities": ["commerce:order/1042"],
  "date": "2026-09-28" }

202 Accepted
{ "job": "j_81c2", "status": "queued" }
```

### MCP server

The MCP server lives at `/mcp`. It uses `rmcp`, the official Rust MCP SDK, over Streamable HTTP, mounted in the same axum app. MCP clients authenticate the same way as HTTP callers.

| Tool | Same as |
|---|---|
| `search` | `POST /search` |
| `graph` | `POST /graph` |
| `path` | `POST /graph/path` |
| `read` | `POST /read` |
| `record_experience` | `POST /ingest/experience` |
| `record_decision` | `POST /ingest/decision` |
| `record_preference` | `POST /ingest/preference` |

The tool descriptions carry the retrieval guidance from "Retrieval for agents". They also say when to record: experience after finishing a task, a decision after a user makes one, and a preference when a user states one. Expertise and graph inserts come from host systems, so they are HTTP-only, like the management operations (lint, proposals, deletion, admin).

### Streaming test chat

The test chat is for developers trying the service out, not part of the real API. A small page at `/dev` sends questions to `POST /dev/chat`, which builds an LLM answer from `/search` results and the follow-ups they point to. The answer streams back as Server-Sent Events, next to the slots it used. Both routes exist only when `KB_DEV=1` is set.

## Crates we'll use

A crate is a Rust library published on crates.io. About seventeen of them cover version 1, including local embeddings, SQLite and the Neo4j driver.

| Crate | Version | Job in this project |
|---|---|---|
| [tokio](https://tokio.rs/tokio/tutorial) | 1.x | Async runtime |
| [axum](https://crates.io/crates/axum) | 0.8 | HTTP server and routing |
| serde, serde_json | 1.x | JSON in and out |
| [tantivy](https://crates.io/crates/tantivy) | 0.26 | BM25 search index, one per tenant |
| [jieba-rs](https://crates.io/crates/jieba-rs) | 0.11 | Chinese word segmentation, inside our own tantivy tokenizer (`tantivy-jieba` can only use one global dictionary or a full copy per tokenizer, so it can't keep each tenant's terms apart) |
| [aho-corasick](https://crates.io/crates/aho-corasick) | 1.x | The name matcher: finds every known alias in a query in one pass |
| [petgraph](https://crates.io/crates/petgraph) | 0.8 | The in-memory copy of the graph: edges, anchors, walks and paths |
| [fastembed](https://docs.rs/fastembed/latest/fastembed/) | latest | Local multilingual embeddings, such as multilingual-e5-small |
| [rusqlite](https://crates.io/crates/rusqlite) | latest | One SQLite file per tenant: jobs, proposals, keys, audit log, embeddings |
| [neo4rs](https://github.com/neo4j-labs/neo4rs) | 0.9 (release candidate) | Neo4j driver for the graph store; a Neo4j Labs project, so pin its version |
| moka | latest | Size-limited caches for open tenants' indexes, vectors, graphs and hot pages |
| reqwest | latest | HTTP client for the librarian's LLM calls |
| async-trait | latest | Async methods on the pluggable LLM trait and the storage traits |
| [rmcp](https://github.com/modelcontextprotocol/rust-sdk) | latest | Official MCP SDK, used for `/mcp` |
| jsonwebtoken | latest | Verifying tokens in JWT mode |
| anyhow, thiserror | 1.x, 2.x | Error handling |
| tracing, tracing-subscriber | 0.1, 0.3 | Logs, with a timing for each step |

We're deliberately leaving these out of version 1:

- [**usearch**](https://docs.rs/usearch) is only needed if brute-force vector search gets slow, around a million vectors in one tenant.
- **Milvus, Qdrant or LanceDB** would add a service for vectors that fit comfortably in SQLite and memory.
- **TiDB or MySQL** are built for many machines sharing one database. Version 1 runs on one machine; if a hosted version ever spreads across machines, Postgres is the step up, behind the same storage trait.
- [**rig**](https://crates.io/crates/rig-core) is a multi-provider LLM library. We keep our own small LLM trait, and rig can replace it later without touching the rest of the code.

## Rust concepts you'll meet

Claude Code will write most of the code, but you'll review it. The ideas below cover almost every line, and each one links to the part of the official [Rust Book](https://doc.rust-lang.org/book/) or crate docs that teaches it.

| Concept | Where it shows up here | Read |
|---|---|---|
| Cargo, crates, modules | Project layout and dependencies | [Book ch. 1](https://doc.rust-lang.org/book/ch01-00-getting-started.html), [ch. 7](https://doc.rust-lang.org/book/ch07-00-managing-growing-projects-with-packages-crates-and-modules.html) |
| Ownership and borrowing | Passing text and pages around without copying them | [ch. 4](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html) |
| Structs and enums | `Page`, `Hit`, `Slot`; `enum Mode { Keyword, Semantic, Both, Auto }` | [ch. 5](https://doc.rust-lang.org/book/ch05-00-structs.html), [ch. 6](https://doc.rust-lang.org/book/ch06-00-enums.html) |
| `Vec`, `String`, `HashMap` | Hit lists, the alias catalog, the graph's adjacency lists | [ch. 8](https://doc.rust-lang.org/book/ch08-00-common-collections.html) |
| `Result` and the `?` operator | Every step that can fail, such as reading a file or calling the network | [ch. 9](https://doc.rust-lang.org/book/ch09-00-error-handling.html) |
| Traits | The pluggable LLM backend, and the `Store` and `GraphStore` storage backends | [ch. 10.2](https://doc.rust-lang.org/book/ch10-02-traits.html) |
| Trait objects (`dyn`) | Choosing Claude or OpenAI-compatible at startup | [ch. 18.2](https://doc.rust-lang.org/book/ch18-02-trait-objects.html) |
| `Arc` (shared ownership) | One `AppState` shared by every request | [ch. 16.3](https://doc.rust-lang.org/book/ch16-03-shared-state.html) |
| `async` / `.await` | HTTP handlers and LLM calls | [ch. 17](https://doc.rust-lang.org/book/ch17-00-async-await.html), [Tokio tutorial](https://tokio.rs/tokio/tutorial) |
| `spawn_blocking` | Embedding queries and committing index writes without stalling requests | [tokio docs](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) |
| Serde derive | JSON request and response types | [serde.rs](https://serde.rs/derive.html) |

Here is how several of these fit together in the pluggable LLM backend. The trait says what any backend must do. Each provider implements it, and config picks one at startup:

```rust
// src/llm/mod.rs
use async_trait::async_trait;

pub struct Prompt {
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
}

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, prompt: &Prompt) -> anyhow::Result<String>;
}

// Each struct implements LlmClient with one HTTP POST via reqwest.
pub struct Anthropic { /* http client, api key, model */ }
pub struct OpenAiCompatible { /* http client, base_url, optional key, model */ }

// In main.rs, chosen once from config:
let llm: Arc<dyn LlmClient> = match cfg.provider.as_str() {
    "anthropic" => Arc::new(Anthropic::new(&cfg)?),
    _ => Arc::new(OpenAiCompatible::new(&cfg)?),
};
```

Nothing else in the program knows which provider is running. A new backend is one new file that implements `LlmClient`.

Here is what an HTTP handler looks like. axum pulls the shared state, the caller's scope and the parsed JSON body out of the request. Because every search function takes a `Scope`, a query that forgets the tenant filter won't compile:

```rust
async fn search(
    State(app): State<AppState>,
    scope: Scope,                  // tenant + user, from the API key or JWT
    Json(req): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    // the one entry: memories, knowledge and relations, each in its own slot
    let response = app.search(&scope, &req).await?;
    Ok(Json(response))
}
```

## Project layout

The project is a single crate, with one module per job. Everything a developer might want to change without touching code lives in `config.toml`, `rules/` and `prompts/`.

```text
kb/
├── Cargo.toml
├── Dockerfile           build on the official Rust image, run on a slim one
├── docker-compose.yml   the service plus the official Neo4j image
├── config.toml          identity mode, librarian LLM, data folder, port, Neo4j
├── rules/               default rules per knowledge kind
├── prompts/             librarian prompts: ingest.md, lint.md
└── src/
    ├── main.rs          read config, start the server
    ├── config.rs        Config struct, loaded with serde
    ├── identity.rs      API key and JWT modes, producing Scope { tenant, user }
    ├── tenants.rs       id-to-folder mapping, cache of open tenants
    ├── kinds.rs         the five knowledge kinds and their rules
    ├── api/             HTTP handlers: the five inserts, jobs, search, follow-ups, pages, admin
    ├── mcp.rs           MCP tools, calling the same core functions
    ├── dev.rs           test chat: SSE stream and a small HTML page
    ├── llm/             LlmClient trait, anthropic.rs, openai.rs
    ├── store/           Store trait and its SQLite backend: jobs, proposals, keys, audit, embeddings
    ├── librarian/       find related pages, plan edits, validate, anchor, apply
    ├── wiki/            pages, front matter, [[links]], anchors, the alias catalog
    ├── search/          name matcher, tokenizers, tantivy, vectors, channels, slots, hop
    ├── graph/           GraphStore trait (Neo4j and SQLite backends), schema descriptions,
    │                    relation-type index, the petgraph copy, walks
    ├── acquisition/     gap finding, external source connectors, candidate proposals
    └── jobs.rs          per-tenant write queue and job status
```

Here is an example config. API keys and passwords stay in environment variables, such as `ANTHROPIC_API_KEY` and `NEO4J_PASSWORD`, never in the file:

```toml
[server]
port = 8080

[data]
dir = "/data"                  # the Docker volume

[identity]
mode = "api_key"               # or "jwt"
user_header = "X-User-Id"
group_header = "X-User-Groups"   # optional; used by restricted graph spaces
# jwt mode:
# public_key_file = "keys/auth.pem"
# tenant_claim = "tenant_id"
# user_claim = "sub"
# groups_claim = "groups"

[librarian]                    # the only LLM the service calls
provider = "anthropic"         # or "openai_compatible"
model = "claude-sonnet-5"
# base_url = "http://localhost:11434/v1"   # for openai_compatible

[store]                        # one SQLite file per tenant, WAL mode
file_name = "kb.sqlite"

[search]
languages = ["en", "zh"]
default_mode = "keyword"       # keyword, semantic, both or auto
slots = { memories = 3, knowledge = 3, relations = 5 }
hop = { top_memories = 3, depth = 1 }

[embedding]                    # local, runs inside the process
model = "multilingual-e5-small"

[graph]
backend = "neo4j"              # or "sqlite", for tests and trying it out
max_depth = 3
max_nodes = 100

[neo4j]
uri = "bolt://neo4j:7687"
user = "neo4j"                 # password from NEO4J_PASSWORD

[acquisition]                  # off by default, opt-in per tenant
enabled = false
providers = []                 # e.g. a web search API, documentation sites
daily_token_budget = 200000

[jobs]
keep_finished_minutes = 60
```

## Packaging with Docker

The service ships as one image built from the official Rust image, and runs beside the official Neo4j image under Docker Compose. `docker compose up` starts both, and all of the service's own data sits in one volume.

```dockerfile
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=build /src/target/release/kb /usr/local/bin/kb
COPY config.toml ./
COPY rules ./rules
COPY prompts ./prompts
VOLUME ["/data"]
EXPOSE 8080
CMD ["kb"]
```

```yaml
services:
  kb:
    build: .
    ports: ["8080:8080"]
    volumes: ["kb-data:/data"]
    environment:
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY}
      NEO4J_PASSWORD: ${NEO4J_PASSWORD}
    depends_on: [neo4j]
  neo4j:
    image: neo4j:5-community
    environment:
      NEO4J_AUTH: neo4j/${NEO4J_PASSWORD}
    volumes: ["neo4j-data:/data"]

volumes:
  kb-data:
  neo4j-data:
```

Four things to get right:

- **Pin Neo4j.** Replace the moving `5-community` tag with the exact 5.x version the neo4rs driver is tested against.
- **Wait for Neo4j.** `depends_on` only orders the start, so the service retries its Neo4j connection at startup instead of failing.
- **Keep the embedding model offline.** fastembed downloads model files on first use; point its cache at the `/data` volume, or download them during the build.
- **Check the ONNX Runtime library.** fastembed runs on ONNX Runtime; make sure the library it needs ends up in the slim runtime image, which is why both stages use the same Debian release.

## Building it with Claude Code

Claude Code writes the code one milestone at a time, and you review each milestone before starting the next.

1. Install Rust with rustup, following the Book's [Installation](https://doc.rust-lang.org/book/ch01-01-installation.html) page. Check it with `rustc --version` and `cargo --version`.
2. Install Docker, and start Neo4j with `docker compose up neo4j` so tests can reach it from M3 on.
3. Create an empty git repository and put this design doc in it, for example as `docs/design.md`.
4. Add a `CLAUDE.md` with the project rules (below).
5. Write the test set before M2: 30–50 questions per knowledge kind, each tagged with its answer and slot, plus your predictions for the hypotheses in the Research plan.
6. Ask Claude Code for one milestone at a time, starting with M1, and have it write tests for each.
7. Before moving on, run the milestone's "done when" check yourself.

Here are some example rules for `CLAUDE.md`:

```markdown
# Rules
- The design is in docs/design.md. Build one milestone at a time.
- Before calling a task done, run: cargo fmt, cargo clippy -- -D warnings, cargo test.
- Every read and write path takes a Scope (tenant + user + groups); never take tenant, user or group ids from request bodies.
- /search is the only search entry. /graph, /graph/path and /read only take ids that /search returns.
- No LLM calls on the read path (/search, /graph, /read, /pages).
- Searches read only the in-memory copies; never query SQLite or Neo4j on the read path.
- Storage sits behind the Store and GraphStore traits, from M1 on.
- Every Neo4j node and edge carries the tenant id, and every Cypher query filters on it.
- Every search stage gets a tracing span with its timing.
- Embeddings use the e5 prefixes: "query: " for queries, "passage: " for stored text.
- The librarian never writes graph data, and every memory page has at least one anchor.
- Tenant, group and user folder names come from the id-mapping functions, never from raw ids.
```

To review the code comfortably, skim these parts of the Rust Book first: ch. 4 (ownership), ch. 9 (errors), ch. 10.2 (traits) and ch. 17 (async). The concepts table above links to each one.

## Build milestones

There are nine milestones, and each ends with a check you can run yourself. Isolation comes first, because every later milestone builds on it.

| # | Build | Done when |
|---|---|---|
| M1 | Skeleton: config, identity in API key mode, tenant folders and id mapping, the `Store` trait with one SQLite file per tenant, pages as files, `/pages`, a tracing span per step | Two tenants' pages and SQLite files are completely invisible to each other over HTTP |
| M2 | One entry, keyword mode: name matcher, English and Chinese tokenizers with the jieba dictionary, per-tenant tantivy index (in memory, rebuilt from the wiki when the tenant opens), the three channels and their slots, routes, `/read` of pages with the read tracker, search traces and `kb-replay`, the personal deployment defaults | The test set's questions come back in the right slot, in both languages |
| M3 | Graph: `/ingest/graph/schema`, `/space` and `/data`, schema definitions and descriptions, spaces under schemas with their visibility, the relation-type index, the `GraphStore` trait with Neo4j and SQLite backends, graph data checked against schemas, instance names in the name matcher, page anchors, the petgraph copy, lifting, `/graph` and `/graph/path` | Relationship questions return the right edges in the relations slot, even when knowledge pages share their keywords; nothing from a space the caller can't see ever appears; and the tests pass against the Neo4j container |
| M4 | Librarian and inserts: `LlmClient` with both backends, plan and verify steps, the other four insert entry points, anchoring, per-tenant write queue, `/jobs`, proposals, rules for all five kinds, raw records in `sources/` and `/read` of the sources a visible page cites | New content updates the related pages instead of duplicating them, every memory is anchored, and unsupported edits are held for review |
| M5 | Hop and meaning: the memory-to-knowledge hop, embeddings computed at write time with the e5 prefixes and stored in SQLite, `mode` with `semantic`, `both` and `auto` | Questions that need a memory plus the knowledge it leads to are answered, and semantic mode recovers cross-language misses |
| M6 | Speed: caches, loading tenants on demand, the tantivy index on disk (`index/tantivy/`) with incremental sync by page hash, a benchmark at 1k, 10k and 100k pages | Per-step timings meet the targets in "How it stays fast", in both modes |
| M7 | MCP, test chat and packaging: the `/mcp` tools with their strategy descriptions, JWT mode, `/dev` chat with streaming, the Dockerfile and `docker-compose.yml` | An MCP client can search, follow routes and record, choosing its own order of calls, and `docker compose up` starts the service and Neo4j on a clean machine |
| M8 | Lint and deletion: contradictions, stale claims, duplicates, broken links, unanchored memories, frequent weak slots; user and tenant deletion | Lint returns findings with page links, and deleted data is gone from files, SQLite, indexes, vectors and Neo4j |
| M9 | Acquisition: gap finding, one external connector, candidate proposals, approval flow, provenance, budgets | An approved external source fills a logged gap, and a planted bad page is stopped before it reaches the wiki |

The thesis experiments run on M2 to M6. Other search entry points are opened only after that, and compared against the wiki-only design, as the Research plan tab describes.

## Open questions

- ☐ **Default identity mode.** Should the first release default to API keys, or to JWTs signed by the host system?
- ☐ **How much agent history to file.** Every task, or only failures and new approaches?
- ☐ **Retention.** How long should raw records be kept, especially task records and user decisions?
- ☑ **Instance data in graph spaces.** Decided 2026-09-30: a schema can back many graph spaces; spaces hold individual nodes (specific orders, customers, machines) in Neo4j. Only creating or changing a schema writes the wiki (through the librarian); creating a space or writing data never does. Instance names and aliases are loaded from the graph data into the name matcher. Watch the matcher's memory for tenants with millions of nodes.
- ☐ **Deleting a graph space and breaking schema changes.** Version 1 only allows additive schema changes. How should a space be deleted, and how should a schema be migrated when a type must be removed or a key changed?
- ☐ **Neo4j at scale.** The Community edition keeps every tenant in one database. Is that enough, or do large deployments need the Enterprise edition, or one Neo4j per group of tenants?
- ☐ **Which entry point to open first.** After the wiki-only experiment: direct graph queries, search over raw chunks, or per-kind search tools?
- ☐ **Embedding model.** Is multilingual-e5-small accurate enough for Chinese, or is a larger model worth the extra latency?
- ☐ **First external sources.** Which connector comes first for acquisition: a general web search API, or specific documentation sites?
- ☐ **License.** MIT, Apache-2.0, or both (the usual choice for Rust crates)?

## Sources

- Andrej Karpathy, [llm-wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f) (gist, April 2026)
- Du et al., [A-RAG: Scaling Agentic Retrieval-Augmented Generation via Hierarchical Retrieval Interfaces](https://arxiv.org/abs/2602.03442) (arXiv, February 2026)
- [tantivy on crates.io](https://crates.io/crates/tantivy)
- [jieba-rs on crates.io](https://crates.io/crates/jieba-rs)
- [aho-corasick on crates.io](https://crates.io/crates/aho-corasick)
- [petgraph on crates.io](https://crates.io/crates/petgraph)
- [rusqlite on crates.io](https://crates.io/crates/rusqlite)
- [SQLite write-ahead logging](https://www.sqlite.org/wal.html)
- [neo4rs, the Neo4j Labs Rust driver](https://github.com/neo4j-labs/neo4rs)
- [Neo4j official Docker image](https://hub.docker.com/_/neo4j)
- [axum on crates.io](https://crates.io/crates/axum)
- [MCP Rust SDK (rmcp)](https://github.com/modelcontextprotocol/rust-sdk)
- [fastembed docs](https://docs.rs/fastembed/latest/fastembed/)
- [usearch docs](https://docs.rs/usearch)
- [rig-core on crates.io](https://crates.io/crates/rig-core)
- [The Rust Programming Language](https://doc.rust-lang.org/book/)
