# Pinakes: how it works and how to use it

This guide explains what Pinakes keeps, how knowledge goes in and comes out, and what each
interface takes. The [README](../README.md) gives the overview and the quick start; every
configuration field is in [configuration.md](configuration.md).

- [Concepts](#concepts)
- [Tenants, users and identity](#tenants-users-and-identity)
- [Adding knowledge](#adding-knowledge)
- [The business graph](#the-business-graph)
- [Searching](#searching)
- [How the librarian files content](#how-the-librarian-files-content)
- [Lint, review and filling gaps](#lint-review-and-filling-gaps)
- [Deletion](#deletion)
- [Search traces and replay](#search-traces-and-replay)
- [Storage layout](#storage-layout)
- [HTTP API](#http-api)
- [MCP server](#mcp-server)

## Concepts

Pinakes works like a library. The wiki is the collection, and an LLM acts as the **librarian**
that files everything that comes in: it finds the pages new content belongs to and updates them,
instead of adding another document. The expensive thinking happens once, at write time, so a
read is only a lookup, and no LLM runs on the read path.

**Layers.**

| Layer | What it holds | Who writes it |
|---|---|---|
| Raw sources | Task records, decision records, documents, schema definitions, preference notes, as they arrived; never changed | The agents and systems that call Pinakes |
| The wiki | Markdown pages per kind and audience, linked to each other and anchored to graph entities, plus `index.md` (a catalog) and `log.md` (one line per job) | The librarian; people through reviewed hand edits |
| Graph data | Graph spaces with their nodes and edges, checked against a schema | The host system, and agents through drafts a person approves; never an LLM |
| Rules | One rules file per kind: page shape, what to merge, what to flag | Shipped with Pinakes (`rules/`); a tenant may override them |

**Five kinds of knowledge**, each with its own insert entry, audiences and page shape:

| Kind | Insert | Audiences | Page shape | Comes back in |
|---|---|---|---|---|
| Agent experience | `/ingest/experience` | tenant, group | One playbook per task type: steps that work, pitfalls and fixes, how often each was seen | memories |
| User decisions | `/ingest/decision` | user, group | One page per decision topic: the decision, context, outcome, date; append-only | memories |
| Vertical expertise | `/ingest/expertise` | tenant, group | Linked concept pages, as in a classic wiki | knowledge |
| Business graph | `/ingest/graph/schema`, `/space`, `/data` | tenant | One description page per schema; spaces, nodes and edges are data, not pages | knowledge (the description), relations (the edges) |
| Preferences | `/ingest/preference` | user | One short profile page per user; the latest statement wins | memories |

`/ingest/note` takes a decision, a lesson or both as an agent recorded them; the librarian
splits the note, filing decisions for the user and lessons (experience) for the tenant.

**Audiences.** Every page is for exactly one audience: the whole tenant, one group, or one user.
The page's folder says which (`groups/g_…/`, `users/u_…/`). A page is visible to the callers of
its audience only, and a page may link or anchor only to pages and nodes that everyone who can
read it can also see. The librarian is shown only what the new content's audience may see, so
content never moves to a wider audience.

**Entity names.** `schema:type` names a node type (`commerce:order`), `space:type/key` one node
(`mainland:order/1042`), and `schema:relation` a relation type (`commerce:placed_by`). Space
names are unique in a tenant and never equal a schema name, so a reference is never ambiguous.

**Anchors.** Pages list the entities and pages they are about in their front matter (`about`).
A page anchors to what it is really about, and to nothing when nothing fits (lint offers
anchors for such pages later). Searches follow anchors from text hits to the graph, and from
memories to the knowledge they rely on.

```markdown
---
kind: experience
title: Handling partial refunds
aliases: [partial refund, 部分退款, refund after shipment]
summary: Check shipment status first; partial refunds after shipment need a manual ledger entry.
about: [commerce:order, expertise/refund-regulations]
sources: [2026-09-28-experience-4812ab0c.json]
updated: 2026-09-28
---
```

Code, never the model, writes `kind`, `updated` and `sources`.

## Tenants, users and identity

Pinakes doesn't manage accounts or groups. The host system passes tenant, user and group ids in,
and Pinakes enforces isolation around them. Ids are opaque strings; folder names are derived
from them by hashing, so an id can never name a path.

| Mode (`[identity] mode`) | Tenant | User | Groups | Fits |
|---|---|---|---|---|
| `api_key` (default) | Bound to the API key | `X-User-Id` header | `X-User-Groups` header, comma-separated | A trusted backend or agent platform |
| `jwt` | A claim of a token the host signs | A claim | A list claim | Tokens that reach Pinakes from users' machines |

In JWT mode the token comes as `Authorization: Bearer`, signed with an asymmetric key (RS/PS,
ES or EdDSA; shared secrets are refused); Pinakes checks it with the host's public key. `exp` is
required; `issuer` and `audience` are checked when configured.

Group membership takes effect on the next request: when the host stops sending a group, that
caller loses its pages and spaces at once. Tenant, user and group ids are never taken from a
request body.

**Deployments** (`[identity] deployment`):

- `multi_tenant` (default): the admin creates tenants with `POST /admin/tenants`, each getting
  an API key. A call without a user sees tenant-level pages only.
- `personal`: one person. The default tenant `000000` is created on first start and its key
  written to `<data>/personal-api-key.txt`. A call without a user acts as the user `admin` (an
  ordinary user id, with no admin rights).

**Two admin credentials.** The **admin key** (`PINAKES_ADMIN_KEY`) is for scripts and host code
calling the admin routes. The **web console** at `/ui` signs in with one admin account (`root`
with `1234` at first; change it in Settings, or set `PINAKES_ADMIN_PASSWORD`). A console session
can do what the admin key does and looks at one tenant at a time, as a user of it would.

**Not found, never forbidden.** A page another user or group owns, a hidden graph space or
node, or anything of another tenant is reported as 404, so callers can't probe for existence.

## Adding knowledge

Every insert except graph spaces and graph data is a background **job**: the call saves the raw
record unchanged in `sources/` and answers 202 with a job id; the librarian files it shortly
after. `GET /jobs/{id}` shows its status and result to the caller who queued it; while a
document is being read it also shows how far the reading has come (`progress`: the lines read of
the total, and the concepts filed so far), which the console draws as a bar.

```json
POST /ingest/decision
X-User-Id: lily

{ "topic": "refund policy",
  "decision": "Refund shipping costs on partial refunds for repeat customers",
  "context": "The customer asked for a partial refund on order 1042 after shipment",
  "outcome": "The customer kept the rest of the order",
  "entities": ["mainland:order/1042"],
  "date": "2026-09-28" }

202 Accepted
{ "job": "j_81c2…", "status": "queued" }
```

**Record fields.**

| Insert | Fields |
|---|---|
| `/ingest/experience` | `task`, `outcome`, optional `details`, `succeeded`, `session` |
| `/ingest/decision` | `topic`, `decision`, optional `context`, `outcome`, `session` |
| `/ingest/note` | `topic`, `content`, optional `context`, `session` |
| `/ingest/preference` | `statement`, optional `observed` |
| `/ingest/expertise` | `title`, `content` (or a file), optional `source_url`, `document` |

All take optional `entities` (the entities it is about, at most 20) and `date` (`YYYY-MM-DD`).
Unknown fields are a 400. `session` is the id the same task searched and read with: the
knowledge pages that task read are offered to the librarian as anchors for the new memory.

**Who it is for** is the `X-Audience` header: `tenant`, `user` (the calling user) or
`group:<id>` (one of the caller's own groups), among the audiences the kind allows. Without it,
experience and expertise are for the tenant, decisions and preferences for the calling user. A
note needs a user, or `group:<id>` to file both parts for that group.

**Documents.** `/ingest/expertise` takes JSON (`content` up to 200,000 characters) or a file as
`multipart/form-data` (`file`, plus the same fields; `title` defaults to the file name). Pinakes
converts files itself, in Rust: Markdown and text as they are; HTML; Word (`.docx`); Excel
(`.xlsx`, `.xlsm`, `.csv`), read cell by cell; PowerPoint (`.pptx`); and PDF, whose text layer is
read by the `pinakes-pdf` helper in a process of its own with capped memory and a deadline. A
scanned PDF (no text layer) is drawn page by page and transcribed by the librarian's model, which
must accept images. The old binary formats (`.doc`, `.xls`, `.ppt`) and PDFs Pinakes can't read
go to the optional MarkItDown container (`[converter] url`, compose profile `markitdown`), and
are refused with what to do when it isn't set. Every reader is bounded: file size, zip entries
and ratios, XML without entities, nesting, cells, output length and time. The original file and
the Markdown are both kept in `sources/`.

**Document versions.** The first time a document is filed, Pinakes gives it an id (`d_…`),
returned in the job's result as `document`. Send it as `document` with the next upload to file
that upload as a new version. Without it, the librarian recognizes a new version by what the
document says it is. A new version is compared line by line with the last one, and only the
concepts whose lines changed are read and filed again; an unchanged upload calls no model.

**Reading like a person.** Code never interprets a document's structure. The librarian reads
the numbered text from the top, one window at a time, and decides where each concept ends; it
keeps a short brief of the document (its subject, scope, terms) and a ledger of the concepts read
so far, and files each concept as the librarian's condensed account on a page, never a copy.
Every line is either filed or skipped as not knowledge (a table of contents, boilerplate), and
pages cite the lines they came from (`sources: [<record>#L120-158]`), which `/read` returns. A
job stopped half way resumes where it was when the same text is sent again.

**Any length.** Because the librarian reads window by window and never holds the whole document
at once, a document of any length is read in full: a manual of thousands of pages the same way
as a one-page note, its cost growing with its length, never limited by the model's context
window. What bounds an upload is the service's own limits: a file of up to 20 MB, whose Markdown
is up to 10 MB, converted within `[converter] timeout_secs`; `content` sent as JSON is up to
200,000 characters.

**Graph schemas** are filed while the call waits, and graph spaces and data are applied at once
without the librarian; see the next section.

## The business graph

A **schema** defines node types and relation types. A **graph space** is one body of data built
from a schema: one `commerce` schema can back a `mainland` space and a `crossborder` space, with
the same structure and separate data. The wiki holds one description per schema; spaces, nodes
and edges live in the graph store (SQLite by default, Neo4j optional) and in memory for
searching.

**A schema** (`POST /ingest/graph/schema`):

```json
{ "schema": "commerce",
  "title": "Commerce",
  "summary": "Orders and the customers who place them.",
  "nodes": {
    "order":    { "key": "order_id", "en": "A customer order", "zh": "客户订单", "aliases": ["orders", "订单"] },
    "customer": { "key": "customer_id", "en": "A buyer account", "zh": "买家账户",
                  "properties": { "tier": { "type": "text", "zh": "会员等级" },
                                  "since": { "type": "date" } } } },
  "relations": {
    "placed_by": { "from": "order", "to": "customer", "via": "customer_id",
                   "en": "The customer who placed the order", "zh": "下单的客户",
                   "aliases": ["ordered by", "buyer", "下单人"] } } }
```

The librarian writes the schema's description page (`graph/<schema>.md`): what each type means in
both languages, aliases and a summary; code copies the structure from the definition, so a model
can never change what data is accepted. The definition and the page are stored together or not at
all. A schema change may only add: types, relations, properties and aliases. Node properties are
declared per type as `text`, `number`, `date` or `bool`; a value of another kind, or an undeclared
property, is refused. Sending the same definition again is answered at once without a model call.

**A space** (`POST /ingest/graph/space`):

```json
{ "space": "crossborder", "schema": "commerce", "name": "跨境站",
  "aliases": ["cross-border store", "跨境"],
  "visibility": "restricted", "members": { "users": ["wang"], "groups": ["risk"] } }
```

| Visibility | Who sees it |
|---|---|
| `shared` (default) | Everyone in the tenant |
| `private` | Only its owner, the user who created it |
| `restricted` | Its owner and the users and groups on its member list |

The owner is the calling user, never a field. `PUT /graph/spaces/{space}` changes visibility and
members (the owner or the admin); `DELETE /graph/spaces/{space}` removes a space with its data
(the admin). A space's display name and aliases go into the name matcher, so "跨境站的订单1042"
narrows a search to that space. Edges never cross spaces, and everything from spaces a caller
can't see is dropped before a response is built.

**Data** (`POST /ingest/graph/data`, up to 10,000 items per call):

```json
{ "space": "mainland",
  "nodes": [ { "type": "order", "key": "1042", "name": "Order 1042", "aliases": ["订单1042"] },
             { "type": "customer", "key": "c_5521", "name": "林晓", "aliases": ["Lin Xiao"],
               "properties": { "tier": "gold" } } ],
  "edges": [ { "from": "order/1042", "type": "placed_by", "to": "customer/c_5521" } ] }
```

Code checks every node and edge against the schema (known types, declared properties, both ends
in the same space). Node names and aliases go into the name matcher as they are written; graph
data never writes the wiki.

**Agents propose graph changes.** An agent never writes the graph directly. `POST /graph/draft`
(MCP `propose_graph_change`) takes `{space, changes, reason}`, where a change puts a node,
removes a node, adds an edge, removes an edge, or deletes the space. A space has at most one
pending draft; each call merges into it and is checked against the graph as the draft would leave
it, so a second agent builds on the first one's changes. The draft is a held proposal: an admin
approves it (applied whole, in one transaction, after checking it again against the graph as it
is now) or rejects it with a reason. Until then searches see the stored graph only.

## Searching

`/search` (MCP `search`) is the only entry. One call brings back three **slots**: **memories**
(experience, decisions, preferences), **knowledge** (expertise and schema descriptions) and
**relations** (graph edges). Scores of different slots are never compared, so long prose can't
crowd out the graph; a slot with nothing good stays empty rather than padded.

How a search runs:

1. **Names.** The query's words are matched against every title, alias and node name the caller
   may see, in Chinese and English. A matched name pins its page, node or relation type.
2. **Keywords** (BM25 over the wiki; titles and aliases count most, then summary and keywords,
   then the body) and, when asked, **meaning** (vectors of page heads and sections), per slot.
3. **Lift.** Each text hit adds the entities it is anchored to, so a prose hit still reaches the
   graph.
4. **Hop.** The knowledge pages that strongly matched memories are anchored to fill the
   knowledge slot's free places, marked `via: memory`.
5. **Relations.** Edges among the named and lifted nodes, edges of named relation types, chains
   to a named node type, and the shortest path between two named nodes.

**Parameters.** Tenant and user are never parameters.

| Parameter | Meaning | Default |
|---|---|---|
| `query` | The request as a sentence with meaning | required |
| `keywords` | Short terms: names, identifiers, both English and Chinese forms | taken from `query` |
| `mode` | `keyword`, `semantic`, `both`, or `auto` (keywords first, meaning for a weak slot) | `[search] default_mode` (`keyword`) |
| `entities` | Entity ids or names known to be involved | none |
| `intent` | `relation`, `procedure` or `history`: shifts the slot sizes | inferred from cue words |
| `slots` | Hits per slot, such as `{"memories": 3, "knowledge": 3, "relations": 5}` | those values, at most 20 each |
| `hop` | Follow top memories to the knowledge they are anchored to | on |
| `session` | An id for the agent's current task, for the read tracker (over MCP, `search` returns one to pass on) | none |

Relationship cue words ("relationship", "depends on", "between", "upstream", 关系, 关联, 依赖,
上下游, 影响) or two named entities give the relations slot more room.

```json
POST /search
{ "query": "which customer placed order 1042, and how do we give them a partial refund after shipment?",
  "keywords": ["order 1042", "partial refund", "部分退款"] }

200 OK
{ "mode": "keyword",
  "entities": ["mainland:order/1042"],
  "intent": "relation",
  "preferences": { "page": "users/u_7b21e0d4a1b2c3d4/preferences", "lines": [
    { "heading": "Preferences", "text": "- Reply language: Chinese" },
    { "heading": "Preferences", "text": "- Role: handles the mainland enterprise accounts" } ] },
  "memories": { "coverage": "strong", "hits": [
    { "page": "experience/refund-handling", "match": "alias",
      "matched_keywords": ["partial refund", "部分退款"],
      "summary": "Check shipment status first; partial refunds after shipment need a manual ledger entry.",
      "about": ["commerce:order", "expertise/refund-regulations"],
      "route": { "read": "experience/refund-handling", "heading": "Pitfalls" }, "read": false } ] },
  "knowledge": { "coverage": "strong", "hits": [
    { "page": "expertise/refund-regulations", "match": "anchor", "via": "memory",
      "route": { "read": "expertise/refund-regulations" }, "read": false } ] },
  "relations": { "coverage": "strong", "relation_types": ["commerce:placed_by"],
    "edges": [ { "space": "mainland", "from": "mainland:order/1042", "from_name": "Order 1042",
                 "type": "placed_by", "to": "mainland:customer/c_5521", "to_name": "林晓",
                 "via": "customer_id", "match": "relation" } ] },
  "unmatched_keywords": [] }
```

Each slot says whether its coverage is `strong` or `weak`; each hit says how it matched (`alias`,
`keyword`, `meaning` or `anchor`), which keywords it matched, and its **route**. The response
lists the keywords that matched nothing. The caller's own preferences page comes whole with every
search (`preferences`, left out while they have none), whatever was asked, so a reply language or
an answer format applies to every answer, not only to those whose search finds the page; it then
takes no place in the memories slot. The page is kept newest first (code moves the lines a filing
adds or changes to the top), and a search sends at most its first 30 lines. The agent decides what to do with these signals: read a
hit, follow an edge, retry with other keywords, or switch to `semantic`.

**Follow-ups** take only ids that `search` returned:

| Call | Returns | Parameters |
|---|---|---|
| `POST /read` | Whole pages, one heading of a page, or a raw source a visible page cites (`sources/<name>`, or its cited lines `#L…`) | `ids`, `heading`, `with_links`, `session` |
| `POST /graph` | The nodes around a node, their edges, and the visible pages anchored to them | `node`, `depth` (1, at most 3), `direction`, `relations`, `max_nodes` (25, at most 100), `session` |
| `POST /graph/path` | The shortest chain of relations between two nodes (at most 6 hops, within one space) | `from`, `to`, `relations`, `session` |

**Read tracker.** Calls with the same `session` share what has been read: reading a page again
answers "already read", and search marks those hits `read: true`. Sessions live in memory and
expire after 30 idle minutes.

**Chinese and English.** Each text field is indexed twice: as words (the tenant's own terms kept
whole, jieba for the rest of the Chinese, English stemmed) and as Chinese character pairs, which
catch terms jieba splits wrongly. A partial node name (冲压线 for "冲压线 L1") can match graph
nodes when it is specific enough.

**Meaning search.** Embeddings are off by default and nothing is downloaded; `semantic` and
`both` then answer with how to turn them on. The admin turns them on in the console (or with
`[embedding] enabled = true`), while the service runs: Pinakes downloads
jina-embeddings-v2-base-zh once into `<data>/models`, quantizes it to 8-bit weights and embeds
every page. Large tenants get an HNSW index over the page vectors. Meaning search is what finds
an English question's Chinese answer without the agent translating its keywords.

Searches read only in-memory copies; nothing on the read path calls an LLM or queries SQLite or
the graph store.

## How the librarian files content

1. **Receive.** The raw record is saved unchanged; tenant, user and groups come from the
   credentials, the audience from `X-Audience`.
2. **Find related pages.** The one search entry runs as the new content's audience, so the
   librarian sees nothing that audience can't.
3. **Plan.** One model call returns edits: create a page, update one section, add aliases,
   keywords or anchors, or supersede a claim (struck through with the date and reason; never
   deleted).
4. **Check.** Code checks each edit: the right kind and folder, limits, anchors and links the
   page's readers can see (none is required), decision pages append-only.
5. **Verify.** A second model call checks every edit against the sources. Refused edits go back
   with the reasons, up to three times; edits the verifier still refuses are held as a proposal
   for review, the rest are applied.
6. **Record.** Changed pages are re-indexed and re-embedded, `index.md` and `log.md` updated.

Records queued together for the same tenant, kind and audience are filed as one batch. Every
model reply is constrained by a strict JSON schema at the API, and every retry stops at three.

**Hand edits.** `PUT /pages/{path}` takes `{version, front_matter, body}` as `GET` returned it.
Code checks it, then the verifier compares it with the page and related pages; it is saved, or
held with the reasons for an admin to approve anyway.

**The model.** Set in `model.conf` (template: `model.conf.example`): `provider` (`anthropic` or
`openai_compatible`), `base_url`, `model`, and `api_key` or `key_env`. Any Anthropic-compatible
endpoint works (DeepSeek's is the template's default), or an OpenAI-compatible one, a local
Ollama or vLLM too. Without a model, reads work and writes answer 503 saying why.

## Lint, review and filling gaps

**Proposals.** Held edits, lint's fixes, graph drafts and candidate sources all wait in
`GET /proposals`, each with its reasons. `POST /proposals/{id}/approve` and
`POST /proposals/{id}/reject` (`{reason}`) take the admin key or the console.

**Auto-approve** (`[librarian] auto_approve`, off by default): every proposal is approved at once
instead of waiting: edits the verifier held (filing, lint's fixes, hand edits), candidate sources
from acquisition, and agents' graph drafts. Each takes the admin's approval path (code checks,
the graph checked again, a source read and filed by the librarian) and is kept as an approved
proposal, with `auto-approve` in the audit log. A job lists the edits it approved in
`result.approved`, an acquisition run its sources in `approved`, and a graph draft's answer says
what it `applied`. Nobody looks first: an edit the verifier rightly stopped is written, a screened
outside page is filed, an agent's change to the graph is applied.

**Lint** (`POST /lint`, a job; `GET /lint` lists the last run's findings the caller may see)
never writes pages. Code checks every page for broken links, anchors readers can't see, memories
without anchors, missing or orphaned sources and unfinished document readings. From the search
traces it reports keywords that keep missing (`frequent_miss`) and keywords that named graph
nodes only partly (`partial_name`, for the host to add as aliases). The model reviews pages that
changed or name a date now passed, for contradictions, duplicates and stale claims, and proposes
fixes (an alias, an anchor, a superseded claim) as held proposals. Contradictions get no fix:
which side is right is for a person.

**Acquisition** (off by default) fills gaps from sites a tenant trusts. The deployment turns it
on (`[acquisition] enabled`), then the admin turns it on per tenant with an allowlist
(`PUT /admin/tenants/{id}/acquisition`: `{enabled, sources: [{domain, license, copy, urls}]}`).
After each lint run it reads the allowlisted sites like a polite crawler (`robots.txt`,
sitemaps, same-site links, one request at a time) into a local index of titles, headings and
terms, never their text; matches lint's gaps against that index on the service (no search
engine or other third party is asked anything); has the model rank candidates and screen each
fetched page for whether it fills the gap and for instructions aimed at an AI; and proposes what
passes. Fetched text is shown to the model only between markers, as data. A person approves a
candidate before it is filed, through the normal expertise ingest; pages built from it are
marked `external: true`. Fetches reach only allowlisted public hosts, redirects included, and a
daily token budget per tenant caps the model calls.

## Deletion

Deletion is hard.

- `DELETE /users/{user_id}` removes a user's pages, raw records, proposals, documents, vectors,
  private graph spaces and drafts, takes them off member lists, and removes their pages from the
  kept wiki history. With a JWT a caller may delete only themselves.
- `DELETE /admin/tenants/{id}/groups/{group_id}` does the same for a group.
- `DELETE /admin/tenants/{id}` removes a tenant's folder and its graph data.

SQLite runs with `secure_delete`, and the search index merges away deleted documents, so a
deleted owner's text is gone from every file of the tenant except the search traces, which
expire with their retention period.

## Search traces and replay

Every answered `/search`, `/read`, `/graph` and `/graph/path` writes one line to the tenant's
`logs/search-<date>.jsonl`, off the read path: the request, what came back per slot, the timing
of each step, the wiki version, the code version and a fingerprint of the search settings.
Callers appear only as hashed folder names. Each wiki version a search ran on is kept in
`history/` (content-addressed, so an unchanged page is stored once). Traces and versions are
kept `[trace] retention_days` (30).

`pinakes-replay` runs recorded searches again with the current code and settings, on the wiki
version each one ran on (or on today's wiki), and reports what changed: pages added, dropped or
reordered, coverage flips.

## Storage layout

Wiki pages and raw records are plain files, one folder per tenant, so they can be read, backed
up or kept in git. The search index and the in-memory copies can be rebuilt from them.

```text
data/
└── tenants/
    └── t_3f9a1c05/            folder name derived from the tenant id
        ├── rules/             optional overrides of the shipped rules, one file per kind
        ├── sources/           raw records and uploaded originals, never changed
        ├── wiki/
        │   ├── index.md       catalog: every tenant page with its summary
        │   ├── log.md         one line per job
        │   ├── experience/    playbooks, e.g. refund-handling.md
        │   ├── expertise/     concept pages
        │   ├── graph/         one description per schema, e.g. commerce.md
        │   ├── groups/g_5c0e91a7/   a group's experience/, expertise/, decisions/
        │   └── users/u_7b21e0d4/    a user's preferences.md and decisions/
        ├── pinakes.sqlite     jobs, proposals, API keys, audit log, schemas and spaces,
        │                      document versions, embeddings, lint and acquisition state
        ├── graph.sqlite       nodes and edges (the default SQLite graph store)
        ├── logs/              search traces, one JSONL file per day
        ├── history/           kept wiki versions for replays
        └── index/             tantivy index and HNSW graphs; derived, safe to delete
```

With the Neo4j backend (`[graph] backend = "neo4j"`, compose profile `neo4j`), nodes and edges
live in one Neo4j database, every node and edge carrying its tenant id and every query filtering
on it. Each tenant records which store holds its graph data; `pinakes-migrate-graph --to
sqlite|neo4j` moves it, with the service stopped.

## HTTP API

Every call carries credentials: the tenant's API key as `Authorization: Bearer <key>` (plus
`X-User-Id`, `X-User-Groups`), or a JWT. Admin routes take the admin key or a console session.

| Method | Path | What it does |
|---|---|---|
| POST | `/ingest/experience`, `/ingest/decision`, `/ingest/note`, `/ingest/preference` | Add a record; 202 with a job id |
| POST | `/ingest/expertise` | Add a document, as JSON or a file; 202 with a job id; the result carries the document id |
| POST | `/ingest/graph/schema` | Create or extend a schema; the description is written while the call waits |
| POST | `/ingest/graph/space` | Create a graph space with its visibility |
| POST | `/ingest/graph/data` | Add or replace nodes and edges in a space |
| GET | `/jobs`, `/jobs/{id}` | The caller's jobs, and one job's status and result |
| POST | `/search` | The only search entry |
| POST | `/read`, `/graph`, `/graph/path` | Follow-ups on ids `/search` returned |
| POST, GET | `/graph/draft` | Propose graph changes into the space's draft; show the draft (`?space=`) |
| GET | `/graph/schemas`, `/graph/spaces`, `/graph/nodes` | Browse schemas, spaces and a space's nodes (for people and host code) |
| PUT, DELETE | `/graph/spaces/{space}` | Change a space's visibility and members; delete a space (admin) |
| GET | `/pages`, `/pages/{path}` | List pages by kind; read one (`?html=true` rendered) |
| PUT | `/pages/{path}` | Edit a page by hand; verified before it is saved |
| GET | `/history/{path}` | The texts a page had in the kept versions |
| GET | `/proposals` | Proposals waiting for review |
| POST | `/proposals/{id}/approve`, `/proposals/{id}/reject` | Decide a proposal (admin) |
| POST, GET | `/lint` | Queue a lint run; the last run's findings |
| GET | `/acquisition` | The tenant's acquisition settings, budget, last crawl and run |
| DELETE | `/users/{user_id}` | Delete a user's data in the tenant |
| POST, GET | `/admin/tenants` | Create a tenant and its first key; list tenants |
| DELETE | `/admin/tenants/{id}`, `/admin/tenants/{id}/groups/{group_id}` | Delete a tenant, or a group's data |
| PUT | `/admin/tenants/{id}/acquisition` | Turn acquisition on or off for a tenant, with its allowlist |
| | `/admin/settings`, `/admin/model`, `/admin/environment`, `/admin/embedding`, `/admin/audit` | The console's settings, model, environment, embedding switch and audit logs |
| GET | `/status` | Librarian, embeddings, converters, graph store, jobs and proposals waiting |
| GET | `/health` | Liveness |
| GET | `/ui` | The web console |

**Conventions.** Keys are snake_case. A wiki page is named `page` everywhere except `/read`
items, which use `id` since they may be raw sources. A node is
`{id, space, type, key, name, aliases, properties, depth}`; an edge is
`{space, from, from_name, type, to, to_name, via, match}`. Lists are always present, possibly
empty; a single value that doesn't apply is left out, never `null`. Errors are
`{"error": {"code", "message"}}`.

## MCP server

The MCP server is at `/mcp` (Streamable HTTP), with the same credentials as HTTP. Each tool calls
the HTTP handler it stands for, so it answers, checks and traces exactly like it.

| Tool | Same as | Parameters |
|---|---|---|
| `search` | `POST /search` | `query`, `keywords`, optional `mode`, `session` |
| `read` | `POST /read` | `ids`, optional `heading`, `session` |
| `graph` | `POST /graph` | `node`, optional `depth`, `relations`, `session` |
| `path` | `POST /graph/path` | `from`, `to`, optional `relations`, `session` |
| `record_decision_or_lesson` | `POST /ingest/note` | `topic`, `content`, optional `context`, `entities`, `session` |
| `record_preference` | `POST /ingest/preference` | `statement`, optional `entities` |
| `propose_graph_change` | `POST /graph/draft` | `space`, `changes`, `reason` |

The tools keep only what an agent has to judge; host code uses the HTTP API for everything else.
Their descriptions (`prompts/tools/`) teach the retrieval strategy: wrap the request into a query
with short bilingual keywords, read the slots before opening pages, follow routes, and retry or
switch to `semantic` on a weak slot. They also ask the agent to record, in the same turn and
without being asked, lasting preferences and standing facts about the user, decisions, and
lessons learned. `session` comes back from `search` and is passed on to the other tools.

**Test chat.** With `PINAKES_DEV=1`, the console's Search page gets a chat tab (and `/dev` a
page) where the librarian's model answers with the read tools, streaming each step.
