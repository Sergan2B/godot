# Sprint 6 evidence — find usages, evidence, and Codex context

**Status:** Accepted locally on macOS arm64; Semantic Alpha / M1 achieved

**Source freeze:** `8a167c18d2e752cfd306d6653999b069ddf01a95`

**Closeout date:** 2026-07-20

## 1. Outcome

Sprint 6 gates `S6-01`–`S6-10` are complete. The implementation joins current
resource, scene, and script generations into one storage-neutral evidence view,
exposes the tenth read-only tool `godot_find_usages`, and publishes bounded
project/scene MCP resources for Codex.

The blocking acceptance path is local, deterministic, and model-free on macOS
arm64. It rebuilt Godot and the release sidecar before exercising a real
headless Godot → Bridge → sidecar → MCP session. Windows, Linux, and remote CI
were not run and are not represented as passing.

## 2. Canonical artifact

| Artifact | SHA-256 | Bytes | Result |
|---|---|---:|---|
| [`sprint-6-semantic-context-macos.json`](../../tests/codex/evidence/sprint-6-semantic-context-macos.json) | `dc83b499a482bc5e4430c176309e8c2fef2347888d1b7026e310c2d45b13c64e` | 8,879 | **PASS** |

The report binds 52 tracked files through
`sha256:b7a74c04f8fb1f50dabf0cf85009a3f58328ef3c7bb0ccae2bd680be9918f22c`.
It also binds the fixture manifest, golden usages, rebuilt Godot binary, and
release sidecar by SHA-256.

## 3. Acceptance result

| Gate | Result |
|---|---|
| Independent fixture/oracle and Draft 2020-12 schemas | **PASS** |
| Python fail-closed evidence regressions | **PASS** |
| Full Rust workspace tests, formatting, and clippy `-D warnings` | **PASS** |
| Incremental macOS Godot editor rebuild | **PASS** |
| Sprint 6 C++ semantic adapter test | **PASS** |
| Release sidecar rebuild | **PASS** |
| Model-free live MCP gate | **PASS** |
| macOS arm64 | **PASS** |
| Windows / Linux / remote CI | `not_run` |

The live oracle matched 7/7 resolvable facts with zero false `exact`. Every
usage carried evidence. The attachment deduplication probe returned one fact
with two independent evidence sources (`scene_node_attachment` and
`scene_relation`). Signed cursor tampering, cross-filter/cross-selector reuse,
and invalid limits failed closed.

The same editor session verified two real changes:

- moving the profile resource while keeping its UID advanced the index from
  revision 3 to 6 and preserved its canonical entity ID;
- renaming `take_damage` to `receive_damage` advanced the index to revision 8,
  preserved the expected call/override facts, and made the old selector return
  `symbol_not_found`.

## 4. Budgets and SLOs

| Measurement | Observed | Limit |
|---|---:|---:|
| `godot_find_usages` cached p95 | 0.591 ms | 300 ms |
| Summary read p95 | 0.654 ms | 300 ms |
| Control ping p95 | 0.259 ms | 200 ms |
| Project summary | 4,042 bytes | 4,096 bytes |
| Scene summary | 2,033 bytes | 2,048 bytes |

Cleanup and redaction checks passed: editor and sidecar stopped, temporary
workspace and Bridge runtime files were removed, and the evidence contains no
absolute project path, endpoint, token, secret, or source bytes.

## 5. Qualification boundary

The installed Codex CLI was detected as `codex-cli 0.145.0-alpha.18`, but the
optional model-facing smoke is recorded as `not_run` because no isolated local
MCP profile was configured. This probe is advisory by contract and does not
replace or weaken the deterministic gate.

The deferred Sprint 5 Windows gate remains a separate known risk. It does not
block the locally agreed Sprint 6 closeout. Sprint 7 may now begin; broader
platform/release qualification remains governed by later roadmap gates.
