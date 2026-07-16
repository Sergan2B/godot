# Sprint 3 Stage 5 — cross-platform E2E and acceptance

**Status:** In progress

**Scope:** `S3-09` and `S3-10`

**Source baseline:** `55a6e8a74a` on `codex/integration`

**Parent:** [SPRINT-3-PLAN.md](SPRINT-3-PLAN.md)

## 1. Required outcome

Stage 5 closes Sprint 3 with local, model-free evidence from real target hosts. It does
not wait for or claim remote CI. The required platform matrix is:

| Gate | macOS arm64 | Windows x86_64 | Linux x86_64 |
|---|---:|---:|---:|
| Editor → Bridge → sidecar → persistent index → MCP | Required | Required | Not required |
| Full D-05 storage/recovery profile | Required | Required | Required |
| Final source revision | Same clean source-freeze commit on every host |

The historical Stage 4 macOS result is development evidence. Final Sprint 3 acceptance
uses fresh macOS and Windows live runs produced after the Stage 5 harness is frozen.

## 2. Live resource gate

The portable gate runs all eight canonical resource phases: base, UID rename, UID-less
rename, delete, re-add, reimport, content edit, and journal gap. Each phase compares
Godot observations, the committed direct/reverse index, and both resource MCP tools
with the independent oracle. The base phase also proves same-session sidecar reopen
without rebuilding or changing the immutable segment set.

Each platform result records:

- platform, host, toolchain and artifact versions plus Godot/sidecar SHA-256;
- source commit, relevant-source digest, clean state, fixture/oracle digests;
- a platform-neutral graph digest for every phase;
- raw query, ordinary incremental visibility, bulk status/ping, startup, reopen, full
  rebuild, and Bridge main-thread samples;
- recomputed p50/p95 values and explicit SLO pass/fail fields;
- cleanup, fixture-integrity, redaction, and all-phase completion state.

Absolute project paths, endpoints, tokens, source bytes, and session identities are not
evidence fields. Partial phase selection is a development run and cannot produce
qualifying final evidence.

## 3. Performance classification

The acceptance SLOs and sample populations are fixed as follows:

| SLO | Qualifying samples | Limit |
|---|---|---:|
| Cached resource query | Calls to the two resource MCP tools after current generation activation | p95 <= 300 ms |
| Changed-file visibility | UID rename, UID-less rename, delete, re-add steps, reimport, and content edit | p95 <= 2,000 ms |
| Control responsiveness | Repeated editor-state/status calls while the forced journal-gap rebuild is active | p95 <= 200 ms |
| Bridge main-thread budget | Every busy Bridge frame captured in evidence mode | zero samples > 2,000 us |

Startup, compatible reopen, and journal-gap full rebuild durations are reported
separately and never mixed into ordinary changed-file visibility. The evidence merger
recomputes nearest-rank percentiles from raw samples and rejects inconsistent summaries,
empty required populations, telemetry overflow, or selective samples.

## 4. Evidence-only telemetry

Bridge RPC remains version 1.2 and the MCP registry remains exactly five read-only
tools. When `GODOT_CODEX_EVIDENCE_TELEMETRY=1`, the editor records the elapsed Bridge
main-thread work for busy frames in a bounded in-memory buffer and emits one normalized,
non-sensitive JSON record at shutdown. The record contains the 2 ms budget, raw
microsecond samples, total busy-frame count, over-budget count, maximum, and buffer
overflow state. A telemetry overflow fails the gate.

No telemetry is collected or emitted by default. The evidence record contains no
project/session data and is not a Bridge RPC or MCP compatibility surface.

## 5. Aggregation and completion rules

The final validator accepts exactly one qualifying `macos-arm64` and one qualifying
`windows-x86_64` live report plus full storage reports for `macos`, `windows`, and
`linux`. It rejects quick profiles, dirty relevant source, source/fixture/oracle
mismatch, missing phases, graph-digest mismatch, failed storage gates, SLO failure,
redaction failure, and unsupported platform coordinates.

Final artifacts are:

- `tests/codex/evidence/sprint-3-resource-graph-macos.json`;
- `tests/codex/evidence/sprint-3-resource-graph-windows.json`;
- three raw reports under `tests/codex/evidence/platform/`;
- `tests/codex/evidence/sprint-3-storage-spike-cross-platform.json`;
- `tests/codex/evidence/sprint-3-acceptance.json`;
- `docs/codex-integration/SPRINT-3-EVIDENCE.md`.

Any source or harness fix after platform execution invalidates all Stage 5 platform
evidence. A failed required platform or SLO remains open; it is not relabeled optional.
`remote_ci` is recorded as `not_run` and is not a Sprint 3 completion requirement.
