#!/bin/bash
# Runs Hindsight natively (pip's hindsight-api 0.10.3, embedded PostgreSQL through pg0) in the
# foreground, as the Docker image would: hindsight_native.sh <instance> <port> <llm tag>
# e.g. hindsight_native.sh bench 8888 hindsight     (the test set)
#      hindsight_native.sh lme 8889 lme-hindsight    (LongMemEval)
# The LLM is DeepSeek deepseek-flash through the counting proxy (which holds the key); the
# embeddings come from the shared Qwen3 server (8072); the reranker stays local, as Hindsight's
# default does (HS_RERANKER below). HOME points into
# a short folder ($HSB, by default %TEMP%/hsb), so pg0 and the model cache stay out of the user's home and under Windows' path limit.
cd "$(dirname "$0")"
name=$1; port=$2; tag=$3
HSB=${HSB:-$(cygpath -m "$TEMP")/hsb}
H="$HSB/$name"; mkdir -p "$H"
export HOME="$H" USERPROFILE="$H"
export LOCALAPPDATA="$H/AppData/Local"
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost PYTHONIOENCODING=utf-8
export HF_ENDPOINT=https://hf-mirror.com
unset HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy
export HINDSIGHT_API_HOST=127.0.0.1 HINDSIGHT_API_PORT="$port"
export HINDSIGHT_API_DATABASE_URL="pg0://$name"
export HINDSIGHT_API_LLM_PROVIDER=openai HINDSIGHT_API_LLM_MODEL=deepseek-flash
export HINDSIGHT_API_LLM_BASE_URL="http://127.0.0.1:8071/$tag/v1" HINDSIGHT_API_LLM_API_KEY=sk-local
export HINDSIGHT_API_EMBEDDINGS_PROVIDER=openai HINDSIGHT_API_EMBEDDINGS_OPENAI_MODEL=qwen3-embedding-0.6b
export HINDSIGHT_API_EMBEDDINGS_OPENAI_BASE_URL=http://127.0.0.1:8072/v1 HINDSIGHT_API_EMBEDDINGS_OPENAI_API_KEY=sk-local
# HS_RERANKER: a local cross-encoder folder; Hindsight's own default (ms-marco-MiniLM-L-6-v2, English)
# unless set, e.g. HS_RERANKER=$HSB/bge-reranker-v2-m3 (multilingual, ~25x larger).
export HINDSIGHT_API_RERANKER_PROVIDER=local HINDSIGHT_API_RERANKER_LOCAL_MODEL=${HS_RERANKER:-$HSB/ms-marco-MiniLM-L-6-v2}
exec venv-hindsight/Scripts/hindsight-api.exe --host 127.0.0.1 --port "$port"
