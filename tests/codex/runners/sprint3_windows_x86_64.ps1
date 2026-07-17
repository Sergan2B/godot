#Requires -Version 5.1

param(
    [Parameter(Mandatory = $true)]
    [string]$Repository,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [switch]$PreflightOnly
)

# This post-freeze wrapper must remain outside the clean checkout passed as Repository.
Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Assert-NoReparseAncestor {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $Current = Get-Item -LiteralPath $Path
    while ($null -ne $Current) {
        if (($Current.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw "$Label must not traverse a symlink or junction: $($Current.FullName)"
        }
        $Current = $Current.Parent
    }
}

function Get-AvailableBytes {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $Root = [System.IO.Path]::GetPathRoot([System.IO.Path]::GetFullPath($Path))
    if ($Root -notmatch '^[A-Za-z]:\\$') {
        throw "qualifying runs require a local Windows drive: $Path"
    }
    return [Int64](Get-PSDrive -Name $Root.Substring(0, 1)).Free
}

function Assert-MinimumFreeSpace {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [Int64]$MinimumBytes,
        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    $AvailableBytes = Get-AvailableBytes -Path $Path
    if ($AvailableBytes -lt $MinimumBytes) {
        throw "$Label requires at least $MinimumBytes free bytes; found $AvailableBytes"
    }
    return $AvailableBytes
}

$FreezeCommit = "1144694d2293af2ff80d72a47e622479e51ee6f6"
$ExpectedSourceSha256 = "sha256:46ef87410e472814f20cac0ff6d32a6b4634a90a28c5d378753a04006bc7dd2f"
$ExpectedOracleSha256 = "sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"
$RunnerPath = (Resolve-Path -LiteralPath $PSCommandPath).Path
$RunnerItem = Get-Item -LiteralPath $RunnerPath
if (($RunnerItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "runner must be a regular non-symlink file"
}
$ManifestHelper = Join-Path $PSScriptRoot "sprint3_transfer_manifest.py"
if (-not (Test-Path -LiteralPath $ManifestHelper -PathType Leaf)) {
    throw "transfer manifest helper is missing beside the runner"
}
$ManifestItem = Get-Item -LiteralPath $ManifestHelper
if (($ManifestItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    throw "transfer manifest helper must not be a symlink"
}

$Repository = (Resolve-Path -LiteralPath $Repository).Path
if ($Repository -match '(?i)(^|\\)[^\\]*~[0-9]+(?=\\|$)') {
    throw "Repository must not use an 8.3 short path alias"
}
Assert-NoReparseAncestor -Path $Repository -Label "Repository"
if (-not (Split-Path -Path $OutputDirectory -IsAbsolute)) {
    throw "OutputDirectory must be an absolute staging path outside the repository"
}
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
$OutputRoot = [System.IO.Path]::GetPathRoot($OutputDirectory)
if ($OutputRoot -notmatch '^[A-Za-z]:\\$') {
    throw "OutputDirectory must be on a local Windows drive"
}
if ($OutputDirectory -match '(?i)(^|\\)[^\\]*~[0-9]+(?=\\|$)') {
    throw "OutputDirectory must not use an 8.3 short path alias"
}

if ($env:OS -ne "Windows_NT") {
    throw "Windows host required"
}
if (-not [System.Environment]::Is64BitOperatingSystem -or
    -not [System.Environment]::Is64BitProcess) {
    throw "native Windows X64 host and X64 PowerShell process required"
}
$ProcessorArchitectures = @(
    Get-CimInstance -ClassName Win32_Processor -ErrorAction Stop |
        Select-Object -ExpandProperty Architecture
)
if ($ProcessorArchitectures.Count -eq 0 -or
    @($ProcessorArchitectures | Where-Object { $_ -ne 9 }).Count -ne 0) {
    throw "native Windows X64 hardware or a full X64 VM is required"
}

$CompatibilityEnvironmentMarkers = @(
    "WINELOADERNOEXEC",
    "WINEPREFIX",
    "WINEDLLPATH",
    "WSL_DISTRO_NAME",
    "WSL_INTEROP"
)
$DetectedCompatibilityMarkers = @(
    $CompatibilityEnvironmentMarkers | Where-Object { Test-Path -LiteralPath "Env:$_" }
)
$DetectedWineRegistry = @(
    @(
        "HKCU:\Software\Wine",
        "HKLM:\Software\Wine"
    ) | Where-Object { Test-Path -LiteralPath $_ }
)
if ($DetectedCompatibilityMarkers.Count -ne 0 -or $DetectedWineRegistry.Count -ne 0) {
    throw "qualifying evidence requires native Windows or a full VM, not Wine or WSL interop"
}

$ContainerEnvironmentMarkers = @(
    "CONTAINER_SANDBOX_MOUNT_POINT",
    "DOTNET_RUNNING_IN_CONTAINER"
)
$DetectedContainerMarkers = @(
    $ContainerEnvironmentMarkers | Where-Object { Test-Path -LiteralPath "Env:$_" }
)
$ControlRegistry = Get-ItemProperty `
    -LiteralPath "HKLM:\SYSTEM\CurrentControlSet\Control" `
    -ErrorAction Stop
$ContainerTypeProperty = $ControlRegistry.PSObject.Properties["ContainerType"]
$ContainerType = if ($null -eq $ContainerTypeProperty) { $null } else { $ContainerTypeProperty.Value }
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

foreach ($RequiredCommand in @("git", "python", "cargo", "rustc")) {
    if ($null -eq (Get-Command $RequiredCommand -ErrorAction SilentlyContinue)) {
        throw "required command is unavailable: $RequiredCommand"
    }
}
python -c "import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 'Python 3.9 or newer is required')"
if ($LASTEXITCODE -ne 0) { throw "Python 3.9 or newer is required" }
python -c "import platform,struct,sys; ok=sys.platform=='win32' and struct.calcsize('P')==8 and platform.machine().lower() in {'amd64','x86_64'}; raise SystemExit(0 if ok else 'native Windows X64 Python is required')"
if ($LASTEXITCODE -ne 0) { throw "native Windows X64 Python is required" }
$SconsVersion = python -c "import re,sys,SCons; p=tuple(map(int,re.match(r'^(\d+)\.(\d+)\.(\d+)',SCons.__version__).groups())); sys.exit('SCons 4.10.1 or newer is required') if p < (4,10,1) else print(SCons.__version__)"
if ($LASTEXITCODE -ne 0) { throw "SCons 4.10.1 or newer is required" }

$VsWhere = Join-Path ${env:ProgramFiles(x86)} "Microsoft Visual Studio\Installer\vswhere.exe"
if (-not (Test-Path -LiteralPath $VsWhere -PathType Leaf)) {
    throw "Visual Studio Installer vswhere.exe is required"
}
$VisualStudioInstallations = @(
    & $VsWhere -latest -products "*" -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
)
if ($LASTEXITCODE -ne 0 -or $VisualStudioInstallations.Count -ne 1) {
    throw "Visual Studio with MSVC X64 build tools is required"
}
$VisualStudio = $VisualStudioInstallations[0].Trim()
$MsvcToolsets = @(
    Get-ChildItem -LiteralPath (Join-Path $VisualStudio "VC\Tools\MSVC") -Directory |
        Sort-Object { [Version]$_.Name } -Descending
)
if ($MsvcToolsets.Count -eq 0) {
    throw "MSVC toolset directory is missing"
}
$MsvcBin = Join-Path $MsvcToolsets[0].FullName "bin\Hostx64\x64"
foreach ($MsvcBinary in @("cl.exe", "link.exe")) {
    if (-not (Test-Path -LiteralPath (Join-Path $MsvcBin $MsvcBinary) -PathType Leaf)) {
        throw "MSVC X64 binary is missing: $MsvcBinary"
    }
}

$KitsRoot10 = $null
foreach ($RegistryPath in @(
        "HKLM:\SOFTWARE\Microsoft\Windows Kits\Installed Roots",
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\Windows Kits\Installed Roots"
    )) {
    $Candidate = Get-ItemPropertyValue -LiteralPath $RegistryPath -Name "KitsRoot10" -ErrorAction SilentlyContinue
    if ($null -ne $Candidate) {
        $KitsRoot10 = $Candidate
        break
    }
}
if ($null -eq $KitsRoot10) {
    throw "Windows SDK KitsRoot10 is unavailable"
}
$SdkVersions = @(
    Get-ChildItem -LiteralPath (Join-Path $KitsRoot10 "Include") -Directory |
        Where-Object {
            (Test-Path -LiteralPath (Join-Path $_.FullName "shared")) -and
            (Test-Path -LiteralPath (Join-Path $_.FullName "ucrt")) -and
            (Test-Path -LiteralPath (Join-Path $_.FullName "um"))
        } |
        Sort-Object { [Version]$_.Name } -Descending
)
if ($SdkVersions.Count -eq 0) {
    throw "complete Windows SDK headers are unavailable"
}
$SdkVersion = $SdkVersions[0].Name
$ResourceCompiler = Join-Path $KitsRoot10 "bin\$SdkVersion\x64\rc.exe"
if (-not (Test-Path -LiteralPath $ResourceCompiler -PathType Leaf)) {
    throw "Windows SDK X64 resource compiler is unavailable"
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
Assert-NoReparseAncestor -Path $OutputDirectory -Label "OutputDirectory"
$LiveOutput = Join-Path $OutputDirectory "sprint-3-resource-graph-windows.json"
$StorageOutput = Join-Path $OutputDirectory "sprint-3-storage-spike-windows.json"
$ReceiptOutput = Join-Path $OutputDirectory "sprint-3-windows.receipt.json"
foreach ($Destination in @($LiveOutput, $StorageOutput, $ReceiptOutput)) {
    if (Test-Path -LiteralPath $Destination) {
        throw "refusing to reuse existing output: $Destination"
    }
}
$WriteProbe = Join-Path $OutputDirectory ".sprint3-windows-preflight-$([Guid]::NewGuid().ToString('N'))"
try {
    New-Item -ItemType Directory -Path $WriteProbe | Out-Null
}
finally {
    Remove-Item -LiteralPath $WriteProbe -Recurse -Force -ErrorAction SilentlyContinue
}

$MinimumRepositoryBytes = [Int64]20 * 1024 * 1024 * 1024
$MinimumTempBytes = [Int64]10 * 1024 * 1024 * 1024
$MinimumOutputBytes = [Int64]1 * 1024 * 1024 * 1024
$RepositoryAvailable = Assert-MinimumFreeSpace `
    -Path $Repository -MinimumBytes $MinimumRepositoryBytes -Label "repository volume"
$TempAvailable = Assert-MinimumFreeSpace `
    -Path ([System.IO.Path]::GetTempPath()) -MinimumBytes $MinimumTempBytes -Label "temporary volume"
$OutputAvailable = Assert-MinimumFreeSpace `
    -Path $OutputDirectory -MinimumBytes $MinimumOutputBytes -Label "output volume"
Write-Host "Windows Sprint 3 preflight passed (SCons=$SconsVersion; MSVC=$($MsvcToolsets[0].Name); SDK=$SdkVersion; repository_free=$RepositoryAvailable; temp_free=$TempAvailable; output_free=$OutputAvailable)"
if ($PreflightOnly) {
    return
}

$WorkDirectory = Join-Path $OutputDirectory ".sprint3-windows-run-$([Guid]::NewGuid().ToString('N'))"
$WorkLiveOutput = Join-Path $WorkDirectory "sprint-3-resource-graph-windows.json"
$WorkStorageOutput = Join-Path $WorkDirectory "sprint-3-storage-spike-windows.json"
$WorkReceipt = Join-Path $WorkDirectory "sprint-3-windows.receipt.json"
New-Item -ItemType Directory -Path $WorkDirectory | Out-Null
$RunCompleted = $false

try {
    python -m SCons platform=windows target=editor dev_build=yes tests=yes module_codex_bridge_enabled=yes accesskit=no d3d12=no angle=no -j8
    if ($LASTEXITCODE -ne 0) { throw "Godot Windows build failed" }

    cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp\Cargo.toml -p godot-codex-mcp
    if ($LASTEXITCODE -ne 0) { throw "sidecar Windows build failed" }

    python tests\codex\sprint3_stage4_index_mcp.py `
        --godot bin\godot.windows.editor.dev.x86_64.console.exe `
        --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
        --evidence $WorkLiveOutput
    if ($LASTEXITCODE -ne 0) { throw "Windows live resource gate failed" }

    python tests\codex\sprint3_acceptance.py validate-live $WorkLiveOutput
    if ($LASTEXITCODE -ne 0) { throw "strict Windows live evidence validation failed" }

    cargo +1.94.1 run --locked --release --manifest-path tests\codex\storage_spike\Cargo.toml -- `
        run --backend all --dataset all --repo-root $Repository --output $WorkStorageOutput
    if ($LASTEXITCODE -ne 0) { throw "Windows storage profile failed" }

    $Live = Get-Content -Raw -LiteralPath $WorkLiveOutput | ConvertFrom-Json
    if ($Live.status -ne "passed" -or
        $Live.platform -ne "windows-x86_64" -or
        $Live.git_commit -ne $FreezeCommit -or
        $Live.git_dirty -or
        $Live.source_tree_sha256 -ne $ExpectedSourceSha256 -or
        $Live.oracle_sha256 -ne $ExpectedOracleSha256 -or
        -not $Live.slo.all_passed) {
        throw "Windows live evidence coordinates or SLOs differ"
    }

    $Storage = Get-Content -Raw -LiteralPath $WorkStorageOutput | ConvertFrom-Json
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

    python $ManifestHelper create `
        --platform windows-x86_64 `
        --runner $RunnerPath `
        --receipt $WorkReceipt `
        --file $WorkLiveOutput `
        --file $WorkStorageOutput
    if ($LASTEXITCODE -ne 0) { throw "unable to create Windows transfer receipt" }
    python $ManifestHelper verify `
        --platform windows-x86_64 `
        --runner $RunnerPath `
        --receipt $WorkReceipt `
        --file $WorkLiveOutput `
        --file $WorkStorageOutput
    if ($LASTEXITCODE -ne 0) { throw "Windows transfer receipt verification failed" }

    Move-Item -LiteralPath $WorkLiveOutput -Destination $LiveOutput
    Move-Item -LiteralPath $WorkStorageOutput -Destination $StorageOutput
    Move-Item -LiteralPath $WorkReceipt -Destination $ReceiptOutput
    python $ManifestHelper verify `
        --platform windows-x86_64 `
        --runner $RunnerPath `
        --receipt $ReceiptOutput `
        --file $LiveOutput `
        --file $StorageOutput
    if ($LASTEXITCODE -ne 0) { throw "installed Windows transfer receipt verification failed" }
    $RunCompleted = $true
}
finally {
    Remove-Item -LiteralPath $WorkDirectory -Recurse -Force -ErrorAction SilentlyContinue
    if (-not $RunCompleted) {
        Remove-Item -LiteralPath $LiveOutput -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $StorageOutput -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $ReceiptOutput -Force -ErrorAction SilentlyContinue
    }
}

Write-Host "Windows Sprint 3 evidence passed: $LiveOutput; $StorageOutput; receipt: $ReceiptOutput"
