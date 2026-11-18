#!/usr/bin/env bash
set -euo pipefail
for f in "${FILES[@]}"; do
  echo "${f##*/} $(wc -l < "$f")" >> "$LOG" # count
done
[[ -n ${TOKEN:-} ]] && curl -fsS -H "X-Token: $TOKEN" https://example.com/hook
