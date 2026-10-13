"""A small stateless MCP server (Streamable HTTP, JSON responses) for the compared systems, and a
/retrieve route for the speed runs. The caller's tenant and user come from headers (X-Tenant,
X-User-Id), as Pinakes takes them from its key and X-User-Id; the agent never names a scope.
Every tool call and retrieval is logged with its server-side time."""

import json
import pathlib
import time
from typing import Any, Awaitable, Callable

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse, Response

Handler = Callable[[dict, str, str | None], Awaitable[Any]]


class Tool:
    def __init__(self, name: str, description: str, schema: dict, handler: Handler):
        self.name, self.description, self.schema, self.handler = name, description, schema, handler


def app(name: str, instructions: str, tools: list[Tool], retrieve: Handler, log: pathlib.Path) -> FastAPI:
    api = FastAPI()
    by_name = {t.name: t for t in tools}

    def record(kind: str, what: str, tenant: str, user: str | None, ms: float, args: dict, ok: bool):
        with log.open("a", encoding="utf-8") as f:
            f.write(json.dumps({"t": time.time(), "kind": kind, "tool": what, "tenant": tenant, "user": user,
                                "ms": round(ms, 2), "ok": ok, "args": args}, ensure_ascii=False) + "\n")

    async def handle(msg: dict, tenant: str, user: str | None) -> dict | None:
        method, mid, params = msg.get("method"), msg.get("id"), msg.get("params") or {}
        if mid is None:
            return None  # a notification
        if method == "initialize":
            result = {
                "protocolVersion": params.get("protocolVersion", "2025-06-18"),
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": name, "version": "bench"},
                "instructions": instructions,
            }
        elif method == "ping":
            result = {}
        elif method == "tools/list":
            result = {"tools": [{"name": t.name, "description": t.description, "inputSchema": t.schema}
                                for t in tools]}
        elif method == "tools/call":
            tool = by_name.get(params.get("name"))
            if tool is None:
                return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32602, "message": "unknown tool"}}
            args = params.get("arguments") or {}
            started = time.perf_counter()
            try:
                out = await tool.handler(args, tenant, user)
                text, err = json.dumps(out, ensure_ascii=False, default=str), False
            except Exception as e:  # the agent reads why, as with any MCP server
                text, err = json.dumps({"error": f"{type(e).__name__}: {e}"}, ensure_ascii=False), True
            record("tool", tool.name, tenant, user, (time.perf_counter() - started) * 1000, args, not err)
            result = {"content": [{"type": "text", "text": text}], "isError": err}
        else:
            return {"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": f"no method {method}"}}
        return {"jsonrpc": "2.0", "id": mid, "result": result}

    @api.post("/mcp")
    async def mcp(request: Request):
        tenant = request.headers.get("x-tenant", "")
        user = request.headers.get("x-user-id") or None
        body = await request.json()
        if isinstance(body, list):
            out = [r for r in [await handle(m, tenant, user) for m in body] if r is not None]
            return JSONResponse(out) if out else Response(status_code=202)
        r = await handle(body, tenant, user)
        return JSONResponse(r) if r is not None else Response(status_code=202)

    @api.get("/mcp")
    async def no_stream():
        return Response(status_code=405)

    @api.delete("/mcp")
    async def no_session():
        return Response(status_code=405)

    @api.post("/retrieve")
    async def do_retrieve(request: Request):
        body = await request.json()
        started = time.perf_counter()
        items = await retrieve(body, body["tenant"], body.get("user"))
        ms = (time.perf_counter() - started) * 1000
        record("retrieve", "retrieve", body["tenant"], body.get("user"), ms, {"query": body["query"]}, True)
        return {"ms": ms, "items": items}

    return api
