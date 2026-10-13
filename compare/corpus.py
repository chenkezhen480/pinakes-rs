"""The Pinakes test set as plain data every compared system ingests the same way.

A scope says who may read something: "t" (the whole tenant), "u:<user>", "g:<group>", or
"s:<space>" (a restricted graph space). Pinakes enforces this itself; for the other systems
the adapters keep each scope apart (their own partitions: Mem0 user_id, Graphiti group_id,
LightRAG workspace) and search only the scopes the caller may read."""

import json
import pathlib

import yaml

REPO = pathlib.Path(__file__).resolve().parents[1]
ROOT = REPO / "testset"


def tenants() -> list[str]:
    return sorted(p.name for p in (ROOT / "tenants").iterdir() if p.is_dir())


def tenant_yaml(tenant: str) -> dict:
    return yaml.safe_load((ROOT / "tenants" / tenant / "tenant.yaml").read_text(encoding="utf-8"))


def pages(tenant: str) -> list[dict]:
    """Every wiki page: id (as questions name it), scope, and the file's whole text."""
    wiki = ROOT / "tenants" / tenant / "wiki"
    out = []
    for f in sorted(wiki.rglob("*.md")):
        rel = f.relative_to(wiki).with_suffix("").as_posix()
        parts = rel.split("/")
        if parts[0] == "users":
            scope = f"u:{parts[1]}"
        elif parts[0] == "groups":
            scope = f"g:{parts[1]}"
        else:
            scope = "t"
        out.append({"id": rel, "scope": scope, "text": f.read_text(encoding="utf-8")})
    return out


def spaces(tenant: str) -> list[dict]:
    """Every graph space with its scope, nodes and edges."""
    meta = {s["id"]: s for s in tenant_yaml(tenant)["graph"]["spaces"]}
    out = []
    for f in sorted((ROOT / "tenants" / tenant / "graph").glob("*.json")):
        d = json.loads(f.read_text(encoding="utf-8"))
        m = meta[d["space"]]
        vis = m.get("visibility", "shared")
        scope = {"shared": "t", "private": f"u:{m.get('owner')}", "restricted": f"s:{d['space']}"}[vis]
        out.append({**d, "scope": scope, "name": m["name"], "aliases": m.get("aliases", [])})
    return out


def node_label(node: dict) -> str:
    aka = ", ".join(node.get("aliases", []))
    return f"{node['name']} ({node['type']} {node['key']}" + (f"; also {aka})" if aka else ")")


def edge_sentences(space: dict) -> list[str]:
    """Each edge as one line of text, for systems that take text only."""
    nodes = {f"{n['type']}/{n['key']}": n for n in space["nodes"]}
    lines = []
    for e in space["edges"]:
        a, b = nodes[e["from"]], nodes[e["to"]]
        lines.append(
            f"[{space['name']} / {space['space']}] {node_label(a)} --{e['type']}--> {node_label(b)}"
        )
    return lines


def visible_scopes(tenant: str, user: str | None) -> list[str]:
    t = tenant_yaml(tenant)
    groups = []
    for u in t["users"]:
        if u["id"] == user:
            groups = u.get("groups", [])
    scopes = ["t"]
    if user:
        scopes.append(f"u:{user}")
    scopes += [f"g:{g}" for g in groups]
    for s in t["graph"]["spaces"]:
        if s.get("visibility") == "restricted":
            mem = s.get("members", {})
            if user in mem.get("users", []) or set(groups) & set(mem.get("groups", [])):
                scopes.append(f"s:{s['id']}")
    return scopes


def questions() -> list[dict]:
    out = []
    for f in sorted((ROOT / "questions").glob("*.yaml")):
        d = yaml.safe_load(f.read_text(encoding="utf-8"))
        for q in d["questions"]:
            out.append({**q, "tenant": d["tenant"]})
    return out


def partition(tenant: str, scope: str) -> str:
    """A name for one tenant's scope in a system's own partitioning: letters, digits, _ only."""
    return f"{tenant}__{scope.replace(':', '_').replace('-', '_')}"
