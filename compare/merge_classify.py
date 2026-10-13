"""The second half of the merged-tool experiment: what a librarian would file from each
`record_decision_or_lesson` call. Each call's fields go to DeepSeek `deepseek-flash` (thinking off,
JSON, through the counting proxy as tag `classify`), which splits the note into decisions (filed
for the user) and lessons (filed for the team). Then every run is scored as record_eval scores the
separate tools, with the kinds filed standing for the tools called:

    python merge_classify.py <label> [<label of separate tools to compare> ...]

Classifications are kept in results/record-<label>/classified.json.
"""

import collections
import concurrent.futures
import json
import pathlib
import sys

import httpx

from record_eval import HERE, cases, records_of

SYSTEM = """You file notes an assistant recorded into a team's knowledge base. Split the note into
items of two kinds:
- "decision": a choice the user or their team made about how things will be done from now on
  (a policy, a threshold, a schedule, who does what). Filed for that user.
- "lesson": what happened in practice and what it taught: what went wrong and why, a fix or
  workaround, a step or check that would help a colleague doing this kind of task. Filed for the
  whole team.
A note may hold one item, or a lesson and the decision taken because of it (then return both). A
rule stated as the way to do a task because of what went wrong is a lesson; it is also a decision
only when the note says the user or team decided or agreed on it. Return JSON:
{"items": [{"kind": "decision" | "lesson", "title": "<short>", "text": "<the item, exact numbers kept>"}]}"""


def classify(client: httpx.Client, args: dict) -> list[dict]:
    note = "\n".join(f"{k}: {v}" for k, v in args.items() if k in ("topic", "content", "context") and v)
    body = {"model": "deepseek-flash", "messages": [{"role": "system", "content": SYSTEM},
                                                     {"role": "user", "content": note}],
            "response_format": {"type": "json_object"}, "thinking": {"type": "disabled"}, "max_tokens": 2000}
    for _ in range(3):
        r = client.post("http://127.0.0.1:8071/classify/v1/chat/completions", json=body)
        if r.status_code == 200:
            try:
                return json.loads(r.json()["choices"][0]["message"]["content"])["items"]
            except (json.JSONDecodeError, KeyError):
                pass
    raise RuntimeError(f"no classification for {note[:80]}")


def filed_kinds(label: str, which: str, f: pathlib.Path, done: dict) -> set[str]:
    recs, _ = records_of(f)
    kinds = set()
    for i, x in enumerate(recs):
        if x["tool"] == "record_decision_or_lesson":
            for item in done[f"{which}/{f.stem}/{i}"]:
                kinds.add({"decision": "record_decision", "lesson": "record_experience"}.get(item["kind"], "?"))
        else:
            kinds.add(x["tool"])
    return kinds


def main():
    label, others = sys.argv[1], sys.argv[2:]
    root = HERE / "results" / f"record-{label}"
    store = root / "classified.json"
    done = json.loads(store.read_text(encoding="utf-8")) if store.exists() else {}
    todo = []
    for which in ("cases", "holdout", "questions"):
        for f in sorted((root / which).glob("*.jsonl")):
            for i, x in enumerate(records_of(f)[0]):
                if x["tool"] == "record_decision_or_lesson" and f"{which}/{f.stem}/{i}" not in done:
                    todo.append((f"{which}/{f.stem}/{i}", x["args"]))
    client = httpx.Client(timeout=300, trust_env=False)
    with concurrent.futures.ThreadPoolExecutor(6) as pool:
        for key, items in zip([k for k, _ in todo], pool.map(lambda t: classify(client, t[1]), todo)):
            done[key] = items
    store.write_text(json.dumps(done, ensure_ascii=False, indent=1), encoding="utf-8")

    for which in ("cases", "holdout", "questions"):
        by_id = {c["id"]: c for c in cases(which)}
        for lab in [label, *others]:
            d = HERE / "results" / f"record-{lab}" / which
            if not d.exists():
                continue
            n = collections.Counter()
            kinds_by = collections.defaultdict(lambda: [0, 0])
            notes = []
            for f in sorted(d.glob("*.jsonl")):
                c = by_id[f.stem.split("~")[0]]
                got = filed_kinds(label, which, f, done) if lab == label else {x["tool"] for x in records_of(f)[0]}
                expect, allow = set(c["expect"]), set(c.get("allow", []))
                n["runs"] += 1
                n["expected"] += len(expect)
                n["hit"] += len(expect & got)
                n["something"] += bool(got) if expect else 0
                n["positive"] += bool(expect)
                extra = got - expect - allow
                n["false"] += len(extra)
                n["false_on_negative"] += bool(got) and not expect
                for k in expect:
                    kinds_by[k][0] += k in got
                    kinds_by[k][1] += 1
                if lab == label and (expect - got or extra):
                    notes.append(f"{f.stem}: filed {sorted(got)}, expected {sorted(expect)}")
            line = f"{which:9} {lab:7} runs {n['runs']:3}"
            if n["expected"]:
                line += (f" | filed as expected {n['hit']}/{n['expected']} = {n['hit'] / n['expected']:.1%}"
                         f" | recorded something {n['something']}/{n['positive']}")
            line += f" | wrong-kind records {n['false']} | records on negatives {n['false_on_negative']}"
            line += " | " + ", ".join(f"{k.removeprefix('record_')} {a}/{b}" for k, (a, b) in sorted(kinds_by.items()))
            print(line)
            for x in notes:
                print("    -", x)


if __name__ == "__main__":
    main()
