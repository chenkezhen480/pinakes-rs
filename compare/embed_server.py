"""An OpenAI-compatible /v1/embeddings server, so every compared system embeds with the same
model. EMBED_MODEL picks it:

- jina (port 8070): jina-embeddings-v2-base-zh, fp32 ONNX, the revision Pinakes downloads (in
  models/, by setup.sh); mean pooling over the attention mask, L2-normalized.
- qwen3 (port 8072): Qwen3-Embedding-0.6B, fp32 ONNX (onnx-community); the last token's state,
  L2-normalized (qwen3.py). Texts are embedded as they come: an OpenAI-style call doesn't say
  whether a text is a query, so no query instruction is added, for any system.
"""

import os
import pathlib
import threading
import time

import numpy as np
import uvicorn
from fastapi import FastAPI
from pydantic import BaseModel

MODEL = os.environ.get("EMBED_MODEL", "jina")
PORT = int(os.environ.get("EMBED_PORT", {"jina": "8070", "qwen3": "8072", "qwen3-int8": "8073"}[MODEL]))

if MODEL.startswith("qwen3"):
    from qwen3 import Qwen3

    # qwen3-int8: onnx-community's 8-bit export of the same model, for the LongMemEval runs.
    _qwen = Qwen3(threads=4, file="onnx/model_int8.onnx" if MODEL == "qwen3-int8" else "onnx/model.onnx")

    def embed_one(text: str) -> np.ndarray:
        return _qwen.embed(text)
else:
    import onnxruntime as ort
    from tokenizers import Tokenizer

    SNAP = pathlib.Path(__file__).parent / "models" / "jina-embeddings-v2-base-zh"
    opts = ort.SessionOptions()
    opts.intra_op_num_threads = 4
    session = ort.InferenceSession(str(SNAP / "onnx/model.onnx"), opts, providers=["CPUExecutionProvider"])
    inputs = {i.name for i in session.get_inputs()}
    tok = Tokenizer.from_file(str(SNAP / "tokenizer.json"))
    tok.enable_truncation(8192)

    def embed_one(text: str) -> np.ndarray:
        enc = tok.encode(text)
        ids = np.array([enc.ids], dtype=np.int64)
        mask = np.array([enc.attention_mask], dtype=np.int64)
        feed = {"input_ids": ids, "attention_mask": mask}
        if "token_type_ids" in inputs:
            feed["token_type_ids"] = np.zeros_like(ids)
        hidden = session.run(None, feed)[0]
        m = mask[..., None].astype(np.float32)
        v = ((hidden * m).sum(1) / m.sum(1))[0]
        return v / np.linalg.norm(v)

lock = threading.Lock()
stats = {"model": MODEL, "calls": 0, "texts": 0, "seconds": 0.0}
app = FastAPI()


class Req(BaseModel):
    input: str | list[str]
    model: str = MODEL
    encoding_format: str | None = None


@app.post("/v1/embeddings")
def embeddings(req: Req):
    texts = [req.input] if isinstance(req.input, str) else req.input
    started = time.perf_counter()
    with lock:  # one text at a time: no padding cost, and pages differ a lot in length
        vecs = [embed_one(t) for t in texts]
    stats["calls"] += 1
    stats["texts"] += len(texts)
    stats["seconds"] += time.perf_counter() - started
    return {
        "object": "list",
        "model": req.model,
        "data": [{"object": "embedding", "index": i, "embedding": v.tolist()} for i, v in enumerate(vecs)],
        "usage": {"prompt_tokens": 0, "total_tokens": 0},
    }


@app.get("/stats")
def get_stats():
    return stats


if __name__ == "__main__":
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")
