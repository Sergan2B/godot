# Sprint 4 evidence — scene semantics and node graph

**Status:** In progress — macOS arm64 PASS; Windows x86_64 and final aggregate pending

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

## macOS arm64 result

The qualifying local run passed all eight phases at source freeze
`75c6a100cc49bd55df9f5a8568866d5fd2195762`.

| Gate | Result |
|---|---:|
| Cached scene query p95 | 1.017 ms |
| Ordinary change visibility p95 | 1,193.994 ms |
| Bulk status ping p95 | 0.608 ms |
| Bridge main-thread p95 | 1,204 µs |
| Bridge frames over 2,000 µs | 0 |

Source and artifact coordinates:

- relevant source: `sha256:ca79eb6f5511947ee8b167e2a8805ba2f260ff26e0afc3dfe6a328ac34fb2351`;
- fixture: `sha256:f29a11a979dea215fb83a013f2b1d7cb0169dccae918765374c9de6db86ce675`;
- Godot editor: `sha256:58d1d466a2eb7be19d4149e953dd2e88c157da5d35ec7072c9e324dae7b94cf8`;
- release sidecar: `sha256:de43df85322c15471d0b68bf3695159166b265f05a5a6c61fe82a43c782d9104`;
- raw report: `tests/codex/evidence/sprint-4-scene-graph-macos.json`.

## Local commands

macOS arm64:

```sh
tests/codex/runners/sprint4_macos_arm64.sh
```

Windows x86_64 PowerShell:

```powershell
git checkout 75c6a100cc49bd55df9f5a8568866d5fd2195762
tests\codex\runners\sprint4_windows_x86_64.ps1
```

The exact checkout is mandatory because the aggregate compares the full source-freeze
commit as well as the scoped source and fixture digests. The Windows run is local; no
Git CI or Linux host is required.

After copying the Windows report into the same source-freeze checkout:

```sh
python3 tests/codex/sprint4_acceptance.py merge \
  --macos tests/codex/evidence/sprint-4-scene-graph-macos.json \
  --windows tests/codex/evidence/sprint-4-scene-graph-windows.json \
  --output tests/codex/evidence/sprint-4-acceptance.json
```
