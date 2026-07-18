$ErrorActionPreference = "Stop"

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

$EvidenceRelativePath = "tests\codex\evidence\sprint-4-scene-graph-windows.json"
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
  Assert-NativeSuccess "Sprint 4 sidecar build" $LASTEXITCODE

  python tests\codex\sprint4_scene_graph_live.py `
    --godot bin\godot.windows.editor.dev.x86_64.console.exe `
    --sidecar godot-codex-mcp\target\release\godot-codex-mcp.exe `
    --evidence $EvidenceRelativePath
  Assert-NativeSuccess "Sprint 4 Windows scene graph gate" $LASTEXITCODE

  python tests\codex\sprint4_acceptance.py validate-platform `
    $EvidenceRelativePath `
    --platform windows-x86_64
  Assert-NativeSuccess "Sprint 4 Windows evidence validation" $LASTEXITCODE
  $Succeeded = $true
}
finally {
  if (-not $Succeeded -and (Test-Path -LiteralPath $EvidencePath)) {
    Remove-Item -LiteralPath $EvidencePath -Force
  }
}
