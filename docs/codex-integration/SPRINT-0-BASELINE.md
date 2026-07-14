# Sprint 0 — local Build Baseline evidence

**Result:** Local baseline passed; remote CI not run

**Run date:** 2026-07-14

**Timezone:** Asia/Bishkek (UTC+06:00)

**Tested commit:** `79188ff597f872553c7806b4df1b70af26fa580e`

**Selected upstream base:** `2c089e9bf0b8712d0bc444c2ceaf9c543ed9c777`

**Fixture:** `tests/codex/fixtures/smoke_project`

## Repository state

| Check | Result |
|---|---|
| Active development branch | `codex/integration` |
| Local `master` | `2c089e9bf0b8712d0bc444c2ceaf9c543ed9c777` |
| Integration parent/base | `2c089e9bf0b8712d0bc444c2ceaf9c543ed9c777` |
| Official upstream fetch URL | `https://github.com/godotengine/godot.git` |
| Upstream push URL | `DISABLED` |
| Shallow repository | `false` |
| Object storage | Blobless partial storage; complete commit history |
| Remote publication | Not performed |

The build was executed in detached worktree `/tmp/godot-codex-sprint0-79188ff597` at the tested commit. The tracked worktree was clean before the build and remained clean after import, editor, runtime, and unit-test runs. `player.gd.uid` is tracked as the script identity; fixture `.godot/` data is ignored.

## Environment

| Component | Observed value |
|---|---|
| Host architecture | `arm64` |
| CPU / memory | 14 logical CPUs / 48 GiB |
| macOS | 26.5.2 (`25F84`) |
| Xcode | 26.3 (`17C529`) |
| Apple Clang | 17.0.0 (`clang-1700.6.4.2`) |
| macOS SDK | 26.2 |
| Python | 3.14.1 |
| SCons | 4.10.1 |

Optional Vulkan, ANGLE, and AccessKit SDKs were disabled by the baseline command. The build used Metal and the built-in OpenGL compatibility path available in the selected source tree.

## Build result

Command:

```sh
BUILD_NAME=codex .venv/bin/scons \
  platform=macos \
  arch=arm64 \
  target=editor \
  dev_mode=yes \
  dev_build=yes \
  vulkan=no \
  accesskit=no \
  angle=no
```

| Evidence | Value |
|---|---|
| Exit status | 0 |
| SCons result | `done building targets` |
| SCons elapsed | 00:02:25.44 |
| Wall time | 147.82 seconds |
| Binary | `bin/godot.macos.editor.dev.arm64` |
| Binary type | Mach-O 64-bit executable arm64 |
| Reported version | `4.8.dev.codex.79188ff59` |
| SHA-256 | `ff48791d567ad2f71987c715daa461bf3a7499c447e61c9f241884255e398daa` |

## Verification results

| Check | Result | Evidence |
|---|---|---|
| Repository hooks | Pass | `prek 0.4.9`; codespell, CODEOWNERS, file-format, and all applicable hooks passed |
| Workflow YAML | Pass | Ruby YAML parser accepted `.github/workflows/macos_builds.yml` |
| Relative Markdown links | Pass | Every local link in the integration documents resolved to an existing file |
| Godot version | Pass | `4.8.dev.codex.79188ff59` |
| Full unit suite | Pass | 1,405/1,405 cases and 421,157/421,157 assertions; 0 failed, 3 skipped; 30.35 seconds |
| Clean fixture import | Pass | Exit 0; 4.96 seconds |
| Headless editor open | Pass | Exit 0; 4.74 seconds |
| Headless runtime | Pass | Exit 0; 0.28 seconds; printed `Codex smoke fixture ready: Smoke Player` |
| Generated-file hygiene | Pass | No tracked diff or untracked non-ignored file after verification |
| GitHub Actions | Not run | Local-only execution was explicitly selected; no push or PR was created |

The diagnostic error and warning lines printed during the unit suite are assertions of negative-path tests. The authoritative doctest summary reported `SUCCESS`, zero failed cases, and zero failed assertions.

## Import shutdown observation

One initial clean import completed filesystem scanning and editor layout loading, then hit an upstream macOS editor shutdown crash at `EditorNode::is_cmdline_mode` with signal 11. No project file was modified or corrupted. The failure could not be reproduced: 15 subsequent runs, each starting after removal of the ignored fixture `.godot/` directory, exited successfully.

This observation is not hidden as a pass. It remains a non-reproduced baseline flake to watch in the first remote CI run and during Sprint 1 lifecycle work. A recurrence makes the affected CI check fail and requires root-cause analysis; it must not be converted to `continue-on-error`.

## Component versions

Sprint 0 does not create bridge, sidecar, Bridge RPC, MCP, index, or transaction artifacts. Their versions are `not_applicable`. No placeholder implementation or fictional version was introduced.

## Gate assessment

- Reproducible local editor/dev build: **pass**.
- Fixture import, editor open, runtime, and base unit tests: **pass**.
- Fork/upstream strategy, documentation index, ownership, and versioning policy: **pass**.
- Remote CI parity: **not run by explicit local-only constraint**.

The local Build Baseline is established. The full Sprint 0 CI gate remains open until this commit is pushed and the prepared macOS GitHub Actions workflow completes successfully.
