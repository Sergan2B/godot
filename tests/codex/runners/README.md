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

## Pinned handoff revision

The qualifying wrappers and current macOS aggregation pins are frozen at runner
commit `0961c5f7fe1bd36e8d62b4966f39c4dbf174b149`. Linux and Windows receipts bind
the exact runner and `sprint3_transfer_manifest.py` bytes; aggregation rejects
reports produced by a different wrapper set.

Before using another device, the pinned commit must be reachable there. After
it is published, fetch `codex/integration`; otherwise transfer the repository by
another Git-safe mechanism. Do not start a host run if either `cat-file` check
below fails.

Bootstrap after the branch is available:

```sh
git clone --filter=blob:none --branch codex/integration \
  https://github.com/Sergan2B/godot.git GodotSTG
CONTROL=/absolute/path/GodotSTG
git -C "$CONTROL" fetch origin codex/integration
git -C "$CONTROL" cat-file -e \
  0961c5f7fe1bd36e8d62b4966f39c4dbf174b149^{commit}
git -C "$CONTROL" cat-file -e \
  a90ddd06c81a6210f44552b46ff533248b93ed90^{commit}
git -C "$CONTROL" merge-base --is-ancestor \
  0961c5f7fe1bd36e8d62b4966f39c4dbf174b149 HEAD
if command -v shasum >/dev/null 2>&1; then
  (cd "$CONTROL/tests/codex/runners" && shasum -a 256 -c MANIFEST.sha256)
else
  (cd "$CONTROL/tests/codex/runners" && sha256sum -c MANIFEST.sha256)
fi
```

On Windows, verify the same manifest with PowerShell:

```powershell
$Control = "C:\absolute\path\GodotSTG"
$RunnerDirectory = Join-Path $Control "tests\codex\runners"
Get-Content (Join-Path $RunnerDirectory "MANIFEST.sha256") | ForEach-Object {
    $Expected, $Name = $_ -split "  ", 2
    $Actual = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $RunnerDirectory $Name)).Hash.ToLowerInvariant()
    if ($Actual -ne $Expected) { throw "runner checksum mismatch: $Name" }
}
```

Do not use a shallow clone: the freeze ancestry check is part of acceptance.

## Frozen coordinates

Every qualifying report must contain these exact values:

| Coordinate | Required value |
|---|---|
| Git commit | `a90ddd06c81a6210f44552b46ff533248b93ed90` |
| Scoped source SHA-256 | `sha256:73e99eec9897f8e2b4c6c210a3c56bd57a91ce347aedba030323eb01bf8438eb` |
| Oracle SHA-256 | `sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b` |

## Use two checkouts

The runners did not exist at the source-freeze commit. Keep a control checkout
on the descendant branch containing this directory and create a separate clean
worktree at the exact freeze:

```text
GodotSTG/                 control checkout with tests/codex/runners
GodotSTG-sprint3-freeze/ clean detached worktree at a90ddd0...
sprint3-raw/              output directory outside both checkouts
```

On Linux or macOS, prepare the worktree with:

```sh
CONTROL=/absolute/path/GodotSTG
FREEZE=/absolute/path/GodotSTG-sprint3-freeze
git -C "$CONTROL" cat-file -e a90ddd06c81a6210f44552b46ff533248b93ed90^{commit}
git -C "$CONTROL" worktree add --detach "$FREEZE" \
  a90ddd06c81a6210f44552b46ff533248b93ed90
test -z "$(git -C "$FREEZE" status --porcelain)"
```

PowerShell equivalent:

```powershell
$Control = "C:\absolute\path\GodotSTG"
$Freeze = "C:\absolute\path\GodotSTG-sprint3-freeze"
git -C $Control cat-file -e "a90ddd06c81a6210f44552b46ff533248b93ed90^{commit}"
if ($LASTEXITCODE -ne 0) { throw "source freeze is unavailable" }
git -C $Control worktree add --detach $Freeze a90ddd06c81a6210f44552b46ff533248b93ed90
if ($LASTEXITCODE -ne 0) { throw "freeze worktree creation failed" }
if ((git -C $Freeze status --porcelain) -join "") { throw "freeze worktree is dirty" }
```

Do not copy the runners into the freeze worktree: an untracked wrapper there
would correctly trip the clean-checkout guard.

## Linux x86_64 storage run

Requirements:

- a real Linux x86_64 host or full Linux VM, not a container;
- native x86_64 Python 3.9 or newer;
- Rustup with toolchain `1.94.1-x86_64-unknown-linux-gnu`;
- a native C linker;
- at least 10 GiB free on the repository and system-temp volumes and 1 GiB on
  the output volume.

Run the cheap fail-closed preflight first. It checks the host, VM/container/WSL
state, CI markers, exact freeze and digests, toolchain, writable output, and
free space without compiling or writing a report:

```sh
CONTROL=/absolute/path/GodotSTG
FREEZE=/absolute/path/GodotSTG-sprint3-freeze
RAW=/absolute/path/sprint3-linux-raw
bash "$CONTROL/tests/codex/runners/sprint3_linux_x86_64.sh" \
  --preflight-only \
  "$FREEZE" \
  "$RAW/sprint-3-storage-spike-linux.json"
```

Then run the same command without `--preflight-only`:

```sh
CONTROL=/absolute/path/GodotSTG
FREEZE=/absolute/path/GodotSTG-sprint3-freeze
RAW=/absolute/path/sprint3-linux-raw
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
sprint-3-linux.receipt.json
```

The report and receipt appear only after the full profile and final receipt
verification succeed. Existing files with either name are never overwritten;
a failed run removes its partial outputs.

## Windows x86_64 live and storage run

Requirements:

- a real Windows x86_64 host or full Windows VM, not Wine, WSL, or a container;
- Windows PowerShell 5.1 or PowerShell 7;
- native x64 Python 3.9 or newer and SCons 4.10.1 or newer;
- Rustup toolchain `1.94.1-x86_64-pc-windows-msvc`;
- Visual Studio/MSVC x64 build tools, `vswhere`, and a complete Windows SDK;
- at least 20 GiB free on the repository volume, 10 GiB on the system-temp
  volume, and 1 GiB on the output volume.

The runner rejects 32-bit processes, ARM64 hosts using x64 emulation, Wine,
WSL interop, containers, CI, UNC output paths, 8.3 aliases, and paths traversing
junctions or symlinks.

Run preflight before the long build:

```powershell
$Control = "C:\absolute\path\GodotSTG"
$Freeze = "C:\absolute\path\GodotSTG-sprint3-freeze"
$Raw = "C:\absolute\path\sprint3-windows-raw"
& "$Control\tests\codex\runners\sprint3_windows_x86_64.ps1" `
    -Repository $Freeze `
    -OutputDirectory $Raw `
    -PreflightOnly
```

Then run without `-PreflightOnly`:

```powershell
$Control = "C:\absolute\path\GodotSTG"
$Freeze = "C:\absolute\path\GodotSTG-sprint3-freeze"
$Raw = "C:\absolute\path\sprint3-windows-raw"
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
sprint-3-windows.receipt.json
```

All three files are published only after both reports and their receipt pass.
Existing exact outputs are never overwritten, and failure removes partial
files.

## Return the raw reports

Copy these five files byte-for-byte into one staging directory outside the
aggregation checkout. Do not open and resave them in an editor:

```text
sprint-3-resource-graph-windows.json
sprint-3-storage-spike-linux.json
sprint-3-storage-spike-windows.json
sprint-3-linux.receipt.json
sprint-3-windows.receipt.json
```

The tracked macOS reports were regenerated at
`a90ddd06c81a6210f44552b46ff533248b93ed90` and their exact hashes are pinned
in `sprint3_aggregate.sh`. Do not modify or resave them. Never mix reports from
another freeze with the pinned macOS inputs.
The two receipts are transfer-integrity/completion controls. They bind exact
report bytes, sizes, freeze/source/oracle coordinates, runner bytes, and helper
bytes. They are consumed by aggregation but are not Sprint evidence and are not
installed or committed. They do not substitute for the real-host guards or
qualifying host execution.

## Deterministic aggregation

On the macOS control checkout, first ensure the worktree is clean and the freeze
is its ancestor. The host needs Bash 3.2+, Python 3.9+, Git, rustup toolchain
`1.94.1`, and the standard macOS `install`, `cmp`, and `shasum` utilities. Then
run:

```sh
CONTROL=/absolute/path/GodotSTG
RAW=/absolute/path/sprint3-raw
test -z "$(git -C "$CONTROL" status --porcelain)"
/bin/bash "$CONTROL/tests/codex/runners/sprint3_aggregate.sh" "$CONTROL" "$RAW"
```

The aggregation wrapper:

1. snapshots every input and verifies both transfer receipts plus the exact
   approved macOS artifact hashes;
2. validates the final macOS and Windows live reports;
3. builds the three-platform D-05 aggregate twice with different input order;
4. requires byte-identical storage outputs and validates the Rust receipt;
5. builds final acceptance twice and requires byte-identical outputs;
6. installs the three raw reports and two aggregates only after all checks pass,
   rolling back the whole set on any installation or post-install failure.

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

Run qualifying gates on an otherwise idle host. If a runner fails, preserve its
console log, correct the host/toolchain issue, and retry only after confirming
that all exact output and receipt names are absent. Aggregation likewise refuses
to overwrite any of the five final artifacts; use a fresh clean checkout for a
retry instead of deleting reviewed evidence casually.
