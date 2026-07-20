# Sprint 5 evidence — script semantics

**Status:** Administratively closed with an explicit Windows waiver; not
two-host qualified

**Source freeze:** `d1bccbf4fe84a88939cb2232408b4758ef5cee86`

**Closeout decision:** 2026-07-20

## 1. Outcome

Sprint 5 implementation gates `S5-01`–`S5-08` are complete. The qualifying
macOS arm64 live gate passed all ten mutation/recovery phases, the independent
accuracy oracle, both symbol MCP contracts, cleanup/redaction checks, and every
local SLO. No qualifying Windows x86_64 report was produced.

The owner explicitly chose to stop the cross-device evidence loop and defer the
Windows rerun. Therefore Sprint 5 is administratively closed so Sprint 6 can
begin, but it is not represented as full two-host acceptance:

| Coordinate | Result |
|---|---|
| macOS arm64 | **PASS** |
| Windows x86_64 | **WAIVED / UNVERIFIED** |
| Linux | Not a Sprint 5 acceptance coordinate |
| Remote CI | `not_run` |
| Sprint closeout | **CLOSED WITH WAIVER** |

`S5-AC-01`–`S5-AC-11` have passing macOS evidence. `S5-AC-12`, which requires
macOS/Windows normalized graph parity on one source freeze, is waived and remains
unverified. This waiver is not evidence that the Windows implementation passes.

## 2. Canonical artifacts

| Artifact | SHA-256 | Bytes | Meaning |
|---|---|---:|---|
| [`sprint-5-script-semantics-macos.json`](../../tests/codex/evidence/sprint-5-script-semantics-macos.json) | `b9869d4d0ec309b58706c1d505e2084759adf77bafa6de6e048775ba4df0e8f4` | 202,940 | Qualifying macOS raw report; **PASS** |
| [`sprint-5-closeout-waiver.json`](../../tests/codex/evidence/sprint-5-closeout-waiver.json) | `4e555ecb0e7932a941ff7e106604905a6a12b3111d2b3f7870b4f146883d9661` | 2,143 | Machine-readable administrative closeout; **not** a two-host aggregate |

The strict two-host path remains unchanged: a future Windows report must bind
the same source, fixture, oracle, adapter profile, and per-phase semantic
digests before `sprint5_acceptance.py merge` can produce a qualifying
`sprint-5-acceptance.json` with `status=passed`.

## 3. macOS result

The macOS report records:

- all 10/10 phases complete;
- 48/48 symbol truth matches;
- 9/9 analyzer-resolvable relation truth matches;
- zero false `exact` results across five dynamic truth cases;
- saved-change visibility p95: 1,845.485 ms (limit 2,000 ms);
- cached symbol query p95: 0.228 ms (limit 300 ms);
- bulk status/ping p95: 0.596 ms (limit 200 ms);
- Bridge main-thread p95: 1,021 us, with zero samples above 2,000 us.

## 4. Accepted risk and resumption rule

The accepted risk is limited to unverified Windows-specific behavior and
unverified macOS/Windows semantic digest parity. It does not reopen the completed
implementation gates or claim a false Windows pass.

If Windows qualification is resumed later, run the existing Windows runner on
the exact source freeze, validate the resulting raw report, and execute the
strict two-report merge. The qualifying aggregate supersedes this waiver record;
the macOS report does not need to be rerun unless its bound source coordinates
change.
