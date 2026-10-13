# Comparing Pinakes with Mem0, Graphiti, LightRAG and Hindsight

The scripts behind [docs/comparison.md](../docs/comparison.md): the same test set, LLM, agent
(Pi) and, within a round, embedding model for every system. They are measurement scripts, not part
of the service; everything they make stays in this folder and is git-ignored.

Two rounds were run. Round 1 (2026-10-12): Pi 0.73 with `pi-mcp-extension`, jina-embeddings-v2
for every system, Mem0, Graphiti, LightRAG and files + grep. Round 2 (2026-10-13): Pi 1.1 with its
own MCP client, Qwen3-Embedding-0.6B for the others (Pinakes keeps its built-in jina), and
Hindsight added. The scripts here are round 2's; round 1 differs only where noted below.

## What runs

| Port | What | Script |
|---|---|---|
| 8070 / 8072 | the embedding model as an OpenAI-compatible `/v1/embeddings`: jina-embeddings-v2-base-zh (round 1) or Qwen3-Embedding-0.6B (round 2), fp32 ONNX | `embed_server.py` (`EMBED_MODEL=jina` or `qwen3`; `qwen3.py`) |
| 8071 | a forwarder to DeepSeek's OpenAI-compatible API that counts every call (`llm_calls.jsonl`); the key is read from `../model.conf` here and nowhere else | `llm_proxy.py` |
| 8081 / 8082 / 8083 / 8086 | Mem0, Graphiti, LightRAG, Hindsight: each system's read tools over MCP and `/retrieve` for the speed runs | `sys_mem0.py`, `sys_graphiti.py`, `sys_lightrag.py` (`serve.sh <system>`), `sys_hindsight.py` |
| 8084 | Pinakes (release build) on its own data folder with the test set | `setup.sh` |
| 7688 | Neo4j for Graphiti | `setup.sh` |
| 8888 | Hindsight's own API (embedded PostgreSQL; LLM through 8071, embeddings from 8072, a local reranker) | `hindsight_native.sh` |

`corpus.py` reads the test set into pages, graph spaces and scopes (tenant, user, group,
restricted space); `mini_mcp.py` is the small MCP server the four adapters share (the caller's
tenant and user come from headers, as Pinakes takes them from its key and `X-User-Id`).

## Running it again

From Git Bash, with `model.conf` in place, Docker, `uv` and Node:

```bash
cd compare
./setup.sh                                   # environments, models, Neo4j, Pi 1.1, Pinakes data
# keep these running (run_in_background):
EMBED_MODEL=qwen3 venv-embed/Scripts/python.exe embed_server.py
venv-embed/Scripts/python.exe llm_proxy.py
./hindsight_native.sh bench 8888 hindsight   # HS_RERANKER=<folder> for another cross-encoder

source q3env.sh                              # the adapters embed with Qwen3 (8072, 1024 dims)
for s in mem0 graphiti lightrag; do BENCH_STORE=$s-q3 venv-$s/Scripts/python.exe sys_$s.py ingest; done
venv-hindsight/Scripts/python.exe sys_hindsight.py ingest
PINAKES_CONFIG=$PWD/pinakes.toml ../target/release/pinakes.exe
BENCH_STORE=mem0-q3 ./serve.sh mem0; ./serve.sh graphiti; BENCH_STORE=lightrag-q3 ./serve.sh lightrag
venv-hindsight/Scripts/python.exe sys_hindsight.py serve

# retrieval alone: speed and ranks (results/retrieval-<label>.jsonl)
PY=venv-embed/Scripts/python.exe
$PY retrieval_eval.py pinakes-kw:keyword pinakes-kw-keyword-r2
$PY retrieval_eval.py pinakes-kw:auto pinakes-kw-auto-r2
for s in mem0 graphiti lightrag; do $PY retrieval_eval.py $s $s-q3; done
$PY retrieval_eval.py hindsight hindsight
BENCH_K=10000 $PY retrieval_eval.py hindsight hindsight-all   # everything its recall returns

# the agent on every question, then the judge (results/agent-v1/, results/judge-v1-<system>.jsonl)
for s in pinakes mem0 graphiti lightrag lightrag+read hindsight files; do
  $PY run_agent.py $s --jobs 4
  JUDGE_PREFIX=judge-v1 AGENT_DIR=agent-v1 $PY judge.py $s
done
$PY report2.py > results/report2.md
```

Run the speed measurements on an idle machine: the other systems share the embedding server,
Hindsight's reranker takes the CPU while it runs, and anything else using the CPU shows in their
times.

Things that help when a run is cut short (an LLM balance running out, say):

- `mem0_reembed.py <store> <new store>` builds a store for another embedding model from an
  existing one without the LLM: same points, payloads and BM25 vectors, dense vectors made again.
  Round 2's Mem0 store was made this way from round 1's (the extraction is the same LLM's).
- `BENCH_TENANTS=a,b` resumes LightRAG's and Hindsight's ingest for those tenants;
  `BENCH_RESUME=1` resumes Graphiti's (episodes a group already holds are kept);
  `lightrag_redo.py <tenant> <scope> <page>` files one failed page again.
- `run_agent.py` and `judge.py` skip what is already in their results.

`<system>+read` gives the agent Pi's `read` as well: Pi 1.1 cuts an MCP tool's text past 20 KB
(the middle goes) and saves the whole of it to a file the agent may read.

`meaning_eval.py` compares embedding models on Pinakes' own units (meaning search alone), the
measurement behind keeping jina in Pinakes.

## Recording unprompted

`record_eval.py` measures whether the agent records what it should without being asked (the
record tools' prompts; design.md "Recording unprompted"). Pi 1.1 gets all of Pinakes' MCP tools
and one user message: `record_cases.json` (66 messages stating a lasting preference or standing
fact, making a decision or telling a lesson, and 18 that must not be recorded),
`record_holdout.json` (18 more, written after the prompts were tuned, only scored) or the test
set's 212 questions (none should be recorded). The service is a copy of the test-set data on 8089
whose librarian model points at a closed port, so record calls are accepted and nothing is filed:

```bash
cp -r pinakes-data pinakes-rec-data      # then a pinakes-rec.toml: port 8089, that folder,
                                         # model_file pointing at a model.conf with base_url http://127.0.0.1:9/anthropic
PINAKES_CONFIG=$PWD/pinakes-rec.toml ../target/release/pinakes.exe
$PY record_eval.py run v2 cases --repeat 2; $PY record_eval.py run v2 holdout --repeat 2
$PY record_eval.py run v2 questions
$PY record_eval.py score base v2         # results/record-<label>/
```

Decisions and lessons now share one tool (design.md "One tool for decisions and lessons"). The
runs that decided it, all kept in `results/`:

- `record-base`: the prompts before (two tools, `record_decision` and `record_experience`);
  `record-v2`: the prompts asking to record unprompted (the two tools); `record-v3`: one sentence
  more, asking for both records, dropped.
- `record-merged`: the merged tool (`merge-experiment.patch` on the code of then, with
  `RECORD_TOOLS=search,read,graph,path,record_decision_or_lesson,record_preference,propose_graph_change`).
  Its notes split three ways: `classified.json` by a first classifier (`merge_classify.py`),
  `split-final.json` and `split-final2.json` by `prompts/note.md` before and after its rule that a
  decision's reasons belong to the decision, sent as the service sends it (`note_eval.py`):

```bash
$PY note_eval.py merged split-final2 v2  # splits the notes once (tag `note`), then scores against v2
```

## Round 1

Pi 0.73 with `pi-mcp-extension` (`pi install npm:pi-mcp-extension -l` into `pi/template`, loaded
with `-e`), jina for everything (`embed_server.py` with `EMBED_MODEL=jina` on 8070, the adapters
without `q3env.sh`, stores `mem0`, `graphiti`, `lightrag`), results in `results/agent/` and
`results/judge-<system>.jsonl`, tables from `report.py`.

## Speed at size

`gen_scale.py 10000` writes filler pages made from the other five scenarios' pages (never
shopline's). Pinakes takes them as files in a second data folder (port 8085); Mem0 and LightRAG
take them into copies of their stores without the LLM (`BENCH_STORE=mem0-scale python
sys_mem0.py scale` or `scale_mem0_bulk.py`, `BENCH_STORE=lightrag-scale python sys_lightrag.py
scale`), Graphiti into its shopline group (`scale_graphiti.py`). Then `retrieval_eval.py` with
`BENCH_TENANT=shopline` and `BENCH_PINAKES`, `BENCH_KEYS` or `BENCH_PORT` pointing at the scale
servers. (Round 1 only, with jina.)

## Fairness, as built

- Every system gets the same pages; each graph space goes through the system's own path for
  structured data.
- Scopes: one partition per tenant scope, and only the caller's are searched (Mem0 one
  `user_id` at a time, merged by score; Graphiti all groups in one query; LightRAG one workspace
  at a time, merged rank by rank; Hindsight one bank per tenant, each item tagged with its scope,
  recalled with the caller's tags `any_strict`). A request's embeddings are cached for that
  request, so a caller with several scopes doesn't pay for the same embedding twice.
- Tools: the names and descriptions of each system's own MCP server where it has one (Graphiti's
  `search_nodes` and `search_memory_facts`, Mem0's `search_memories`, Hindsight's `recall` and
  `reflect`); none write.
- Defaults otherwise: Hindsight's recall budget (`high`) and reranker
  (`cross-encoder/ms-marco-MiniLM-L-6-v2`) are its own; the multilingual one was measured beside it.
- The judge sees everything the asker may read, so it can tell a true fact from an invented one,
  and page ids are not graded.
