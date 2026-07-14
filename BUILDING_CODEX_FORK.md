# Building the Godot × Codex fork on macOS

This guide defines the Sprint 0 Build Baseline. Run commands from the repository root. The baseline intentionally disables optional external Vulkan, ANGLE, and AccessKit SDKs so a clean machine needs only the Apple toolchain, Python, and SCons.

## Verified toolchain

| Component | Verified version | Repository minimum or policy |
|---|---:|---|
| macOS | 26.5.2 | Host supported by the selected Xcode |
| Xcode | 26.3 | Xcode 16 or newer |
| Apple Clang | 17.0.0 | Apple Clang 16 or newer |
| macOS SDK | 26.2 | Provided by the selected Xcode |
| Python | 3.14.1 | Python 3.9 or newer |
| SCons | 4.10.1 | SCons 4.4 or newer; the fork pins 4.10.1 |
| Architecture | arm64 | macOS arm64 baseline; cross-platform work starts later |

Confirm the active Apple toolchain:

```sh
xcodebuild -version
xcrun --show-sdk-version
clang --version
python3 --version
```

## Prepare the Python environment

The virtual environment is ignored by Git. Install the exact SCons version used by the fork:

```sh
python3 -m venv .venv
.venv/bin/python -m pip install scons==4.10.1
.venv/bin/scons --version
```

## Build the editor

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

`dev_mode=yes` enables tests, strict checks, extra warnings, and warnings-as-errors. `dev_build=yes` produces the developer binary:

```text
bin/godot.macos.editor.dev.arm64
```

Optional SDKs may be enabled in later builds, but a failure in an optional SDK must not prevent reproducing this baseline.

## Verify the baseline

```sh
GODOT=bin/godot.macos.editor.dev.arm64
FIXTURE=tests/codex/fixtures/smoke_project

"$GODOT" --version
"$GODOT" --test --force-colors
"$GODOT" --headless --import --path "$FIXTURE"
"$GODOT" --headless --editor --path "$FIXTURE" --quit-after 2
"$GODOT" --headless --path "$FIXTURE" --quit-after 2
```

The import, editor, and runtime commands must exit successfully. The generated fixture `.godot/` directory and all build outputs must remain untracked.

## Clean-checkout verification

Sprint evidence must be produced from a clean checkout or a detached worktree at the commit being tested. A maintainer may use:

```sh
git worktree add --detach /tmp/godot-codex-sprint0 <commit>
cd /tmp/godot-codex-sprint0
python3 -m venv .venv
.venv/bin/python -m pip install scons==4.10.1
```

Then run the build and verification commands above. Remove the temporary worktree only after recording the tested commit, toolchain versions, commands, and results.

## Troubleshooting

- **SCons is missing:** use `.venv/bin/scons`; do not depend on a global installation.
- **Wrong Apple compiler:** select Xcode with `sudo xcode-select -s /Applications/Xcode.app/Contents/Developer` outside this repository, then re-run the version checks.
- **MoltenVK error:** the baseline command must include `vulkan=no`.
- **ANGLE or AccessKit warning:** the baseline command explicitly sets `angle=no accesskit=no`.
- **Generated files appear in Git:** stop and inspect `git status`; never commit `bin/`, `.godot/`, `.sconsign*.dblite`, or `.venv/`.

## Upstream and version policy

See [ADR-000](docs/codex-integration/ADR-000-fork-and-upstream-strategy.md). The Build Baseline is tied to a concrete Godot commit; changing that commit requires a new recorded baseline run.
