# D-05 storage spike

This locked Rust workspace compares bundled SQLite and the immutable segment candidate
without adding either physical backend to the production sidecar dependency graph.

From the repository root, run the full decision profile:

```bash
cargo run --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  run --backend all --dataset all --repo-root "$PWD" \
  --output tests/codex/evidence/sprint-3-storage-spike.json
```

Use `--quick` only while developing the harness. It reduces dataset/iteration counts,
skips the 100k/500k stress graph and packaging builds, records `profile=quick`, and
cannot qualify the local D-05 decision.

Each full raw report contains all timing samples, the exact backend configuration,
4 KiB write amplification relative to the normalized rename batch, the detailed
cancellation/hard-kill/corruption matrix, and a feature-specific dependency/license
manifest. The hard-kill cases pause a child at the selected durable boundary; the
parent terminates that process and proves writer-lock release plus idempotent reopen.

The full local report is the canonical D-05 evidence and selects a backend using the
correctness-first same-host scoring rule. Publication and remote CI are not required.

The optional `merge` command can later combine full reports produced locally on Linux,
macOS, and Windows. It accepts only clean source-identical decision profiles and does
not imply that such platform runs have already happened.

```bash
cargo run --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  merge tests/codex/evidence/sprint-3-storage-spike.json \
  tests/codex/evidence/platform/sprint-3-storage-spike-linux.json \
  tests/codex/evidence/platform/sprint-3-storage-spike-macos.json \
  tests/codex/evidence/platform/sprint-3-storage-spike-windows.json
```

`worker-fault` and `worker-open` are internal subprocess entry points for hard-kill and
process-lock tests. They are not production commands.
