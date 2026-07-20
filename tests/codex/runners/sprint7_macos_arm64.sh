#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$repo_root"

evidence="tests/codex/evidence/sprint-7-live-editor-macos.json"
if [[ -e "$evidence" ]]; then
  echo "Refusing to overwrite existing evidence: $evidence" >&2
  exit 1
fi

succeeded=0
cleanup() {
  if [[ "$succeeded" -ne 1 && -e "$evidence" ]]; then
    rm -f -- "$evidence"
  fi
}
trap cleanup EXIT

python3 tests/codex/sprint7_acceptance.py \
  --output "$evidence" \
  --timeout 120
python3 tests/codex/sprint7_acceptance.py \
  --validate "$evidence"

succeeded=1
