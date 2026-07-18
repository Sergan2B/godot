# Sprint 3 Stage 5 — cross-platform E2E and acceptance

**Status:** In progress

**Scope:** `S3-09` and `S3-10`

**Source baseline:** the identical `git_commit`, scoped source digest, and oracle digest
recorded by every qualifying platform report. The commit is frozen before host execution;
later evidence-only commits may descend from it without changing the scoped source tree.

**Parent:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

## 1. Required outcome

Stage 5 closes Sprint 3 with local, model-free evidence from real target hosts. It does
not wait for or claim remote CI. The required platform matrix is:

| Gate | macOS arm64 | Windows x86_64 |
|---|---:|---:|
| Editor → Bridge → sidecar → persistent index → MCP | Required | Required |
| Full D-05 storage/recovery profile | Required | Required |
| Final source revision | Same clean source-freeze commit on every host |

Linux is not part of the Sprint 3 acceptance matrix. Its future product support is
unchanged, but no Linux host, report, or receipt is required to close this sprint.

The historical Stage 4 macOS result is development evidence. Final Sprint 3 acceptance
uses fresh macOS and Windows live runs produced after the Stage 5 harness is frozen.

## 2. Live resource gate

The portable gate runs all eight canonical resource phases: base, UID rename, UID-less
rename, delete, re-add, reimport, content edit, and journal gap. Each phase compares
Godot observations, the committed direct/reverse index, and both resource MCP tools
with the independent oracle. The base phase performs a full editor-and-sidecar reopen,
requires a new editor session, and proves that the compatible persistent generation
and immutable segment set are reused without rebuilding.

Each platform result records:

- platform, host, toolchain and artifact versions plus Godot/sidecar SHA-256;
- source commit, relevant-source digest, clean state, fixture/oracle digests;
- a platform-neutral graph digest for every phase;
- raw per-phase query and active-rebuild status traces, ordinary incremental visibility,
  startup, reopen, full rebuild, and per-editor-session Bridge main-thread samples;
- recomputed p50/p95 values and explicit SLO pass/fail fields;
- cleanup, fixture-integrity, redaction, and all-phase completion state.

Absolute project paths, endpoints, tokens, source bytes, and session identities are not
evidence fields. Partial phase selection is a development run and cannot produce
qualifying final evidence.

## 3. Performance classification

The acceptance SLOs and sample populations are fixed as follows:

| SLO | Qualifying samples | Limit |
|---|---|---:|
| Cached resource query | Every paged direct/reverse oracle-verification call after current generation activation; contract probes are excluded | p95 <= 300 ms |
| Changed-file visibility | UID rename, UID-less rename, delete, re-add steps, reimport, and content edit, ending only after the exact activated generation passes its phase oracle | p95 <= 2,000 ms |
| Control responsiveness | Repeated editor-state/status calls while the forced journal-gap rebuild is active | p95 <= 200 ms |
| Bridge main-thread budget | Every busy Bridge frame captured in evidence mode | zero samples > 2,000 us |

Startup, compatible reopen, and journal-gap full rebuild durations are reported
separately and never mixed into ordinary changed-file visibility. The evidence merger
recomputes nearest-rank percentiles from raw samples and rejects inconsistent summaries,
empty required populations, telemetry overflow, a missing editor session, or any
aggregate population that differs from its exact phase timings and traces.
The re-add removal step is independently bound to the canonical delete oracle before
the resource is restored; its final step is bound to the canonical re-add oracle.

## 4. Evidence-only telemetry

Bridge RPC remains version 1.2 and the MCP registry remains exactly five read-only
tools. Closed resource-tool inputs are live-probed against the pinned `rmcp 2.2.0`
schema-rejection envelope: one exact serde error text block, `isError: true`, and no
structured business result or JSON-RPC protocol error. When
`GODOT_CODEX_EVIDENCE_TELEMETRY=1`, each editor process records the elapsed
Bridge main-thread work for busy frames in a 16,384-sample bounded in-memory buffer and
emits one normalized, non-sensitive JSON record at shutdown. The base phase therefore
requires two records, one from each side of the editor-and-sidecar reopen; every other
phase requires one. The records contain the 2 ms budget, raw microsecond samples, total
busy-frame count, over-budget count, maximum, and buffer overflow state. Any omitted
session or telemetry overflow fails the gate.

No telemetry is collected or emitted by default. The evidence record contains no
project/session data and is not a Bridge RPC or MCP compatibility surface.

## 5. Aggregation and completion rules

The final validator accepts exactly one qualifying `macos-arm64` and one qualifying
`windows-x86_64` live report plus full storage reports for `macos` and `windows`.
It rejects quick profiles, dirty relevant source, source/fixture/oracle
mismatch, missing phases, graph-digest mismatch, failed storage gates, SLO failure,
redaction failure, and unsupported platform coordinates. Qualifying producers reject
common hosted-CI environment markers. The merger recomputes D-05 scores and confidence
intervals from raw samples through the pinned Rust validator, binds its receipt to the
exact evidence-byte SHA-256, and requires exact closed phase schemas. The source-freeze
commit must exist and be an ancestor of the aggregation checkout; the oracle and every
producer-scoped byte must still match it. Later evidence-only commits and the closed
acceptance-policy paths listed by the validator are allowed.

Final artifacts are:

- `tests/codex/evidence/sprint-3-resource-graph-macos.json`;
- `tests/codex/evidence/sprint-3-resource-graph-windows.json`;
- two raw reports under `tests/codex/evidence/platform/`;
- `tests/codex/evidence/sprint-3-storage-spike-cross-platform.json`;
- `tests/codex/evidence/sprint-3-acceptance.json`;
- `docs/codex-integration/SPRINT-3-EVIDENCE.md`.

Any producer or product-source fix after platform execution invalidates all Stage 5
platform evidence. A reviewed acceptance-policy-only amendment may reuse raw reports
from the pinned source freeze when the validator proves that no producer scope changed.
A failed required platform or SLO remains open; it is not relabeled optional.
`remote_ci` is recorded as `not_run` and is not a Sprint 3 completion requirement.

## 6. Local execution runbook

Every host checks out the same source-freeze commit and starts with a clean relevant
source tree. The evidence destinations below are intentional and platform-specific.

macOS arm64:

```sh
python -m SCons platform=macos arch=arm64 target=editor dev_build=yes tests=yes \
  module_codex_bridge_enabled=yes accesskit=no angle=no metal=yes vulkan=no -j8
cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp/Cargo.toml \
  -p godot-codex-mcp
python3 tests/codex/sprint3_stage4_index_mcp.py \
  --godot bin/godot.macos.editor.dev.arm64 \
  --sidecar godot-codex-mcp/target/release/godot-codex-mcp \
  --evidence tests/codex/evidence/sprint-3-resource-graph-macos.json
cargo +1.94.1 run --locked --release --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  run --backend all --dataset all --repo-root "$PWD" \
  --output tests/codex/evidence/platform/sprint-3-storage-spike-macos.json
```

Windows x86_64 PowerShell:

```powershell
python -m SCons platform=windows target=editor dev_build=yes tests=yes `
  module_codex_bridge_enabled=yes accesskit=no d3d12=no angle=no -j8
cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp\Cargo.toml `
  -p godot-codex-mcp
python tests\codex\sprint3_stage4_index_mcp.py `
  --godot bin\godot.windows.editor.dev.x86_64.console.exe `
  --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
  --evidence tests\codex\evidence\sprint-3-resource-graph-windows.json
cargo +1.94.1 run --locked --release --manifest-path tests\codex\storage_spike\Cargo.toml -- `
  run --backend all --dataset all --repo-root (Get-Location).Path `
  --output tests\codex\evidence\platform\sprint-3-storage-spike-windows.json
```

After copying the two Windows raw reports into the checkout that already contains the
two pinned macOS reports, produce and validate the two aggregates:

```sh
cargo +1.94.1 run --locked --release --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  merge tests/codex/evidence/sprint-3-storage-spike-cross-platform.json \
  tests/codex/evidence/platform/sprint-3-storage-spike-macos.json \
  tests/codex/evidence/platform/sprint-3-storage-spike-windows.json
python3 tests/codex/sprint3_acceptance.py merge \
  --macos-live tests/codex/evidence/sprint-3-resource-graph-macos.json \
  --windows-live tests/codex/evidence/sprint-3-resource-graph-windows.json \
  --storage tests/codex/evidence/sprint-3-storage-spike-cross-platform.json \
  --output tests/codex/evidence/sprint-3-acceptance.json
```
