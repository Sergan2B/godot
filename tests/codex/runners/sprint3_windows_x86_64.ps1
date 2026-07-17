#Requires -Version 5.1

param(
    [Parameter(Mandatory = $true)]
    [string]$Repository,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory
)

# This post-freeze wrapper must remain outside the clean checkout passed as Repository.
Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

$FreezeCommit = "75364c2cc5fe50de5a508315c41cc43200b90024"
$ExpectedSourceSha256 = "sha256:0540dc092e2a5d6c94ac8e23d84bd2bc7224ddfb2c78456f09443d14e80950d2"
$ExpectedOracleSha256 = "sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"
$Repository = (Resolve-Path -LiteralPath $Repository).Path
if (-not (Split-Path -Path $OutputDirectory -IsAbsolute)) {
    throw "OutputDirectory must be an absolute staging path outside the repository"
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)

if ($env:OS -ne "Windows_NT") {
    throw "Windows host required"
}
if ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString() -ne "X64") {
    throw "native Windows X64 host required"
}

$ContainerEnvironmentMarkers = @(
    "CONTAINER_SANDBOX_MOUNT_POINT",
    "DOTNET_RUNNING_IN_CONTAINER"
)
$DetectedContainerMarkers = @(
    $ContainerEnvironmentMarkers | Where-Object { Test-Path -LiteralPath "Env:$_" }
)
$ContainerType = Get-ItemPropertyValue `
    -LiteralPath "HKLM:\SYSTEM\CurrentControlSet\Control" `
    -Name "ContainerType" `
    -ErrorAction SilentlyContinue
if ($DetectedContainerMarkers.Count -ne 0 -or $null -ne $ContainerType) {
    throw "qualifying evidence requires a real Windows host, not a container"
}

$RepositoryPrefix = $Repository.TrimEnd([System.IO.Path]::DirectorySeparatorChar) + [System.IO.Path]::DirectorySeparatorChar
if ($OutputDirectory.Equals($Repository, [System.StringComparison]::OrdinalIgnoreCase) -or
    $OutputDirectory.StartsWith($RepositoryPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "OutputDirectory must be outside the repository"
}

$CiMarkers = @(
    "APPVEYOR",
    "BITBUCKET_BUILD_NUMBER",
    "BUILDKITE",
    "CI",
    "CIRCLECI",
    "CODEBUILD_BUILD_ID",
    "CONTINUOUS_INTEGRATION",
    "DRONE",
    "GITEA_ACTIONS",
    "GITHUB_ACTIONS",
    "GITLAB_CI",
    "JENKINS_URL",
    "TEAMCITY_VERSION",
    "TF_BUILD",
    "TRAVIS",
    "WOODPECKER"
)
$DetectedCiMarkers = @($CiMarkers | Where-Object { Test-Path -LiteralPath "Env:$_" })
if ($DetectedCiMarkers.Count -ne 0) {
    throw "qualifying evidence requires a local host; detected CI markers: $($DetectedCiMarkers -join ', ')"
}

Set-Location -LiteralPath $Repository
$HeadCommit = (git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { throw "unable to read source revision" }
if ($HeadCommit -ne $FreezeCommit) {
    throw "wrong source freeze"
}
$Status = (git status --porcelain) -join ""
if ($LASTEXITCODE -ne 0) { throw "unable to inspect checkout state" }
if ($Status) {
    throw "checkout is dirty"
}
$RustVersion = rustc +1.94.1 -vV
if ($LASTEXITCODE -ne 0) { throw "unable to inspect Rust toolchain" }
if (-not ($RustVersion -contains "host: x86_64-pc-windows-msvc")) {
    throw "Rust 1.94.1 MSVC X64 host toolchain required"
}

$SourceJson = python -c "import json,sys; sys.path.insert(0, 'tests/codex'); from sprint3_stage4_index_mcp import source_coordinates; print(json.dumps(source_coordinates(), sort_keys=True))"
if ($LASTEXITCODE -ne 0) { throw "unable to calculate source coordinates" }
$Source = $SourceJson | ConvertFrom-Json
if ($Source.git_dirty -or $Source.source_tree_sha256 -ne $ExpectedSourceSha256) {
    throw "source scope differs from the frozen digest"
}

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$LiveOutput = Join-Path $OutputDirectory "sprint-3-resource-graph-windows.json"
$StorageOutput = Join-Path $OutputDirectory "sprint-3-storage-spike-windows.json"

python -m SCons platform=windows target=editor dev_build=yes tests=yes module_codex_bridge_enabled=yes accesskit=no d3d12=no angle=no -j8
if ($LASTEXITCODE -ne 0) { throw "Godot Windows build failed" }

cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp\Cargo.toml -p godot-codex-mcp
if ($LASTEXITCODE -ne 0) { throw "sidecar Windows build failed" }

python tests\codex\sprint3_stage4_index_mcp.py `
    --godot bin\godot.windows.editor.dev.x86_64.console.exe `
    --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
    --evidence $LiveOutput
if ($LASTEXITCODE -ne 0) { throw "Windows live resource gate failed" }

python tests\codex\sprint3_acceptance.py validate-live $LiveOutput
if ($LASTEXITCODE -ne 0) { throw "strict Windows live evidence validation failed" }

cargo +1.94.1 run --locked --release --manifest-path tests\codex\storage_spike\Cargo.toml -- `
    run --backend all --dataset all --repo-root $Repository --output $StorageOutput
if ($LASTEXITCODE -ne 0) { throw "Windows storage profile failed" }

$Live = Get-Content -Raw -LiteralPath $LiveOutput | ConvertFrom-Json
if ($Live.status -ne "passed" -or
    $Live.platform -ne "windows-x86_64" -or
    $Live.git_commit -ne $FreezeCommit -or
    $Live.git_dirty -or
    $Live.source_tree_sha256 -ne $ExpectedSourceSha256 -or
    $Live.oracle_sha256 -ne $ExpectedOracleSha256 -or
    -not $Live.slo.all_passed) {
    throw "Windows live evidence coordinates or SLOs differ"
}

$Storage = Get-Content -Raw -LiteralPath $StorageOutput | ConvertFrom-Json
if ($Storage.profile -ne "decision" -or
    $Storage.os -ne "windows" -or
    $Storage.architecture -ne "x86_64" -or
    $Storage.git_commit -ne $FreezeCommit -or
    $Storage.git_dirty -or
    $Storage.source_tree_sha256 -ne $ExpectedSourceSha256 -or
    $Storage.oracle_sha256 -ne $ExpectedOracleSha256 -or
    $Storage.backends.Count -ne 2) {
    throw "Windows storage evidence coordinates differ"
}
$SegmentCandidates = @($Storage.backends | Where-Object { $_.backend -eq "segment" })
if ($SegmentCandidates.Count -ne 1) {
    throw "Windows storage evidence must contain exactly one segment backend"
}
$Segment = $SegmentCandidates[0]
if (-not $Segment.qualified -or $Segment.errors.Count -ne 0) {
    throw "Windows segment backend did not qualify"
}
foreach ($Gate in $Segment.gates.PSObject.Properties) {
    if (-not $Gate.Value) { throw "Windows segment storage gate failed: $($Gate.Name)" }
}
foreach ($Fault in $Segment.fault_matrix.PSObject.Properties) {
    if (-not $Fault.Value) { throw "Windows segment storage fault case failed: $($Fault.Name)" }
}

Write-Host "Windows Sprint 3 evidence passed: $LiveOutput; $StorageOutput"
