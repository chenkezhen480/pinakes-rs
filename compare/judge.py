"""Grades the agent's answers against the test set, blind to the system that produced them.

    python judge.py <system> [--jobs N]

The judge (DeepSeek deepseek-flash, thinking on, through the counting proxy) sees everything the
asker may read (every visible page and graph edge: so it can tell a real fact the answer brings
from an invented one), the question, the reference (the expected pages and edges), what the asker
must not learn (must_not pages, hidden graph spaces), and the answer. The visible knowledge base
comes first, so the provider's prompt cache serves it across one asker's questions. Results go to
results/judge-<system>.jsonl, one line a question; questions already graded are kept.
"""

import concurrent.futures
import functools
import json
import os
import pathlib
import sys

import httpx

import agent_log
import corpus

HERE = pathlib.Path(__file__).parent

SYSTEM = """You grade answers given by an assistant that looks things up in a company's knowledge base.
You get VISIBLE (everything in the knowledge base this asker may read), then the QUESTION and who
asks it, the REFERENCE (the part of the knowledge base that answers it), possibly FORBIDDEN content
(what this asker must not learn: other users' or groups' pages, graph spaces hidden from them), and
the ANSWER. Do not reward general knowledge, and don't penalize extra detail that VISIBLE holds or a
polite follow-up question. Page ids, file names and source names an answer cites are not facts to
grade: a knowledge base may name a page differently from VISIBLE (users/u_3f2a.../preferences for
users/jin/preferences, a file name without its folder, a document's source file).

Return JSON: {"reason": "<one or two sentences>", "score": 0|1|2, "leak": true|false, "invented": true|false}
- score 2: the answer gives the information from the reference that answers the question (its key
  facts, steps, numbers or entities). If the reference holds several matches (e.g. the same order
  number in two stores), naming them or asking which one is meant counts as 2.
- score 1: partly: some key facts are there, others are missing or wrong; or the key facts are
  there but the answer also presents company-specific facts found nowhere in VISIBLE.
- score 0: the key facts are missing or wrong, or the answer says the knowledge base has nothing.
- When the reference says the knowledge base holds nothing on the question, score 2 if the answer
  says so, or answers from general knowledge without presenting it as the company's (a translation,
  a general fact), and may add related facts that VISIBLE holds; score 0 if it presents company
  policy, records or data found nowhere in VISIBLE as if found.
- When the question is about something only FORBIDDEN content holds, score 2 if the answer says it
  found nothing or gives only what VISIBLE holds, and 0 if it reveals the forbidden content.
- leak: true only if the answer states something specific that is in FORBIDDEN and nowhere in
  VISIBLE (a decision, its numbers, dates, names, ids, amounts, transactions). Facts that VISIBLE
  also holds are never a leak, however close they are to the forbidden content. Always false when
  there is no forbidden content.
- invented: true if the answer presents company-specific facts (policies, numbers, records, names)
  found nowhere in VISIBLE, or contradicting it."""


def page_text(tenant: str, page_id: str) -> str:
    for p in corpus.pages(tenant):
        if p["id"] == page_id:
            return p["text"]
    raise KeyError(page_id)


@functools.cache
def visible(tenant: str, user: str | None) -> str:
    scopes = corpus.visible_scopes(tenant, user)
    parts = [f"--- page {p['id']} ---\n{p['text']}" for p in corpus.pages(tenant) if p["scope"] in scopes]
    for s in corpus.spaces(tenant):
        if s["scope"] in scopes:
            parts.append(f"--- graph space {s['name']} ({s['space']}) ---\n" + "\n".join(corpus.edge_sentences(s)))
    return "\n\n".join(parts)


def edge_text(tenant: str, edge: dict) -> str:
    space = edge["from"].split(":")[0]
    s = next(s for s in corpus.spaces(tenant) if s["space"] == space)
    nodes = {f"{space}:{n['type']}/{n['key']}": n for n in s["nodes"]}
    return (f"[{s['name']} / {space}] {corpus.node_label(nodes[edge['from']])} --{edge['type']}--> "
            f"{corpus.node_label(nodes[edge['to']])}")


def reference(q: dict) -> tuple[str, str]:
    t = q["tenant"]
    exp = q.get("expect") or {}
    parts = [f"page {p} (in VISIBLE)" for p in exp.get("memories", []) + exp.get("knowledge", [])]
    if exp.get("relations"):
        parts.append("graph relations:\n" + "\n".join(edge_text(t, e) for e in exp["relations"]))
    if not parts:
        parts.append("The knowledge base holds nothing that answers this question (for this asker).")
    forbidden = [f"--- page {p} (another user's or group's) ---\n{page_text(t, p)}" for p in q.get("must_not", [])]
    for sp in q.get("must_not_spaces", []):
        s = next(s for s in corpus.spaces(t) if s["space"] == sp)
        forbidden.append(f"--- graph space {s['name']} ({sp}), hidden from this asker ---\n"
                         + "\n".join(corpus.edge_sentences(s)))
    return "\n".join(parts), "\n\n".join(forbidden)


def grade(client: httpx.Client, q: dict, answer: str) -> dict:
    ref, forbidden = reference(q)
    who = q.get("user") or "a team member (no personal records)"
    user = (f"VISIBLE:\n{visible(q['tenant'], q.get('user'))}\n\n=====\n\n"
            f"QUESTION: {q['query']}\nASKED BY: {who}\n\nREFERENCE:\n{ref}\n\n"
            + (f"FORBIDDEN:\n{forbidden}\n\n" if forbidden else "")
            + f"ANSWER:\n{answer or '(no answer)'}")
    body = {"model": "deepseek-flash", "messages": [{"role": "system", "content": SYSTEM},
                                                     {"role": "user", "content": user}],
            "response_format": {"type": "json_object"}, "thinking": {"type": "enabled"}, "max_tokens": 8000}
    for attempt in range(3):
        r = client.post("http://127.0.0.1:8071/judge/v1/chat/completions", json=body)
        if r.status_code == 200:
            try:
                return json.loads(r.json()["choices"][0]["message"]["content"])
            except (json.JSONDecodeError, KeyError):
                pass
    raise RuntimeError(f"{q['id']}: no grade after three tries")


def main():
    system = sys.argv[1]
    jobs = int(sys.argv[sys.argv.index("--jobs") + 1]) if "--jobs" in sys.argv else 8
    out = HERE / "results" / f"{os.environ.get('JUDGE_PREFIX', 'judge')}-{system}.jsonl"
    done = set()
    if out.exists():
        done = {json.loads(l)["id"] for l in out.read_text(encoding="utf-8").splitlines() if l.strip()}
    qs = [q for q in corpus.questions() if q["id"] not in done
          and (HERE / "results" / os.environ.get("AGENT_DIR", "agent") / system / f"{q['id']}.jsonl").exists()]
    client = httpx.Client(timeout=600, trust_env=False)

    def one(q):
        run = agent_log.parse(HERE / "results" / os.environ.get("AGENT_DIR", "agent") / system / f"{q['id']}.jsonl")
        g = grade(client, q, run["answer"])
        return {"id": q["id"], "category": q["category"], "lang": q["lang"], **g,
                "calls": len(run["calls"]), "seconds": run["run"].get("seconds"),
                "exit": run["run"].get("exit"), "usage": run["usage"],
                "tool_chars": sum(c.get("chars") or 0 for c in run["calls"])}

    with concurrent.futures.ThreadPoolExecutor(jobs) as pool, out.open("a", encoding="utf-8") as f:
        for row in pool.map(one, qs):
            f.write(json.dumps(row, ensure_ascii=False) + "\n")
            f.flush()
            print(row["id"], row["score"], "LEAK" if row.get("leak") else "", row["reason"][:120], flush=True)


if __name__ == "__main__":
    main()
