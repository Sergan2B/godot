# Sprint 3 — interim evidence report

**Status:** In progress — not accepted

**Snapshot:** 2026-07-18

**Scope:** Stage 5 (`S3-09`, `S3-10`)

**Current result:** macOS arm64 live and storage reports are tracked and pass at the
frozen coordinates below. The Windows x86_64 run is reported complete by the operator,
but its two raw reports and transfer receipt are not present in this checkout or the
remote branch yet, so they cannot be independently validated or aggregated. Linux was
removed from the Sprint 3 acceptance matrix by an explicit scope decision; no Linux
artifact is required.

## Decision summary

| Gate | macOS arm64 | Windows x86_64 | Overall |
|---|---|---|---|
| Editor → Bridge → sidecar → persistent index → MCP | **PASS** — tracked | **AWAITING IMPORT** — operator run complete | Open |
| Full D-05 storage and recovery profile | **PASS** — tracked | **AWAITING IMPORT** — operator run complete | Open |
| Cross-platform storage aggregate | Input ready | Input awaiting import | Not generated |
| Final Sprint 3 acceptance | Input ready | Input awaiting import | Not generated |
| Remote CI | `not_run` | `not_run` | Not required |

The earlier Docker `linux/amd64` development preflight remains historical only. Linux
is outside this sprint's acceptance claim rather than represented by synthetic evidence.

## Frozen source coordinates

| Coordinate | Exact value |
|---|---|
| Source-freeze commit | `a90ddd06c81a6210f44552b46ff533248b93ed90` |
| Scoped source SHA-256 | `sha256:73e99eec9897f8e2b4c6c210a3c56bd57a91ce347aedba030323eb01bf8438eb` |
| Golden oracle SHA-256 | `sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b` |
| Previous macOS live evidence commit | `d7339412f0e88475faab7d5f4264629671853888` |
| Previous macOS storage evidence commit | `7d53adb993c463e4062a6cbfa4be17dd0319a163` |

Every macOS or Windows raw report must record the exact freeze commit and both exact
digests above. The tracked macOS reports match those coordinates. Product or producer
source changes require another freeze and regeneration. The schema-3 two-platform
aggregate is an acceptance-policy-only amendment and explicitly validates raw reports
against their original frozen tree, so it does not invalidate those platform runs.

## Tracked current macOS artifacts

| Artifact | Status | File SHA-256 | Bytes |
|---|---|---|---:|
| [`sprint-3-resource-graph-macos.json`](../../tests/codex/evidence/sprint-3-resource-graph-macos.json) | Current-freeze macOS live **PASS** | `sha256:bb56c6e7e514a7741ae1f29de5eb1127a22757691b736cd9285e37d3f93f7ef3` | 184,357 |
| [`sprint-3-storage-spike-macos.json`](../../tests/codex/evidence/platform/sprint-3-storage-spike-macos.json) | Current-freeze macOS D-05 **PASS** | `sha256:7ecffdf270148712a492228de0bef83df6d688891e1b8385bf348682e0ff5311` | 352,548 |

The legacy `tests/codex/evidence/sprint-3-storage-spike.json` is excluded from
Stage 5. It records an older commit, a dirty tree, and different source/oracle
coordinates; it is not the final macOS raw report.

## Current macOS live gate

The following result was generated from the clean current source freeze
`a90ddd06c81a6210f44552b46ff533248b93ed90` and passes strict Stage 5
validation.

The schema-3 report records `status=passed`, `execution=local_model_free`,
`profile=acceptance`, and `platform=macos-arm64` on
`macOS-26.5.2-arm64-arm-64bit-Mach-O`.

Artifact and toolchain identity:

- Godot `4.8.dev.custom_build.a90ddd06c`, SHA-256
  `sha256:5f692a7a80ab01f82e85b936cde033f19ac0475b6bd22aa931b190fdb5726027`;
- `godot-codex-mcp 0.1.0`, SHA-256
  `sha256:7a44091ed0860c1d92f9260a022e37b2bf57a630a1f31670e686584e4d273202`;
- CPython `3.14.6`, SCons
  `4.10.1.055b01f429d58b686701a56df863a817c36bb103`;
- Rust/Cargo `1.94.1`, target `aarch64-apple-darwin`.

All eight canonical phases completed in order. Every phase passed direct and
reverse oracle comparison, diagnostics comparison, stable project scope, and
cleanup. The report contains 416 qualifying cached-query samples, 673 control
status samples, nine editor telemetry sessions, all eleven MCP contract flags,
and complete cleanup/redaction claims.

```text
base → rename_uid → rename_uidless → delete → re_add → reimport →
content_edit → journal_gap
```

| Phase | Resources | Dependencies | Diagnostics | Normalized graph SHA-256 |
|---|---:|---:|---:|---|
| Base | 18 | 12 | 2 | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| UID rename | 18 | 12 | 2 | `b1020bb72cfa2e6f00ce61a602fae487feb9e7b787b48bee804bba138d843912` |
| UID-less rename | 18 | 12 | 2 | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Delete | 17 | 12 | 3 | `99876df25ad329127f0763ca5671268960acaea61f9282725c38eb85b4fdadca` |
| Re-add | 18 | 12 | 2 | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Reimport | 18 | 12 | 2 | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Content edit | 18 | 12 | 2 | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |
| Journal gap | 18 | 12 | 2 | `95535bd595688dd34440de4e353c1620397380979ab79296840b1fbb5da04703` |

The base phase reopened both editor and sidecar and reused the same compatible
generation and immutable segment set. The UID rename preserved entity identity
with one incremental commit and no full rebuild. The forced journal gap used a
full rebuild and kept control calls responsive.

Strict validation command:

```sh
python3 tests/codex/sprint3_acceptance.py validate-live \
  tests/codex/evidence/sprint-3-resource-graph-macos.json
```

Result: **PASS**.

### macOS SLOs

| Population | Samples | p50 | p95 | Limit | Result |
|---|---:|---:|---:|---:|---|
| Cached resource query | 416 | 0.145 ms | 0.322 ms | ≤300 ms | **PASS** |
| Ordinary incremental visibility | 7 | 420.414 ms | 509.421 ms | ≤2,000 ms | **PASS** |
| Control status/ping during gap rebuild | 673 | 0.405 ms | 0.651 ms | ≤200 ms | **PASS** |
| Bridge main-thread busy frames | 4,887 | 265 µs | 435 µs | zero >2,000 µs | **PASS** |

Bridge telemetry recorded maximum `1,714 µs`, over-budget count `0`, no buffer
overflow, and a `2,000 µs` budget. Separately reported non-incremental timings
were startup p95 `3,349.316 ms`, compatible reopen `4,200.841 ms`, and forced
journal-gap rebuild `37,697.666 ms`; they are not mixed into ordinary visibility.

## Current macOS D-05 storage and recovery

The schema-2 decision report records `os=macos`, `architecture=aarch64`, a local
runner with 14 logical CPUs, and Rust `1.94.1`. The full dataset contains 10,000
resources and 50,000 edges, with five builds, 50 renames, 10,000 timed queries,
and a 100,000-resource/500,000-edge stress profile.

Both candidates passed all twelve storage gates and all thirteen fault cases,
including graceful cancellation, hard-kill recovery, corruption isolation,
locking/reopen, migration, concurrent activation, and the stress reopen. Both
error lists are empty.

| Backend | Weighted score | 95% CI | Build p50/p95 | Rename p50/p95 | Query p50/p95 | Artifact | Binary |
|---|---:|---|---|---|---|---:|---:|
| SQLite | `0.3635030575266095` | `[0.3621476484355271, 0.3647677629397902]` | 857.250 / 865.316 ms | 1,291.327 / 1,321.484 ms | 78.386375 / 80.688042 ms | 262,033,408 B | 4,052,384 B |
| Segment | `0.8126262900738505` | `[0.8125363973283607, 0.8127450928333679]` | 13,578.809 / 13,614.465 ms | 530.791 / 564.004 ms | 0.022916 / 0.023542 ms | 70,011,685 B | 1,951,136 B |

The raw macOS decision selected `segment`: both backends qualified and segment
had the higher weighted score (`0.8126`). This is one platform result, not the
missing canonical two-platform D-05 aggregate or validation receipt.

## Current freeze local repository verification

The current freeze and regenerated macOS evidence were checked locally with:

- Godot macOS arm64 editor rebuilt from the exact freeze and reported version
  suffix `a90ddd06c`;
- strict live validator: eight canonical phases and every SLO passed;
- full D-05 profile: both backends passed 12 gates and 13 fault cases, with
  `segment` selected;
- Python acceptance/fixture/receipt suite: 45 passed;
- production sidecar workspace: 34 passed, Rustfmt passed, and index-store
  Clippy passed with warnings denied;
- runner manifest, Bash syntax, freeze digest, and clean-worktree checks passed.

Remote CI was intentionally not run and is not awaited for Sprint 3 acceptance.

## Remaining Windows artifact handoff

The Windows procedure is executable and documented in
[`tests/codex/runners/README.md`](../../tests/codex/runners/README.md). It uses
a separate clean worktree at the exact freeze and fail closed on wrong
architecture, dirty source, digest mismatch, CI markers, containers, incomplete
SLOs, or failed segment gates/fault cases. The Windows producer and receipt helper
remain pinned to `0961c5f7fe1bd36e8d62b4966f39c4dbf174b149`; only the downstream
two-platform aggregation policy changed.

Required reports to return from the Windows device:

```text
tests/codex/evidence/sprint-3-resource-graph-windows.json
tests/codex/evidence/platform/sprint-3-storage-spike-windows.json
```

The device must also return `sprint-3-windows.receipt.json`. This deterministic receipt binds the exact
raw bytes, freeze coordinates, runner, and transfer helper. They are required
for aggregation but are transport controls, not final evidence, and are never
installed or committed.

After those raw reports return, the deterministic aggregation runner must create:

```text
tests/codex/evidence/sprint-3-storage-spike-cross-platform.json
tests/codex/evidence/sprint-3-acceptance.json
```

No synthetic or re-labeled platform evidence is permitted.

## Open acceptance items

| Item | Status | Closure evidence |
|---|---|---|
| `S3-09` cross-platform smoke | **OPEN** | Import and validate the real Windows live and storage reports |
| `S3-10` final audit | **OPEN** | Canonical storage and acceptance aggregates plus final document update |
| `S3-AC-01`–`S3-AC-11` | Current macOS claims pass; cross-platform closure pending | Final acceptance aggregate |
| `S3-AC-12` normalized Windows/macOS graph equality | **DEFERRED** | Strict Windows report and per-phase digest equality |
| Sprint 3 Definition of Done | **NOT MET** | Every preceding item closed |

## Operational notes

- The first real Windows run built Godot and the sidecar successfully, then
  failed closed before the base live phase because protocol-shaped generation
  IDs containing `:` were used directly as physical segment-store filenames.
  Windows Rust tests reproduced `StorageIo(os error 87)`. The fix at the current
  freeze hashes only physical artifact stems and preserves logical IDs; no
  partial Windows evidence was retained.
- The next Windows run reached `rename_uid`, then failed closed because Python
  used the CP1251 host locale to decode UTF-8 Godot output. The current fixture
  explicitly decodes Godot output as UTF-8 with replacement for malformed log
  bytes; a Windows-independent regression test covers that path. Again, no
  partial Windows evidence was retained.
- One earlier previous-freeze macOS live attempt observed a single `4,869 µs`
  Bridge frame while the host was under unrelated heavy Docker/emulator load.
  It was not committed or relabeled. The current-freeze controlled run passed
  with maximum `1,714 µs`; future platform gates should use a dedicated host
  load profile.
- A full Docker `linux/amd64` storage preflight passed both backends and selected
  segment, and the aggregation workflow was rehearsed on explicitly synthetic
  temporary inputs. Neither result is committed or used as acceptance evidence;
  Linux is no longer a Sprint 3 acceptance coordinate.
- The deferred runners have cheap fail-closed preflight modes, no-overwrite and
  failed-run cleanup, transfer receipts, pinned macOS inputs, and rollback-safe
  four-file installation. Receipt unit tests and injected success, rollback, and
  input-mutation rehearsals pass outside the evidence tree.

The tracked macOS evidence proves the current freeze. Windows production-chain parity,
cross-platform D-05 completion, `S3-AC-12`, and Sprint 3 Definition of Done remain open
until the two Windows reports and receipt are imported and the canonical aggregates
exist and validate.
