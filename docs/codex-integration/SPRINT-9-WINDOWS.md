# Sprint 9 native Windows support

Sprint 9 supports a native `windows-x86_64` editor and sidecar coordinate. The
Windows path uses the loopback-only TCP Bridge transport, a native MSVC Godot
build, and the `x86_64-pc-windows-msvc` Rust target. It does not require WSL.

## Qualification command

Run from a Visual Studio developer-capable PowerShell:

```powershell
python tests\codex\sprint9_acceptance.py --timeout 90
```

The wrapper selects these artifacts on Windows:

- `bin\godot.windows.editor.dev.x86_64.console.exe`
- `godot-codex-mcp\target\release\godot-codex-mcp.exe`
- `tests\codex\evidence\sprint-9-editor-transactions-windows.json`

It pins Cargo, Rustfmt, and Clippy to the channel in
`godot-codex-mcp\rust-toolchain.toml`, builds Godot with MSVC and the Windows
SDK, then runs the same fixture, conformance, C++, direct Bridge, full
model-free MCP, Sprint 7, and Sprint 8 headless/GUI gates used by the macOS
coordinate.

The command is intentionally fail-closed: qualification requires a clean
worktree, refuses to overwrite evidence, binds results to the source commit and
artifact hashes, and emits the Windows evidence atomically. Revalidate a
committed evidence-only child with:

```powershell
python tests\codex\sprint9_acceptance.py `
  --validate tests\codex\evidence\sprint-9-editor-transactions-windows.json
```

## Windows-specific behavior covered

- Bridge discovery accepts only canonical `127.0.0.1:<port>` endpoints and
  retains the per-project token handshake.
- Project IDs normalize Win32 and extended-length path spellings before
  hashing.
- Transaction journal durability does not attempt the Unix-only operation of
  opening a directory as a file.
- Live runners use Windows temporary directories, tolerate atomic file-replace
  sharing races, and terminate editor/game process trees with `taskkill`.
- Runtime viewport capture requests a game frame even when the game runs in a
  floating window rather than embedded in the editor.

## Current evidence status

The native Windows build and the complete S7-S9 live matrix have passed on a
Windows x86_64 working tree, including GUI viewport capture, crash/hang
recovery, all eight transaction families, seven fault scenarios, targeted and
native Undo/Redo, cleanup, and redaction checks. A canonical evidence JSON must
still be generated from the final clean source commit; ad-hoc working-tree
results are not a substitute for that source-bound artifact.
