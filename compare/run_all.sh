#!/bin/bash
# The agent runs, two systems at a time (memory), then the grading.
cd "$(dirname "$0")"
export PYTHONIOENCODING=utf-8
PY=venv-embed/Scripts/python.exe
for pair in "pinakes mem0" "graphiti lightrag" "files"; do
  for s in $pair; do $PY run_agent.py $s --jobs 3 > "agent-$s.log" 2>&1 & done
  wait
done
for s in pinakes mem0 graphiti lightrag files; do $PY judge.py $s --jobs 8 > "judge-$s.log" 2>&1; done
echo all done
