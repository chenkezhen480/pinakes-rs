"""Tables of round 2 (Pi 1.1, Qwen3-Embedding-0.6B for the others, Hindsight added), next to round 1.

    python report2.py > results/report2.md
"""

import collections
import json
import pathlib
import statistics
from datetime import datetime

from report import NAMES, R, pct, rows

HERE = pathlib.Path(__file__).parent
NAMES = {**NAMES, "hindsight": "Hindsight", "lightrag+read": "LightRAG, agent may `read`",
         "hindsight+read": "Hindsight, agent may `read`"}
AGENT = ["pinakes", "mem0", "graphiti", "lightrag", "lightrag+read", "hindsight", "hindsight+read", "files"]


def retrieval():
    print("## Retrieval alone (212 questions, one at a time, warm)\n")
    print("| Configuration | Median | p95 | R@1 | R@3 | R@5 | Edges found | Leaks | Returned per search |")
    print("|---|---|---|---|---|---|---|---|---|")
    for label, name in [("pinakes-kw-keyword-r2", "Pinakes, keyword, query + keywords"),
                        ("pinakes-kw-auto-r2", "Pinakes, auto, query + keywords"),
                        ("mem0-q3", "Mem0 (Qwen3)"), ("mem0", "Mem0 (jina, round 1)"),
                        ("graphiti-q3", "Graphiti (Qwen3)"), ("graphiti", "Graphiti (jina, round 1)"),
                        ("lightrag-q3", "LightRAG (Qwen3)"), ("lightrag", "LightRAG (jina, round 1)"),
                        ("hindsight", "Hindsight (Qwen3, its default reranker)"),
                        ("hindsight-all", "Hindsight, everything its recall returns"),
                        ("hindsight-bge", "Hindsight (Qwen3, bge-reranker-v2-m3)")]:
        f = R / f"retrieval-{label}.jsonl"
        if not f.exists():
            continue
        rs = rows(f)
        ms = [r["ms"] for r in rs]
        ranks = [x for r in rs for x in r["ranks"]]
        at = lambda k: sum(1 for x in ranks if x is not None and x <= k) / len(ranks)
        e = sum(r["edges"] for r in rs)
        ef = sum(r["edges_found"] for r in rs)
        print(f"| {name} | {statistics.median(ms):.1f} ms | {pct(ms, .95):.1f} ms | {at(1):.3f} | {at(3):.3f} "
              f"| {at(5):.3f} | {ef}/{e} | {sum(len(r['leaks']) for r in rs)} "
              f"| {statistics.mean(r['bytes'] for r in rs) / 1024:.1f} KB | n={len(rs)}")
    print()


def agent():
    data = {s: rows(R / f"judge-v1-{s}.jsonl") for s in AGENT if (R / f"judge-v1-{s}.jsonl").exists()}
    old = {s: rows(R / f"judge-{s}.jsonl") for s in AGENT if (R / f"judge-{s}.jsonl").exists()}
    print("## The agent's answers (Pi 1.1 + deepseek-flash, graded blind)\n")
    print("| System | Answered fully | Partly | Wrong or missing | Mean (0-2) | Round 1 mean | Leaks (judge) "
          "| Tool calls | Tool output | Input tokens (median) | Time (median) | Cost (USD) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for s, rs in data.items():
        n = len(rs)
        c = collections.Counter(r["score"] for r in rs)
        inp = [r["usage"]["input"] + r["usage"]["cacheRead"] for r in rs]
        r1 = f"{statistics.mean(r['score'] for r in old[s]):.2f}" if s in old else "-"
        print(f"| {NAMES[s]} | {c[2] / n:.1%} | {c[1] / n:.1%} | {c[0] / n:.1%} "
              f"| {statistics.mean(r['score'] for r in rs):.2f} | {r1} | {sum(1 for r in rs if r.get('leak'))} "
              f"| {statistics.mean(r['calls'] for r in rs):.1f} "
              f"| {statistics.mean(r['tool_chars'] for r in rs) / 1000:.1f}k chars "
              f"| {statistics.median(inp) / 1000:.1f}k "
              f"| {statistics.median(r['seconds'] or 0 for r in rs):.1f} s "
              f"| {sum(r['usage'].get('cost', 0) for r in rs):.3f} | n={n}")
    print("\nMean score by category:\n")
    cats = list(dict.fromkeys(r["category"] for rs in data.values() for r in rs))
    print("| Category | n | " + " | ".join(NAMES[s] for s in data) + " |")
    print("|---|---|" + "---|" * len(data))
    for c in cats:
        cells = []
        for s, rs in data.items():
            xs = [r["score"] for r in rs if r["category"] == c]
            cells.append(f"{statistics.mean(xs):.2f}" if xs else "-")
        n = sum(1 for r in next(iter(data.values())) if r["category"] == c)
        print(f"| {c} | {n} | " + " | ".join(cells) + " |")
    print()
    print("Runs with a tool output cut by Pi (20 KB):\n")
    for s in AGENT:
        d = R / "agent-v1" / s
        if d.exists():
            cut = sum(1 for f in d.glob("*.jsonl") if "Warning: truncated output" in f.read_text(encoding="utf-8"))
            reads = sum(1 for f in d.glob("*.jsonl") if '"toolName":"read"' in f.read_text(encoding="utf-8")
                        or '"toolName": "read"' in f.read_text(encoding="utf-8"))
            print(f"- {NAMES[s]}: {cut} of {len(list(d.glob('*.jsonl')))} runs cut, {reads} used `read`")
    print()


def llm_spend():
    """The LLM calls of round 2 by tag, from the forwarder's log (deepseek-flash list prices)."""
    print("## LLM calls through the forwarder, round 2\n")
    print("| Tag | Calls | Input tokens (cached) | Output tokens | Active time |")
    print("|---|---|---|---|---|")
    by = collections.defaultdict(list)
    for r in rows(HERE / "llm_calls.jsonl"):
        by[r["tag"]].append(r)
    for tag, rs in sorted(by.items()):
        ts = sorted(r["t"] for r in rs)
        active, start, last = 0.0, ts[0], ts[0]
        for t in ts[1:]:
            if t - last > 600:
                active += last - start
                start = t
            last = t
        active += last - start
        i = sum(r["prompt_tokens"] or 0 for r in rs)
        h = sum(r["cache_hit_tokens"] or 0 for r in rs)
        o = sum(r["completion_tokens"] or 0 for r in rs)
        print(f"| {tag} | {len(rs)} | {i / 1e6:.2f} M ({h / 1e6:.2f} M) | {o / 1000:.0f}k | {active:.0f} s "
              f"| {datetime.fromtimestamp(ts[0]):%m-%d %H:%M} to {datetime.fromtimestamp(ts[-1]):%m-%d %H:%M}")
    print()


if __name__ == "__main__":
    retrieval()
    agent()
    llm_spend()
