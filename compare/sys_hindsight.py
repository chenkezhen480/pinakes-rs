"""Hindsight (vectorize-io, the ghcr.io/vectorize-io/hindsight image: API on 127.0.0.1:8888) over
the test set.

ingest: one bank per tenant; every wiki page retained as one item (its kind as context, its date
        as timestamp, its id as document), every graph space as one item of edge lines; each item
        tagged with its scope (`scope:t`, `scope:u:lily`, ...). Hindsight's LLM extracts facts.
serve:  the read tools of Hindsight's MCP server, `recall` and `reflect`, with their own
        descriptions, and /retrieve (recall), on port 8086. A caller's scopes are its tags,
        matched `any_strict`, so one recall ranks everything the caller may read.
"""

import concurrent.futures
import json
import os
import pathlib
import sys
import time

import httpx

import corpus
import mini_mcp

HERE = pathlib.Path(__file__).parent
API = os.environ.get("HINDSIGHT_API", "http://127.0.0.1:8888")
PORT = int(os.environ.get("BENCH_PORT", "8086"))
client = httpx.Client(timeout=1800, trust_env=False)


def bank(tenant: str) -> str:
    return tenant.replace("-", "_")


def tag(scope: str) -> str:
    return f"scope:{scope}"


def retain(tenant: str, items: list[dict]):
    r = client.post(f"{API}/v1/default/banks/{bank(tenant)}/memories", json={"items": items, "async": False})
    r.raise_for_status()
    return r.json()


def page_date(text: str) -> str:
    for line in text.splitlines():
        if line.startswith("updated:"):
            return line.split(":", 1)[1].strip() + "T00:00:00Z"
    return "2026-09-30T00:00:00Z"


def ingest():
    started = time.perf_counter()
    report = {}

    def one_tenant(t):
        t0 = time.perf_counter()
        items = [{"content": p["text"], "context": p["id"].split("/")[-2] if "/" in p["id"] else "wiki",
                  "timestamp": page_date(p["text"]), "document_id": p["id"], "tags": [tag(p["scope"])],
                  "metadata": {"page": p["id"]}} for p in corpus.pages(t)]
        items += [{"content": "\n".join(corpus.edge_sentences(s)), "context": "business graph data",
                   "timestamp": "2026-09-30T00:00:00Z", "document_id": f"graph:{s['space']}",
                   "tags": [tag(s["scope"])], "metadata": {"page": f"graph:{s['space']}"}}
                  for s in corpus.spaces(t)]
        for i in range(0, len(items), 4):
            retain(t, items[i:i + 4])
            print(t, i + len(items[i:i + 4]), "/", len(items), flush=True)
        report[t] = round(time.perf_counter() - t0, 1)

    # Resume: just these tenants (comma-separated); a page retained again replaces its document.
    only = os.environ.get("BENCH_TENANTS")
    with concurrent.futures.ThreadPoolExecutor(3) as pool:
        list(pool.map(one_tenant, [t for t in corpus.tenants() if not only or t in only.split(",")]))
    report["seconds"] = round(time.perf_counter() - started, 1)
    out = HERE / "stores" / "hindsight"
    out.mkdir(parents=True, exist_ok=True)
    (out / (f"ingest-{only}.json" if only else "ingest.json")).write_text(json.dumps(report, indent=1))
    print(report)


def retain_sessions(tenant: str, sessions: list):
    """A LongMemEval history: (session id, datetime, transcript), oldest first."""
    items = [{"content": text, "context": "conversation with the user",
              "timestamp": dt.strftime("%Y-%m-%dT%H:%M:00Z"), "document_id": sid, "tags": [tag("t")],
              "metadata": {"page": sid}} for sid, dt, text in sessions]
    for i in range(0, len(items), 4):
        retain(tenant, items[i:i + 4])


def recall(query, tenant, user, max_tokens=4096, budget="high", sources=False):
    """sources: also the facts an observation was consolidated from (`source_facts`), so /retrieve can
    name an observation's pages; the agent's tool doesn't ask for them."""
    tags = [tag(s) for s in corpus.visible_scopes(tenant, user)]
    body = {"query": query, "tags": tags, "tags_match": "any_strict", "budget": budget, "max_tokens": max_tokens}
    if sources:
        body["include"] = {"source_facts": {}}
    r = client.post(f"{API}/v1/default/banks/{bank(tenant)}/memories/recall", json=body)
    r.raise_for_status()
    d = r.json()
    if sources:
        facts = d.get("source_facts") or {}
        for x in d.get("results", []):
            x["pages"] = [page_of(x)] if page_of(x) else list(dict.fromkeys(
                page_of(facts[i]) for i in x.get("source_fact_ids") or [] if i in facts and page_of(facts[i])))
    return d.get("results", [])


def page_of(x) -> str | None:
    return (x.get("metadata") or {}).get("page") or x.get("document_id")


def serve():
    import asyncio

    import uvicorn

    async def retrieve(body, tenant, user):
        res = await asyncio.to_thread(recall, body["query"], tenant, user, sources=True)
        return [{"text": x["text"], "sources": x["pages"] or [None]} for x in res[: body.get("k", 10)]]

    async def recall_tool(args, tenant, user):
        res = await asyncio.to_thread(recall, args["query"], tenant, user, int(args.get("max_tokens") or 4096),
                                      args.get("budget") or "high")
        return {"results": [{k: x.get(k) for k in ("text", "type", "entities", "context", "occurred_start",
                                                   "occurred_end", "mentioned_at") if x.get(k)} for x in res]}

    async def reflect_tool(args, tenant, user):
        tags = [tag(s) for s in corpus.visible_scopes(tenant, user)]
        body = {"query": args["query"], "tags": tags, "tags_match": "any_strict",
                "budget": args.get("budget") or "low"}
        if args.get("context"):
            body["context"] = args["context"]
        r = await asyncio.to_thread(client.post, f"{API}/v1/default/banks/{bank(tenant)}/reflect", json=body)
        r.raise_for_status()
        d = r.json()
        return {"text": d.get("text") or d.get("response") or d}

    recall_desc = ("Search memories to provide personalized, context-aware responses.\n\n"
                   "Use this tool PROACTIVELY to:\n- Check user's preferences before making suggestions\n"
                   "- Recall user's history to provide continuity\n- Remember user's goals and context\n"
                   "- Personalize responses based on past interactions\n\nArgs:\n"
                   "    query: Natural language search query (e.g., \"user's food preferences\", "
                   "\"what projects is user working on\")\n"
                   "    max_tokens: Token budget for the facts' text (default: 4096).\n"
                   "    budget: Search budget - 'low', 'mid', or 'high' (default: 'high'). Higher budgets "
                   "search more thoroughly.")
    reflect_desc = ("Generate thoughtful analysis by synthesizing stored memories with the bank's personality.\n\n"
                    "WHEN TO USE THIS TOOL:\nUse reflect when you need reasoned analysis, not just fact retrieval. "
                    "This tool thinks through the question using everything the bank knows and its personality "
                    "traits.\n\nHOW IT DIFFERS FROM RECALL:\n- recall: Returns raw facts matching your search "
                    "(fast lookup)\n- reflect: Reasons across memories to form a synthesized answer (deeper "
                    "analysis)\n\nUse recall for \"what did I say about X?\" and reflect for \"what should I do "
                    "about X?\"")
    tools = [
        mini_mcp.Tool("recall", recall_desc, {"type": "object", "properties": {
            "query": {"type": "string"}, "max_tokens": {"type": "integer"},
            "budget": {"type": "string", "enum": ["low", "mid", "high"]}}, "required": ["query"]}, recall_tool),
        mini_mcp.Tool("reflect", reflect_desc, {"type": "object", "properties": {
            "query": {"type": "string"}, "context": {"type": "string"},
            "budget": {"type": "string", "enum": ["low", "mid", "high"]}}, "required": ["query"]}, reflect_tool),
    ]
    app = mini_mcp.app("hindsight", "Hindsight memory: recall facts, or reflect to reason over them.", tools,
                       retrieve, HERE / "calls-hindsight.jsonl")
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")


if __name__ == "__main__":
    {"ingest": ingest, "serve": serve}[sys.argv[1]]()
