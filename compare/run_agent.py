"""The same agent (Pi 1.1, DeepSeek deepseek-flash, thinking off) answers every test-set question
with one system's read tools over MCP, or with plain files.

    python run_agent.py <system> [ids-file] [--jobs N]

system: pinakes, mem0, graphiti, lightrag, hindsight, or files (Pi's own read/grep/find/ls over a
folder holding the pages and graph files the caller may see, no MCP); `<system>+read` also gives
the agent Pi's `read`, so it can open the full output Pi 1.1 saves when it cuts a tool's text past
20 KB (results/agent-v1/<system>+read). Pi's own MCP client
connects to the system (`.pi/mcp.json` in the run's folder, trusted with `-a`), its tools exposed
`direct`, as built-in tools are; writing tools are left out for every system. Each run's events
go to results/agent-v1/<system>/<question id>.jsonl; a run already there is skipped. Pi's own
settings live in pi/agent-home, not the user's ~/.pi. The DeepSeek key is read from model.conf
into Pi's environment, never printed.
"""

import concurrent.futures
import json
import os
import pathlib
import shutil
import subprocess
import sys
import time
import tomllib

import corpus

HERE = pathlib.Path(__file__).parent
RUNS = HERE / "pi" / "runs-v1"
OUT = HERE / "results" / "agent-v1"
AGENT_HOME = HERE / "pi" / "agent-home"
# Node with Pi's entry point, not a .cmd shim: cmd.exe would mangle quotes in the questions.
PI = [shutil.which("node"),
      str(HERE / "pi/v1/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js")]
MODEL = "deepseek/deepseek-flash"

SERVERS = {
    "pinakes": "http://127.0.0.1:8084/mcp",
    "mem0": "http://127.0.0.1:8081/mcp",
    "graphiti": "http://127.0.0.1:8082/mcp",
    "lightrag": "http://127.0.0.1:8083/mcp",
    "hindsight": "http://127.0.0.1:8086/mcp",
}
# The read tools each system offers; nothing that writes.
TOOLS = {
    "pinakes": ["search", "read", "graph", "path"],
    "mem0": ["search_memories"],
    "graphiti": ["search_nodes", "search_memory_facts"],
    "lightrag": ["query_knowledge"],
    "hindsight": ["recall", "reflect"],
}

PROMPT = (
    "You are the assistant of a team at {business}. {description} "
    "Look things up with the knowledge tools you have, answer from what they return, and say so "
    "when they hold nothing on the question. Answer in the language of the question."
)


def groups_of(tenant, user):
    for u in corpus.tenant_yaml(tenant)["users"]:
        if u["id"] == user:
            return u.get("groups", [])
    return []


def run_dir(system: str, tenant: str, user: str | None) -> pathlib.Path:
    d = RUNS / system / f"{tenant}-{user or 'none'}"
    if system == "files":
        if not d.exists():
            make_files_dir(d, tenant, user)
        return d
    (d / ".pi").mkdir(parents=True, exist_ok=True)
    system = system.split("+")[0]
    if system == "pinakes":
        keys = json.loads((HERE / "pinakes-keys.json").read_text())
        headers = {"Authorization": f"Bearer {keys[tenant]}"}
        if user:
            headers["X-User-Id"] = user
        if groups_of(tenant, user):
            headers["X-User-Groups"] = ",".join(groups_of(tenant, user))
    else:
        headers = {"X-Tenant": tenant}
        if user:
            headers["X-User-Id"] = user
    cfg = {"mcpServers": {"kb": {"url": SERVERS[system], "headers": headers, "timeout": 300,
                                 "exposure": "direct"}}}
    (d / ".pi" / "mcp.json").write_text(json.dumps(cfg, indent=1), encoding="utf-8")
    return d


def make_files_dir(d: pathlib.Path, tenant: str, user: str | None):
    """The pages and graph data the caller may read, as files: what an agent with a folder of
    Markdown (an "LLM wiki") would have."""
    scopes = corpus.visible_scopes(tenant, user)
    root = corpus.ROOT / "tenants" / tenant
    for p in corpus.pages(tenant):
        if p["scope"] in scopes:
            f = d / "wiki" / (p["id"] + ".md")
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text(p["text"], encoding="utf-8")
    for s in corpus.spaces(tenant):
        if s["scope"] in scopes:
            f = d / "graph" / f"{s['space']}.json"
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_text((root / "graph" / f"{s['space']}.json").read_text(encoding="utf-8"), encoding="utf-8")


def key_env() -> dict:
    conf = tomllib.load(open(corpus.REPO / "model.conf", "rb"))
    key = conf.get("api_key") or os.environ[conf["key_env"]]
    env = {k: v for k, v in os.environ.items()
           if k.upper() not in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY")}
    env.update({"DEEPSEEK_API_KEY": key, "NO_PROXY": "127.0.0.1,localhost", "no_proxy": "127.0.0.1,localhost",
                "PI_CODING_AGENT_DIR": str(AGENT_HOME), "PI_OFFLINE": "1"})
    return env


def args_for(system: str, prompt: str, question: str) -> list[str]:
    args = [*PI, "-p", "--mode", "json", "--no-session", "--no-context-files", "--thinking", "off",
            "--model", MODEL, "--append-system-prompt", prompt, "-ns", "-np"]
    if system == "files":
        args += ["-ne", "--no-mcp", "--tools", "read,grep,find,ls"]
    else:
        base, _, extra = system.partition("+")
        args += ["-a", "--tools", ",".join([f"mcp__kb__{t}" for t in TOOLS[base]] + ([extra] if extra else []))]
    return [*args, "--", question]


def run(system: str, q: dict, env: dict) -> str:
    out = OUT / system / f"{q['id']}.jsonl"
    if out.exists() and out.stat().st_size > 0:
        return f"{q['id']}: kept"
    out.parent.mkdir(parents=True, exist_ok=True)
    d = run_dir(system, q["tenant"], q.get("user"))
    t = corpus.tenant_yaml(q["tenant"])
    prompt = PROMPT.format(business=t["business"], description=" ".join(t.get("description", "").split()))
    started = time.perf_counter()
    tmp = out.with_suffix(".part")
    with open(tmp, "wb") as f:
        try:
            p = subprocess.run(args_for(system, prompt, q["query"]), cwd=d, env=env, stdin=subprocess.DEVNULL,
                               stdout=f, stderr=subprocess.PIPE, timeout=600)
            code, err = p.returncode, p.stderr.decode("utf-8", "replace")[-2000:]
        except subprocess.TimeoutExpired:
            code, err = "timeout", ""
    took = time.perf_counter() - started
    # The streaming deltas repeat the whole message each time; the finished messages are enough.
    kept = [l for l in tmp.read_text(encoding="utf-8", errors="replace").splitlines()
            if not l.startswith('{"type":"message_update"')]
    tmp.write_text("\n".join(kept) + "\n", encoding="utf-8")
    with open(tmp, "a", encoding="utf-8") as f:
        f.write(json.dumps({"type": "bench_run", "exit": code, "seconds": round(took, 2), "stderr": err},
                           ensure_ascii=False) + "\n")
    tmp.replace(out)
    return f"{q['id']}: exit {code} in {took:.0f}s"


def main():
    system = sys.argv[1]
    jobs = 4
    ids = None
    rest = sys.argv[2:]
    if "--jobs" in rest:
        i = rest.index("--jobs")
        jobs = int(rest[i + 1])
        rest = rest[:i] + rest[i + 2:]
    if rest:
        ids = set(pathlib.Path(rest[0]).read_text().split())
    qs = [q for q in corpus.questions() if ids is None or q["id"] in ids]
    env = key_env()
    (AGENT_HOME / "bin").mkdir(parents=True, exist_ok=True)
    for exe in ("rg.exe", "fd.exe"):  # Pi fetches these on first use and can't unpack them on Windows
        src = pathlib.Path.home() / ".pi/agent/bin" / exe
        if src.exists() and not (AGENT_HOME / "bin" / exe).exists():
            shutil.copy(src, AGENT_HOME / "bin" / exe)
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        for line in pool.map(lambda q: run(system, q, env), qs):
            print(line, flush=True)


if __name__ == "__main__":
    main()
