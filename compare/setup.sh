#!/bin/bash
# Prepares everything the comparison needs, in this folder: one Python environment per system
# (pinned versions; uv), a Neo4j for Graphiti, Pi 1.1 with its own settings, both rounds'
# embedding models (in models/), Hindsight's rerankers (in $HSB), and a Pinakes data folder with
# the test set (release build, vectors on). Run from Git Bash; see README.md.
set -e
cd "$(dirname "$0")"
REPO=$(cd .. && pwd)

uv venv -q -p 3.11 venv-embed
VIRTUAL_ENV=venv-embed uv pip install -q onnxruntime==1.31.0 tokenizers==0.23.3 fastapi==0.143.0 \
  uvicorn==0.54.0 numpy==2.4.6 httpx==0.28.1 pyyaml==6.0.3

uv venv -q -p 3.11 venv-mem0
VIRTUAL_ENV=venv-mem0 uv pip install -q "mem0ai[nlp]==2.2.1" fastembed==0.9.0 qdrant-client==1.19.1 \
  fastapi==0.143.0 uvicorn==0.54.0 httpx==0.28.1 pyyaml==6.0.3 \
  "en_core_web_sm @ https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl"

uv venv -q -p 3.11 venv-graphiti
VIRTUAL_ENV=venv-graphiti uv pip install -q graphiti-core==0.30.2 neo4j==6.4.0 fastapi==0.143.0 \
  uvicorn==0.54.0 httpx==0.28.1 pyyaml==6.0.3

uv venv -q -p 3.11 venv-lightrag
VIRTUAL_ENV=venv-lightrag uv pip install -q "lightrag-hku[api]==1.5.7" fastapi==0.143.0 uvicorn==0.54.0 \
  httpx==0.28.1 pyyaml==6.0.3

# Graphiti's graph: a Neo4j of its own (FalkorDB's image of 2026-10-08 refuses graphiti-core 0.30.2's
# index calls).
docker run -d --name bench-neo4j-graphiti -p 127.0.0.1:7475:7474 -p 127.0.0.1:7688:7687 \
  -e NEO4J_AUTH=neo4j/benchpass123 -e NEO4J_server_memory_heap_max__size=1G neo4j:5.26.28

# Hindsight, natively (pip's hindsight-api with an embedded PostgreSQL; the image's ghcr pull is
# slow from here). Its adapter needs only httpx and pyyaml on top.
uv venv -q -p 3.11 venv-hindsight
VIRTUAL_ENV=venv-hindsight uv pip install -q hindsight-api==0.10.3 httpx==0.28.1 pyyaml==6.0.3

# Round 1's embedding model for the other systems, at the revision Pinakes uses, in full
# precision (the service keeps only its own 8-bit copy).
HF=${HF_ENDPOINT:-https://huggingface.co}
J=models/jina-embeddings-v2-base-zh
mkdir -p $J/onnx
for f in tokenizer.json onnx/model.onnx; do
  curl -sfL -o $J/$f "$HF/jinaai/jina-embeddings-v2-base-zh/resolve/c1ff9086a89a1123d7b5eff58055a665db4fb4b9/$f"
done

# Round 2's embedding model for the other systems (Qwen3-Embedding-0.6B, fp32 ONNX, a fixed
# revision) and Hindsight's rerankers: its default (English) and a multilingual one.
Q=models/qwen3-embedding-0.6b
mkdir -p $Q/onnx
for f in config.json tokenizer.json tokenizer_config.json special_tokens_map.json added_tokens.json          onnx/model.onnx onnx/model.onnx_data; do
  curl -sfL -o $Q/$f "$HF/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/c25a394dd583836952667c12f008335071b3f43d/$f"
done
HSB=${HSB:-$(cygpath -m "$TEMP")/hsb}
HF_ENDPOINT=$HF venv-hindsight/Scripts/python.exe -I -c "
from huggingface_hub import snapshot_download
for repo in ('cross-encoder/ms-marco-MiniLM-L-6-v2', 'BAAI/bge-reranker-v2-m3'):
    snapshot_download(repo, local_dir='$HSB/' + repo.split('/')[1],
                      allow_patterns=['*.json', '*.txt', '*.model', 'model.safetensors'])"

# Pi 1.1 in a folder of its own, with its own settings (PI_CODING_AGENT_DIR=pi/agent-home): every
# project trusted, so its native MCP client reads each run's .pi/mcp.json. Nothing goes to ~/.pi.
mkdir -p pi/v1 pi/agent-home
(cd pi/v1 && npm init -y > /dev/null && npm i -s @earendil-works/pi-coding-agent@1.1.0)
echo '{"defaultProjectTrust": "always"}' > pi/agent-home/settings.json

# Pinakes: a data folder of its own with the test set, vectors on, port 8084.
(cd "$REPO" && cargo build --release --bins --example load_testset)
mkdir -p pinakes-data/models
cp -r "$REPO/target/models/models--jinaai--jina-embeddings-v2-base-zh" pinakes-data/models/
venv-embed/Scripts/python.exe - "$REPO" "$(pwd)" <<'EOF'
import re, sys
repo, here = sys.argv[1], sys.argv[2]
s = open(f"{repo}/config.toml", encoding="utf-8").read()
s = re.sub(r'(?m)^port = 8080', 'port = 8084', s, count=1)
s = re.sub(r'(?m)^dir = "\./data"', f'dir = "{here}/pinakes-data"', s, count=1)
s = s.replace('model_file = "model.conf"', f'model_file = "{repo}/model.conf"')
s = re.sub(r'(?m)^enabled = false(\s+# off: keyword search only)', r'enabled = true\1', s, count=1)
open(f"{here}/pinakes.toml", "w", encoding="utf-8", newline="\n").write(s)
EOF
"$REPO/target/release/examples/load_testset.exe" "$(pwd)/pinakes-data" "$(pwd)/pinakes.toml" > pinakes-load.txt
venv-embed/Scripts/python.exe - <<'EOF'
import json, re
keys = {m.group(1): m.group(2) for l in open("pinakes-load.txt", encoding="utf-8")
        if (m := re.match(r"^(\w+)\s+key (pnx_\S+)", l))}
json.dump(keys, open("pinakes-keys.json", "w"))
EOF
echo ready
