$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..\..")).Path
Set-Location $RepoRoot

function Assert-NativeSuccess {
  param(
    [Parameter(Mandatory = $true)][string]$Step,
    [Parameter(Mandatory = $true)][int]$ExitCode
  )

  if ($ExitCode -ne 0) {
    throw "$Step failed with exit code $ExitCode."
  }
}

$EvidenceRelativePath = "tests\codex\evidence\sprint-5-script-semantics-windows.json"
$EvidencePath = Join-Path $RepoRoot $EvidenceRelativePath
if (Test-Path -LiteralPath $EvidencePath) {
  throw "Refusing to overwrite existing evidence: $EvidencePath"
}

$Succeeded = $false
try {
  python -m SCons platform=windows target=editor dev_build=yes tests=yes `
    module_codex_bridge_enabled=yes accesskit=no d3d12=no angle=no -j8
  Assert-NativeSuccess "Godot Windows build" $LASTEXITCODE

  cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp\Cargo.toml `
    -p godot-codex-mcp
  Assert-NativeSuccess "Sprint 5 sidecar build" $LASTEXITCODE

  cargo +1.94.1 build --locked --release --manifest-path godot-codex-mcp\Cargo.toml `
    -p godot-codex-bridge-client --example script_graph_live
  Assert-NativeSuccess "Sprint 5 script probe build" $LASTEXITCODE

  python tests\codex\sprint5_script_semantics_live.py `
    --godot bin\godot.windows.editor.dev.x86_64.console.exe `
    --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
    --script-probe godot-codex-mcp\target\release\examples\script_graph_live.exe `
    --evidence $EvidenceRelativePath `
    --timeout 180
  Assert-NativeSuccess "Sprint 5 Windows script semantics gate" $LASTEXITCODE

  python tests\codex\sprint5_acceptance.py validate-platform `
    $EvidenceRelativePath `
    --platform windows-x86_64
  Assert-NativeSuccess "Sprint 5 Windows evidence validation" $LASTEXITCODE
  $Succeeded = $true
}
finally {
  if (-not $Succeeded -and (Test-Path -LiteralPath $EvidencePath)) {
    Remove-Item -LiteralPath $EvidencePath -Force
  }
}
