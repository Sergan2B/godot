$ErrorActionPreference = "Stop"

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
Set-Location $RepoRoot

python -m SCons platform=windows target=editor dev_build=yes tests=yes `
  module_codex_bridge_enabled=yes accesskit=no d3d12=no angle=no -j8
cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp\Cargo.toml `
  -p godot-codex-mcp
python tests\codex\sprint4_scene_graph_live.py `
  --godot bin\godot.windows.editor.dev.x86_64.console.exe `
  --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
  --evidence tests\codex\evidence\sprint-4-scene-graph-windows.json
python tests\codex\sprint4_acceptance.py validate-platform `
  tests\codex\evidence\sprint-4-scene-graph-windows.json `
  --platform windows-x86_64
