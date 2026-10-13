"""Tables of the comparison: retrieval alone, the agent's answers, and what ingest cost.

    python report.py > results/report.md
"""

import collections
import json
import pathlib
import statistics

HERE = pathlib.Path(__file__).parent
R = HERE / "results"
SYSTEMS = ["pinakes", "mem0", "graphiti", "lightrag", "files"]
NAMES = {"pinakes": "Pinakes", "mem0": "Mem0", "graphiti": "Graphiti", "lightrag": "LightRAG",
         "files": "Files + grep"}


def rows(path):
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))]


def retrieval():
    print("## Retrieval alone (212 questions, one at a time, warm)\n")
    print("| Configuration | Median | p95 | R@1 | R@3 | R@5 | Edges found | Leaks | Returned per search |")
    print("|---|---|---|---|---|---|---|---|---|")
    for label, name in [("pinakes-kw-keyword", "Pinakes, keyword mode, query + keywords"),
                        ("pinakes-kw-auto", "Pinakes, auto mode, query + keywords"),
                        ("pinakes-q-keyword", "Pinakes, keyword mode, query only"),
                        ("pinakes-q-auto", "Pinakes, auto mode, query only"),
                        ("mem0", "Mem0"), ("graphiti", "Graphiti"), ("lightrag", "LightRAG (mix)")]:
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
              f"| {statistics.mean(r['bytes'] for r in rs) / 1024:.1f} KB |")
    print()


def agent():
    data = {s: rows(R / f"judge-{s}.jsonl") for s in SYSTEMS if (R / f"judge-{s}.jsonl").exists()}
    if not data:
        return
    print("## The agent's answers (Pi + deepseek-v4-flash, graded blind)\n")
    print("| System | Answered fully | Partly | Wrong or missing | Mean score (0-2) | Leaks | Tool calls | "
          "Tool output | Input tokens | Time | Cost (USD) |")
    print("|---|---|---|---|---|---|---|---|---|---|---|")
    for s, rs in data.items():
        n = len(rs)
        c = collections.Counter(r["score"] for r in rs)
        inp = [r["usage"]["input"] + r["usage"]["cacheRead"] for r in rs]
        print(f"| {NAMES[s]} | {c[2] / n:.1%} | {c[1] / n:.1%} | {c[0] / n:.1%} "
              f"| {statistics.mean(r['score'] for r in rs):.2f} | {sum(1 for r in rs if r.get('leak'))} "
              f"| {statistics.mean(r['calls'] for r in rs):.1f} "
              f"| {statistics.mean(r['tool_chars'] for r in rs) / 1000:.1f}k chars "
              f"| {statistics.median(inp) / 1000:.1f}k "
              f"| {statistics.median(r['seconds'] or 0 for r in rs):.1f} s "
              f"| {sum(r['usage'].get('cost', 0) for r in rs):.3f} |")
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


def ingest():
    print("## Ingest (the same 111 pages and 10 graph spaces)\n")
    calls = collections.defaultdict(lambda: [0, 0, 0, 0])
    for r in rows(HERE / "llm_calls.jsonl"):
        x = calls[r["tag"]]
        x[0] += 1
        x[1] += r["prompt_tokens"]
        x[2] += r["completion_tokens"]
        x[3] += r["cache_hit_tokens"]
    print("| System | Time | LLM calls | Input tokens (cached) | Output tokens | LLM calls per search |")
    print("|---|---|---|---|---|---|")
    print("| Pinakes | (pages copied in; written by its librarian beforehand) | 0 | 0 | 0 | 0 |")
    for s in ["mem0", "graphiti", "lightrag"]:
        rep = json.loads((HERE / "stores" / s / "ingest.json").read_text())
        n, i, o, h = calls[f"{s}-ingest"]
        per = "1 (keywords)" if s == "lightrag" else "0"
        print(f"| {NAMES[s]} | {rep['seconds']:.0f} s | {n} | {i / 1e6:.2f} M ({h / 1e6:.2f} M) | {o / 1000:.0f}k | {per} |")
    print()


if __name__ == "__main__":
    retrieval()
    agent()
    ingest()
