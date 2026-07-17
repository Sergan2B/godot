# Sprint 3 deferred host runs

**Status:** ready for later execution on real Linux x86_64 and Windows x86_64
devices.

These wrappers complete the deferred host matrix from
[Sprint 3 Stage 5](../../../docs/codex-integration/SPRINT-3-STAGE-5-PLAN.md).
They are post-freeze orchestration files and are intentionally outside
`tests/codex/sprint3_source_scopes.txt`. The evidence producers and canonical
validators remain the frozen files in the clean source checkout.

Do not use Docker, Wine, WSL, synthetic platform rewrites, or hosted CI as a
replacement for either required host. `remote_ci` stays `not_run`.

## Frozen coordinates

Every qualifying report must contain these exact values:

| Coordinate | Required value |
|---|---|
| Git commit | `75364c2cc5fe50de5a508315c41cc43200b90024` |
| Scoped source SHA-256 | `sha256:0540dc092e2a5d6c94ac8e23d84bd2bc7224ddfb2c78456f09443d14e80950d2` |
| Oracle SHA-256 | `sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b` |

## Use two checkouts

The runners did not exist at the source-freeze commit. Keep a control checkout
on the descendant branch containing this directory and create a separate clean
worktree at the exact freeze:

```text
GodotSTG/                 control checkout with tests/codex/runners
GodotSTG-sprint3-freeze/ clean detached worktree at 75364c2...
sprint3-raw/              output directory outside both checkouts
```

On Linux or macOS, prepare the worktree with:

```sh
CONTROL=/absolute/path/GodotSTG
FREEZE=/absolute/path/GodotSTG-sprint3-freeze
git -C "$CONTROL" cat-file -e 75364c2cc5fe50de5a508315c41cc43200b90024^{commit}
git -C "$CONTROL" worktree add --detach "$FREEZE" \
  75364c2cc5fe50de5a508315c41cc43200b90024
test -z "$(git -C "$FREEZE" status --porcelain)"
```

PowerShell equivalent:

```powershell
$Control = "C:\absolute\path\GodotSTG"
$Freeze = "C:\absolute\path\GodotSTG-sprint3-freeze"
git -C $Control cat-file -e "75364c2cc5fe50de5a508315c41cc43200b90024^{commit}"
if ($LASTEXITCODE -ne 0) { throw "source freeze is unavailable" }
git -C $Control worktree add --detach $Freeze 75364c2cc5fe50de5a508315c41cc43200b90024
if ($LASTEXITCODE -ne 0) { throw "freeze worktree creation failed" }
if ((git -C $Freeze status --porcelain) -join "") { throw "freeze worktree is dirty" }
```

Do not copy the runners into the freeze worktree: an untracked wrapper there
would correctly trip the clean-checkout guard.

## Linux x86_64 storage run

Requirements:

- a real Linux x86_64 host or full Linux VM, not a container;
- Python 3;
- Rustup with toolchain `1.94.1-x86_64-unknown-linux-gnu`;
- enough free space for the Rust builds and full 100k-resource stress profile.

Run from the control checkout:

```sh
CONTROL=/absolute/path/GodotSTG
FREEZE=/absolute/path/GodotSTG-sprint3-freeze
RAW=/absolute/path/sprint3-raw
bash "$CONTROL/tests/codex/runners/sprint3_linux_x86_64.sh" \
  "$FREEZE" \
  "$RAW/sprint-3-storage-spike-linux.json"
```

The wrapper checks the real host, architecture, absence of container and CI
markers, exact freeze and digests, clean checkout, and Rust host triple. It then
runs the full Rust workspace plus the non-quick D-05 profile and requires the
segment backend to pass every gate and fault case.

Expected output:

```text
sprint-3-storage-spike-linux.json
```

## Windows x86_64 live and storage run

Requirements:

- a real Windows x86_64 host or full Windows VM, not Wine, WSL, or a container;
- Windows PowerShell 5.1 or PowerShell 7;
- Python and SCons;
- Rustup toolchain `1.94.1-x86_64-pc-windows-msvc`;
- the MSVC and Windows SDK dependencies required for a Godot editor build.

Run from the control checkout:

```powershell
$Control = "C:\absolute\path\GodotSTG"
$Freeze = "C:\absolute\path\GodotSTG-sprint3-freeze"
$Raw = "C:\absolute\path\sprint3-raw"
& "$Control\tests\codex\runners\sprint3_windows_x86_64.ps1" `
    -Repository $Freeze `
    -OutputDirectory $Raw
```

From a non-interactive launcher, either supported shell can invoke the same
script:

```powershell
powershell.exe -NoProfile -ExecutionPolicy Bypass -File `
    "$Control\tests\codex\runners\sprint3_windows_x86_64.ps1" `
    -Repository $Freeze -OutputDirectory $Raw
pwsh.exe -NoProfile -File `
    "$Control\tests\codex\runners\sprint3_windows_x86_64.ps1" `
    -Repository $Freeze -OutputDirectory $Raw
```

The wrapper builds Godot and the sidecar, executes all eight live phases, runs
the strict live validator, executes the full D-05 profile, and checks the exact
freeze, digests, SLO result, segment gates, and fault matrix.

Expected outputs:

```text
sprint-3-resource-graph-windows.json
sprint-3-storage-spike-windows.json
```

## Return the raw reports

Copy these three files byte-for-byte into one staging directory outside the
aggregation checkout. Do not open and resave them in an editor:

```text
sprint-3-resource-graph-windows.json
sprint-3-storage-spike-linux.json
sprint-3-storage-spike-windows.json
```

Keep the macOS reports already tracked in the repository unchanged.

## Deterministic aggregation

On the macOS control checkout, first ensure the worktree is clean and the freeze
is its ancestor. The host needs Bash 3.2+, Python 3, Git, rustup toolchain
`1.94.1`, and the standard macOS `install`, `cmp`, and `shasum` utilities. Then
run:

```sh
CONTROL=/absolute/path/GodotSTG
RAW=/absolute/path/sprint3-raw
test -z "$(git -C "$CONTROL" status --porcelain)"
/bin/bash "$CONTROL/tests/codex/runners/sprint3_aggregate.sh" "$CONTROL" "$RAW"
```

The aggregation wrapper:

1. validates the final macOS and Windows live reports;
2. builds the three-platform D-05 aggregate twice with different input order;
3. requires byte-identical storage outputs and validates the Rust receipt;
4. builds final acceptance twice and requires byte-identical outputs;
5. installs the three raw reports and two aggregates only after all checks pass.

Installed files:

```text
tests/codex/evidence/sprint-3-resource-graph-windows.json
tests/codex/evidence/platform/sprint-3-storage-spike-linux.json
tests/codex/evidence/platform/sprint-3-storage-spike-windows.json
tests/codex/evidence/sprint-3-storage-spike-cross-platform.json
tests/codex/evidence/sprint-3-acceptance.json
```

Review and commit them separately in this order:

1. Linux raw storage report;
2. Windows raw live report;
3. Windows raw storage report;
4. cross-platform storage aggregate;
5. final acceptance aggregate;
6. final update to `docs/codex-integration/SPRINT-3-EVIDENCE.md`.

If any source-scoped file or frozen validator changes before these runs, stop.
Create a new freeze and regenerate every Stage 5 platform report instead of
mixing coordinates.
