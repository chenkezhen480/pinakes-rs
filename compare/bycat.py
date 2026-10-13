import json, sys, collections
labels = sys.argv[1:]
cats = collections.OrderedDict()
data = {}
for l in labels:
    rows = [json.loads(x) for x in open(f"results/retrieval-{l}.jsonl", encoding="utf-8")]
    data[l] = rows
    for r in rows:
        cats.setdefault(r["category"], None)
print(f"{'category':16}" + "".join(f"{l[:14]:>16}" for l in labels))
for c in cats:
    line = f"{c:16}"
    for l in labels:
        rows = [r for r in data[l] if r["category"] == c]
        ranks = [x for r in rows for x in r["ranks"]]
        e = sum(r["edges"] for r in rows); ef = sum(r["edges_found"] for r in rows)
        r3 = f"{sum(1 for x in ranks if x and x <= 3)}/{len(ranks)}" if ranks else "-"
        ed = f" e{ef}/{e}" if e else ""
        line += f"{r3 + ed:>16}"
    print(line)
