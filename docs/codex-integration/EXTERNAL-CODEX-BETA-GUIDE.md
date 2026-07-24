# External Codex Beta — local macOS guide

**Status:** Sprint 11 candidate guide. Use only with a package whose detached
manifest and archive pass the package validator, then run the installed
`"$HOME/Library/Application Support/GodotCodex/current/bin/godot-codex" doctor`;
this document does not turn a developer build into a qualified release.

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
