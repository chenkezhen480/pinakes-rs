"""The librarian's split of notes, as the service runs it (`librarian::note`, prompt
`prompts/note.md`), on the notes the agent recorded with `record_decision_or_lesson` in a
record_eval run: each note goes to DeepSeek's Anthropic-compatible endpoint as the service sends
it (the note as `## Note`, a forced `reply` tool with the reply schema, thinking off, through the
counting proxy as tag `note`); a reply without items, four times, leaves the note one decision.
Then every run is scored as record_eval scores the separate tools, the kinds filed standing for
the tools called, against other runs:

    python note_eval.py <run label> <store name> [<label of separate tools to compare> ...]

Splits are kept in results/record-<run label>/<store name>.json.
"""

import collections
import concurrent.futures
import json
import pathlib
import sys

import httpx

from record_eval import HERE, cases, records_of

REPO = pathlib.Path(__file__).resolve().parents[1]
SYSTEM = (REPO / "prompts" / "note.md").read_text(encoding="utf-8")
ATTEMPTS = 4
ITEM = {"type": "object", "additionalProperties": False, "required": ["kind", "title", "text"],
        "properties": {"kind": {"type": "string", "enum": ["decision", "lesson"]},
                       "title": {"type": "string"}, "text": {"type": "string"}}}
SCHEMA = {"type": "object", "additionalProperties": False, "required": ["items"],
          "properties": {"items": {"type": "array", "items": ITEM}}}


def note_text(args: dict) -> str:
    return "\n".join(str(args[k]).strip() for k in ("topic", "content", "context") if args.get(k)
                     and str(args[k]).strip())


def split(client: httpx.Client, args: dict) -> list[dict]:
    body = {"model": "deepseek-flash", "max_tokens": 16000, "system": SYSTEM,
            "messages": [{"role": "user", "content": f"## Note\n\n{note_text(args)}\n"}],
            "tools": [{"name": "reply", "description": "Give your answer. The input is the whole answer.",
                       "input_schema": SCHEMA}],
            "tool_choice": {"type": "tool", "name": "reply"}, "thinking": {"type": "disabled"}}
    for _ in range(ATTEMPTS):
        r = client.post("http://127.0.0.1:8071/note/anthropic/v1/messages", json=body,
                        headers={"anthropic-version": "2023-06-01"})
        if r.status_code != 200:
            raise RuntimeError(f"{r.status_code} {r.text[:200]}")
        use = next((b for b in r.json()["content"] if b.get("type") == "tool_use"), None)
        items = [i for i in ((use or {}).get("input") or {}).get("items", []) if str(i.get("text", "")).strip()]
        if items:
            return items[:4]
    return [{"kind": "decision", "title": args.get("topic", ""), "text": note_text(args), "fallback": True}]


def main():
    label, store_name, others = sys.argv[1], sys.argv[2], sys.argv[3:]
    root = HERE / "results" / f"record-{label}"
    store = root / f"{store_name}.json"
    done = json.loads(store.read_text(encoding="utf-8")) if store.exists() else {}
    todo = []
    for which in ("cases", "holdout", "questions"):
        for f in sorted((root / which).glob("*.jsonl")):
            for i, x in enumerate(records_of(f)[0]):
                if x["tool"] == "record_decision_or_lesson" and f"{which}/{f.stem}/{i}" not in done:
                    todo.append((f"{which}/{f.stem}/{i}", x["args"]))
    client = httpx.Client(timeout=300, trust_env=False)
    with concurrent.futures.ThreadPoolExecutor(6) as pool:
        for key, items in zip([k for k, _ in todo], pool.map(lambda t: split(client, t[1]), todo)):
            done[key] = items
    store.write_text(json.dumps(done, ensure_ascii=False, indent=1), encoding="utf-8")
    print(f"{len(done)} notes split ({len(todo)} now), {sum(1 for v in done.values() for i in v if i.get('fallback'))} fell back")

    def kinds_of(which, f):
        recs, _ = records_of(f)
        kinds = set()
        for i, x in enumerate(recs):
            if x["tool"] == "record_decision_or_lesson":
                for item in done[f"{which}/{f.stem}/{i}"]:
                    kinds.add({"decision": "record_decision", "lesson": "record_experience"}[item["kind"]])
            else:
                kinds.add(x["tool"])
        return kinds

    for which in ("cases", "holdout", "questions"):
        by_id = {c["id"]: c for c in cases(which)}
        for lab in [label, *others]:
            d = HERE / "results" / f"record-{lab}" / which
            if not d.exists():
                continue
            n = collections.Counter()
            per = collections.defaultdict(lambda: [0, 0])
            notes = []
            for f in sorted(d.glob("*.jsonl")):
                c = by_id[f.stem.split("~")[0]]
                got = kinds_of(which, f) if lab == label else {x["tool"] for x in records_of(f)[0]}
                expect, allow = set(c["expect"]), set(c.get("allow", []))
                n["runs"] += 1
                n["expected"] += len(expect)
                n["hit"] += len(expect & got)
                n["positive"] += bool(expect)
                n["something"] += bool(expect) and bool(got)
                extra = got - expect - allow
                n["extra"] += len(extra)
                # A lesson no one expected: text shown to the team that the user may have meant for themselves.
                n["lesson_beyond"] += "record_experience" in extra
                n["on_negative"] += bool(got) and not expect
                for k in expect:
                    per[k][0] += k in got
                    per[k][1] += 1
                if lab == label and (expect - got or extra):
                    notes.append(f"{f.stem}: filed {sorted(t.removeprefix('record_') for t in got)}, "
                                 f"expected {sorted(t.removeprefix('record_') for t in expect)}")
            line = f"{which:9} {lab:7} runs {n['runs']:3}"
            if n["expected"]:
                line += (f" | filed as expected {n['hit']}/{n['expected']} = {n['hit'] / n['expected']:.1%}"
                         f" | recorded something {n['something']}/{n['positive']}")
            line += (f" | other-kind records {n['extra']} (unexpected team lessons {n['lesson_beyond']})"
                     f" | records on negatives {n['on_negative']}")
            line += " | " + ", ".join(f"{k.removeprefix('record_')} {a}/{b}" for k, (a, b) in sorted(per.items()))
            print(line)
            for x in notes:
                print("    -", x)


if __name__ == "__main__":
    main()
