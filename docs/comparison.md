# Pinakes compared with Mem0, Graphiti, LightRAG and Hindsight

Measured with a real agent (Pi) in two rounds: 2026-10-12 (Pi 0.73, one embedding model for all,
everything below up to "Round 2") and 2026-10-13 ("Round 2": Pi 1.1, a current embedding model
for the others, Hindsight added). The scripts and how to run them again are in
[`compare/`](../compare/README.md); the design is in [design.md](design.md).

## What was compared, and why these

The most cited open-source projects that keep knowledge or memory for LLM agents and can be
self-hosted. Each one is backed by a paper and widely used:

| System | Version | What it is | Backed by |
|---|---|---|---|
| [Mem0](https://github.com/mem0ai/mem0) | mem0ai 2.2.1 (OSS) | Memory layer: an LLM extracts facts from what it is given; hybrid search (vectors, BM25, spaCy entity boost) | Mem0 paper (arXiv 2504.19413); Apache-2.0 |
| [Graphiti](https://github.com/getzep/graphiti) | graphiti-core 0.30.2, Neo4j 5.26 | Zep's temporal knowledge graph: an LLM extracts entities and facts per episode; hybrid search over facts and entities | Zep paper (arXiv 2501.13956); Apache-2.0 |
| [LightRAG](https://github.com/HKUDS/LightRAG) | lightrag-hku 1.5.7, default local storages | Graph RAG over documents: an LLM extracts entities and relations per chunk; the query's keywords are extracted by the LLM, then graph and vectors are searched ("mix") | HKU (arXiv 2410.05779); MIT |
| [Hindsight](https://github.com/vectorize-io/hindsight) (round 2) | hindsight-api 0.10.3, embedded PostgreSQL | Agent memory: an LLM extracts facts from what is retained and consolidates them into observations; recall searches four ways (semantic, keyword, graph, time) and reranks with a local cross-encoder; `reflect` answers with the LLM over the memories | claims the best LongMemEval score (2026-01); MIT |
| Files + grep | Pi's own read, grep, find, ls | The pages as Markdown files in a folder (the "LLM wiki" pattern), no service | baseline |

Microsoft GraphRAG was left out: its indexing (community reports) is costly and it has no notion
of users or scopes; LightRAG is its lighter successor on the same idea. Letta (MemGPT) was left
out because it is an agent runtime, not a service an agent calls.

## How

Everything the systems share was made the same, so the differences are theirs:

- **The corpus**: the test set (`testset/`, six scenarios, Chinese and English): 111 pages
  (experience, expertise, decisions, preferences, graph schemas) and 10 graph spaces (112 nodes,
  106 edges). Pinakes takes the pages as they are; each other system gets every page through its
  normal ingest (Mem0 `add`, Graphiti text episodes, LightRAG `ainsert`) and each graph space
  through its structured path (Graphiti a JSON episode, LightRAG `ainsert_custom_kg`, Mem0 one
  memory per edge as written).
- **The model**: DeepSeek `deepseek-flash` (V4.1 Flash), thinking off, for every system's ingest
  and queries, through a local forwarder that counts every call.
- **The embedding model**: jina-embeddings-v2-base-zh, the same revision Pinakes uses. The others
  get it from a local OpenAI-compatible server (fp32 ONNX, 4 threads, about 33 ms a query
  embedding); Pinakes runs its 8-bit copy in-process (about 14 ms).
- **Who may see what**: Pinakes enforces tenants, users, groups and graph-space access itself.
  The others have partitions (Mem0 `user_id`, Graphiti `group_id`, LightRAG workspaces): each
  tenant scope (the tenant, each user, each group, each restricted space) got its own, and the
  adapter searches only the scopes the caller may read, merged by score (Mem0), jointly (Graphiti
  ranks several groups in one query) or rank by rank (LightRAG).
- **The agent**: Pi 0.73 with `deepseek-v4-flash`, thinking off, the same system prompt for all
  ("look things up with the knowledge tools ..., say so when they hold nothing"), connected by MCP
  (`pi-mcp-extension`). Each system offers its own read tools, as its MCP server does: Pinakes
  `search`, `read`, `graph`, `path`; Mem0 `search_memories`; Graphiti `search_nodes`,
  `search_memory_facts`; LightRAG one tool returning its retrieved context. Writing tools were
  left out everywhere.
- **The judge**: DeepSeek `deepseek-flash` with thinking on, blind to the system. It sees
  everything the asker may read, the question, the reference (the expected pages and edges), what
  the asker must not learn, and the answer, and gives 2 (answered), 1 (partly, or with invented
  company facts) or 0, plus whether the answer leaks forbidden content.
- **The machine**: Intel i5-11500H (6 cores), 16 GB, Windows 11; Pinakes release build.

## Retrieval speed and quality, without the agent

All 212 questions, one at a time after a warm-up, timed from the client over HTTP on 127.0.0.1.
Each system returns about ten items (Pinakes: five pages a slot and ten edges). R@k: the share of
expected pages among the first k distinct pages returned (Pinakes: in the slot the page belongs
to).

| Configuration | Median | p95 | R@1 | R@3 | R@5 | Expected edges found | Leaks | Returned per search |
|---|---|---|---|---|---|---|---|---|
| Pinakes, `keyword`, query + keywords | **3.6 ms** | 4.4 ms | 0.912 | 0.936 | 0.936 | 50/50 | 0 | 1.6 KB |
| Pinakes, `auto`, query + keywords | 19.0 ms | 27.0 ms | **0.971** | **1.000** | **1.000** | 50/50 | 0 | 2.4 KB |
| Pinakes, `keyword`, query only | 3.4 ms | 4.2 ms | 0.819 | 0.877 | 0.889 | 42/50 | 0 | 2.9 KB |
| Pinakes, `auto`, query only | 18.1 ms | 23.4 ms | 0.947 | 0.988 | 0.988 | 42/50 | 0 | 3.5 KB |
| Mem0 | 106 ms | 240 ms | 0.719 | 0.953 | 0.971 | 49/50 | 0 | 2.4 KB |
| Graphiti | 80 ms | 106 ms | 0.561 | 0.749 | 0.766 | 40/50 | 0 | 1.3 KB |
| LightRAG (`mix`) | 854 ms | 1105 ms | 0.520 | 0.982 | 0.994 | 50/50 | 0 | 42 KB |

Where the time goes, for a caller with one scope (134 of the questions), median: Pinakes 3.6 ms
(keyword) or 19.6 ms (`auto`, of which about 14 ms embed the query); Graphiti 84 ms (33 ms the
query embedding, the rest Neo4j's full-text, vector and graph queries); Mem0 94 ms (embedding,
BM25 sparse encoding, spaCy entities, embedded Qdrant); LightRAG 866 ms (an LLM call extracts the
query's keywords: 753 ms median, then entity, relation and chunk vectors). Mem0 grows with the
caller's scopes (243 ms with four), since it searches one `user_id` at a time.

What the numbers say:

- **Speed.** Pinakes answers in 3 to 4 ms by keywords (22 to 29 times faster than Graphiti and
  Mem0) and in about 19 ms with meaning (4 to 6 times faster; most of it is the query's embedding,
  which the others pay too), and 45 to 240 times faster than LightRAG, whose every search waits
  on the LLM. Pinakes calls no model on the read path. The gap widens with the tenant's size (see
  "Speed at size").
- **Quality.** With meaning (`auto`) and the keywords an agent passes, every expected page is in
  the first three, and 97% first. Mem0 and LightRAG find nearly as much by the third or fifth item
  but rank it first far less often (72% and 52%). Graphiti's facts lose what the extraction
  dropped (preferences: 12 of 30 pages found). LightRAG returns about 42 KB a search, about the
  whole tenant at this size, so finding things is no achievement there; R@k measures its ranking
  of chunks. (A first run merged LightRAG's workspaces one after another, which put a user's own
  chunks after twenty of the tenant's: R@3 0.632, and with the agent 94.8% answered, preferences
  1.30. They are now merged rank by rank like the others, and both were run again.)
- **Query only.** Without keywords Pinakes loses graph edges (42/50): the name matcher finds a
  node from the keywords an agent writes ("order 1042"), and a bare question may not name it the
  same way. Agents pass keywords (the tool descriptions ask for them); a host calling `/search`
  directly should too.
- **No leaks** in any system: the partitions held.

## With the agent

Pi answered all 212 questions once per system (1,060 runs, all finished), and the judge graded
each answer.

| System | Answered | Partly | Wrong or missing | Mean (0-2) | Leaks | Tool calls | Tool output | Input tokens (median) | Model cost, 212 questions |
|---|---|---|---|---|---|---|---|---|---|
| **Pinakes** | **99.1%** | 0.9% | 0.0% | **1.99** | 0 | 2.0 | 3.4k chars | 9.2k | $0.075 |
| LightRAG | 97.6% | 0.5% | 1.9% | 1.96 | 0 | 1.8 | 79.3k chars | 25.4k | $0.679 |
| Mem0 | 95.8% | 1.9% | 2.4% | 1.93 | 0 | 1.9 | 3.9k chars | 3.0k | $0.063 |
| Files + grep | 91.0% | 0.9% | 8.0% | 1.83 | 0 | 4.6 | 3.1k chars | 19.7k | $0.091 |
| Graphiti | 67.9% | 16.5% | 15.6% | 1.52 | 0 | 2.2 | 4.6k chars | 3.5k | $0.077 |

A question took 6 to 14 s (median per system), nearly all of it the model's turns; the runs
shared the machine with other work to different degrees (the speed-at-size fills), so their times
aren't compared.

By category (mean score):

| Category | n | Pinakes | LightRAG | Mem0 | Files + grep | Graphiti |
|---|---|---|---|---|---|---|
| experience | 38 | 2.00 | 2.00 | 1.95 | 1.89 | 1.47 |
| expertise | 36 | 2.00 | 2.00 | 2.00 | 1.78 | 1.50 |
| decision | 31 | 1.97 | 1.97 | 1.94 | 2.00 | 1.77 |
| preference | 30 | 2.00 | 1.73 | 1.80 | 1.47 | 1.03 |
| graph | 35 | 2.00 | 2.00 | 1.91 | 1.83 | 1.66 |
| hop (memory + its knowledge page) | 12 | 1.92 | 2.00 | 2.00 | 2.00 | 1.42 |
| cross-language | 12 | 2.00 | 2.00 | 2.00 | 1.83 | 1.50 |
| off-topic | 6 | 2.00 | 2.00 | 2.00 | 2.00 | 2.00 |
| another user's or group's memory | 8 | 2.00 | 2.00 | 1.88 | 2.00 | 2.00 |
| a hidden graph space | 4 | 2.00 | 2.00 | 2.00 | 2.00 | 1.50 |

What the runs show:

- **Pinakes answered 210 of 212 fully**; the other two were right but added a detail the
  knowledge base doesn't hold (the model's, not the search's). It lost nothing on preferences,
  where every other system did: a preference page is short and says how the user wants answers,
  which Graphiti's extraction mostly drops and which the others' searches, given "how should you
  answer me?", often don't rank.
- **LightRAG** comes next because it hands the agent about 79k characters a question (25k input
  tokens): nearly everything the asker may read, at this size. That costs nine times the model
  spend of the others, and would not hold for a larger tenant (53 KB a search at 10,000 pages,
  with the agent's window filling up). Its misses are preferences: among that much text, the
  agent said the asker's own preference page wasn't there.
- **Mem0** answers about as well with an eighth of the text. Its misses are answers that said the
  knowledge base has nothing when it had (a decision, a preference, a graph edge): its short
  facts sometimes don't match the question's words or meaning well enough.
- **Files + grep** works for a small wiki, but the agent needs 4.6 tool calls and 20k tokens a
  question, and misses what its grep words don't match (cross-language, preferences).
- **Graphiti** loses the detail its extraction drops: steps, numbers and preferences that don't
  become entity-to-entity facts.
- **Input tokens** (summed over the agent's turns): Pinakes' are three times Mem0's and Graphiti's.
  Its tool descriptions and server instructions teach the retrieval strategy (the first turn's
  context is 2.5k tokens against 0.9k to 1k), and the agent takes 2.9 turns instead of 2 (search,
  then read or walk the graph). Most of it is cached, so the model cost per question is about
  the same.
- **No leaks** anywhere. Two early "leaks" the judge reported turned out to be facts the asker may
  read on another page (see "How the judge was checked").

## Ingest

The same 111 pages and 10 graph spaces:

| System | Time | LLM calls | Input tokens (of which cached) | Output tokens | LLM calls per search |
|---|---|---|---|---|---|
| Pinakes | the pages copied in | 0 | 0 | 0 | 0 |
| Mem0 | 287 s | 111 | 0.99 M (0.87 M) | 23k | 0 |
| LightRAG | 459 s | 261 | 0.73 M (0.60 M) | 167k | 1 |
| Graphiti | 522 s | 931 | 1.35 M (0.10 M) | 103k | 0 |

This favours Pinakes and must be read with that in mind: its pages were written beforehand, the
way its librarian writes them, so their cost isn't here. The other systems had to extract from
those pages. What the table does show is what each system spends to take in a page, and that only
LightRAG calls the model again on every search.

## Speed at size

The `shopline` tenant filled up to 10,018 pages with pages made from the other five scenarios
(`compare/gen_scale.py`; never shopline's own, so its 34 questions keep their answers), then the
same 34 questions on an idle machine. Each system holds one unit per filler page, put in without
the LLM: Pinakes a page (31,357 vectors), Mem0 a memory (10,473 in the tenant's scope; its add
with `infer=False`, then the same points in batches through its vector store, since one add at a
time slowed past 5 s a page on embedded Qdrant), LightRAG a chunk (10,014), Graphiti one fact
between two entities (10,058 facts, 10,114 entities, written with its own models). With the
LLM's extraction the others would hold several units a page, so this is the smaller index for
them.

| | 111 pages (shopline's questions) | 10,018 pages | Growth | R@1 / R@3 at 10,018 |
|---|---|---|---|---|
| Pinakes, `keyword` | 3.5 ms (p95 4.2) | **4.3 ms** (p95 5.5) | 1.2× | 0.893 / 0.929 |
| Pinakes, `auto` | 19.8 ms (p95 25.2) | **25.5 ms** (p95 31.6) | 1.3× | 0.929 / 1.000 |
| Pinakes, `semantic` | 18.8 ms (p95 22.5) | 25.0 ms (p95 31.0) | 1.3× | 0.929 / 1.000 |
| Mem0 | 74.5 ms (p95 202) | 491 ms (p95 895) | 6.6× | 0.750 / 0.893 |
| Graphiti | 76.3 ms (p95 89) | 263 ms (p95 318) | 3.5× | 0.643 / 0.714 |
| LightRAG | 892 ms (p95 1262) | 875 ms (p95 1098) | 1.0× (the LLM call dominates) | 0.571 / 1.000 |

Pinakes' own benchmark (`tests/bench.rs`, [performance.md](performance.md)) goes on to 100,000
pages: keyword search about 10 ms, a semantic search about 3 ms past the query's embedding with
its graph index. Mem0's embedded Qdrant compares every vector in Python, so it grows with the
tenant; a Qdrant server would keep it flatter, at the cost of another service. Graphiti's time
grows with Neo4j's full-text and vector indexes. LightRAG's stays flat because the keyword call to
the LLM dominates it, and its answer grows instead (53 KB a search at 10,018 pages).

## Round 2: Pi 1.1, Qwen3 embeddings, Hindsight

Round 1's agent (Pi 0.73) and embedding model (jina v2, 2024) were no longer current, and
Hindsight had become the most cited newcomer. What changed, everything else as in round 1:

- **The agent**: Pi 1.1.0 (`@earendil-works/pi-coding-agent`, 2026-10-07) with its own MCP client
  (each system's tools exposed as built-in tools are), DeepSeek `deepseek-flash`, thinking off,
  the same system prompt.
- **The embedding model of the others**: Qwen3-Embedding-0.6B (fp32 ONNX, 1,024 dimensions, the
  last token pooled; no query instruction, since an OpenAI-style call doesn't say which text is
  the query) for Mem0, Graphiti, LightRAG and Hindsight. Pinakes keeps its built-in jina (see
  "Should Pinakes switch to Qwen3?" below).
- **The stores**: Mem0's memories as round 1 extracted them, embedded again (the same LLM's
  extraction; no new one); LightRAG's from its LLM cache (the same extraction); Graphiti's
  extracted afresh, so its graph differs from round 1's; Hindsight got every page retained as
  one item tagged with its scope (`scope:t`, `scope:u:lily`, ...) and each graph space as its
  edge lines, one bank per tenant, recalled with the caller's tags.
- **Hindsight as shipped**: recall budget `high` and its default reranker,
  `cross-encoder/ms-marco-MiniLM-L-6-v2` (English); the multilingual `BAAI/bge-reranker-v2-m3`
  was measured beside it.

### Retrieval alone

| Configuration | Median | p95 | R@1 | R@3 | R@5 | Expected edges found | Leaks | Returned per search |
|---|---|---|---|---|---|---|---|---|
| Pinakes, `keyword`, query + keywords | **3.4 ms** | 4.1 ms | 0.912 | 0.936 | 0.936 | 50/50 | 0 | 1.6 KB |
| Pinakes, `auto`, query + keywords | 18.9 ms | 24.5 ms | **0.971** | **1.000** | **1.000** | 50/50 | 0 | 2.4 KB |
| Mem0, Qwen3 | 254 ms | 543 ms | 0.731 | 0.947 | 0.959 | 44/50 | 0 | 2.5 KB |
| Graphiti, Qwen3 | 186 ms | 250 ms | 0.591 | 0.801 | 0.836 | 40/50 | 0 | 1.4 KB |
| LightRAG (`mix`), Qwen3 | 1,076 ms | 1,396 ms | 0.538 | 0.988 | 1.000 | 50/50 | 0 | 51 KB |
| Hindsight, its default reranker, first 10 | 1,911 ms | 2,749 ms | 0.404 | 0.544 | 0.556 | 43/50 | 0 | 2.0 KB |
| Hindsight, everything its recall returns | 1,895 ms | 2,716 ms | 0.404 | 0.573 | 0.667 | 50/50 | 0 | 20 KB |
| Hindsight, bge-reranker-v2-m3, first 10 | 32,711 ms | 43,354 ms | 0.860 | 0.977 | 0.982 | 48/50 | 0 | 2.0 KB |

Pinakes' rows are round 1's again (the same model and pages). The others, against round 1:

- **Qwen3 ranks a little better and costs time.** Graphiti's R@1 went from 0.561 to 0.591 and its
  R@3 from 0.749 to 0.801, Mem0's R@1 from 0.719 to 0.731; LightRAG's barely moved. Every search
  took 100 to 220 ms longer: Qwen3 embeds a query in about 120 ms on this CPU, jina in 31 ms
  (fp32). Mem0 found fewer edges (44 against 49): the edges are memories written as they are,
  and Qwen3 ranks them below the extracted facts more often.
- **Hindsight's default reranker reads English only.** Its first ten items hold 75 of the 89
  expected pages of English questions and 21 of the 81 of Chinese ones: the cross-encoder puts a
  Chinese question's answer below unrelated facts. Everything its recall returns (about 20 KB,
  its default budget of 4,096 tokens of facts) holds 170 of 171 pages and every edge, and that
  is what its tool hands the agent. With the multilingual bge-reranker-v2-m3 its first ten hold
  168 of 171 (78 of the 81 Chinese) and rank them well (R@1 0.860), at 33 s a search. Its time is
  nearly all reranking: 300 candidates through the cross-encoder on the CPU, about 1.9 s with
  MiniLM and 25 times longer with bge-reranker-v2-m3, which is 25 times larger (that run shared
  the machine with a build and the agent runs of the next section, so its time is approximate). Pages count for an observation through the facts it was consolidated from.
  About half of its items come in pairs: an observation and the fact behind it, saying the same.

### With the agent

Pi 1.1 answered all 212 questions once per system, and the judge (unchanged) graded each answer.

| System | Answered | Partly | Wrong or missing | Mean (0-2) | Round 1 mean | Leaks | Tool calls | Tool output | Input tokens (median) | Time (median) | Model cost, 212 questions |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Pinakes** | **99.1%** | 0.9% | 0.0% | **1.99** | 1.99 | 0 | 2.0 | 3.4k chars | 9.4k | 3.9 s | $0.22 |
| Hindsight | 95.3% | 2.8% | 1.9% | 1.93 | - | 0 | 1.9 | 31.9k chars | 15.3k | 15.7 s | $0.85 |
| Mem0 | 92.9% | 2.4% | 4.7% | 1.88 | 1.93 | 0 | 1.8 | 4.0k chars | 3.4k | 4.3 s | $0.17 |
| LightRAG, agent may `read` | 92.0% | 2.8% | 5.2% | 1.87 | - | 0 | 2.3 | 43.9k chars | 13.5k | 8.2 s | $0.94 |
| Files + grep | 92.5% | 1.4% | 6.1% | 1.86 | 1.83 | 0 | 4.9 | 3.0k chars | 7.5k | 4.5 s | $0.25 |
| LightRAG | 86.3% | 3.8% | 9.9% | 1.76 | 1.96 | 0 | 2.1 | 37.9k chars | 13.0k | 5.7 s | $0.83 |
| Graphiti | 62.7% | 20.3% | 17.0% | 1.46 | 1.52 | 0 | 2.3 | 5.0k chars | 3.9k | 3.9 s | $0.21 |

By category (mean score):

| Category | n | Pinakes | Hindsight | Mem0 | LightRAG, `read` | Files + grep | LightRAG | Graphiti |
|---|---|---|---|---|---|---|---|---|
| experience | 38 | 1.97 | 1.97 | 1.89 | 1.97 | 1.84 | 1.95 | 1.47 |
| expertise | 36 | 2.00 | 1.89 | 2.00 | 2.00 | 1.97 | 2.00 | 1.28 |
| decision | 31 | 2.00 | 1.97 | 1.94 | 1.94 | 1.94 | 1.90 | 1.74 |
| preference | 30 | 2.00 | 1.87 | 1.60 | 1.60 | 1.60 | 1.43 | 0.83 |
| graph | 35 | 2.00 | 1.94 | 1.86 | 1.69 | 1.91 | 1.20 | 1.69 |
| hop | 12 | 2.00 | 1.83 | 1.83 | 2.00 | 1.83 | 2.00 | 1.42 |
| cross-language | 12 | 1.92 | 2.00 | 2.00 | 2.00 | 1.75 | 2.00 | 1.50 |
| off-topic | 6 | 2.00 | 2.00 | 2.00 | 2.00 | 2.00 | 2.00 | 2.00 |
| another user's or group's memory | 8 | 2.00 | 2.00 | 2.00 | 1.75 | 2.00 | 2.00 | 2.00 |
| a hidden graph space | 4 | 2.00 | 2.00 | 2.00 | 2.00 | 2.00 | 2.00 | 1.50 |

What the runs show:

- **Pinakes scored as in round 1**: 210 of 212 fully, the other two right with a detail added
  that the knowledge base doesn't hold. A newer agent with another MCP client changed nothing.
- **Pi 1.1 cuts a tool's text past 20 KB**: it keeps the head and the tail, saves the whole to a
  file and tells the agent where. LightRAG's answers (about 50 KB a search) were cut in 198 of 212
  runs and Hindsight's (20 KB and more) in 204; Pinakes', Mem0's and Graphiti's never. LightRAG
  lost what fell in the middle, its graph relations above all (graph questions 2.00 in round 1,
  1.20 now). Given Pi's `read` as well, the agent opened the saved output in 44 runs and LightRAG
  came back to 1.87, still below round 1. Hindsight lost little to the cut: the agent opened the
  file in one of 44 runs given `read`, so that variant was left there.
- **Hindsight is second**, best after Pinakes on preferences (1.87): its `recall` description
  tells the agent to check the user's preferences proactively, and its long answer holds them.
  It is also the slowest: 15.7 s a question (about 2 s a recall, and `reflect` runs the LLM
  inside Hindsight), and its 32k characters a question cost four times Pinakes' model spend.
- **Mem0 and Graphiti dropped a little** (1.93 to 1.88, 1.52 to 1.46), mostly preferences (Mem0
  1.80 to 1.60) with the new agent and embeddings; Graphiti's graph was also extracted afresh.
- **No leaks.** The judge reported one in both LightRAG runs (`swiftcargo-iso-01`), read by
  hand: the HS code 8518.30 is on the HS code page Emma may read, and Emma's colleague's hidden
  decision never reached the tool output; the answer called the code "the one we settled on",
  which the judge rightly scored 0 as an invented decision. It is not counted above.
- **Model cost** uses Pi 1.1's price table for `deepseek-flash` ($0.30 a million input tokens,
  $1.20 output), round 1 Pi 0.73's ($0.14 and $0.28), so compare it within a round. Hindsight's
  `reflect` also calls the LLM inside Hindsight (107 calls, 1.2 M input tokens over the runs),
  which isn't in its column.

Hindsight's ingest: 222 LLM calls, 1.13 M input tokens (0.58 M cached) and 140k output for the
111 pages and 10 graph spaces, consolidation included, about 14 minutes of LLM time with three
tenants at a time: about Graphiti's cost (it was cut short once by the LLM balance and resumed,
so per-tenant times weren't kept).

### Should Pinakes switch to Qwen3?

On Pinakes' own units (`compare/meaning_eval.py`, meaning search alone, the 171 expected pages):

| Model | R@1 | R@3 | Cross-language (first 3 / first) | Off-topic below every answer | Query embedding |
|---|---|---|---|---|---|
| jina-embeddings-v2-base-zh | 0.936 | 0.988 | 12 / 12 | no | 31.5 ms fp32 (Pinakes' 8-bit: about 14 ms) |
| Qwen3-Embedding-0.6B | 0.977 | 1.000 | 12 / 11 | yes | 122.6 ms fp32 |

Qwen3 ranks better alone, but Pinakes searches with keywords and meaning together (`auto`), which
already reaches 0.971 / 1.000, so it could gain only on R@1, for four to eight times the
embedding time and a model over three times larger (2.0 GB in fp32 against 612 MB;
onnx-community's 8-bit export drifts too far, a cosine of 0.908 to fp32). Pinakes keeps jina (the user, 2026-10-13).

### LongMemEval, left out

A LongMemEval run (18 questions sampled by type, 860 chat sessions to file into every system) was
started and dropped. LongMemEval hands each system whole chat transcripts afterwards and asks
what it extracted from them. Pinakes takes chat memory the other way (decided 2026-10-13): the
agent, which already holds the conversation and runs a model, extracts what is worth keeping in
the turn and calls `record_preference` or `record_decision_or_lesson`, which file it as a job
without blocking the answer; nothing mines transcripts afterwards. Feeding it
transcripts would measure a path it doesn't have, and filing the sessions through each system's
LLM was nearly all of the cost. (What the partial run showed is out of scope for the same reason:
the librarian, reading a transcript as an expertise document, rejects some of them after its
checks.)

## How the judge was checked

Three things the first gradings got wrong, each fixed and every system graded again:

1. The judge saw only the reference pages, so an answer bringing true facts from another visible
   page was marked as inventing them, and an answer quoting a visible page that resembles a
   hidden one as leaking it (`paybridge-iso-01`: the KYC page's "a review can be postponed once,
   by up to 3 months" against Chen's hidden decision; `factoryops-iso-02`: the two-hour run-in is
   on a shared page as well as in the Wuhan group's playbook). The judge now sees everything the
   asker may read.
2. Page ids: Pinakes names user and group folders by hash (`users/u_7f43…/preferences`), and the
   judge took those as invented. Ids are no longer graded.
3. Off-topic questions ("Translate 'good morning' into French") answered from general knowledge
   were marked down; they now count as answered when nothing is presented as the company's.

Every "leak" was read by hand; none was one.

## Limits of this comparison

- **The test set was written for Pinakes** (its pages carry the aliases and keywords its librarian
  writes, and its questions follow its five kinds of knowledge). The others get the same text, but
  the shape suits Pinakes.
- **It is small**: about 20 KB a tenant, which is why LightRAG can return nearly all of it and
  files + grep still works. "Speed at size" covers search speed at 10,000 pages, not answers.
- **One model and one agent**: DeepSeek V4.1 Flash for everything, one agent (Pi, in two
  versions) and one judge of the same family; answers were read by hand only where a grade looked
  wrong. Pi 1.1's cut of long tool outputs (round 2) is one agent's choice; other agents cut at
  other sizes or not at all.
- **No reranker** was added to Mem0, Graphiti or LightRAG, and none to Pinakes; Hindsight's is part
  of it.
- **The others' adapters are ours**: the official MCP tools' names and descriptions were kept, but
  the scoping (several partitions searched and merged, Hindsight's scope tags) was written for
  this comparison. Mem0's OSS MCP server was archived in March 2026, LightRAG has none.
- **Configurations**: each system as its documentation sets it up by default (Mem0 with embedded
  Qdrant, LightRAG with its local storages, Graphiti on Neo4j, Hindsight with its embedded
  PostgreSQL and local reranker); servers (Qdrant, PostgreSQL) would change their speed at size,
  and a GPU Hindsight's reranking. "Speed at size" is round 1's only.

## Findings for Pinakes

- **A tenant's first open is tied to the request that triggers it.** `Tenant::snapshot` builds the
  snapshot (reading the pages, and embedding every page that has no stored vectors) inside the
  request's future. When the client gives up first, the request is dropped: the blocking work
  runs on but its result is thrown away, the `building` lock is free, and the next request starts
  the whole build again, alongside the abandoned one. New vectors are stored only when a build
  finishes. Opening the 10,000-page tenant with every page new (31,357 vectors to embed on a busy
  machine) never finished while a probe gave up after 20 seconds every 30 seconds; with one
  request and no timeout it took 42 minutes. A deployment whose pages arrive through the
  librarian doesn't meet it (each write embeds its own pages), but a restore, a model change or
  pages copied in would. **Fixed the same day:** the build runs as a task of its own (callers
  that give up leave it running, later ones wait for it), the graph's load likewise, and new
  vectors are stored 256 at a time as they are made (`vectors::build_keeping`), so a build cut
  short keeps them. Checked by `tenants::tests::a_first_search_that_gives_up_leaves_the_build_running`
  (fails without the fix: every unit embedded twice) and
  `vectors_made_before_a_failure_are_kept`.
- In three of 424 Pinakes tool calls the agent passed an id the tools don't take: a node type
  (`payments:merchant`) from `entities` to `graph`, and a source file name from a page's
  `sources` to `read`. The error told it what to pass and it recovered, but `graph` could take a
  node type (listing its nodes), or `entities` could keep types apart from nodes.
- Its read tools' descriptions and instructions add about 1.6k tokens to every turn; they are
  cached, but a shorter version would matter for agents without prompt caching.
