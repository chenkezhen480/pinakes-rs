"""The rest of the filler pages into Mem0's scale store, in batches: the same points Mem0's
add(infer=False) writes (its embedding of the text, its payload with the BM25 lemmas), through
its own vector store's insert, many at a time. One add at a time slowed past 5 s a page on the
embedded Qdrant (local mode) after 2,000 pages.

    BENCH_STORE=mem0-scale python scale_mem0_bulk.py
"""

import hashlib
import json
import time
import uuid
from datetime import datetime, timezone

import corpus
import sys_mem0
from sys_mem0 import HERE


def main():
    from mem0.utils.lemmatization import lemmatize_for_bm25

    m = sys_mem0.memory("mem0-scale")
    c = m.vector_store.client
    have, offset = set(), None
    while True:
        pts, offset = c.scroll("bench", limit=1000, offset=offset, with_payload=["page"], with_vectors=False)
        have |= {p.payload.get("page") for p in pts}
        if offset is None:
            break
    pages = [json.loads(l) for l in (HERE / "scale" / "pages.jsonl").read_text(encoding="utf-8").splitlines()]
    todo = [p for p in pages if p["id"] not in have]
    print("present", len(pages) - len(todo), "to add", len(todo), flush=True)
    started = time.perf_counter()
    for i in range(0, len(todo), 50):
        batch = todo[i:i + 50]
        vectors = m.embedding_model.embed_batch([p["text"] for p in batch], "add")
        now = datetime.now(timezone.utc).isoformat()
        payloads = [{"page": p["id"], "tenant": "shopline", "user_id": corpus.partition("shopline", "t"),
                     "role": "user", "data": p["text"], "hash": hashlib.md5(p["text"].encode()).hexdigest(),
                     "created_at": now, "updated_at": now, "text_lemmatized": lemmatize_for_bm25(p["text"])}
                    for p in batch]
        m.vector_store.insert(vectors=vectors, payloads=payloads, ids=[str(uuid.uuid4()) for _ in batch])
        print(i + len(batch), round(time.perf_counter() - started), flush=True)
    print("done", c.count("bench").count, flush=True)


if __name__ == "__main__":
    main()
