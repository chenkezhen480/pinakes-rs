"""Which embedding model for Pinakes: jina-embeddings-v2-base-zh (the current one) against
Qwen3-Embedding-0.6B, by design.md's M5 measures, with meaning alone (Pinakes' `semantic` mode):

- every expected page's rank in its slot (memories: experience, decisions, preferences;
  knowledge: expertise, graph schemas), among the pages the caller may read; R@1, R@3, and the
  twelve cross-language questions on their own;
- off-topic: the best similarity an off-topic question reaches against the worst similarity of a
  right answer (a cut between them lets weak hits be dropped);
- the time to embed a query.

Pages are cut into units as `search::vectors::units` does (a head: title, aliases, keywords,
summary; then each section with text, prefixed with the title and heading); a page is as similar
as its best unit.

    python meaning_eval.py jina|qwen3|qwen3-instruct
"""

import json
import pathlib
import statistics
import sys
import time

import numpy as np
import onnxruntime as ort
import yaml
from tokenizers import Tokenizer

import corpus

HERE = pathlib.Path(__file__).parent
JINA = HERE / "models" / "jina-embeddings-v2-base-zh"
QWEN = HERE / "models" / "qwen3-embedding-0.6b"
INSTRUCT = "Instruct: Given a question, retrieve the knowledge-base passage that answers it\nQuery: "


class Model:
    def __init__(self, name):
        self.name = name
        if name == "qwen3":
            from qwen3 import Qwen3

            self.qwen = Qwen3()
            return
        opts = ort.SessionOptions()
        opts.intra_op_num_threads = 4
        self.sess = ort.InferenceSession(str(JINA / "onnx/model.onnx"), opts)
        self.tok = Tokenizer.from_file(str(JINA / "tokenizer.json"))
        self.tok.enable_truncation(8192)
        self.inputs = [i.name for i in self.sess.get_inputs()]

    def embed(self, text):
        if self.name == "qwen3":
            return self.qwen.embed(text)
        ids = np.array([self.tok.encode(text).ids], dtype=np.int64)
        feed = {"input_ids": ids, "attention_mask": np.ones_like(ids)}
        if "token_type_ids" in self.inputs:
            feed["token_type_ids"] = np.zeros_like(ids)
        v = self.sess.run(None, feed)[0][0].mean(0)  # mean pooling
        return v / np.linalg.norm(v)


def front_and_body(text):
    text = text.replace("\r\n", "\n")
    _, head, body = text.split("---\n", 2)
    return yaml.safe_load(head), body


def units(text):
    front, body = front_and_body(text)
    title = str(front.get("title", ""))
    head = title
    for key in ("aliases", "keywords"):
        vals = front.get(key) or []
        if vals:
            head += "\n" + "; ".join(str(v) for v in vals)
    if front.get("summary"):
        head += "\n" + str(front["summary"])
    out = [head]
    heading, lines = None, []

    def flush():
        if lines:
            t = title + (f" — {heading}" if heading and heading != title else "")
            out.append(t + "\n" + "\n".join(lines))

    for line in body.splitlines():
        if line.startswith("#"):
            flush()
            heading, lines = line.lstrip("#").strip(), []
        elif line.strip():
            lines.append(line.strip())
    flush()
    return out


SLOT = {"experience": "memories", "decisions": "memories", "preferences": "memories",
        "expertise": "knowledge", "graph": "knowledge"}


def slot_of(page_id):
    parts = page_id.split("/")
    if parts[0] in ("users", "groups"):
        parts = parts[2:]
    return SLOT.get(parts[0], "knowledge")


def main():
    variant = sys.argv[1]
    model = Model("jina" if variant == "jina" else "qwen3")
    prefix = INSTRUCT if variant == "qwen3-instruct" else ""
    vecs = {}
    started = time.perf_counter()
    for t in corpus.tenants():
        for p in corpus.pages(t):
            vecs[(t, p["id"])] = np.stack([model.embed(u) for u in units(p["text"])])
    print(f"{variant}: pages embedded in {time.perf_counter() - started:.0f} s", flush=True)

    ranks, xl, answer_sims, offtopic, q_ms = [], [], [], [], []
    for q in corpus.questions():
        t0 = time.perf_counter()
        qv = model.embed(prefix + q["query"])
        q_ms.append((time.perf_counter() - t0) * 1000)
        scopes = corpus.visible_scopes(q["tenant"], q.get("user"))
        visible = [p["id"] for p in corpus.pages(q["tenant"]) if p["scope"] in scopes]
        sims = {pid: float((vecs[(q["tenant"], pid)] @ qv).max()) for pid in visible}
        if q["category"] == "negative":
            offtopic.append(max(sims.values()))
            continue
        exp = q.get("expect") or {}
        for slot in ("memories", "knowledge"):
            for pid in exp.get(slot, []):
                pool = sorted((p for p in visible if slot_of(p) == slot), key=lambda p: -sims[p])
                r = pool.index(pid) + 1
                ranks.append(r)
                answer_sims.append(sims[pid])
                if q["category"] == "cross_lang":
                    xl.append(r)
    at = lambda rs, k: sum(1 for r in rs if r <= k)
    print(json.dumps({
        "variant": variant,
        "R@1": round(at(ranks, 1) / len(ranks), 3), "R@3": round(at(ranks, 3) / len(ranks), 3),
        "pages": len(ranks),
        "cross_lang_top3": at(xl, 3), "cross_lang_first": at(xl, 1), "cross_lang_n": len(xl),
        "best_offtopic": round(max(offtopic), 3), "worst_answer": round(min(answer_sims), 3),
        "answers_below_best_offtopic": sum(1 for s in answer_sims if s <= max(offtopic)),
        "query_ms_median": round(statistics.median(q_ms), 1),
    }))


if __name__ == "__main__":
    main()
