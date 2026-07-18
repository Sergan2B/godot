#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../../.." && pwd)"
cd "$repo_root"

.venv/bin/scons platform=macos arch=arm64 target=editor dev_build=yes tests=yes \
  module_codex_bridge_enabled=yes accesskit=no angle=no metal=yes vulkan=no -j8
cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-mcp
python3 tests/codex/sprint4_scene_graph_live.py \
  --godot bin/godot.macos.editor.dev.arm64 \
  --sidecar godot-codex-mcp/target/release/godot-codex-mcp \
  --evidence tests/codex/evidence/sprint-4-scene-graph-macos.json
python3 tests/codex/sprint4_acceptance.py validate-platform \
  tests/codex/evidence/sprint-4-scene-graph-macos.json \
  --platform macos-arm64
