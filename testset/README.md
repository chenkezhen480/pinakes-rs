# Test set

The questions every change to search is measured against (design doc, "Search signals" → "The
test set"). It is a **starter set**: synthetic but realistic content across six business
scenarios, written so M2 can be built and measured. Real questions from real deployments should
replace or extend it over time; the thesis experiments need them.

`cargo test --test integration testset::` checks that the set is internally consistent (see "Validation").

## Scenarios

Each scenario is one tenant, so the set also exercises tenant isolation.

| Tenant | Business | Main language | Graph schema | Graph spaces |
|---|---|---|---|---|
| `shopline` | E-commerce marketplace | Chinese | `commerce` | `mainland` (国内站), `crossborder` (跨境站) |
| `cloudnote` | B2B note-taking SaaS | English | `accounts` | `customers`, `jin_prospects` (private to jin) |
| `swiftcargo` | Cross-border logistics | Mixed | `network` | `forwarding` |
| `paybridge` | Payments and risk | English | `payments` | `acquiring`, `investigations` (restricted: marco, group risk-team) |
| `mediclinic` | Clinic network, bookings and billing | Chinese | `care` | `clinics` |
| `factoryops` | Factory equipment maintenance | Chinese | `plant` | `suzhou` (苏州工厂), `wuhan` (武汉工厂, restricted: sarah, group wuhan-maintenance) |

Spaces are shared unless marked otherwise. In `tenant.yaml` a space can say
`visibility: private` with an `owner`, or `visibility: restricted` with
`members: { users: [...], groups: [...] }`.

Every tenant has all five knowledge kinds: agent experience playbooks, vertical expertise pages,
one graph schema description with one or more graph spaces of instance data, and two users with
preferences and decision history. `shopline` and `factoryops` have two spaces on one schema, with
a key that exists in both (order 1042, machine CNC-07), to test that spaces keep data apart.

## Layout

```
testset/
├── README.md
├── tenants/<tenant>/
│   ├── tenant.yaml               id, business, users, graph schemas and spaces
│   ├── wiki/                     same layout as a tenant's data/…/wiki folder
│   │   ├── experience/*.md
│   │   ├── expertise/*.md
│   │   ├── graph/<schema>.md     schema description (nodes, relations); spaces get no page
│   │   ├── groups/<group-id>/    raw group id here; the loader maps it with group_dir_name()
│   │   │   └── experience/, expertise/, decisions/
│   │   └── users/<user-id>/      raw user id here; the loader maps it with user_dir_name()
│   │       ├── preferences.md
│   │       └── decisions/*.md
│   └── graph/<space>.json        one space's nodes and edges, for /ingest/graph/data (M3)
└── questions/<tenant>.yaml
```

Pages are written the way the librarian would write them: front matter with `kind`, `title`,
bilingual `aliases` and `keywords`, a 1–3 sentence `summary`, `about` anchors, `sources` and
`updated`. Every memory page (experience, decision, preference) has at least one anchor.

Anchors (`about`) are either page paths (`expertise/refund-regulations`) or entities:
`schema:type` for a node type (`commerce:order`), `space:type/key` for one node
(`mainland:order/1042`). Space names never equal schema names, so the two can't be confused.

## Questions

```yaml
tenant: shopline
questions:
  - id: shopline-exp-01          # unique across the set
    category: experience         # see below
    milestone: M2                # first milestone that can answer it
    lang: en                     # language of the query: en, zh or mixed
    user: lily                   # optional; the X-User-Id of the caller
    query: "..."                 # what the agent sends as `query`
    keywords: ["...", "..."]     # optional; what a good agent would pass as `keywords`
    expect:                      # what must show up, per slot
      memories: [experience/partial-refund-after-shipment]
      knowledge: [expertise/refund-regulations]
      relations:
        - { from: "mainland:order/1042", type: placed_by, to: "mainland:customer/c_5521" }
    expect_empty: [relations]    # optional; slots that must come back empty or weak
    must_not: [users/lily/decisions/double11-coupon-budget]   # optional; must appear nowhere
    must_not_spaces: [investigations]   # optional; nothing from these spaces may appear
    trap: [expertise/refund-regulations]   # optional; prose pages sharing the query's keywords
    notes: "why this question exists"
```

User and group pages are referenced with raw ids (`users/lily/...`, `groups/risk-team/...`),
like the fixture folders. A question's caller brings the groups listed for its user in
`tenant.yaml` (`users: - { id: marco, ..., groups: [risk-team] }`), exactly as a host would send
them; a question without a user has no groups.

### Categories and milestones

| Category | What it tests | Milestone |
|---|---|---|
| `experience` | Agent playbooks come back in the memories slot | M2 |
| `expertise` | Domain pages come back in the knowledge slot | M2 |
| `decision` | The caller's decision history comes back in memories | M2 |
| `preference` | The caller's profile comes back in memories | M2 |
| `graph` | Real edges come back in the relations slot, even when prose shares the keywords (`trap`) | M3 |
| `hop` | A memory plus the knowledge page it is anchored to (`via: memory`) | M5 |
| `cross_lang` | Query and page in different languages with no alias in common; needs `mode: semantic` | M5 |
| `negative` | Out-of-scope question: every slot empty or weak | M2 |
| `isolation` | Another user's or another group's memories never appear | M2 |
| `space_isolation` | Nothing from a graph space the caller can't see appears: no edges, no node or space names | M3 |

A question counts as passed when every expected item is in the top `k` of its slot (default slot
sizes: memories 3, knowledge 3, relations 5), nothing in `must_not` appears anywhere, and every
slot in `expect_empty` is empty or marked weak. Other hits in a slot don't fail a question.

## Validation

`tests/testset.rs` checks, without running any search:

- every page parses, its `kind` matches its folder, and it has a title, summary and aliases;
- every memory page has at least one anchor, and every anchor resolves to a page, a node type or a
  node of the same tenant;
- graph data only uses node and relation types from the schema, with matching ends, and every
  edge's ends exist;
- every question's expected pages exist, sit in the right slot for their kind, and are visible to
  the question's caller (user and groups); `must_not` pages are not; expected edges exist in the
  graph data;
- every anchor points only at pages and nodes that all readers of the page can see;
- space visibility is well-formed; every expected edge is in a space the caller can see, every
  `must_not_spaces` entry is one the caller can't see, and tenant-level pages anchor only to
  shared spaces;
- ids are unique, categories and milestones are consistent, and each of the five knowledge kinds
  has at least 30 questions.

## Graph data and the wiki

As decided for open question 4 (2026-09-30): a graph schema backs any number of graph spaces.
Only a schema has a wiki page (`wiki/graph/<schema>.md`, written by the librarian when the schema
is created or changed). Spaces are declared in `tenant.yaml` with a display name and aliases, and
each space's nodes and edges live in `graph/<space>.json`, which M3 sends to
`/ingest/graph/space` and `/ingest/graph/data` and which ends up in Neo4j, never in the wiki.
Node names and aliases, and space names and aliases, feed the name matcher: that is how
"林晓下过哪些订单？" finds its starting node and "跨境站的订单1042" picks one space.
