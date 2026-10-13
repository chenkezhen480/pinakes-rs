#!/bin/bash
# Runs one compared system's server in the foreground: serve.sh mem0|graphiti|lightrag
cd "$(dirname "$0")"
export NO_PROXY=127.0.0.1,localhost no_proxy=127.0.0.1,localhost PYTHONIOENCODING=utf-8
export MEM0_TELEMETRY=False GRAPHITI_TELEMETRY_ENABLED=false
# Mem0 and Graphiti only talk to local services; LightRAG keeps the proxy for tiktoken's download.
[ "$1" != lightrag ] && unset HTTP_PROXY HTTPS_PROXY ALL_PROXY
exec "venv-$1/Scripts/python.exe" -u "sys_$1.py" serve > "serve-$1.log" 2>&1
