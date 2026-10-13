"""Builds the Qwen3 Mem0 store from the round-1 one without calling the LLM again:

    mem0_reembed.py <source store dir> <target store dir>

Mem0 extracted the memories (and their entities) with DeepSeek in round 1; only the embedding
model changes, so every point keeps its id, payload and BM25 sparse vector and gets its dense
vector again from the Qwen3 server, as Mem0 would have embedded the same text (the memory's
`data`, the entity's name). history.db is copied as it is.
"""

import shutil
import sys

import httpx
from qdrant_client import QdrantClient, models

EMBED_URL = "http://127.0.0.1:8072/v1/embeddings"
DIM = 1024

src_dir, dst_dir = sys.argv[1], sys.argv[2]
shutil.rmtree(dst_dir, ignore_errors=True)
shutil.copytree(src_dir, dst_dir, ignore=shutil.ignore_patterns("qdrant"))
src = QdrantClient(path=f"{src_dir}/qdrant")
dst = QdrantClient(path=f"{dst_dir}/qdrant")
http = httpx.Client(timeout=600, trust_env=False)


def embed(texts):
    r = http.post(EMBED_URL, json={"input": texts, "model": "qwen3-embedding-0.6b"})
    r.raise_for_status()
    return [d["embedding"] for d in r.json()["data"]]


for col in src.get_collections().collections:
    info = src.get_collection(col.name).config.params
    dst.create_collection(
        col.name,
        vectors_config=models.VectorParams(size=DIM, distance=info.vectors.distance, on_disk=True),
        sparse_vectors_config=info.sparse_vectors,
    )
    offset, n = None, 0
    while True:
        pts, offset = src.scroll(col.name, limit=64, offset=offset, with_payload=True, with_vectors=True)
        if not pts:
            break
        dense = embed([p.payload["data"] for p in pts])
        dst.upsert(col.name, [
            models.PointStruct(id=p.id, payload=p.payload,
                               vector={"": d, **{k: v for k, v in p.vector.items() if k != ""}})
            for p, d in zip(pts, dense)])
        n += len(pts)
        print(col.name, n, flush=True)
        if offset is None:
            break
    assert dst.get_collection(col.name).points_count == src.get_collection(col.name).points_count
src.close()
dst.close()
print("done")
