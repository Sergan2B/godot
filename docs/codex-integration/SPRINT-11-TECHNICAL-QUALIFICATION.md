# Sprint 11 technical qualification and usability deferral

**Decision date:** 2026-07-30

**Status:** Technical qualification in progress; independent human usability
is deferred and the External Codex Beta milestone is not qualified.

## Decision

The product owner is developing and commercializing the product alone and has
confirmed that three real usability participants, including one person
independent of the implementation, are not available for Sprint 11.

Sprint 11 therefore follows two deliberately separate outcomes:

1. complete the package, automated regressions, same-project ownership,
   multi-project isolation, and real App/CLI/IDE technical surface workflows;
2. leave S11-12 `deferred_unacquired` until a future commercial-beta study can
   use real consenting participants.

The machine-readable decision is
[`sprint-11-usability-deferral-waiver.json`](../../tests/codex/evidence/sprint-11-usability-deferral-waiver.json).

## Claim boundary

The technical track may describe package `0.1.8` as an **engineering
candidate** or **private alpha** after its remaining technical gates pass. It
must not describe the package as:

- usability-qualified;
- External Codex Beta qualified;
- commercially release-qualified;
- proven usable without developer assistance.

Founder/operator dogfooding may identify defects, but it is not an independent
participant run and cannot be copied, relabelled, or rehashed into qualifying
human evidence. No synthetic report, model-authored attestation, self-declared
independence flag, or administrative waiver can satisfy S11-12.

The canonical `sprint11_acceptance.py --validate` External Codex Beta path,
human schemas, consent contract, three-participant minimum, and independent
participant requirement remain unchanged and fail closed.

## Technical qualification scope

The technical track still requires:

- the frozen `0.1.8` package and exact Godot prerequisite;
- all automated package, previous-sprint, same-project takeover, multi-project,
  redaction, cleanup, and reproducibility gates;
- fresh official Codex App, CLI, and IDE runs using the canonical prompt pack;
- manual project Trust and real host form outcomes where required;
- semantic parity of saved, offline, runtime, write/validation/Undo, approval,
  restart, and project-isolation behavior;
- bounded, redacted, source/package-bound surface evidence.

Completing these items can produce a separate technical qualification report.
It does not create
`tests/codex/evidence/sprint-11-external-codex-beta-macos.json`, because that
filename and schema assert a fully passing human-usability gate.

## Deferred commercial-beta exit gate

The deferral may be superseded only by a new qualifying acquisition against an
exact future release candidate:

- at least three clean-start participants;
- at least one participant externally attested as implementation-independent;
- consent observed before tasks;
- complete trace, rubric, consent, defect-ledger, and external-authority
  artifacts for every participant;
- the original task, recovery, isolation, comprehension, privacy, and SLO
  requirements;
- retest after every high or critical usability fix.

Until that evidence passes the unchanged validator, the remaining risk is
explicit: technical correctness and host parity do not prove that external
users can discover, understand, diagnose, and recover without developer help.
