"""A local forwarder to DeepSeek's OpenAI-compatible API that counts every call.

The key is read here from model.conf and never leaves this process; the compared systems get a
placeholder key and the base URL http://127.0.0.1:8071/<tag>/v1, where <tag> names who calls
(mem0-ingest, graphiti-query, ...). Each call is appended to llm_calls.jsonl with its tag,
latency and token usage."""

import json
import os
import pathlib
import time
import tomllib

import httpx
import uvicorn
from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response

conf = tomllib.load(open(pathlib.Path(__file__).resolve().parents[1] / "model.conf", "rb"))
KEY = conf.get("api_key") or os.environ[conf["key_env"]]
UPSTREAM = "https://api.deepseek.com"
LOG = pathlib.Path(__file__).with_name("llm_calls.jsonl")

app = FastAPI()
client = httpx.AsyncClient(timeout=600)


@app.post("/{tag}/v1/{path:path}")
async def forward(tag: str, path: str, request: Request):
    body = await request.json()
    if body.get("stream"):
        return JSONResponse({"error": {"message": "streaming is not forwarded"}}, status_code=400)
    # Thinking off for every system, as the Pinakes librarian runs (fewer tokens, same model).
    body.setdefault("thinking", {"type": "disabled"})
    started = time.perf_counter()
    r = await client.post(
        f"{UPSTREAM}/v1/{path}",
        json=body,
        headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"},
    )
    took = time.perf_counter() - started
    usage = {}
    try:
        usage = r.json().get("usage") or {}
    except Exception:
        pass
    with LOG.open("a", encoding="utf-8") as f:
        f.write(json.dumps({
            "t": time.time(), "tag": tag, "path": path, "status": r.status_code,
            "seconds": round(took, 3), "model": body.get("model"),
            "prompt_tokens": usage.get("prompt_tokens", 0),
            "completion_tokens": usage.get("completion_tokens", 0),
            "cache_hit_tokens": usage.get("prompt_cache_hit_tokens", 0),
        }) + "\n")
    return Response(r.content, status_code=r.status_code, media_type="application/json")


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=8071, log_level="warning")
