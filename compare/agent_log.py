"""Reads one Pi run (--mode json events): tool calls, the final answer, usage and time.

    python agent_log.py <file.jsonl>     prints the run
"""

import json
import pathlib
import sys


def parse(path: pathlib.Path) -> dict:
    calls, answer, usage, run = [], "", {"input": 0, "output": 0, "cacheRead": 0, "turns": 0}, {}
    results = {}
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        try:
            e = json.loads(line)
        except json.JSONDecodeError:
            continue
        t = e.get("type")
        if t == "tool_execution_start":
            calls.append({"id": e.get("toolCallId"), "tool": e.get("toolName"), "args": e.get("args")})
        elif t == "tool_execution_end":
            res = e.get("result") or {}
            text = "".join(c.get("text", "") for c in res.get("content", []) if isinstance(c, dict))
            results[e.get("toolCallId")] = {"chars": len(text), "error": bool(e.get("isError")), "text": text}
        elif t == "message_end":
            m = e.get("message") or {}
            if m.get("role") == "assistant":
                u = m.get("usage") or {}
                usage["input"] += u.get("input", 0)
                usage["output"] += u.get("output", 0)
                usage["cacheRead"] += u.get("cacheRead", 0)
                usage["cost"] = usage.get("cost", 0) + ((u.get("cost") or {}).get("total") or 0)
                usage["turns"] += 1
                text = "".join(c.get("text", "") for c in m.get("content", []) if c.get("type") == "text")
                if text.strip():
                    answer = text
                if m.get("errorMessage"):
                    run["error"] = m["errorMessage"]
        elif t == "bench_run":
            run.update(e)
    for c in calls:
        c.update(results.get(c["id"], {}))
    return {"calls": calls, "answer": answer, "usage": usage, "run": run}


if __name__ == "__main__":
    r = parse(pathlib.Path(sys.argv[1]))
    for c in r["calls"]:
        print(f"CALL {c['tool']} {json.dumps(c['args'], ensure_ascii=False)[:300]}")
        print(f"  -> {c.get('chars')} chars{' ERROR' if c.get('error') else ''}: {c.get('text', '')[:300]!r}")
    print("ANSWER:", r["answer"][:1500])
    print("USAGE:", r["usage"], "RUN:", {k: v for k, v in r["run"].items() if k != "stderr"})
    if r["run"].get("stderr"):
        print("STDERR:", r["run"]["stderr"][-600:])
