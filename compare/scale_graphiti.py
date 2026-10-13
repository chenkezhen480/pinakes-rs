"""The filler pages into Graphiti's shopline group without the LLM: per page an entity named by its
title (summary attached) and one by its first keyword, joined by one fact (the page's summary),
each embedded as Graphiti embeds them, written with Graphiti's own node and edge models.

    python scale_graphiti.py [N]
"""

import asyncio
import json
import sys
import time
from datetime import datetime, timezone

import corpus
import sys_graphiti
from sys_graphiti import HERE


async def main():
    from graphiti_core.edges import EntityEdge
    from graphiti_core.nodes import EntityNode

    n = int(sys.argv[1]) if len(sys.argv) > 1 else 10_000
    g = sys_graphiti.graphiti("graphiti-scale")
    gid = corpus.partition("shopline", "t")
    now = datetime(2026, 9, 30, tzinfo=timezone.utc)
    pages = [json.loads(l) for l in (HERE / "scale" / "pages.jsonl").read_text(encoding="utf-8").splitlines()][:n]
    topics: dict[str, EntityNode] = {}
    started = time.perf_counter()
    for i in range(0, len(pages), 100):
        nodes, edges = [], []
        for p in pages[i:i + 100]:
            head = dict(l.split(":", 1) for l in p["text"].split("\n---\n")[0].strip("-\n").splitlines() if ":" in l)
            title, summary = head["title"].strip(), head["summary"].strip()
            topic = (head.get("keywords", "").strip(" []").split(",")[0].strip() or "general")
            page_node = EntityNode(name=title, group_id=gid, labels=["Entity"], summary=summary, created_at=now)
            nodes.append(page_node)
            if topic not in topics:
                topics[topic] = EntityNode(name=topic, group_id=gid, labels=["Entity"], summary="", created_at=now)
                nodes.append(topics[topic])
            edges.append(EntityEdge(group_id=gid, source_node_uuid=page_node.uuid,
                                    target_node_uuid=topics[topic].uuid, name="DESCRIBES", fact=summary,
                                    episodes=[], created_at=now, valid_at=now))
        await asyncio.gather(*(x.generate_name_embedding(g.embedder) for x in nodes))
        await asyncio.gather(*(e.generate_embedding(g.embedder) for e in edges))
        for x in nodes:
            await x.save(g.driver)
        for e in edges:
            await e.save(g.driver)
        print(i + 100, round(time.perf_counter() - started), flush=True)
    print("done", round(time.perf_counter() - started), flush=True)


if __name__ == "__main__":
    asyncio.run(main())
