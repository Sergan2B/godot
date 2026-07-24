# Sprint 11 human-usability acquisition kit

This directory contains the frozen, content-minimizing materials used to
acquire Sprint 11 usability evidence. It intentionally contains no acquired
participant data and no fixture answers. Every committed template is marked
`qualification: false`, `acquisition_kind: "template"`, and
`status: "unacquired"`.

The participant receives only `sprint11-consent-v1.md` and
`sprint11-participant-script-v1.json`, plus the allowed product help listed in
the script. `sprint11-operator-protocol-v1.json` is operator-only because it
contains the fault identities, expected diagnostic contracts, and safe reset
rules.

The operator invokes `sprint11_fault_harness.py` from the frozen checkout for
all five faults. Discovery sockets require a short private run root (for
example a fresh mode-0700 child of `/private/tmp`); the helper refuses the
repository, normal user installation, symlinked ancestry, reused state, or a
target outside the marker-bound run root. `missing_binary` may touch only the
disposable installed package, and `invalid_project_config` may touch only a
valid-receipt-owned, parseable config stanza.

Every injection writes a private phase journal before mutation. `recover`
continues safely after normal exit or a crash, rejects foreign/ambiguous state,
and publishes a receipt only after the exact target pre/post digest matches.
It is idempotent after success. If recovery cannot prove ownership or exact
restoration, the operator stops the run and preserves the private state for
manual inspection; it must never be reported as a recovered fault.

For a real run:

1. freeze and verify the package, fixture, surface/host coordinate, prompt
   pack, participant script, protocol, consent, trace schema, and rubric;
2. create a clean disposable fixture and private operator ledger;
3. record consent before tasks and derive the participant pseudonym using the
   protocol's salted SHA-256 construction;
4. commit to a random fault order before the first fault, then exercise each
   of the exact five scenarios with its inject/check/reset gate;
5. record only content-free event projections, bounded timing, wrong turns,
   allowed help identifiers, closed rubric outcomes, and defect IDs;
6. validate trace, rubric, consent receipt, and defect ledger together before
   copying their digests into the aggregate usability report;
7. keep private consent/salt material outside the repository and destroy it at
   the declared retention deadline.

Synthetic contract tests use `synthetic_contract_fixture` and are permanently
non-qualifying. Relabelling a template or synthetic fixture as `real_human`
does not create evidence and is rejected by the validator's provenance and
attestation checks. Primary bundle validation by itself is always structural
and non-qualifying, even when every internal digest is recomputed.

A qualifying final run adds one separate
`s11-human-acquisition-authority/1.0` artifact for each participant. The final
acceptance validator reads the trace, rubric, consent receipt, defect ledger,
and authority as exact Git blobs from one source commit. All five files must
share the canonical directory
`tests/codex/acquisition/sprint11/human/s11u-run-<32-lowercase-hex>/` and use
the exact names `trace.json`, `rubric.json`, `consent.json`,
`defect-ledger.json`, and `authority.json`; source templates, fixtures, and
arbitrary repository paths are ineligible.

The authority is an external-operator attestation, not a digital identity
proof. Git and SHA-256 prove which bytes were reviewed and prevent silent
rebinding; they cannot cryptographically prove that a person participated,
consented, or was implementation-independent. Those facts remain the explicit
external study-operator trust boundary, are cross-checked against the primary
bundle, and must never be described as cryptographically established.

Run the self-contained contract tests with:

```sh
python3 -m unittest tests.codex.usability.test_human_acquisition_kit
```

The validator uses only the Python standard library. The JSON Schema files are
under `godot-codex-mcp/schemas/godot_codex/` and use draft 2020-12.
