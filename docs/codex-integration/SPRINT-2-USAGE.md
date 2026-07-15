# Sprint 2 usage — live Godot read tools in Codex

**Status:** Windows x86_64 and macOS arm64 automated live gates complete

## Prerequisites

- the custom Godot editor built with `module_codex_bridge_enabled=yes`;
- Rust 1.94.1 (the workspace toolchain file selects it automatically);
- Windows x86_64 for the loopback TCP workflow verified in Sprint 2, or macOS
  on Apple Silicon for the Unix Domain Socket profile;
- a trusted Codex project, because project-scoped `.codex/config.toml` is not
  loaded for untrusted repositories.

## Build and configure

From the project root:

```sh
cargo build --release --manifest-path godot-codex-mcp/Cargo.toml
```

Copy `.codex/config.toml.example` into `.codex/config.toml` inside the Godot
project that the editor will open—not necessarily this source repository root.
Put the release binary on `PATH`, or replace `command = "godot-codex-mcp"` with
its absolute path. The example passes `--project-root .`, marks the server
required, and enables only:

- `godot_get_editor_state`;
- `godot_get_current_scene`;
- `godot_get_selected_nodes`.

Restart the Codex task from that same Godot project root after adding the
configuration and trusting the project. Open that exact project in the custom
Godot editor. The editor publishes private discovery under `.godot/codex`; the
sidecar canonicalizes `.` and rejects a discovery record for any other project
or editor session. Windows discovery advertises only a canonical
`127.0.0.1:<ephemeral-port>` endpoint and the editor protects its token and
runtime files with a restricted DACL.

## Smoke workflow

1. Open `tests/codex/fixtures/smoke_project/project.godot` in the custom editor.
2. Select `Player` (`CharacterBody2D`) in `main.tscn`.
3. Change `movement_speed` in the Inspector without saving.
4. Ask Codex for the selected Godot nodes.
5. Confirm the result includes the live node path, `res://player.gd`, the
   unsaved inspector value, `freshness: "current"`, and the scene revision.
6. Change the property again and repeat. The sidecar must invalidate the old
   generation, complete a new atomic snapshot, and return a larger revision.

If Godot is stopped, reconnecting, or a snapshot fails validation, MCP remains
available but the tools return a structured retryable error. They do not return
the previous generation as current.

## Automated model-free live gate

On Windows, run the verified gate directly from PowerShell:

```powershell
python tests\codex\sprint2_live_smoke.py `
  --godot bin\godot.windows.editor.dev.x86_64.console.exe `
  --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
  --project-root tests\codex\fixtures\smoke_project `
  --evidence tests\codex\evidence\sprint-2-live-smoke.json `
  --timeout 40
```

On macOS, copy the fixture to a short path first because Unix Domain Socket
paths are length-limited:

```sh
mkdir -p /tmp/gcb-s2
rsync -a --delete --exclude .godot/ \
  tests/codex/fixtures/smoke_project/ /tmp/gcb-s2/project/
python3 tests/codex/sprint2_live_smoke.py \
  --godot /path/to/Godot.app/Contents/MacOS/Godot \
  --sidecar godot-codex-mcp/target/release/godot-codex-mcp \
  --project-root /tmp/gcb-s2/project \
  --evidence tests/codex/evidence/sprint-2-live-smoke-macos.json
```

The fixture plugin is opt-in and remains inert outside this harness. The test
launches the real editor and sidecar, performs two native unsaved property
changes, calls MCP without a model, verifies `275.0 → 310.0`, dirty state,
identity/evidence fields, increasing revisions and a new snapshot generation,
then confirms the disk scene stayed at `240.0`.

For the external-Codex run, also copy the template to
`/tmp/gcb-s2/project/.codex/config.toml`, set `command` to the absolute release
sidecar path, and start/trust the Codex task with `/tmp/gcb-s2/project` as its
project root. This exact-root binding is part of the acceptance assertion.

```sh
mkdir -p /tmp/gcb-s2/project/.codex
cp .codex/config.toml.example /tmp/gcb-s2/project/.codex/config.toml
# Edit command in the copied file to the absolute release-sidecar path.
```

On Windows, create
`tests/codex/fixtures/smoke_project/.codex/config.toml` from the same template
and use either `godot-codex-mcp.exe` from `PATH` or an absolute path written
with `/` separators. Start the new trusted Codex task from that fixture
directory, not from the engine repository root.

## Verification commands

```sh
cargo fmt --all --manifest-path godot-codex-mcp/Cargo.toml -- --check
cargo test --workspace --all-targets --manifest-path godot-codex-mcp/Cargo.toml
cargo clippy --workspace --all-targets --manifest-path godot-codex-mcp/Cargo.toml -- -D warnings
```

The automated Windows and macOS gates are recorded in
`tests/codex/evidence/sprint-2-live-smoke.json` and
`tests/codex/evidence/sprint-2-live-smoke-macos.json`. The optional
human-facing external-Codex workflow is recorded separately in
`tests/codex/evidence/sprint-2-live-checklist.md`; it uses the same exact-root
configuration and three read-only tools.
