"""Graphiti (graphiti-core 0.30.2, Zep's temporal knowledge graph) on Neo4j 5.26 over the test set.
(FalkorDB's image of 2026-10-08 refuses graphiti-core's index calls, so its other backend.)

ingest: every wiki page as a text episode, every graph space as one JSON episode (Graphiti's way
        for structured data); the LLM extracts entities and facts. One group_id per tenant scope;
        episodes of a group in order, groups four at a time.
serve:  the read tools of Graphiti's MCP server (search_memory_facts, search_nodes) and
        /retrieve (facts, as Graphiti.search returns them), on port 8082.
"""

import asyncio
import json
import os
import pathlib
import sys
import time
from datetime import datetime, timezone

import corpus
import mini_mcp

HERE = pathlib.Path(__file__).parent
STORE = HERE / "stores" / "graphiti"
EMBED_URL = os.environ.get("EMBED_URL", "http://127.0.0.1:8070/v1")
EMBED_MODEL_NAME = os.environ.get("EMBED_MODEL_NAME", "jina-embeddings-v2-base-zh")
EMBED_DIM = int(os.environ.get("EMBED_DIM", "768"))


def graphiti(tag: str):
    from graphiti_core import Graphiti
    from graphiti_core.cross_encoder.openai_reranker_client import OpenAIRerankerClient
    from graphiti_core.driver.neo4j_driver import Neo4jDriver
    from graphiti_core.embedder.openai import OpenAIEmbedder, OpenAIEmbedderConfig
    from graphiti_core.llm_client.config import LLMConfig
    from graphiti_core.llm_client.openai_generic_client import OpenAIGenericClient

    llm = LLMConfig(api_key="sk-local", model="deepseek-flash", small_model="deepseek-flash",
                    base_url=f"http://127.0.0.1:8071/{tag}/v1", temperature=0.0, max_tokens=8192)
    return Graphiti(
        graph_driver=Neo4jDriver("bolt://127.0.0.1:7688", "neo4j", "benchpass123"),
        llm_client=OpenAIGenericClient(llm, max_tokens=8192, structured_output_mode="json_object"),
        embedder=OpenAIEmbedder(OpenAIEmbedderConfig(
            api_key="sk-local", base_url=EMBED_URL,
            embedding_model=EMBED_MODEL_NAME, embedding_dim=EMBED_DIM)),
        cross_encoder=OpenAIRerankerClient(llm),
    )


def updated(text: str) -> datetime:
    for line in text.splitlines():
        if line.startswith("updated:"):
            return datetime.fromisoformat(line.split(":", 1)[1].strip()).replace(tzinfo=timezone.utc)
    return datetime(2026, 9, 30, tzinfo=timezone.utc)


async def ingest_async():
    from graphiti_core.nodes import EpisodeType

    STORE.mkdir(parents=True, exist_ok=True)
    g = graphiti("graphiti-ingest")
    await g.build_indices_and_constraints()
    groups: dict[str, list] = {}
    for t in corpus.tenants():
        for p in corpus.pages(t):
            groups.setdefault(corpus.partition(t, p["scope"]), []).append(
                (p["id"], p["text"], EpisodeType.text, "wiki page", updated(p["text"])))
        for s in corpus.spaces(t):
            body = json.dumps({"graph_space": s["space"], "space_name": s["name"], "aliases": s["aliases"],
                               "nodes": s["nodes"], "edges": s["edges"]}, ensure_ascii=False)
            groups.setdefault(corpus.partition(t, s["scope"]), []).append(
                (f"graph:{s['space']}", body, EpisodeType.json, "business graph data",
                 datetime(2026, 9, 30, tzinfo=timezone.utc)))
    sem = asyncio.Semaphore(4)
    report = {"groups": {}}
    started = time.perf_counter()

    async def one_group(gid, episodes):
        from graphiti_core.nodes import EpisodicNode

        async with sem:
            t0 = time.perf_counter()
            # Resume (BENCH_RESUME=1): episodes the group already holds are kept, the rest added in order.
            have = set()
            if os.environ.get("BENCH_RESUME"):
                have = {e.name for e in await EpisodicNode.get_by_group_ids(g.driver, [gid])}
            for name, body, kind, desc, when in episodes:
                if name in have:
                    continue
                for attempt in range(3):
                    try:
                        r = await g.add_episode(name=name, episode_body=body, source_description=desc,
                                                reference_time=when, source=kind, group_id=gid)
                        print(f"{gid} {name}: {len(r.nodes)} nodes, {len(r.edges)} facts", flush=True)
                        break
                    except Exception as e:
                        print(f"{gid} {name}: attempt {attempt + 1} failed: {e!r}"[:400], flush=True)
                else:
                    report.setdefault("failed", []).append(f"{gid} {name}")
            report["groups"][gid] = round(time.perf_counter() - t0, 1)

    await asyncio.gather(*(one_group(gid, eps) for gid, eps in groups.items()))
    report["seconds"] = round(time.perf_counter() - started, 1)
    out = "ingest-resume.json" if os.environ.get("BENCH_RESUME") else "ingest.json"
    (STORE / out).write_text(json.dumps(report, indent=1))
    print(report, flush=True)


def serve():
    import uvicorn
    from graphiti_core.search.search_config_recipes import NODE_HYBRID_SEARCH_RRF
    from graphiti_core.search.search_filters import SearchFilters

    g = graphiti("graphiti-query")

    def gids(tenant, user):
        return [corpus.partition(tenant, s) for s in corpus.visible_scopes(tenant, user)]

    episodes: dict[str, str] = {}

    async def episode_names(tenant, user):
        from graphiti_core.nodes import EpisodicNode

        todo = [gid for gid in gids(tenant, user) if gid not in episodes]
        if todo:
            for ep in await EpisodicNode.get_by_group_ids(g.driver, todo):
                episodes[ep.uuid] = ep.name
            for gid in todo:
                episodes[gid] = gid  # this group is loaded

    async def facts(query, tenant, user, n):
        return await g.search(query, group_ids=gids(tenant, user), num_results=n)

    async def retrieve(body, tenant, user):
        edges = await facts(body["query"], tenant, user, body.get("k", 10))
        await episode_names(tenant, user)
        return [{"text": e.fact, "sources": [episodes.get(u) for u in e.episodes]} for e in edges]

    async def search_memory_facts(args, tenant, user):
        n = int(args.get("max_facts") or 10)
        edges = await facts(args["query"], tenant, user, n)
        if not edges:
            return {"message": "No relevant facts found", "facts": []}
        return {"message": "Facts retrieved successfully", "facts": [
            {"name": e.name, "fact": e.fact, "valid_at": e.valid_at, "invalid_at": e.invalid_at}
            for e in edges]}

    async def search_nodes(args, tenant, user):
        n = int(args.get("max_nodes") or 10)
        res = await g.search_(query=args["query"], config=NODE_HYBRID_SEARCH_RRF,
                              group_ids=gids(tenant, user), search_filter=SearchFilters())
        nodes = res.nodes[:n]
        if not nodes:
            return {"message": "No relevant nodes found", "nodes": []}
        return {"message": "Nodes retrieved successfully", "nodes": [
            {"name": x.name, "labels": x.labels, "summary": x.summary, "attributes": x.attributes}
            for x in nodes]}

    tools = [
        mini_mcp.Tool(
            "search_nodes",
            "Search for nodes (entities) in the graph memory.\n\nArgs:\n    query: The search query\n"
            "    max_nodes: Maximum number of nodes to return (default: 10)",
            {"type": "object", "properties": {"query": {"type": "string"}, "max_nodes": {"type": "integer"}},
             "required": ["query"]},
            search_nodes),
        mini_mcp.Tool(
            "search_memory_facts",
            "Search the graph memory for relevant facts (entity edges).\n\nArgs:\n    query: The search query\n"
            "    max_facts: Maximum number of facts to return (default: 10)",
            {"type": "object", "properties": {"query": {"type": "string"}, "max_facts": {"type": "integer"}},
             "required": ["query"]},
            search_memory_facts),
    ]
    instructions = (
        "Graphiti is a memory service for AI agents built on a temporally-aware knowledge graph. "
        "Graphiti organizes data as episodes (content snippets), nodes (entities), and facts "
        "(relationships between entities). search_nodes: semantic + keyword + graph search over "
        "entities. search_memory_facts: search over facts (edges). When searching, use specific queries.")
    app = mini_mcp.app("graphiti", instructions, tools, retrieve, HERE / "calls-graphiti.jsonl")
    uvicorn.run(app, host="127.0.0.1", port=8082, log_level="warning")


if __name__ == "__main__":
    if sys.argv[1] == "ingest":
        asyncio.run(ingest_async())
    else:
        serve()
