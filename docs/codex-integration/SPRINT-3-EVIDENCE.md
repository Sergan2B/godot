# Sprint 3 — interim evidence report

**Status:** In progress — not accepted

**Snapshot:** 2026-07-18

**Scope:** Stage 5 (`S3-09`, `S3-10`)

**Current result:** real Windows x86_64 attempts exposed non-portable physical
generation filenames in the production segment store and locale-dependent Godot
log decoding in the live fixture. Both source fixes are frozen at the coordinates
below. They invalidate the earlier macOS arm64 Stage 5 reports, so macOS, Windows,
and Linux qualifying reports must all be regenerated before the canonical
aggregates can close Sprint 3.

## Decision summary

| Gate | macOS arm64 | Windows x86_64 | Linux x86_64 | Overall |
|---|---|---|---|---|
| Editor → Bridge → sidecar → persistent index → MCP | **STALE** — rerun required | **IN PROGRESS** — no artifact | Not required | Open |
| Full D-05 storage and recovery profile | **STALE** — rerun required | **IN PROGRESS** — no artifact | **DEFERRED** — no artifact | Open |
| Cross-platform storage aggregate | Input stale | Input missing | Input missing | Not generated |
| Final Sprint 3 acceptance | Input stale | Input missing | Input missing | Not generated |
| Remote CI | `not_run` | `not_run` | `not_run` | Not required |

Docker `linux/amd64` was exercised only as a development preflight. It is not a
real target host, no Docker report is committed, and it must not be represented
as Linux acceptance evidence.

## Frozen source coordinates

| Coordinate | Exact value |
|---|---|
| Source-freeze commit | `a90ddd06c81a6210f44552b46ff533248b93ed90` |
| Scoped source SHA-256 | `sha256:73e99eec9897f8e2b4c6c210a3c56bd57a91ce347aedba030323eb01bf8438eb` |
| Golden oracle SHA-256 | `sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b` |
| Superseded macOS live evidence commit | `d7339412f0e88475faab7d5f4264629671853888` |
| Superseded macOS storage evidence commit | `7d53adb993c463e4062a6cbfa4be17dd0319a163` |

Every new macOS, Linux, or Windows raw report must record the exact freeze commit
and both exact digests above. The tracked macOS reports record the previous
freeze and remain only as historical development evidence. A further
source-scoped or validator change requires another freeze and regeneration of
every Stage 5 platform report.

## Tracked superseded macOS artifacts

| Artifact | Status | File SHA-256 | Bytes |
|---|---|---|---:|
| [`sprint-3-resource-graph-macos.json`](../../tests/codex/evidence/sprint-3-resource-graph-macos.json) | Historical macOS live **PASS**, wrong freeze for current acceptance | `sha256:aeabdd3bc2501c5333d3464470199f0cd5fc05e5bbf253001b6596a4004f4040` | 199,017 |
| [`sprint-3-storage-spike-macos.json`](../../tests/codex/evidence/platform/sprint-3-storage-spike-macos.json) | Historical macOS decision **PASS**, wrong freeze for current acceptance | `sha256:655d62f705d1678c45e61dc293bc0e5c5002df6725cbc4592832c85085841494` | 352,547 |

The legacy `tests/codex/evidence/sprint-3-storage-spike.json` is excluded from
Stage 5. It records an older commit, a dirty tree, and different source/oracle
coordinates; it is not the final macOS raw report.

## Superseded macOS live gate

The following result describes the previous source freeze. It must not be used
as current Stage 5 acceptance and must be replaced by a run at `a90ddd06c81a`.

The schema-3 report records `status=passed`, `execution=local_model_free`,
`profile=acceptance`, and `platform=macos-arm64` on
`macOS-26.5.2-arm64-arm-64bit-Mach-O`.

Artifact and toolchain identity:

- Godot `4.8.dev.custom_build.75364c2cc`, SHA-256
  `sha256:87089706a22b5dd02026025f47364248714fd3b62e5111657c907b62c6256a55`;
- `godot-codex-mcp 0.1.0`, SHA-256
  `sha256:bbb447c3c7a5d9704fb7b6e1f607bb485e8f286e8af4a559b30e0b69aaebf075`;
- CPython `3.14.1`, SCons
  `4.10.1.055b01f429d58b686701a56df863a817c36bb103`;
- Rust/Cargo `1.94.1`, target `aarch64-apple-darwin`.

All eight canonical phases completed in order. Every phase passed direct and
reverse oracle comparison, diagnostics comparison, stable project scope, and
cleanup. The report contains 416 qualifying cached-query samples, 681 control
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
| Cached resource query | 416 | 0.157 ms | 0.345 ms | ≤300 ms | **PASS** |
| Ordinary incremental visibility | 7 | 441.732 ms | 561.196 ms | ≤2,000 ms | **PASS** |
| Control status/ping during gap rebuild | 681 | 0.395 ms | 0.663 ms | ≤200 ms | **PASS** |
| Bridge main-thread busy frames | 5,402 | 324 µs | 411 µs | zero >2,000 µs | **PASS** |

Bridge telemetry recorded maximum `1,510 µs`, over-budget count `0`, no buffer
overflow, and a `2,000 µs` budget. Separately reported non-incremental timings
were startup p95 `2,503.067 ms`, compatible reopen `3,999.849 ms`, and forced
journal-gap rebuild `40,609.468 ms`; they are not mixed into ordinary visibility.

## Superseded macOS D-05 storage and recovery

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
| SQLite | `0.3693609988706344` | `[0.366695961304978, 0.3714947974887653]` | 792.431 / 806.177 ms | 1,219.630 / 1,254.502 ms | 75.807625 / 81.859458 ms | 262,033,408 B | 4,052,080 B |
| Segment | `0.8119081254790795` | `[0.8114522337516358, 0.812114686755438]` | 13,309.086 / 13,838.896 ms | 525.174 / 557.754 ms | 0.020833 / 0.021334 ms | 70,011,685 B | 1,950,848 B |

The raw macOS decision selected `segment`: both backends qualified and segment
had the higher weighted score (`0.8119`). This is one platform result, not the
missing canonical three-platform D-05 aggregate or validation receipt.

## Superseded freeze local repository verification

The previous freeze and its macOS evidence were checked locally with:

- Python acceptance/fixture suite: 39 passed;
- storage spike Rust suite: 18 passed, formatting and Clippy passed;
- Rust conformance suite: 11 passed, formatting and Clippy passed;
- production sidecar workspace: 32 passed, formatting and Clippy passed;
- focused Godot Codex suite: 42 cases and 5,018 assertions passed;
- full Godot suite: 1,447 cases and 426,175 assertions passed, with three
  declared skips and zero failures;
- all repository pre-commit hooks passed.

Remote CI was intentionally not run and is not awaited for Sprint 3 acceptance.

## Deferred host matrix

The deferred procedures are executable and documented in
[`tests/codex/runners/README.md`](../../tests/codex/runners/README.md). They use
a separate clean worktree at the exact freeze and fail closed on wrong
architecture, dirty source, digest mismatch, CI markers, containers, incomplete
SLOs, or failed segment gates/fault cases. The wrapper set is pinned to
`a8dd7a73a756cc9b636dee4fe0552f94dc322593`.

Required reports to return from the other devices:

```text
tests/codex/evidence/sprint-3-resource-graph-windows.json
tests/codex/evidence/platform/sprint-3-storage-spike-linux.json
tests/codex/evidence/platform/sprint-3-storage-spike-windows.json
```

The devices must also return `sprint-3-linux.receipt.json` and
`sprint-3-windows.receipt.json`. These deterministic receipts bind the exact
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
| `S3-09` cross-platform smoke | **OPEN** | Real Windows live report plus real Windows/Linux storage reports |
| `S3-10` final audit | **OPEN** | Canonical storage and acceptance aggregates plus final document update |
| `S3-AC-01`–`S3-AC-11` | Previous macOS claims are stale; current cross-platform closure pending | Final acceptance aggregate |
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
  It was not committed or relabeled. A controlled full rerun passed with maximum
  `1,510 µs`; future platform gates should use a dedicated host load profile.
- A full Docker `linux/amd64` storage preflight passed both backends and selected
  segment, and the aggregation workflow was rehearsed on explicitly synthetic
  temporary inputs. Neither result is committed or used as acceptance evidence.
- The deferred runners have cheap fail-closed preflight modes, no-overwrite and
  failed-run cleanup, transfer receipts, pinned macOS inputs, and rollback-safe
  five-file installation. Five receipt unit tests and injected success,
  rollback, and input-mutation rehearsals pass outside the evidence tree.

The available tracked macOS evidence proves the previous freeze only and does
not qualify the current one. Linux storage portability, Windows production-chain
parity, current macOS results, cross-platform D-05 completion, `S3-AC-12`, and
Sprint 3 Definition of Done remain open until the regenerated real-host
artifacts and canonical aggregates exist and validate.
