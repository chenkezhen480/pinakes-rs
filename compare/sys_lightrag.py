"""LightRAG (lightrag-hku 1.5.7, HKU) over the test set, default local storages
(JSON KV, NanoVectorDB, NetworkX).

ingest: every wiki page through ainsert (chunked, the LLM extracts entities and relations), every
        graph space through ainsert_custom_kg (its documented path for a given graph). One
        workspace per tenant scope.
serve:  one tool returning LightRAG's retrieved context (aquery_data, mode "mix": the knowledge
        graph plus vector-retrieved chunks, the default), and /retrieve, on port 8083. The query's
        keywords are extracted once (one LLM call, LightRAG's design) and reused for the caller's
        other scopes. No LLM cache on queries, so repeated questions cost what a new one does.
"""

import asyncio
import json
import os
import pathlib
import sys
import time
from functools import partial

import corpus
import mini_mcp

HERE = pathlib.Path(__file__).parent
STORE = HERE / "stores" / os.environ.get("BENCH_STORE", "lightrag")
PORT = int(os.environ.get("BENCH_PORT", "8083"))
EMBED_URL = os.environ.get("EMBED_URL", "http://127.0.0.1:8070/v1")
EMBED_MODEL_NAME = os.environ.get("EMBED_MODEL_NAME", "jina-embeddings-v2-base-zh")
EMBED_DIM = int(os.environ.get("EMBED_DIM", "768"))

# A query's embeddings, kept for one request when serving: LightRAG searches one workspace at a
# time, so a caller's scopes would embed the same texts again (cleared per request).
EMB_CACHE: dict[str, object] | None = None


async def make(tenant: str, scope: str, tag: str, cache: bool):
    from lightrag import LightRAG
    from lightrag.kg.shared_storage import initialize_pipeline_status
    from lightrag.llm.openai import openai_complete_if_cache, openai_embed
    from lightrag.utils import EmbeddingFunc

    async def llm(prompt, system_prompt=None, history_messages=None, **kw):
        kw.pop("hashing_kv", None)
        return await openai_complete_if_cache(
            "deepseek-flash", prompt, system_prompt=system_prompt, history_messages=history_messages or [],
            base_url=f"http://127.0.0.1:8071/{tag}/v1", api_key="sk-local", **kw)

    async def embed(texts, **kw):
        import numpy as np

        if EMB_CACHE is None:
            return await openai_embed.func(texts, model=EMBED_MODEL_NAME,
                                           base_url=EMBED_URL, api_key="sk-local", **kw)
        todo = [t for t in dict.fromkeys(texts) if t not in EMB_CACHE]
        if todo:
            vecs = await openai_embed.func(todo, model=EMBED_MODEL_NAME,
                                           base_url=EMBED_URL, api_key="sk-local", **kw)
            EMB_CACHE.update(zip(todo, vecs))
        return np.array([EMB_CACHE[t] for t in texts])

    rag = LightRAG(
        working_dir=str(STORE),
        workspace=corpus.partition(tenant, scope),
        llm_model_func=llm,
        llm_model_name="deepseek-flash",
        embedding_func=EmbeddingFunc(embedding_dim=EMBED_DIM, max_token_size=8192, func=embed),
        enable_llm_cache=cache,
    )
    await rag.initialize_storages()
    await initialize_pipeline_status(workspace=corpus.partition(tenant, scope))
    return rag


def custom_kg(space: dict) -> dict:
    src = f"graph:{space['space']}"
    nodes = {f"{n['type']}/{n['key']}": n for n in space["nodes"]}
    lines = corpus.edge_sentences(space)
    return {
        "chunks": [{"content": "\n".join(lines), "source_id": src, "file_path": src}],
        "entities": [{"entity_name": n["name"], "entity_type": n["type"],
                      "description": f"{corpus.node_label(n)}, in the graph space {space['name']} ({space['space']})",
                      "source_id": src, "file_path": src} for n in space["nodes"]],
        "relationships": [{"src_id": nodes[e["from"]]["name"], "tgt_id": nodes[e["to"]]["name"],
                           "description": line, "keywords": e["type"], "weight": 1.0,
                           "source_id": src, "file_path": src}
                          for e, line in zip(space["edges"], lines)],
    }


async def ingest_async():
    STORE.mkdir(parents=True, exist_ok=True)
    started = time.perf_counter()
    report = {}
    only = os.environ.get("BENCH_TENANTS")  # resume: just these tenants (comma-separated)
    for t in corpus.tenants():
        if only and t not in only.split(","):
            continue
        by_scope: dict[str, list] = {}
        for p in corpus.pages(t):
            by_scope.setdefault(p["scope"], []).append(p)
        spaces = corpus.spaces(t)
        for scope in sorted(set(by_scope) | {s["scope"] for s in spaces}):
            t0 = time.perf_counter()
            rag = await make(t, scope, "lightrag-ingest", cache=True)
            ps = by_scope.get(scope, [])
            if ps:
                await rag.ainsert([p["text"] for p in ps], ids=[f"{t}/{p['id']}" for p in ps],
                                  file_paths=[p["id"] for p in ps])
            for s in spaces:
                if s["scope"] == scope:
                    await rag.ainsert_custom_kg(custom_kg(s))
            await rag.finalize_storages()
            report[corpus.partition(t, scope)] = round(time.perf_counter() - t0, 1)
            print(corpus.partition(t, scope), report[corpus.partition(t, scope)], flush=True)
    report["seconds"] = round(time.perf_counter() - started, 1)
    (STORE / (f"ingest-{only}.json" if only else "ingest.json")).write_text(json.dumps(report, indent=1))


def serve():
    import uvicorn
    from lightrag import QueryParam

    rags: dict[str, object] = {}

    async def rag_for(tenant, scope):
        key = corpus.partition(tenant, scope)
        if key not in rags:
            rags[key] = await make(tenant, scope, "lightrag-query", cache=False)
        return rags[key]

    async def context(query, tenant, user, mode="mix"):
        global EMB_CACHE
        EMB_CACHE = {}
        scopes = corpus.visible_scopes(tenant, user)
        first = await (await rag_for(tenant, scopes[0])).aquery_data(query, QueryParam(mode=mode))
        kw = (first.get("metadata") or {}).get("keywords") or {}
        rest = []
        for s in scopes[1:]:
            if not (STORE / corpus.partition(tenant, s)).exists():
                continue
            rest.append(await (await rag_for(tenant, s)).aquery_data(query, QueryParam(
                mode=mode, hl_keywords=kw.get("high_level") or [], ll_keywords=kw.get("low_level") or [])))
        per = {"entities": [], "relationships": [], "chunks": []}
        for d in [first, *rest]:
            data = d.get("data") or {}
            per["entities"].append([{"name": e["entity_name"], "type": e.get("entity_type"),
                                     "description": e.get("description"), "source": e.get("file_path")}
                                    for e in data.get("entities", [])])
            per["relationships"].append([{"from": r["src_id"], "to": r["tgt_id"], "description": r.get("description"),
                                          "keywords": r.get("keywords"), "source": r.get("file_path")}
                                         for r in data.get("relationships", [])])
            per["chunks"].append([{"content": c["content"], "source": c.get("file_path")}
                                  for c in data.get("chunks", [])])

        # Each scope's own ranking, taken in turn (a workspace's results carry no comparable score).
        def interleave(lists):
            out = []
            for i in range(max((len(x) for x in lists), default=0)):
                out += [x[i] for x in lists if i < len(x)]
            return out

        return {k: interleave(v) for k, v in per.items()}

    async def retrieve(body, tenant, user):
        c = await context(body["query"], tenant, user)
        src = lambda x: (x.get("source") or "").split("<SEP>")
        return ([{"text": f"{e['name']}: {e['description']}", "sources": src(e)} for e in c["entities"]]
                + [{"text": f"{r['from']} -> {r['to']}: {r['description']}", "sources": src(r)}
                   for r in c["relationships"]]
                + [{"text": x["content"], "sources": src(x), "kind": "chunk"} for x in c["chunks"]])

    async def query_knowledge(args, tenant, user):
        c = await context(args["query"], tenant, user, args.get("mode") or "mix")
        for x in c["entities"] + c["relationships"] + c["chunks"]:
            x.pop("source", None)
        return c

    tools = [mini_mcp.Tool(
        "query_knowledge",
        "Retrieve context from the LightRAG knowledge base (a knowledge graph of entities and "
        "relationships extracted from the documents, plus document chunks) for a query. Returns the "
        "entities, relationships and chunks relevant to it.",
        {"type": "object", "properties": {
            "query": {"type": "string", "description": "The question or topic to retrieve context for."},
            "mode": {"type": "string", "enum": ["mix", "local", "global", "hybrid", "naive"],
                     "description": "Retrieval mode (default mix: knowledge graph plus vector chunks)."}},
         "required": ["query"]},
        query_knowledge)]
    app = mini_mcp.app("lightrag", "LightRAG knowledge base: retrieve context before answering.", tools,
                       retrieve, HERE / "calls-lightrag.jsonl")
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")


async def scale_async():
    """The filler pages as chunks of the shopline tenant's workspace (custom KG with chunks only:
    embedded, no LLM extraction)."""
    if (STORE / "scale.done").exists():
        print("already filled")
        return
    rag = await make("shopline", "t", "lightrag-scale", cache=True)
    pages = [json.loads(l) for l in (HERE / "scale" / "pages.jsonl").read_text(encoding="utf-8").splitlines()]
    started = time.perf_counter()
    for i in range(int(os.environ.get("BENCH_START", "0")), len(pages), 200):
        batch = pages[i:i + 200]
        await rag.ainsert_custom_kg({"chunks": [{"content": p["text"], "source_id": p["id"], "file_path": p["id"]}
                                                for p in batch]})
        print(i + len(batch), round(time.perf_counter() - started), flush=True)
    await rag.finalize_storages()
    print("done", round(time.perf_counter() - started), flush=True)


if __name__ == "__main__":
    if sys.argv[1] == "ingest":
        asyncio.run(ingest_async())
    elif sys.argv[1] == "scale":
        asyncio.run(scale_async())
    else:
        serve()
