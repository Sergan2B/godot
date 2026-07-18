# Sprint 3 — final evidence report

**Status:** Accepted — `S3-01` through `S3-10` complete

**Snapshot:** 2026-07-18

**Scope:** ResourceUID identity, direct/reverse resource dependency graph, Bridge RPC
1.2, persistent segment index, two project-scoped resource MCP tools, recovery, and
cross-platform acceptance on macOS arm64 and Windows x86_64.

## 1. Final result

Sprint 3 passes its complete local, model-free acceptance matrix. Both target hosts ran
the eight-phase editor → Bridge → sidecar → persistent index → MCP gate from the same
clean source freeze. Both hosts also ran the full D-05 storage/recovery profile. The
normalized macOS and Windows graphs are identical in every phase, all twelve acceptance
criteria pass, and the canonical D-05 aggregate selects the segment store.

Linux is not a Sprint 3 acceptance coordinate. This explicit scope decision does not
change future product-level Linux support. Remote CI is recorded as `not_run` and was
never a Sprint 3 completion requirement.

| Gate | macOS arm64 | Windows x86_64 | Overall |
|---|---|---|---|
| Editor → Bridge → sidecar → persistent index → MCP | **PASS** | **PASS** | **PASS** |
| Full D-05 storage and recovery profile | **PASS** | **PASS** | **PASS** |
| Normalized graph parity | Same eight phase digests | Same eight phase digests | **PASS** |
| Cross-platform storage aggregate | Input accepted | Input accepted | **PASS** — segment |
| Final Sprint 3 acceptance | Input accepted | Input accepted | **PASS** |
| Remote CI | `not_run` | `not_run` | Not required |

## 2. Frozen source coordinates

| Coordinate | Exact value |
|---|---|
| Source-freeze commit | `a90ddd06c81a6210f44552b46ff533248b93ed90` |
| Scoped source SHA-256 | `sha256:73e99eec9897f8e2b4c6c210a3c56bd57a91ce347aedba030323eb01bf8438eb` |
| Golden oracle SHA-256 | `sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b` |

Every raw report records these exact coordinates and `git_dirty=false`. The final
validator reconstructs the frozen tree from Git, proves that no producer or product
source changed, and permits only the reviewed two-platform acceptance-policy amendment.

## 3. Canonical artifacts

| Artifact | SHA-256 | Bytes | Status |
|---|---|---:|---|
| [`sprint-3-resource-graph-macos.json`](../../tests/codex/evidence/sprint-3-resource-graph-macos.json) | `bb56c6e7e514a7741ae1f29de5eb1127a22757691b736cd9285e37d3f93f7ef3` | 184,357 | **PASS** |
| [`sprint-3-resource-graph-windows.json`](../../tests/codex/evidence/sprint-3-resource-graph-windows.json) | `ae9308db6d15b5ab72059aa893f7bc1e229c6b989d0a2fb979b04b6f2ac0b4e3` | 143,456 | **PASS** |
| [`sprint-3-storage-spike-macos.json`](../../tests/codex/evidence/platform/sprint-3-storage-spike-macos.json) | `7ecffdf270148712a492228de0bef83df6d688891e1b8385bf348682e0ff5311` | 352,548 | **PASS** |
| [`sprint-3-storage-spike-windows.json`](../../tests/codex/evidence/platform/sprint-3-storage-spike-windows.json) | `df827948f2004122551fed7ce7792077f362fce3a6f52783938d48964f45a81d` | 361,849 | **PASS** |
| [`sprint-3-storage-spike-cross-platform.json`](../../tests/codex/evidence/sprint-3-storage-spike-cross-platform.json) | `4a1ae7633c73c0fe3f9c1eef1483f1be6e6e6d1404acd3d8652d559cb0fb882d` | 880,924 | **PASS** — schema 3 |
| [`sprint-3-acceptance.json`](../../tests/codex/evidence/sprint-3-acceptance.json) | `c42c8463ee2c0155d0dfe9bea62536aa39be8ecc9f21caa10a51abb78d9cf358` | 3,630 | **PASS** — schema 3 |

The transfer receipt is a byte-integrity control and is intentionally not committed.
It bound the exact Windows report bytes, source coordinates, runner, and manifest helper
before aggregation installed either report.

## 4. Live resource-index acceptance

The macOS and Windows reports both completed, in order:

```text
base → rename_uid → rename_uidless → delete → re_add → reimport →
content_edit → journal_gap
```

Every phase passed direct/reverse oracle comparison, diagnostics comparison, stable
project scope, cleanup, and the five-tool MCP contract. The base phase reopened both
editor and sidecar and reused the compatible persistent generation. UID rename preserved
entity identity with one incremental commit and no full rebuild. The journal-gap phase
forced a full rebuild while control calls remained responsive.

| Phase | Normalized graph SHA-256 on both hosts |
|---|---|
| Base | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| UID rename | `b1020bb72cfa2e6f00ce61a602fae487feb9e7b787b48bee804bba138d843912` |
| UID-less rename | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Delete | `99876df25ad329127f0763ca5671268960acaea61f9282725c38eb85b4fdadca` |
| Re-add | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Reimport | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Content edit | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Journal gap | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |

### 4.1 Toolchain and artifacts

| Coordinate | macOS arm64 | Windows x86_64 |
|---|---|---|
| Host | `macOS-26.5.2-arm64-arm-64bit-Mach-O` | `Windows-11-10.0.26200-SP0` |
| Python | CPython 3.14.6 | CPython 3.14.5 |
| SCons | 4.10.1 | 4.10.1 |
| Rust/Cargo | 1.94.1 | 1.94.1 |
| Rust target | `aarch64-apple-darwin` | `x86_64-pc-windows-msvc` |
| Godot | `4.8.dev.custom_build.a90ddd06c` | `4.8.dev.custom_build.a90ddd06c` |
| Godot SHA-256 | `5f692a7a80ab01f82e85b936cde033f19ac0475b6bd22aa931b190fdb5726027` | `3b2b8892c475e5ce861237fa3ad2c8b2f02711dd59078f98e7e9f6c229047534` |
| Sidecar SHA-256 | `7a44091ed0860c1d92f9260a022e37b2bf57a630a1f31670e686584e4d273202` | `ba5515625e5ba0c62f45dba04e066a88cb247e261d55bc3fe87be2797819bcfb` |

### 4.2 SLO evidence

| Population | macOS p50 / p95 | Windows p50 / p95 | Limit | Result |
|---|---:|---:|---:|---|
| Cached resource query, 416 samples/host | 0.145 / 0.322 ms | 0.171 / 0.216 ms | p95 ≤ 300 ms | **PASS** |
| Ordinary incremental visibility, 7 samples/host | 420.414 / 509.421 ms | 441.527 / 546.254 ms | p95 ≤ 2,000 ms | **PASS** |
| Control status during gap rebuild | 0.405 / 0.651 ms (673) | 0.214 / 0.256 ms (595) | p95 ≤ 200 ms | **PASS** |
| Bridge busy frames | 265 / 435 µs (4,887) | 238 / 351 µs (3,196) | zero > 2,000 µs | **PASS** |

The maximum Bridge main-thread sample was `1,714 µs` on macOS and `996 µs` on
Windows; both over-budget counts are zero and neither telemetry buffer overflowed.
Startup, compatible reopen, and journal-gap rebuild are classified separately from
ordinary incremental visibility. Their p95 values were respectively 3,349.316,
4,200.841, and 37,697.666 ms on macOS and 3,185.753, 6,981.026, and 31,048.502 ms on
Windows.

## 5. D-05 storage and recovery

Both hosts ran the full 10,000-resource/50,000-edge decision dataset, including the
100,000-resource/500,000-edge stress reopen and all thirteen cancellation, hard-kill,
and corruption fault coordinates. Segment passed all twelve gates and all thirteen
fault cases on both hosts with no errors.

| Host/backend | Qualified | Weighted score | 95% CI | Query p95 | Rename p95 | Result |
|---|---|---:|---|---:|---:|---|
| macOS SQLite | Yes | 0.363503 | 0.362148–0.364768 | 80.688 ms | 1,321.484 ms | Pass |
| macOS Segment | Yes | 0.812626 | 0.812536–0.812745 | 0.023542 ms | 564.004 ms | Pass |
| Windows SQLite | No | 0 | 0–0 | 352.153 ms | 3,706.530 ms | Fails query and rename SLOs |
| Windows Segment | Yes | 0.988057 | 0.976567–0.993653 | 0.0502 ms | 654.704 ms | Pass |

The schema-3 canonical aggregate was built twice with reversed input order and produced
byte-identical output. Its Rust validation receipt binds the exact aggregate SHA-256.

| Backend | Qualified on both hosts | Median score | Cross-platform 95% CI |
|---|---|---:|---|
| SQLite | No | 0.181752 | 0.181074–0.182363 |
| Segment | Yes | 0.900341 | 0.894585–0.903199 |

Final D-05 decision: **segment**. It is the only backend that passed every gate on both
macOS and Windows.

## 6. Acceptance criteria

| Criterion | Evidence | Result |
|---|---|---|
| `S3-AC-01` | Direct/reverse oracle parity in every phase and host | **PASS** |
| `S3-AC-02` | UID rename preserves identity and commits incrementally | **PASS** |
| `S3-AC-03` | Missing and stale UID diagnostics match the oracle | **PASS** |
| `S3-AC-04` | Compatible editor/sidecar reopen plus storage reopen | **PASS** |
| `S3-AC-05` | Graceful cancellation matrix | **PASS** |
| `S3-AC-06` | Crash, corruption, migration, and recovery matrix | **PASS** |
| `S3-AC-07` | Text/binary/import formats and all eight phases | **PASS** |
| `S3-AC-08` | Delete, re-add, reimport, and journal-gap behavior | **PASS** |
| `S3-AC-09` | Both resource MCP tools and closed five-tool registry | **PASS** |
| `S3-AC-10` | Cached-query and incremental-visibility SLOs | **PASS** |
| `S3-AC-11` | Control responsiveness and Bridge main-thread budget | **PASS** |
| `S3-AC-12` | Exact Windows/macOS normalized graph equality | **PASS** |

The canonical acceptance artifact records `status=passed`, `remote_ci=not_run`, and all
twelve criteria as passed.

## 7. Verification and completion

The final audit requires and verifies:

- exact transfer receipt verification before Windows evidence installation;
- strict validation of both live reports, including raw sample recomputation;
- two input-order-independent storage merges and byte comparison;
- canonical Rust validation of every raw storage score, interval, gate, and decision;
- two deterministic acceptance merges and byte comparison;
- frozen source/oracle reconstruction and clean producer-scope verification;
- Python contract, fixture, acceptance, and receipt tests;
- storage-spike and production-sidecar Rust suites, Rustfmt, Clippy, and repository hooks.

`S3-09` cross-platform smoke is complete. `S3-10` final audit is complete. The Sprint 3
Definition of Done is met, and the next planned milestone is Sprint 4 (`PackedScene` /
`SceneState` structural index).
