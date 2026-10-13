"""Mem0 (OSS 2.2.1, hybrid search: vectors + BM25 + spaCy entity boost) over the test set.

ingest: every wiki page through Mem0's normal add (the LLM extracts memories; infer=True), each
        graph edge as one memory as written (infer=False: already one fact). One Mem0 user_id
        per tenant scope.
serve:  the tool Mem0's MCP server offered (search_memories) and /retrieve, on port 8081.
"""

import asyncio
import json
import os
import pathlib
import sys
import time

import corpus
import mini_mcp

HERE = pathlib.Path(__file__).parent
STORE = HERE / "stores" / os.environ.get("BENCH_STORE", "mem0")
PORT = int(os.environ.get("BENCH_PORT", "8081"))
EMBED_URL = os.environ.get("EMBED_URL", "http://127.0.0.1:8070/v1")
EMBED_MODEL_NAME = os.environ.get("EMBED_MODEL_NAME", "jina-embeddings-v2-base-zh")
EMBED_DIM = int(os.environ.get("EMBED_DIM", "768"))


def memory(tag: str):
    from mem0 import Memory

    return Memory.from_config({
        "llm": {"provider": "deepseek", "config": {
            "model": "deepseek-flash", "api_key": "sk-local", "temperature": 0.1, "max_tokens": 8000,
            "deepseek_base_url": f"http://127.0.0.1:8071/{tag}/v1"}},
        "embedder": {"provider": "openai", "config": {
            "model": EMBED_MODEL_NAME, "api_key": "sk-local",
            "openai_base_url": EMBED_URL, "embedding_dims": EMBED_DIM}},
        "vector_store": {"provider": "qdrant", "config": {
            "collection_name": "bench", "path": str(STORE / "qdrant"), "embedding_model_dims": EMBED_DIM,
            "on_disk": True}},
        "history_db_path": str(STORE / "history.db"),
    })


def ingest():
    STORE.mkdir(parents=True, exist_ok=True)
    m = memory("mem0-ingest")
    started = time.perf_counter()
    report = {}
    for t in corpus.tenants():
        t0 = time.perf_counter()
        n = 0
        for p in corpus.pages(t):
            r = m.add(p["text"], user_id=corpus.partition(t, p["scope"]),
                      metadata={"page": p["id"], "tenant": t}, infer=True)
            n += len(r.get("results", []))
            print(f"{t} {p['id']}: {len(r.get('results', []))} memories", flush=True)
        for s in corpus.spaces(t):
            for i, line in enumerate(corpus.edge_sentences(s)):
                m.add(line, user_id=corpus.partition(t, s["scope"]),
                      metadata={"page": f"graph:{s['space']}", "tenant": t, "edge": i}, infer=False)
                n += 1
        report[t] = {"memories": n, "seconds": round(time.perf_counter() - t0, 1)}
        print(t, report[t], flush=True)
    report["seconds"] = round(time.perf_counter() - started, 1)
    (STORE / "ingest.json").write_text(json.dumps(report, indent=1))


def serve():
    import uvicorn

    m = memory("mem0-query")
    lock = asyncio.Lock()

    # Mem0 takes one user_id per search, so a caller's scopes are searched one by one; the same
    # query's embeddings are kept for the request, so each text is embedded once, as one search
    # over all scopes would.
    cache: dict = {}
    embed, embed_batch = m.embedding_model.embed, m.embedding_model.embed_batch

    def cached_embed(text, action=None):
        key = ("one", text, action)
        if key not in cache:
            cache[key] = embed(text, action)
        return cache[key]

    def cached_embed_batch(texts, action=None):
        key = ("batch", tuple(texts), action)
        if key not in cache:
            cache[key] = embed_batch(texts, action)
        return cache[key]

    m.embedding_model.embed, m.embedding_model.embed_batch = cached_embed, cached_embed_batch

    def search(query: str, tenant: str, user: str | None, limit: int) -> list[dict]:
        cache.clear()
        hits = []
        for scope in corpus.visible_scopes(tenant, user):
            res = m.search(query, top_k=limit, filters={"user_id": corpus.partition(tenant, scope)})
            hits += res["results"] if isinstance(res, dict) else res
        hits.sort(key=lambda r: -r["score"])
        return [{"memory": r["memory"], "score": round(r["score"], 4),
                 "source": (r.get("metadata") or {}).get("page")} for r in hits[:limit]]

    async def retrieve(body, tenant, user):
        async with lock:  # the embedded Qdrant is one client
            items = await asyncio.to_thread(search, body["query"], tenant, user, body.get("k", 10))
        return [{"text": i["memory"], "sources": [i["source"]]} for i in items]

    async def search_memories(args, tenant, user):
        limit = int(args.get("limit") or 10)
        async with lock:
            items = await asyncio.to_thread(search, args["query"], tenant, user, limit)
        return {"results": [{"memory": i["memory"], "score": i["score"]} for i in items]}

    tools = [mini_mcp.Tool(
        "search_memories",
        "Semantic search across existing memories (filters + limit supported).",
        {"type": "object", "properties": {
            "query": {"type": "string", "description": "What to look for."},
            "limit": {"type": "integer", "description": "How many memories to return (default 10)."}},
         "required": ["query"]},
        search_memories)]
    app = mini_mcp.app("mem0", "Mem0 memory: search the stored memories before answering.", tools, retrieve,
                       HERE / "calls-mem0.jsonl")
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")


def scale():
    """The filler pages as memories of the shopline tenant, as written (infer=False: no LLM)."""
    m = memory("mem0-scale")
    started = time.perf_counter()
    for i, line in enumerate((HERE / "scale" / "pages.jsonl").read_text(encoding="utf-8").splitlines()):
        p = json.loads(line)
        m.add(p["text"], user_id=corpus.partition("shopline", "t"), metadata={"page": p["id"], "tenant": "shopline"},
              infer=False)
        if i % 500 == 0:
            print(i, round(time.perf_counter() - started), flush=True)
    print("done", round(time.perf_counter() - started), flush=True)


if __name__ == "__main__":
    {"ingest": ingest, "serve": serve, "scale": scale}[sys.argv[1]]()
