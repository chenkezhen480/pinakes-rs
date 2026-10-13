import json, sys, collections
c = collections.defaultdict(lambda: [0, 0, 0, 0, 0.0, 0])
for l in open("llm_calls.jsonl", encoding="utf-8"):
    r = json.loads(l)
    x = c[r["tag"]]
    x[0] += 1; x[1] += r["prompt_tokens"]; x[2] += r["completion_tokens"]; x[3] += r["cache_hit_tokens"]; x[4] += r["seconds"]; x[5] += r["status"] != 200
for k, (n, p, o, h, s, bad) in sorted(c.items()):
    print(f"{k:20} calls={n:5} in={p:9} out={o:8} cached={h:9} llm_s={s:8.0f} errors={bad}")
