"""Retrieval alone, no agent: every test-set question sent once to a system's search, timed from
the client (HTTP on 127.0.0.1) and on the server, and scored against the question's expectations.

    python retrieval_eval.py <system> [label]

BENCH_K changes how many items the other systems return (10 by default; Hindsight's recall returns
everything within its token budget, so a large BENCH_K scores what its tool hands the agent).
For the run at size (the scale copies), BENCH_PINAKES, BENCH_KEYS and BENCH_PORT point at the other
servers and BENCH_TENANT keeps one tenant's questions.

system: pinakes-kw (query + the question's keywords, as an agent passes them), pinakes-q (query
only), mem0, graphiti, lightrag. Pinakes runs with whatever embedding switch the service has;
the label names the run (e.g. pinakes-kw-vec). Results: results/retrieval-<label>.jsonl.

Scoring, the same for every system:
- pages: an expected page counts as found when any returned item comes from it (Pinakes: a hit
  in any slot; Mem0: the memory's source page; Graphiti: an episode behind the fact; LightRAG:
  the chunk's, entity's or relation's file).
- edges: an expected edge counts as found when Pinakes returns it, or, for the others, when one
  line of one returned item names both ends (a name, alias or key of each node).
- leaks: an item from a must_not page or from a must_not_spaces space.
"""

import json
import os
import pathlib
import statistics
import sys
import time

import httpx

import corpus

HERE = pathlib.Path(__file__).parent
PORTS = {"mem0": 8081, "graphiti": 8082, "lightrag": 8083, "hindsight": 8086}
PINAKES = os.environ.get("BENCH_PINAKES", "http://127.0.0.1:8084")


def node_terms():
    terms = {}
    for t in corpus.tenants():
        for s in corpus.spaces(t):
            for n in s["nodes"]:
                ref = f"{s['space']}:{n['type']}/{n['key']}"
                terms[ref] = [x.lower() for x in [n["name"], n["key"], *n.get("aliases", [])]]
    return terms


TERMS = node_terms()


def same_page(expected: str, got: str) -> bool:
    """Pinakes names user and group folders by hash; compare the rest."""
    if expected == got:
        return True
    if "/" not in got and ":" not in got:  # LightRAG keeps only a file's name
        return expected.split("/")[-1] == got
    e, g = expected.split("/"), got.split("/")
    return e[0] in ("users", "groups") and g[0] == e[0] and len(e) == len(g) and e[2:] == g[2:]


def pinakes_call(client, keys, q, with_keywords, mode):
    t = corpus.tenant_yaml(q["tenant"])
    groups = next((u.get("groups", []) for u in t["users"] if u["id"] == q.get("user")), [])
    headers = {"Authorization": f"Bearer {keys[q['tenant']]}"}
    if q.get("user"):
        headers["X-User-Id"] = q["user"]
    if groups:
        headers["X-User-Groups"] = ",".join(groups)
    # Up to ten pages (five a slot) and ten edges, as the others return ten items.
    body = {"query": q["query"], "slots": {"memories": 5, "knowledge": 5, "relations": 10}}
    if mode:
        body["mode"] = mode
    if with_keywords and q.get("keywords"):
        body["keywords"] = [str(k) for k in q["keywords"]]
    r = client.post(f"{PINAKES}/search", json=body, headers=headers)
    if r.status_code != 200:
        raise SystemExit(f"{q['id']}: {r.status_code} {r.text}")
    d = r.json()
    items = []
    for slot in ("memories", "knowledge"):
        for h in d[slot]["hits"]:
            items.append({"text": json.dumps(h, ensure_ascii=False), "sources": [h["page"]], "slot": slot})
    edges = [(e["from"], e.get("type") or e.get("ty"), e["to"]) for e in d["relations"]["edges"]]
    for e in d["relations"]["edges"]:
        items.append({"text": json.dumps(e, ensure_ascii=False), "sources": [f"graph:{e['from'].split(':')[0]}"]})
    return items, edges, None, len(r.content)


def other_call(client, system, q):
    port = int(os.environ.get("BENCH_PORT") or PORTS[system])
    r = client.post(f"http://127.0.0.1:{port}/retrieve",
                    json={"tenant": q["tenant"], "user": q.get("user"), "query": q["query"],
                          "k": int(os.environ.get("BENCH_K", "10"))})
    r.raise_for_status()
    d = r.json()
    return d["items"], None, d["ms"], len(r.content)


def edge_found(edge, items, pinakes_edges) -> bool:
    if pinakes_edges is not None:
        return (edge["from"], edge["type"], edge["to"]) in pinakes_edges
    a, b = TERMS[edge["from"]], TERMS[edge["to"]]
    for it in items:
        for line in it["text"].lower().splitlines():
            if any(x in line for x in a) and any(x in line for x in b):
                return True
    return False


def page_ranks(q, items, system):
    """Each expected page's rank (1-based) among the distinct pages returned, or None. Pinakes: within
    the slot the page belongs to. LightRAG: among its chunks (its entities and relations come in
    blocks of their own before them)."""
    exp = q.get("expect") or {}
    out = []
    for slot in ("memories", "knowledge"):
        for p in exp.get(slot, []):
            if system.startswith("pinakes"):
                pool = [it for it in items if it.get("slot") == slot]
            elif system == "lightrag":
                pool = [it for it in items if it.get("kind") == "chunk"]
            else:
                pool = items
            seen = []
            for it in pool:
                for src in it["sources"]:
                    if src and src not in seen:
                        seen.append(src)
            out.append(next((i + 1 for i, src in enumerate(seen) if same_page(p, src)), None))
    return out


def score(q, items, pinakes_edges):
    sources = [s for it in items for s in it["sources"] if s]
    exp_pages = (q.get("expect") or {}).get("memories", []) + (q.get("expect") or {}).get("knowledge", [])
    found = [p for p in exp_pages if any(same_page(p, s) for s in sources)]
    exp_edges = (q.get("expect") or {}).get("relations", [])
    efound = [e for e in exp_edges if edge_found(e, items, pinakes_edges)]
    leaks = [p for p in q.get("must_not", []) if any(same_page(p, s) for s in sources)]
    leaks += [f"graph:{sp}" for sp in q.get("must_not_spaces", []) if f"graph:{sp}" in sources]
    return {"pages": len(exp_pages), "pages_found": len(found), "edges": len(exp_edges),
            "edges_found": len(efound), "leaks": leaks}


def main():
    system = sys.argv[1]
    label = sys.argv[2] if len(sys.argv) > 2 else system
    qs = [q for q in corpus.questions() if q["tenant"] == os.environ.get("BENCH_TENANT", q["tenant"])]
    keyfile = HERE / os.environ.get("BENCH_KEYS", "pinakes-keys.json")
    keys = json.loads(keyfile.read_text()) if system.startswith("pinakes") else None
    out = HERE / "results" / f"retrieval-{label}.jsonl"
    out.parent.mkdir(exist_ok=True)
    with httpx.Client(timeout=300, trust_env=False) as client:
        def call(q):
            if system.startswith("pinakes"):
                base, _, mode = system.partition(":")
                return pinakes_call(client, keys, q, base == "pinakes-kw", mode or None)
            return other_call(client, system, q)

        # Warm up: every tenant opened, models loaded, connections made (two questions each).
        for t in corpus.tenants():
            for q in [q for q in qs if q["tenant"] == t][:2]:
                call(q)
        rows = []
        for q in qs:
            started = time.perf_counter()
            items, pedges, server_ms, size = call(q)
            ms = (time.perf_counter() - started) * 1000
            row = {"id": q["id"], "category": q["category"], "lang": q["lang"], "ms": round(ms, 2),
                   "server_ms": round(server_ms, 2) if server_ms is not None else None,
                   "bytes": size, "items": len(items), **score(q, items, pedges),
                   "ranks": page_ranks(q, items, system)}
            rows.append(row)
    out.write_text("".join(json.dumps(r, ensure_ascii=False) + "\n" for r in rows), encoding="utf-8")
    ms = sorted(r["ms"] for r in rows)
    pages = sum(r["pages"] for r in rows)
    edges = sum(r["edges"] for r in rows)
    ranks = [r for row in rows for r in row["ranks"]]
    at = lambda k: sum(1 for r in ranks if r is not None and r <= k) / len(ranks)
    print(f"{label}: R@1={at(1):.3f} R@3={at(3):.3f} R@5={at(5):.3f} (pages ranked, n={len(ranks)})")
    print(f"{label}: n={len(rows)} median={statistics.median(ms):.1f}ms p95={ms[int(len(ms) * .95)]:.1f}ms "
          f"mean={statistics.mean(ms):.1f}ms | pages {sum(r['pages_found'] for r in rows)}/{pages} "
          f"edges {sum(r['edges_found'] for r in rows)}/{edges} leaks {sum(len(r['leaks']) for r in rows)} "
          f"| {statistics.mean(r['bytes'] for r in rows) / 1024:.1f} KB/answer")


if __name__ == "__main__":
    main()
