# External Codex Beta — local macOS guide

**Status:** Sprint 11 candidate guide. Use only with a package whose detached
manifest and archive pass the package validator, then run the installed
`"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" doctor`;
this document does not turn a developer build into a qualified release.

The 2026-07-30 product-owner decision defers independent human usability to a
future commercial beta. App/CLI/IDE technical acquisition may continue, but
the result is limited to an engineering candidate/private alpha and must not
be called External Codex Beta qualified. See
[SPRINT-11-TECHNICAL-QUALIFICATION](SPRINT-11-TECHNICAL-QUALIFICATION.md).

## Support coordinate

The Sprint 11 gate covers local macOS arm64 with:

- the separately distributed, exact Godot Bridge build named by the package
  prerequisite manifest;
- the packaged `godot-codex` and `godot-codex-mcp` binaries;
- Codex desktop, Codex CLI, and the official `openai.chatgpt` extension on the
  exact versions listed by the compatibility matrix;
- one trusted Godot project and one stdio sidecar per Codex task.

Windows, Linux, remote CI, Cursor, the embedded Dock, signing/notarization, and
Stable 1.0 are outside this beta coordinate unless a later matrix says
otherwise.

Packages `0.1.0` through `0.1.5`, plus every operator workspace created for
their Sprint 11 acceptance, are superseded by the independently issued
`0.1.6` candidate. Packages `0.1.2` through `0.1.5` remain immutable historical
evidence: `0.1.2` cannot qualify long project roots, while `0.1.3` allowed
accepted Bridge UDS descriptors to survive into a launched game process, and
`0.1.4` rejected its own full-beta MCP registry during `doctor`. Package
`0.1.5` remains bound to the superseded Codex Desktop `26.721.81911` host
coordinate. Do not replace old binaries in place, reuse an old detached
manifest, or relabel old evidence. Install `0.1.6` into a clean operator root
and repeat App, CLI, and IDE technical acceptance from the beginning.

## Install and verify

1. Verify the archive and detached manifest with the package's documented
   SHA-256 command.
2. Run the included user-local installer without `sudo`. It must not edit a
   shell profile, trust a project, download code, or write project config.

   ```sh
   ./install.sh install
   ./install.sh verify
   ```

3. Keep the installer-owned `current` link intact. Project setup writes the
   absolute stable launcher
   `~/Library/Application Support/GodotCodex/current/bin/godot-codex-mcp`;
   App and IDE startup never depends on a shell `PATH`. Setup and doctor reject
   a basename, a checkout-relative binary, a `current` link to another
   version, or a package whose ownership/checksum proof changed.
4. Verify both commands:

   ```sh
   "$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" version
   "$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
     doctor --project-root /path/to/project --json
   ```

5. Install the separately distributed Godot prerequisite at
   `~/Applications/Godot Codex.app/Contents/MacOS/Godot`, then run the exact
   argv vectors carried by the manifest:

   ```sh
   /usr/bin/shasum -a 256 "$HOME/Applications/Godot Codex.app/Contents/MacOS/Godot"
   "$HOME/Applications/Godot Codex.app/Contents/MacOS/Godot" --version
   ```

   Both results must match the manifest and doctor. A same-looking editor with
   another commit, hash, build ID, or architecture is not interchangeable.

Doctor is model-free, network-free, and non-mutating unless a future command
explicitly says otherwise. Default JSON and text redact private absolute paths
and discovery/authentication material.

## Update Codex host compatibility without replacing the package

Package `0.1.6` can consume a later, independently released App/CLI/IDE
compatibility bundle while keeping the sidecar, Godot, protocol, schemas,
registry, and Bridge contract unchanged. Obtain all three release inputs from
the same trusted channel:

- `surface-compatibility-bundle.json`;
- its detached `host-coordinate-profile.json`;
- the published raw SHA-256 of the bundle file.

Preview and inspect the exact bounded update:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  compatibility install \
  --bundle /path/to/surface-compatibility-bundle.json \
  --host-profile /path/to/host-coordinate-profile.json \
  --expected-sha256 sha256:<published-bundle-digest> \
  --dry-run --json
```

Apply only that preview:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  compatibility install --apply-plan sha256:<preview-plan-digest> --json
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  compatibility status --json
```

The updater rejects a profile whose bytes do not match the bundle binding and
rejects any equal or lower sequence. It stores the bundle and profile as one
private atomic state document under the package-version compatibility
namespace. A malformed active document makes doctor fail with
`package_invalid`; it is never ignored in favor of an older support claim.
The bundle SHA is external trust input, not a self-signature, so do not install
files received through an untrusted channel.

## Upgrade or roll back the package

After `install.sh install` selects a newer package, or `install.sh rollback`
selects the verified previous package, refresh every configured project with
the operations binary under the installer-owned `current` link. The narrowest
workflow preserves the receipt-owned profile and guidance:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --repair --project-root /path/to/project --dry-run --json
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --apply-plan sha256:<new-digest>
```

Rerunning normal interactive `setup` with the intended profile and guidance is
also supported. Both paths accept only an exact receipt-owned config and
guidance state, preview the migration, and refresh the private receipt to the
currently verified package. Historical package hashes and launcher paths are
inert receipt metadata: setup never resolves or executes them as current
authority. Remove remains available for exact receipt-owned content after an
upgrade or rollback.

Never reuse a plan digest created before `current` changed. Every new plan is
bound to the current package version, manifest identity, launcher path, and
launcher bytes; switching or modifying that package between preview and apply
invalidates the plan without changing the project.

## Configure one project

Interactive setup previews its exact owned changes and defaults to no:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --project-root /path/to/project \
  --profile full-beta --guidance all
```

For automation, acquire a current JSON dry-run plan and apply only its exact
digest:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --project-root /path/to/project \
  --profile read-only --guidance skill --dry-run --json
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --apply-plan sha256:<digest>
```

Changing the project root, current config, package, profile, guidance, or plan
expiry invalidates the digest. Setup owns only
`[mcp_servers.godot_editor]`, its receipt-bound AGENTS block, and a generated
skill whose whole-file digest still matches. It preserves unrelated TOML,
comments, instructions, and skills. A conflicting table fails closed.
The private plan and project receipt bind SHA-256 identities for the absolute
launcher path and executable. Human/JSON previews redact the absolute path;
the receipt stores the digests, not the path.

## Repair a receipt-owned project config

When doctor returns `project_config_invalid` with remediation
`repair_project_config`, use the operations binary from the installed stable
package. Do not edit TOML by hand:

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  doctor --project-root /path/to/project --json
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --repair --project-root /path/to/project --dry-run --json
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --apply-plan sha256:<digest>
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  doctor --project-root /path/to/project --json
```

Repair derives profile and guidance from the existing private setup receipt;
there are no profile or guidance flags to choose. Preview is exact, redacted,
expires after ten minutes, binds the receipt, current config, launcher,
package identity, and defaults to no. Applying the exact digest uses the same
atomic journal, stale-input checks, rollback, and receipt commit point as
setup. An exact receipt from a previously installed package can be migrated in
either upgrade or explicit rollback direction; the resulting plan and receipt
always bind the currently verified package.

The repair surface is intentionally narrow. It can restore only a missing or
drifted `mcp_servers.godot_editor` stanza in an existing, bounded,
syntactically valid TOML file previously proven owned by the receipt. It
preserves every unrelated TOML item/comment and the current file mode, then
refreshes the private receipt. A missing, malformed, foreign, or non-private
receipt; an unowned stanza; malformed whole-file TOML; a missing or oversized
config; a symlink target/ancestor; a current package change after preview; or
an intervening edit fails closed before mutation.

Setup cannot trust the project. Open the exact project in each Codex surface,
review the host trust request, and restart that surface if doctor returns
`project_config_not_effective` or `surface_restart_required`. Tasks rooted in
subdirectories must still resolve the same canonical Godot root.

## Start every workflow with status

Call `godot_get_connection_status` or read
`godot://connection/status`. Do not infer readiness from a running process.

- `ready`: compatible Bridge, live editor snapshot, and current cache are
  proven for this project.
- `syncing`/`connecting`: wait or follow the returned remediation.
- `project_session_busy`: another Codex task owns this project session. Close
  that task or wait for it to finish; this task takes over automatically.
  Only the connection-status tool/resource remains readable while waiting.
- `offline_cached`: only source-hash-verified saved semantics are current.
- `offline_empty`: no safe static generation is available.
- `misconfigured`, `incompatible`, or `auth_failed`: stop and run doctor.
- `overloaded`: respect retry/bounds; do not fan out requests.

Status never grants permission to run or mutate the editor.

## Read, runtime, and write workflows

For saved or live questions, prefer semantic identities and returned evidence
over raw serialized files. Preserve confidence, freshness, project/session,
and revision coordinates.

For runtime diagnosis:

1. explicitly run the current scene or project;
2. retain the new `runtime_session_id`;
3. inspect the remote tree, bounded properties, diagnostics, and stack;
4. capture a bounded viewport only when useful;
5. pause, continue, or stop only as requested.

For a change:

1. read a consistent snapshot;
2. prepare the smallest supported operation/change set;
3. inspect the immutable preview, risk, save scope, validation, revisions, and
   digest;
4. approve the normal Codex tool action and, separately, the exact semantic
   form;
5. apply once, query status after response loss, and never replay past the
   commit point;
6. read back and inspect the validation report;
7. use targeted Undo when requested and verify restoration.

Project trust, Codex sandbox/tool approval, semantic form confirmation,
validation, and native Undo are five distinct controls. Neither AGENTS,
skills, config, model prose, nor a boolean tool argument can replace them.

## Offline behavior

With Godot closed, a validated compatible cache may answer only saved project,
resource/dependency, scene/node, script/symbol, and find-usages queries.
Results must say `offline_cached` and name the verified
generation/freshness.

Selection, Inspector, tabs, history, viewport, editor-only diagnostics,
all diagnostics, runtime, screenshots, run control,
prepare/apply/status/Undo, validation, and confirmation-policy operations are
unavailable offline. An empty success or a historical editor/runtime session
is a product defect.

If a source file was added, removed, renamed, or changed after the cached
generation, use the returned `rebuild_static_cache` or
`start_matching_editor` remediation. Do not label the retained generation
current.

## Multiple projects

Open each Godot project in its own Codex task so each loads its own
`.codex/config.toml` and starts its own sidecar. Never copy discovery files,
tokens, cache directories, session/entity IDs, cursors, transactions, reports,
or setup receipts between roots.

There is no global editor picker or sibling-root scan. A selector from project
A must fail in project B. A crash, cache rebuild, approval, transaction, or
runtime session in one project must not change the other.

The qualifying isolation gate also copies and swaps discovery/token material,
swaps config/root/cwd coordinates, restarts one editor, rebuilds one cache,
and injects package/version mismatches. Every case must show no fallback and
no cross-project data, mutation, approval, or transaction leakage, then
restore/stop all fault resources. If any of those coordinates differ, stop
and repair the affected task; never retry by searching for another editor or
reusing a sibling task's package/config.

## Acquire one qualifying App, CLI, or IDE surface

This procedure is for Sprint 11 release qualification, not ordinary product
use. It records one bounded, content-free transport artifact from the
installed sidecar and joins it with a separate external-operator attestation.
It does not enable telemetry or put a proxy between the official host and MCP.

Complete the host-provenance acquisition first, using the exact command and
paths in `SPRINT-11-HOST-CONTRACT.md`. Do not hand-author
`measurement.json`. Freeze the package/source coordinate and install that
package before preparing any surface.

Assign two explicit roles. The **surface user** operates the selected official
host, sends the frozen intents, makes every sandbox choice, chooses
accept/decline/cancel on the semantic forms, and intentionally leaves the
timeout form unanswered. The **external operator** prepares/resets faults,
performs safety checks, personally reviews and accepts the exact project Trust
request, and later attests only what they observed. Except for that Trust
decision, the operator never answers a sandbox or semantic form for the
surface user.

Use a different canonical project root and a different private working
directory for each of `app`, `cli`, and `ide`. The examples below show `app`;
replace only `S11_SURFACE` and the corresponding fresh fault/project/work
roots for later runs. One new operator root may hold the single frozen
candidate installation shared sequentially by all three surfaces; do not
reuse the normal user-local data root or any superseded candidate root.
The stop/restart fault phase and the measured capture phase have disjoint
project and data roots. Every fault/work/project root named below must be new,
non-symlinked, owned by the operator, and mode `0700`:

```sh
S11_REPOSITORY=/absolute/path/to/GodotSTG
S11_PACKAGE_ROOT=/absolute/path/to/extracted/frozen-package
S11_PACKAGE_MANIFEST=/absolute/path/to/capture-capable/sprint11-package-manifest.json
S11_OPERATOR_ROOT=/absolute/path/to/new/private/s11-candidate
S11_SURFACE=app

S11_CAPTURE_DATA_ROOT="$S11_OPERATOR_ROOT/candidate-data"
S11_CAPTURE_BIN_DIR="$S11_OPERATOR_ROOT/candidate-bin"

S11_FAULT_RUN_ROOT="$S11_OPERATOR_ROOT/fault-app"
S11_FAULT_DATA_ROOT="$S11_FAULT_RUN_ROOT/data"
S11_FAULT_BIN_DIR="$S11_FAULT_RUN_ROOT/bin"
S11_FAULT_PROJECT_ROOT="$S11_FAULT_RUN_ROOT/project"

S11_CAPTURE_PROJECT_ROOT="$S11_OPERATOR_ROOT/projects/app-primary"
S11_CAPTURE_WORK_ROOT="$S11_OPERATOR_ROOT/work/app"

/usr/bin/install -d -m 700 \
  "$S11_OPERATOR_ROOT" \
  "$S11_CAPTURE_DATA_ROOT" \
  "$S11_CAPTURE_BIN_DIR" \
  "$S11_FAULT_RUN_ROOT" \
  "$S11_FAULT_DATA_ROOT" \
  "$S11_FAULT_BIN_DIR" \
  "$S11_FAULT_PROJECT_ROOT" \
  "$S11_CAPTURE_PROJECT_ROOT" \
  "$S11_CAPTURE_WORK_ROOT"
```

Install and verify the frozen package into the new candidate roots without
leaking those installer variables into the later official host:

```sh
(
  export GODOT_CODEX_DATA_ROOT="$S11_CAPTURE_DATA_ROOT"
  export GODOT_CODEX_BIN_DIR="$S11_CAPTURE_BIN_DIR"
  cd "$S11_PACKAGE_ROOT"
  ./install.sh install
  ./install.sh verify
)
```

The package manifest, installed tree, `current` target, and package-owned
launcher must match the frozen candidate before continuing. Reuse this exact
candidate installation sequentially for the three surface runs and leave
their finalized private runs unconsumed until final acceptance. Before each
surface, rerun `./install.sh verify` in a subshell with the same two candidate
roots; never reinstall over or switch `current` during acquisition.

Copy the frozen fixture separately into `S11_FAULT_PROJECT_ROOT` and
`S11_CAPTURE_PROJECT_ROOT`; neither copy may contain prior `.godot`, `.codex`,
cache, discovery, token, socket, receipt, or acquisition state. The already
installed package under `S11_CAPTURE_DATA_ROOT` is the measured capture
coordinate and must not be faulted. Configure each new capture project with
that candidate's package-owned `godot-codex setup`; the resulting project
config embeds the custom data root for the official host.

Before arming capture, export the disposable installer roots and complete the
stop/restart-required phase with no active capture lease:

```sh
export GODOT_CODEX_DATA_ROOT="$S11_FAULT_DATA_ROOT"
export GODOT_CODEX_BIN_DIR="$S11_FAULT_BIN_DIR"
```

1. use goal 1 (`install_and_connect`) from
   `tests/codex/usability/sprint11-participant-script-v1.json` to install and
   configure the frozen package only in `S11_FAULT_DATA_ROOT`,
   `S11_FAULT_BIN_DIR`, and `S11_FAULT_PROJECT_ROOT`;
2. use goal 4 (`doctor_fault_matrix`) while the external operator follows only
   the matching preconditions/inject/reset steps in
   `tests/codex/usability/sprint11-operator-protocol-v1.json`;
3. still in that disposable preliminary surface, complete the host-owned
   prelaunch negatives from **S11-08** of `SPRINT-11-PLAN.md`: foreign
   config/root/cwd, package digest/version mismatch, and absence of sibling
   fallback. These are external-operator observations paired with the public
   prelaunch authority gate; they are not a model tool-call recipe and must not
   be deferred to the captured `project_isolation` goal;
4. after every doctor and prelaunch-negative reset passes, close that
   preliminary surface task and the
   editor, stop every sidecar/process bound to the fault roots, and verify the
   disposable package, fixture, config, setup receipt, root/cwd, and launcher
   bindings are restored;
5. unset both phase-only variables before touching the capture coordinate:

   ```sh
   unset GODOT_CODEX_DATA_ROOT GODOT_CODEX_BIN_DIR
   ```

   Confirm both variables are absent, and never point them at
   `S11_CAPTURE_DATA_ROOT` for fault injection.

Those faults may stop or restart the sidecar and therefore must never be run
inside a one-shot capture. Their observed host-negative facts are joined later
by external attestation; the private transport artifact covers the continuous
post-reset read/runtime/write/offline/isolation session. Never fault the
operator's normal user-local installation or reuse the disposable fault
package/project for capture. S11-08 is explicit that config/root/cwd and
package/version cases combine the public prelaunch authority with the
Git-bound App/CLI/IDE operator attestation; the five-fault human operator
protocol alone does not cover those host-owned coordinates.

`S11_CAPTURE_PROJECT_ROOT` must now be configured independently by the verified
`0.1.6` package under `S11_CAPTURE_DATA_ROOT`; the fault-phase config or receipt
is not reusable. Run doctor from that measured capture package. A
`project_config_invalid`, wrong package/receipt, symlinked root, or fixture
digest mismatch is a stop condition:

```sh
"$S11_CAPTURE_DATA_ROOT/current/bin/godot-codex" \
  doctor --project-root "$S11_CAPTURE_PROJECT_ROOT" --json
```

Prepare capture metadata from measured inputs. This command measures the
installed package, Git source, host provenance, and fixture itself; it has no
flags for caller-supplied hashes or host identity:

The detached and installed internal manifests must agree on the exact package
version and source commit and must both bind executable
`bin/godot-codex`, executable `bin/godot-codex-mcp`, and these package-owned
mode-`0644` schemas:

- `sprint11-recorder-journal.schema.json`
- `sprint11-surface-capture-artifact.schema.json`
- `sprint11-surface-metadata.schema.json`

The installed schema bytes must equal the Git blobs at that package source
commit. A missing record, copied newer schema beside an older source commit,
or the superseded tracked manifest is a stop condition before metadata is
written.

```sh
python3 "$S11_REPOSITORY/tests/codex/sprint11_surface_artifacts.py" prepare \
  --surface "$S11_SURFACE" \
  --package-manifest "$S11_PACKAGE_MANIFEST" \
  --data-root "$S11_CAPTURE_DATA_ROOT" \
  --measurement \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/host-provenance-v013/measurement.json" \
  --fixture-root "$S11_CAPTURE_PROJECT_ROOT" \
  --repository-root "$S11_REPOSITORY" \
  --output "$S11_CAPTURE_WORK_ROOT/metadata.json"
```

Close every other Codex task rooted in `S11_CAPTURE_PROJECT_ROOT`, then arm
exactly one one-shot lease. Save both the returned 64-character `run_id` and
`metadata_sha256`; the lease expires after at most 1,800 seconds if it is not
claimed. Once claimed, that TTL does not limit the workflow duration:

```sh
"$S11_CAPTURE_DATA_ROOT/current/bin/godot-codex" surface-capture arm \
  --surface "$S11_SURFACE" \
  --project-root "$S11_CAPTURE_PROJECT_ROOT" \
  --metadata "$S11_CAPTURE_WORK_ROOT/metadata.json" \
  --ttl-seconds 1800 \
  --json
```

Proceed only when stdout contains
`schema_version: godot-codex-surface-capture-result/1.1`,
`state: armed`, and `disposition: created|recovered`. `recovered` is the
idempotent response to an exact retry and returns the same `run_id`, including
when the first successful arm response was lost. A different active lease, a
claimed lease, or an exact expired lease instead produces a non-authorizing
JSON report on stderr and exit status 2. Preserve its existing `run_id`, use
`status`, then either finish/consume the existing run or use the narrowly
applicable `cancel`/`abandon` recovery before retrying. Never start the host
after a non-authorizing arm result, and never arm around an unresolved claimed
run.

`--surface` binds the intended acquisition label and measured host metadata,
but MCP has no trustworthy App/CLI/IDE process-origin signal. Claim selection
therefore uses the exact project and installed-package bindings. If any
unintended old or concurrent task claims the lease, discard that run; do not
relabel it. The external operator's observation is the explicit surface-origin
trust boundary.

Now start only the selected official host:

- App: open `S11_CAPTURE_PROJECT_ROOT` as the local project and create a new
  task.
- CLI: start the measured client directly, without a PATH substitute:

  ```sh
  "/Applications/ChatGPT.app/Contents/Resources/codex" \
    --strict-config --cd "$S11_CAPTURE_PROJECT_ROOT"
  ```

- IDE: open the exact folder in the measured VS Code Stable build and create
  the task in the official measured extension.

When the official host asks whether to trust the project, the external
operator must personally review the exact displayed project root and manually
accept Trust in that host. Capture tooling, setup, AGENTS, a model, and an
automation script must never answer Trust. A run without that visible,
personal decision cannot qualify. Restart only that surface if the newly
trusted project configuration was not loaded.

Open the matching Bridge-enabled Godot editor on the same project. Execute the
frozen prompt pack
`tests/codex/prompts/sprint11-external-beta-v1.json` without substituting
another task, root, package, or MCP process. The surface user sends the exact
`generic_goals[].intent` strings for these goal IDs, one message at a time and
in this order:

1. `install_and_connect` — now a verification-only goal; it explicitly must
   not rerun setup;
2. `saved_semantic_context`;
3. `offline_context` — close and reopen only Godot Editor while keeping this
   Codex task and sidecar alive, then establish the current revision in the
   reconnected editor session;
4. `runtime_diagnostics` — start runtime, stop it, start a second distinct
   runtime session, and bind the bounded error stack to that second session;
5. `compound_write_and_undo` — use the established current revision; the
   surface user chooses accept, decline, and cancel, but intentionally gives
   no answer for timeout, while the external operator only observes. After
   accepted apply, validation, and exact Undo, separately restart only Godot
   Editor, read the current scene once in the new editor session, and prove
   rejection of one old pre-restart guard as stale;
6. `project_isolation`.

Do not send `doctor_fault_matrix` again: it was completed and reset before
metadata measurement and capture. Likewise, do not repeat the host-owned
foreign config/root/cwd or package digest/version prelaunch negatives after
arming; captured `project_isolation` is limited to cross-project identities and
selectors rejected without fallback. Do not disclose fixture answers or
prescribe model tool calls.

Personally observe the captured host sandbox layer, semantic forms, all four
form outcomes, and cross-project selector rejection. Carry forward the
separately observed, fully reset pre-arm root/cwd/config and package-mismatch
negatives when completing external attestation; never recreate them in the
capture phase. A `project_session_busy` status means another task still owns
this project: close that owner and allow the selected task to recover. It is
not `syncing`, and repeated waiting cannot make two owners qualify.

After the frozen workflow finishes and every response is complete, terminate
the selected official host normally so it closes stdio; merely switching or
archiving a task does not prove MCP shutdown:

- App: choose **Codex → Quit Codex** (`Cmd+Q`) and wait for the app to exit;
- CLI: press `Ctrl+D` at an empty idle prompt and wait for the measured CLI to
  exit normally; do not interrupt or kill it;
- IDE: choose **Code → Quit Visual Studio Code** (`Cmd+Q`) and wait for the
  extension host and VS Code to exit; do not reload or force-quit.

From a separate terminal, query the saved run exactly once:

```sh
S11_RUN_ID=<64-lowercase-hex-from-arm>
"$S11_CAPTURE_DATA_ROOT/current/bin/godot-codex" surface-capture status \
  --run-id "$S11_RUN_ID" --json
```

`finalized` is necessary but not sufficient: a failed MCP service can also
finalize a diagnostic artifact, and `derive` accepts only
`outcome: completed`. `armed` means the matching sidecar never claimed the
lease. `claimed` after the task exited means the capture was interrupted and
cannot qualify. `cancel` is valid only for an unclaimed `armed` lease.

For a failed inactive `claimed` run, first confirm the exact host and sidecar
have exited and preserve whatever diagnosis is needed. This cleanup path is
limited to a bare claim or a narrowly recognized unrecoverable partial
finalization. Then, with a separate explicit decision to discard that
nonqualifying private state, use the `metadata_sha256` returned by `arm`:

```sh
S11_METADATA_SHA256=sha256:<64-lowercase-hex-from-arm>
"$S11_CAPTURE_DATA_ROOT/current/bin/godot-codex" surface-capture abandon \
  --run-id "$S11_RUN_ID" \
  --metadata-sha256 "$S11_METADATA_SHA256" \
  --json
```

`abandon` accepts only the exact metadata-bound bare claim or an allowlisted
structurally unrecoverable partial. It rejects a live claim, an
armed/finalized run, a valid recoverable partial, a wrong digest, unknown
extra state, unsafe permissions, or a symlink without deletion. A recoverable
partial returns `surface_capture_recovery_required` and must be completed by
the next matching sidecar startup; a healthy finalized run must instead use
digest-bound `consume`.

Derive the canonical, safe journal and pending trace from the private
artifact. The raw file stays under the owner-only installed data root and is
never committed:

```sh
python3 "$S11_REPOSITORY/tests/codex/sprint11_surface_artifacts.py" derive \
  --metadata "$S11_CAPTURE_WORK_ROOT/metadata.json" \
  --capture \
    "$S11_CAPTURE_DATA_ROOT/surface-capture-v1/runs/$S11_RUN_ID/journal.json" \
  --output-directory "$S11_CAPTURE_WORK_ROOT/derived"
```

Attestation must run interactively from a controlling TTY. Enter an operator
identifier; only its domain-separated hash is stored. Type `yes` only for an
observation personally made in that official-host run. Any `no`, incomplete
answer, reused directory, or non-TTY invocation publishes nothing.

The closed checklist separately asks whether the operator personally reviewed
the exact root in the official host and manually accepted its Trust request;
project configuration loading or a pre-existing trusted state is not a
substitute for that observation. After all answers are positive, the helper
creates only the missing canonical `surfaces/` parent through no-follow
descriptors and atomically publishes the new surface directory. Run:

```sh
python3 "$S11_REPOSITORY/tests/codex/sprint11_surface_artifacts.py" attest \
  --metadata "$S11_CAPTURE_WORK_ROOT/metadata.json" \
  --journal "$S11_CAPTURE_WORK_ROOT/derived/recorder-journal.json" \
  --trace "$S11_CAPTURE_WORK_ROOT/derived/pending-trace.json" \
  --repository-root "$S11_REPOSITORY" \
  --output-directory \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/$S11_SURFACE"
```

`attest` validates the deterministic journal/trace derivation before its
atomic publication. Keep the private raw run and record the
`capture_journal_sha256` returned by `derive`. Repeat the complete procedure
from a new fixture copy and private working directory for the other two
surfaces; do not consume any run yet.

After all three directories exist, run the nonqualifying semantic parity
check:

```sh
python3 "$S11_REPOSITORY/tests/codex/sprint11_acceptance.py" \
  --compare-traces \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/app/trace.json" \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/cli/trace.json" \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/ide/trace.json" \
  --compare-journals \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/app/recorder-journal.json" \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/cli/recorder-journal.json" \
    "$S11_REPOSITORY/tests/codex/acquisition/sprint11/surfaces/ide/recorder-journal.json"
```

This parity command does not validate external authority or qualify the
release. Keep all three private runs until the package-live, multi-project,
surface, human-usability, and final evidence artifacts have been committed and
the final wrapper reports `status: passed`:

```sh
python3 "$S11_REPOSITORY/tests/codex/sprint11_acceptance.py" \
  --validate \
    "$S11_REPOSITORY/tests/codex/evidence/sprint-11-external-codex-beta-macos.json" \
  --artifact-root /absolute/path/to/detached-package-root \
  --timeout 180
```

Only then consume each exact private finalized run. Use that run's own
`capture_journal_sha256`; a wrong digest or any non-finalized/unsafe state
changes nothing:

```sh
S11_CAPTURE_SHA256=sha256:<64-lowercase-hex-from-derive>
"$S11_CAPTURE_DATA_ROOT/current/bin/godot-codex" surface-capture consume \
  --run-id "$S11_RUN_ID" \
  --capture-sha256 "$S11_CAPTURE_SHA256" \
  --json
```

The owner-only store has a hard run-count bound and refuses a new arm before
creating state when full. Consuming finalized runs or explicitly abandoning
an already diagnosed eligible inactive claim frees capacity; there is no
automatic deletion of published diagnostic state.

Never invoke `sprint11_surface_recorder.py` or
`sprint11_surface_transport_live.py` for a real acquisition: they are
fixture-only regression code and intentionally fail closed. A legacy
`s11-recorder-journal/1.0` cannot qualify.

## Troubleshooting map

| Diagnostic | Meaning | Safe next action |
|---|---|---|
| `binary_missing` / `binary_not_executable` / `binary_arch_mismatch` / `package_invalid` | package install or stable absolute launcher cannot be used | reinstall/rollback the matching macOS arm64 package; do not substitute a PATH basename |
| `project_invalid` | root is not the canonical Godot project | pass the directory containing `project.godot` |
| `project_config_missing` | project config is absent | preview normal setup; repair does not create a missing whole config |
| `project_config_invalid` + `repair_project_config` | a receipt-owned stanza is missing or drifted | preview `setup --repair`, apply its exact digest, then rerun doctor; never hand-edit |
| `project_config_invalid` without a valid repair preview | TOML/receipt/ownership/path/package proof is unsafe | stop and restore a known-good owned config/package; repair fails closed |
| `project_config_not_effective` | host did not load the trusted project layer | open exact root, trust it, restart the surface |
| `bridge_discovery_missing` | matching editor is not publishing discovery | start the exact Bridge-enabled Godot build/project |
| `bridge_discovery_stale` | discovery does not describe a live current editor | close stale editor/process and restart the exact project |
| `permissions_invalid` | private runtime files are unsafe | restore owner-only directory/file permissions |
| `bridge_version_incompatible` | matrix rejects this Bridge/package pair | install the exact prerequisite/package pair |
| `bridge_authentication_failed` | private handshake failed | restart the matching editor; never copy tokens |
| `project_binding_mismatch` | editor and task roots differ | open the exact project; do not auto-select another editor |
| `static_cache_stale` / `static_cache_corrupt` | offline facts are not authoritative | rebuild via a matching live editor |
| `surface_restart_required` | App/CLI/IDE has stale config | restart only that Codex surface |
| `transaction_recovery_required` | a commit may be unresolved | query status and follow recovery; never replay apply |

Use `--show-paths` only for local human troubleshooting; do not paste its
output into prompts, issues, or evidence without redaction.

## Remove setup or roll back

```sh
"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" \
  setup --remove --project-root /path/to/project
```

This removes only receipt-matching setup-owned content. If a user edited an
owned artifact, removal fails and shows a safe remediation instead of deleting
it.

Use the package-owned rollback/uninstall command from the detached manifest.
It must preserve projects, `.codex` files, caches, unrelated user binaries,
and foreign links. Remove project setup separately before uninstall only when
that is the user's intent.
