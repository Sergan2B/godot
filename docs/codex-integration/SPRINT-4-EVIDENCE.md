# Sprint 4 evidence — scene semantics and node graph

**Status:** In progress — runner/source freeze pending; no platform result claimed yet

**Acceptance hosts:** macOS arm64 and Windows x86_64, local execution only

**Remote CI:** `not_run` by policy

## Gate

The Sprint 4 gate opens the real fixture in the Godot 4.8 editor and exercises
Bridge RPC 1.3 → Rust coordinator → persistent `segment-v2` → both scene MCP tools.
It runs the canonical phases `base`, `node_rename`, `node_reparent`,
`property_override`, `instance_mutation`, `signal_group`, `animation_fix`, and
`journal_gap`.

Each platform report binds one clean source-freeze commit, scoped source/fixture/oracle
digests, editor and sidecar artifacts, toolchains, raw SLO samples, Bridge main-thread
telemetry, deterministic semantic digests, cleanup, and redaction. The base phase also
reopens editor and sidecar, checks D-06 identity stability, verifies nested built-in
subresources through public MCP output, and compares the order-equivalent scene pair.

The aggregate is accepted only when macOS and Windows reports use the same source
freeze and every phase semantic digest matches exactly. Linux and remote CI are not
Sprint 4 acceptance coordinates.

## Local commands

macOS arm64:

```sh
tests/codex/runners/sprint4_macos_arm64.sh
```

Windows x86_64 PowerShell:

```powershell
tests\codex\runners\sprint4_windows_x86_64.ps1
```

After copying the Windows report into the same source-freeze checkout:

```sh
python3 tests/codex/sprint4_acceptance.py merge \
  --macos tests/codex/evidence/sprint-4-scene-graph-macos.json \
  --windows tests/codex/evidence/sprint-4-scene-graph-windows.json \
  --output tests/codex/evidence/sprint-4-acceptance.json
```
