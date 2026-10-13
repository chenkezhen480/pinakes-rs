"""Does the agent record what it should, unprompted? Pi 1.1 with all of Pinakes' MCP tools (the
record tools included) gets one user message and answers; the run is scored on the record calls
it made.

    python record_eval.py run <label> [cases|holdout|questions] [--repeat N] [--jobs N]
    python record_eval.py score <label> [<label> ...]

cases: record_cases.json, user messages that state a lasting preference, make a decision or
report an experience worth keeping (often next to a question), and messages that must not be
recorded (a one-off instruction, a hypothetical, someone else's preference, a plain question).
holdout: record_holdout.json, more such messages, written after the prompts were tuned on the
cases and only scored. questions: the test set's 212 questions, every one a plain lookup: any record call is a false
one.

The service is a copy of the test-set data (BENCH_PINAKES, default http://127.0.0.1:8089) whose
librarian model points at a closed port: a record call is accepted as a job and fails later, so
nothing is filed and no librarian call is paid for. Runs go to results/record-<label>/.
"""

import collections
import concurrent.futures
import json
import os
import pathlib
import subprocess
import sys
import time

import agent_log
import corpus
from run_agent import AGENT_HOME, MODEL, PI, PROMPT, groups_of, key_env

HERE = pathlib.Path(__file__).parent
URL = os.environ.get("BENCH_PINAKES", "http://127.0.0.1:8089") + "/mcp"
TOOLS = os.environ.get("RECORD_TOOLS", "search,read,graph,path,record_experience,record_decision,"
                       "record_preference,propose_graph_change").split(",")
RECORDS = ("record_experience", "record_decision", "record_preference", "record_decision_or_lesson")


def cases(which: str) -> list[dict]:
    if which in ("cases", "holdout"):
        return json.loads((HERE / f"record_{which}.json").read_text(encoding="utf-8"))
    return [{"id": q["id"], "tenant": q["tenant"], "user": q.get("user"), "kind": "question",
             "message": q["query"], "expect": [], "terms": []} for q in corpus.questions()]


def run_dir(tenant: str, user: str | None) -> pathlib.Path:
    d = HERE / "pi" / "runs-record" / f"{tenant}-{user or 'none'}"
    (d / ".pi").mkdir(parents=True, exist_ok=True)
    keys = json.loads((HERE / "pinakes-keys.json").read_text())
    headers = {"Authorization": f"Bearer {keys[tenant]}"}
    if user:
        headers["X-User-Id"] = user
    if groups_of(tenant, user):
        headers["X-User-Groups"] = ",".join(groups_of(tenant, user))
    cfg = {"mcpServers": {"kb": {"url": URL, "headers": headers, "timeout": 300, "exposure": "direct"}}}
    (d / ".pi" / "mcp.json").write_text(json.dumps(cfg, indent=1), encoding="utf-8")
    return d


def run_one(out: pathlib.Path, c: dict, env: dict) -> str:
    if out.exists() and out.stat().st_size > 0:
        return f"{out.stem}: kept"
    out.parent.mkdir(parents=True, exist_ok=True)
    t = corpus.tenant_yaml(c["tenant"])
    prompt = PROMPT.format(business=t["business"], description=" ".join(t.get("description", "").split()))
    args = [*PI, "-p", "--mode", "json", "--no-session", "--no-context-files", "--thinking", "off",
            "--model", MODEL, "--append-system-prompt", prompt, "-ns", "-np", "-a",
            "--tools", ",".join(f"mcp__kb__{x}" for x in TOOLS), "--", c["message"]]
    started = time.perf_counter()
    tmp = out.with_suffix(".part")
    with open(tmp, "wb") as f:
        try:
            p = subprocess.run(args, cwd=run_dir(c["tenant"], c.get("user")), env=env, stdin=subprocess.DEVNULL,
                               stdout=f, stderr=subprocess.PIPE, timeout=600)
            code, err = p.returncode, p.stderr.decode("utf-8", "replace")[-2000:]
        except subprocess.TimeoutExpired:
            code, err = "timeout", ""
    kept = [l for l in tmp.read_text(encoding="utf-8", errors="replace").splitlines()
            if not l.startswith('{"type":"message_update"')]
    tmp.write_text("\n".join(kept) + "\n", encoding="utf-8")
    with open(tmp, "a", encoding="utf-8") as f:
        f.write(json.dumps({"type": "bench_run", "exit": code, "seconds": round(time.perf_counter() - started, 2),
                            "stderr": err}, ensure_ascii=False) + "\n")
    tmp.replace(out)
    return f"{out.stem}: exit {code}"


def run(label: str, which: str, repeat: int, jobs: int):
    env = key_env()
    todo = [(HERE / "results" / f"record-{label}" / which / f"{c['id']}~{r}.jsonl", c)
            for r in range(repeat) for c in cases(which)]
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        for line in pool.map(lambda x: run_one(x[0], x[1], env), todo):
            print(line, flush=True)


def records_of(path: pathlib.Path) -> tuple[list[dict], dict]:
    r = agent_log.parse(path)
    return [{"tool": c["tool"].removeprefix("mcp__kb__"), "args": c.get("args") or {}, "error": c.get("error")}
            for c in r["calls"] if (c["tool"] or "").removeprefix("mcp__kb__") in RECORDS], r


def score(labels: list[str]):
    for label in labels:
        root = HERE / "results" / f"record-{label}"
        for which in ("cases", "holdout", "questions"):
            d = root / which
            if not d.exists():
                continue
            by_id = {c["id"]: c for c in cases(which)}
            n = collections.Counter()
            misses, false, wrong_terms = [], [], []
            per_kind = collections.defaultdict(lambda: [0, 0])
            cost = 0.0
            for f in sorted(d.glob("*.jsonl")):
                c = by_id[f.stem.split("~")[0]]
                recs, r = records_of(f)
                cost += r["usage"].get("cost", 0)
                tools = {x["tool"] for x in recs}
                expect = set(c["expect"])
                allow = set(c.get("allow", []))
                n["runs"] += 1
                n["expected"] += len(expect)
                hit = expect & tools
                n["hit"] += len(hit)
                extra = tools - expect - allow
                n["false"] += len(extra)
                n["false_runs"] += bool(extra)
                n["negative_runs"] += not expect
                n["errors"] += sum(1 for x in recs if x["error"])
                n["answered"] += bool(r["answer"].strip())
                if expect - tools:
                    misses.append(f"{f.stem}: missed {sorted(expect - tools)}")
                if extra:
                    false.append(f"{f.stem}: {sorted(extra)} "
                                 + json.dumps([x["args"] for x in recs if x["tool"] in extra], ensure_ascii=False)[:200])
                if hit and c["terms"]:
                    text = json.dumps([x["args"] for x in recs], ensure_ascii=False).lower()
                    if not any(t.lower() in text for t in c["terms"]):
                        wrong_terms.append(f"{f.stem}: lacks {c['terms']}")
                per_kind[c["kind"]][0] += len(hit) if expect else (0 if extra else 1)
                per_kind[c["kind"]][1] += len(expect) if expect else 1
            print(f"== {label} / {which}: {n['runs']} runs, cost ${cost:.3f}")
            if n["expected"]:
                print(f"   recall {n['hit']}/{n['expected']} = {n['hit'] / n['expected']:.1%}")
            print(f"   false records {n['false']} in {n['false_runs']} runs "
                  f"({n['false_runs']}/{n['runs']} runs; {n['negative_runs']} runs expect none)"
                  f" | record errors {n['errors']} | answered {n['answered']}/{n['runs']}")
            print("   by kind: " + ", ".join(f"{k} {a}/{b}" for k, (a, b) in sorted(per_kind.items())))
            for line in misses + false + wrong_terms:
                print("   -", line)


if __name__ == "__main__":
    cmd = sys.argv[1]
    if cmd == "run":
        rest = sys.argv[2:]
        opt = lambda k, d: int(rest[rest.index(k) + 1]) if k in rest else d
        pos = [a for i, a in enumerate(rest) if not a.startswith("--") and (i == 0 or not rest[i - 1].startswith("--"))]
        run(pos[0], pos[1] if len(pos) > 1 else "cases", opt("--repeat", 1), opt("--jobs", 4))
    else:
        score(sys.argv[2:])
