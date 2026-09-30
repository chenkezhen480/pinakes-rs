# Test set

The questions every change to search is measured against (design doc, "Search signals" → "The
test set"). It is a **starter set**: synthetic but realistic content across six business
scenarios, written so M2 can be built and measured. Real questions from real deployments should
replace or extend it over time; the thesis experiments need them.

`cargo test --test testset` checks that the set is internally consistent (see "Validation").

## Scenarios

Each scenario is one tenant, so the set also exercises tenant isolation.

| Tenant | Business | Main language | Graph space |
|---|---|---|---|
| `shopline` | E-commerce marketplace | Chinese | `commerce` |
| `cloudnote` | B2B note-taking SaaS | English | `accounts` |
| `swiftcargo` | Cross-border logistics | Mixed | `network` |
| `paybridge` | Payments and risk | English | `payments` |
| `mediclinic` | Clinic network, bookings and billing | Chinese | `care` |
| `factoryops` | Factory equipment maintenance | Chinese | `plant` |

Every tenant has all five knowledge kinds: agent experience playbooks, vertical expertise pages,
one graph schema description with instance data, and two users with preferences and decision
history.

## Layout

```
testset/
├── README.md
├── tenants/<tenant>/
│   ├── tenant.yaml               id, business, users, graph spaces
│   ├── wiki/                     same layout as a tenant's data/…/wiki folder
│   │   ├── experience/*.md
│   │   ├── expertise/*.md
│   │   ├── graph/<space>.md      schema description (nodes, relations)
│   │   └── users/<user-id>/      raw user id here; the loader maps it with user_dir_name()
│   │       ├── preferences.md
│   │       └── decisions/*.md
│   └── graph/<space>.json        nodes and edges, what /ingest/graph will receive (M3)
└── questions/<tenant>.yaml
```

Pages are written the way the librarian would write them: front matter with `kind`, `title`,
bilingual `aliases` and `keywords`, a 1–3 sentence `summary`, `about` anchors, `sources` and
`updated`. Every memory page (experience, decision, preference) has at least one anchor.

Anchors (`about`) are either page paths (`expertise/refund-regulations`) or entities:
`space:type` for a node type, `space:type/key` for one node (`commerce:order/1042`).

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
        - { from: "commerce:order/1042", type: placed_by, to: "commerce:customer/c_5521" }
    expect_empty: [relations]    # optional; slots that must come back empty or weak
    must_not: [users/lily/decisions/double11-coupon-budget]   # optional; must appear nowhere
    trap: [expertise/refund-regulations]   # optional; prose pages sharing the query's keywords
    notes: "why this question exists"
```

User pages are referenced with the raw user id (`users/lily/...`), like the fixture folders.

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
| `isolation` | Another user's memories never appear | M2 |

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
- every question's expected pages exist, sit in the right slot for their kind, and belong to the
  question's user when user-scoped; expected edges exist in the graph data;
- ids are unique, categories and milestones are consistent, and each of the five knowledge kinds
  has at least 30 questions.

## Open question it assumes an answer to

Graph spaces here hold instance data (orders, shipments, machines…), not only schemas. This is
open question 4 in the design ("Instance data in graph spaces"); the `graph` questions depend on
it. If the answer is "schema only", those questions need rewriting before M3.
