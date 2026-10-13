"""Qwen3-Embedding-0.6B (onnx-community's fp32 export, revision c25a394d) in ONNX Runtime: the
text's tokens (the tokenizer appends <|endoftext|>), an empty key/value cache for each of its
28 layers, and the last token's hidden state, L2-normalized, as the model card pools it."""

import pathlib

import numpy as np
import onnxruntime as ort
from tokenizers import Tokenizer

DIR = pathlib.Path(__file__).parent / "models" / "qwen3-embedding-0.6b"
MAX_TOKENS = 8192
DIM = 1024


class Qwen3:
    def __init__(self, threads: int = 4, file: str = "onnx/model.onnx"):
        opts = ort.SessionOptions()
        opts.intra_op_num_threads = threads
        self.sess = ort.InferenceSession(str(DIR / file), opts, providers=["CPUExecutionProvider"])
        self.tok = Tokenizer.from_file(str(DIR / "tokenizer.json"))
        self.tok.enable_truncation(MAX_TOKENS)
        self.past = [(i.name, [8, 128]) for i in self.sess.get_inputs() if i.name.startswith("past_key_values")]

    def embed(self, text: str) -> np.ndarray:
        ids = np.array([self.tok.encode(text).ids], dtype=np.int64)
        n = ids.shape[1]
        feed = {"input_ids": ids, "attention_mask": np.ones_like(ids),
                "position_ids": np.arange(n, dtype=np.int64)[None, :]}
        for name, (heads, dim) in self.past:
            feed[name] = np.zeros((1, heads, 0, dim), dtype=np.float32)
        hidden = self.sess.run(["last_hidden_state"], feed)[0][0]
        v = hidden[-1]
        return v / np.linalg.norm(v)
