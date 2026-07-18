# Sprint 3 host and aggregation runbook

**Status:** completed and retained for deterministic reproduction.

These wrappers reproduce the completed host matrix from
[Sprint 3 Stage 5](../../../docs/codex-integration/SPRINT-3-STAGE-5-PLAN.md).
They are post-freeze orchestration files and are intentionally outside
`tests/codex/sprint3_source_scopes.txt`. The evidence producers and canonical
validators remain the frozen files in the clean source checkout.

Do not use Docker, Wine, WSL, synthetic platform rewrites, or hosted CI as a
replacement for the required Windows host. `remote_ci` stays `not_run`. Linux is
not a Sprint 3 acceptance coordinate.

The legacy Linux runner and its manifest entries remain byte-stable only to preserve
the digest of the already-issued transfer helper. The aggregate runner does not read
Linux output or a Linux receipt.

## Pinned handoff revision

The qualifying Windows producer and transfer helper are frozen at runner commit
`0961c5f7fe1bd36e8d62b4966f39c4dbf174b149`. The Windows receipt binds their exact
bytes; aggregation rejects reports produced by a different producer set. The
two-platform aggregation policy is taken from the current `codex/integration` head.

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

On macOS, prepare the worktree with:

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

Copy these three files byte-for-byte into one staging directory outside the
aggregation checkout. Do not open and resave them in an editor:

```text
sprint-3-resource-graph-windows.json
sprint-3-storage-spike-windows.json
sprint-3-windows.receipt.json
```

The tracked macOS reports were regenerated at
`a90ddd06c81a6210f44552b46ff533248b93ed90` and their exact hashes are pinned
in `sprint3_aggregate.sh`. Do not modify or resave them. Never mix reports from
another freeze with the pinned macOS inputs.
The receipt is a transfer-integrity/completion control. It binds exact
report bytes, sizes, freeze/source/oracle coordinates, runner bytes, and helper
bytes. It is consumed by aggregation but is not Sprint evidence and is not
installed or committed. It does not substitute for the real-host guards or
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

1. snapshots every input and verifies the Windows transfer receipt plus the exact
   approved macOS artifact hashes;
2. validates the final macOS and Windows live reports;
3. builds the macOS/Windows D-05 aggregate twice with different input order;
4. requires byte-identical storage outputs and validates the Rust receipt;
5. builds final acceptance twice and requires byte-identical outputs;
6. installs the two Windows raw reports and two aggregates only after all checks pass,
   rolling back the whole set on any installation or post-install failure.

Installed files:

```text
tests/codex/evidence/sprint-3-resource-graph-windows.json
tests/codex/evidence/platform/sprint-3-storage-spike-windows.json
tests/codex/evidence/sprint-3-storage-spike-cross-platform.json
tests/codex/evidence/sprint-3-acceptance.json
```

Review and commit them separately in this order:

1. Windows raw live report;
2. Windows raw storage report;
3. cross-platform storage aggregate;
4. final acceptance aggregate;
5. final update to `docs/codex-integration/SPRINT-3-EVIDENCE.md`.

If any product or evidence-producer source changes before these runs, stop. Create a
new freeze and regenerate every Stage 5 platform report instead of mixing coordinates.
The reviewed schema-3 two-platform aggregation policy is the sole post-freeze exception.

Run qualifying gates on an otherwise idle host. If a runner fails, preserve its
console log, correct the host/toolchain issue, and retry only after confirming
that all exact output and receipt names are absent. Aggregation likewise refuses
to overwrite any of the four final artifacts; use a fresh clean checkout for a
retry instead of deleting reviewed evidence casually.
