#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$repo_root"

evidence="tests/codex/evidence/sprint-5-script-semantics-macos.json"
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

.venv/bin/scons platform=macos arch=arm64 target=editor dev_build=yes tests=yes \
  module_codex_bridge_enabled=yes accesskit=no angle=no metal=yes vulkan=no -j8
cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-mcp
cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-bridge-client --example script_graph_live
python3 tests/codex/sprint5_script_semantics_live.py \
  --godot bin/godot.macos.editor.dev.arm64 \
  --sidecar godot-codex-mcp/target/release/godot-codex-mcp \
  --script-probe godot-codex-mcp/target/release/examples/script_graph_live \
  --evidence "$evidence" \
  --timeout 180
python3 tests/codex/sprint5_acceptance.py validate-platform \
  "$evidence" \
  --platform macos-arm64

succeeded=1
