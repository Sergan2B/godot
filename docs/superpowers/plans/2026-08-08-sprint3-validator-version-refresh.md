# Sprint 3 Validator Version Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make every supported Sprint 3 storage-spike entry point automatically reconcile the one local `godot-codex-index-store` lock version in an isolated temporary workspace, without mutating the checkout or weakening canonical Rust evidence validation.

**Architecture:** A new Python launcher copies the small storage-spike crate into a private temporary tree, rewrites only the exact local dependency path and lock version, and executes Cargo with `--locked --offline`. The acceptance validator and operator runners call this launcher; the Rust fixture receives the explicit current-model compatibility field it needs, while unrelated dependency or schema drift still fails closed.

**Tech Stack:** Python 3.14 standard library (`contextlib`, `copy`, `os`, `re`, `shutil`, `subprocess`, `sys`, `tempfile`, `tomllib`, `unittest`), Cargo/Rust 1.94.1, Bash.

## Global Constraints

- Do not modify any Sprint 5 source, manifest, evidence, or test.
- Never write generated lock changes back into the checkout.
- Refresh only the `godot-codex-index-store` package when it is an exact local path package with no `source` field.
- Keep Cargo execution reproducible and network-free with `--locked --offline`.
- Preserve canonical Rust raw-sample scoring and the exact validation receipt contract.
- Future Rust API/schema changes must remain explicit compilation failures; only package-version drift is automatic.
- Surface bounded Cargo diagnostics instead of reporting every child failure as a scoring mismatch.

---

### Task 1: Isolated lock-version refresh primitive

**Files:**
- Create: `tests/codex/sprint3_storage_spike.py`
- Create: `tests/codex/test_sprint3_storage_spike.py`

**Interfaces:**
- Consumes: a Cargo lock byte snapshot, exact path-package name, and current workspace version.
- Produces: `StorageSpikeLauncherError` and `rewrite_path_package_version(lock_bytes: bytes, package_name: str, version: str) -> bytes`.

- [ ] **Step 1: Write failing unit tests for exact lock refresh**

```python
def test_refreshes_only_exact_local_index_store_version(self) -> None:
    stale = b'''version = 4\n\n[[package]]\nname = "godot-codex-index-store"\nversion = "0.1.0"\ndependencies = ["serde"]\n'''
    refreshed = launcher.rewrite_path_package_version(
        stale, "godot-codex-index-store", "0.1.19"
    )
    self.assertIn(
        b'name = "godot-codex-index-store"\nversion = "0.1.19"', refreshed
    )
    self.assertEqual(refreshed.count(b"0.1.19"), 1)

def test_rejects_registry_backed_or_duplicate_index_store_entries(self) -> None:
    registry = b'''version = 4\n\n[[package]]\nname = "godot-codex-index-store"\nversion = "0.1.0"\nsource = "registry+https://example.invalid"\n'''
    with self.assertRaisesRegex(launcher.StorageSpikeLauncherError, "local path package"):
        launcher.rewrite_path_package_version(
            registry, "godot-codex-index-store", "0.1.19"
        )
    duplicate = registry.replace(b'source = "registry+https://example.invalid"\n', b"") * 2
    with self.assertRaisesRegex(launcher.StorageSpikeLauncherError, "exactly once"):
        launcher.rewrite_path_package_version(
            duplicate, "godot-codex-index-store", "0.1.19"
        )

def test_rejects_malformed_version_lock_and_unrelated_changes(self) -> None:
    local = b'''version = 4\n\n[[package]]\nname = "godot-codex-index-store"\nversion = "0.1.0"\n\n[[package]]\nname = "serde"\nversion = "1.0.228"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n'''
    with self.assertRaisesRegex(launcher.StorageSpikeLauncherError, "SemVer"):
        launcher.rewrite_path_package_version(
            local, "godot-codex-index-store", "next"
        )
    with self.assertRaisesRegex(launcher.StorageSpikeLauncherError, "lock is invalid"):
        launcher.rewrite_path_package_version(
            b"not valid TOML = [", "godot-codex-index-store", "0.1.19"
        )
    refreshed = tomllib.loads(
        launcher.rewrite_path_package_version(
            local, "godot-codex-index-store", "0.1.19"
        ).decode("utf-8")
    )
    serde = next(item for item in refreshed["package"] if item["name"] == "serde")
    self.assertEqual(serde["version"], "1.0.228")
    self.assertIn("source", serde)
```

- [ ] **Step 2: Run the new tests and verify RED**

```bash
PYTHONPATH=tests/codex python3 -m unittest tests/codex/test_sprint3_storage_spike.py
```

Expected: import failure because `sprint3_storage_spike` does not exist.

- [ ] **Step 3: Implement strict lock parsing and replacement**

```python
class StorageSpikeLauncherError(RuntimeError):
    """The isolated Sprint 3 storage-spike launcher could not be prepared."""


def rewrite_path_package_version(lock_bytes: bytes, package_name: str, version: str) -> bytes:
    if not re.fullmatch(
        r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)",
        version,
    ):
        raise StorageSpikeLauncherError("workspace package version is not bounded SemVer")
    try:
        text = lock_bytes.decode("utf-8")
        document = tomllib.loads(text)
    except (UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise StorageSpikeLauncherError("storage-spike lock is invalid") from error
    matches = [
        item for item in document.get("package", []) if item.get("name") == package_name
    ]
    if len(matches) != 1:
        raise StorageSpikeLauncherError("local path package must appear exactly once")
    if "source" in matches[0]:
        raise StorageSpikeLauncherError("eligible package must be a local path package")
    package_blocks = list(re.finditer(
        r"(?ms)^\[\[package\]\]\n.*?(?=^\[\[package\]\]\n|\Z)", text
    ))
    selected = [
        block for block in package_blocks
        if re.search(
            rf'(?m)^name = "{re.escape(package_name)}"$', block.group(0)
        )
    ]
    if len(selected) != 1:
        raise StorageSpikeLauncherError("local path package block must appear exactly once")
    block = selected[0]
    replacement, count = re.subn(
        r'(?m)^version = "[^"]+"$',
        f'version = "{version}"',
        block.group(0),
        count=1,
    )
    if count != 1:
        raise StorageSpikeLauncherError("local path package must have one version")
    refreshed = text[:block.start()] + replacement + text[block.end():]
    refreshed_document = tomllib.loads(refreshed)
    expected_document = copy.deepcopy(document)
    expected_match = next(
        item for item in expected_document["package"] if item["name"] == package_name
    )
    expected_match["version"] = version
    if refreshed_document != expected_document:
        raise StorageSpikeLauncherError("lock refresh changed unrelated material")
    return refreshed.encode("utf-8")
```

- [ ] **Step 4: Run the lock tests and verify GREEN**

Run the Task 1 command again. Expected: all lock-refresh tests pass.

- [ ] **Step 5: Commit the primitive**

```bash
git add tests/codex/sprint3_storage_spike.py tests/codex/test_sprint3_storage_spike.py
git commit -m "test(codex): isolate Sprint 3 validator lock refresh"
```

---

### Task 2: Temporary workspace and network-free Cargo launcher

**Files:**
- Modify: `tests/codex/sprint3_storage_spike.py`
- Modify: `tests/codex/test_sprint3_storage_spike.py`

**Interfaces:**
- Consumes: `rewrite_path_package_version(...)` from Task 1 and the exact repository layout.
- Produces: path constants including `CANONICAL_STORAGE_EVIDENCE`, `workspace_version(repository_root: Path = REPOSITORY_ROOT) -> str`, `replace_exact_index_store_path(manifest_text: str, index_store: Path) -> str`, `prepared_storage_spike(repository_root: Path = REPOSITORY_ROOT) -> Iterator[Path]`, `storage_spike_command(manifest: Path, arguments: Sequence[str]) -> list[str]`, `run_storage_spike(arguments: Sequence[str], *, capture_output: bool = False) -> subprocess.CompletedProcess[str]`, and a forwarding CLI.

- [ ] **Step 1: Write failing workspace and command tests**

```python
def test_prepared_workspace_refreshes_version_without_mutating_checkout(self) -> None:
    original_manifest = launcher.SPIKE_MANIFEST.read_bytes()
    original_lock = launcher.SPIKE_LOCK.read_bytes()
    with launcher.prepared_storage_spike() as manifest:
        prepared = tomllib.loads(
            manifest.with_name("Cargo.lock").read_text(encoding="utf-8")
        )
        package = next(
            item for item in prepared["package"]
            if item["name"] == "godot-codex-index-store"
        )
        self.assertEqual(package["version"], launcher.workspace_version())
        self.assertNotEqual(manifest, launcher.SPIKE_MANIFEST)
    self.assertEqual(launcher.SPIKE_MANIFEST.read_bytes(), original_manifest)
    self.assertEqual(launcher.SPIKE_LOCK.read_bytes(), original_lock)

def test_command_is_locked_offline_and_uses_prepared_manifest(self) -> None:
    with launcher.prepared_storage_spike() as manifest:
        command = launcher.storage_spike_command(
            manifest, ["validate", "evidence.json"]
        )
        self.assertIn("--locked", command)
        self.assertIn("--offline", command)
        self.assertEqual(command[command.index("--manifest-path") + 1], str(manifest))

def test_manifest_rewrite_requires_one_exact_local_dependency(self) -> None:
    original = launcher.SPIKE_MANIFEST.read_text(encoding="utf-8")
    duplicated = original + '\ngodot-codex-index-store = { path = "duplicate" }\n'
    with self.assertRaisesRegex(
        launcher.StorageSpikeLauncherError, "dependency must appear exactly once"
    ):
        launcher.replace_exact_index_store_path(
            duplicated, launcher.INDEX_STORE_ROOT
        )
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run the Task 1 command. Expected: failures because workspace/command functions are absent.

- [ ] **Step 3: Implement isolated preparation and execution**

```python
@contextlib.contextmanager
def prepared_storage_spike(repository_root: Path = REPOSITORY_ROOT) -> Iterator[Path]:
    with tempfile.TemporaryDirectory(prefix="sprint3-storage-validator-") as temporary:
        destination = Path(temporary) / "tests/codex/storage_spike"
        shutil.copytree(SPIKE_ROOT / "src", destination / "src")
        shutil.copy2(SPIKE_ROOT / "README.md", destination / "README.md")
        manifest_text = replace_exact_index_store_path(
            SPIKE_MANIFEST.read_text(encoding="utf-8"),
            repository_root / "godot-codex-mcp/crates/index-store",
        )
        (destination / "Cargo.toml").write_text(manifest_text, encoding="utf-8")
        refreshed = rewrite_path_package_version(
            SPIKE_LOCK.read_bytes(),
            "godot-codex-index-store",
            workspace_version(repository_root),
        )
        (destination / "Cargo.lock").write_bytes(refreshed)
        shutil.copy2(SOURCE_SCOPES, destination.parent / SOURCE_SCOPES.name)
        yield destination / "Cargo.toml"
```

`run_storage_spike` must call `subprocess.run(..., cwd=REPOSITORY_ROOT, check=False, text=True)` and set `CARGO_TARGET_DIR=tests/codex/storage_spike/target` in a copied environment mapping.

Implement the forwarding boundary exactly as:

```python
def storage_spike_command(manifest: Path, arguments: Sequence[str]) -> list[str]:
    return [
        "cargo", "+1.94.1", "run", "--quiet", "--locked", "--offline",
        "--release", "--manifest-path", str(manifest), "--", *arguments,
    ]


def run_storage_spike(
    arguments: Sequence[str], *, capture_output: bool = False
) -> subprocess.CompletedProcess[str]:
    with prepared_storage_spike() as manifest:
        environment = os.environ.copy()
        environment["CARGO_TARGET_DIR"] = str(SPIKE_ROOT / "target")
        return subprocess.run(
            storage_spike_command(manifest, arguments),
            cwd=REPOSITORY_ROOT,
            env=environment,
            check=False,
            capture_output=capture_output,
            text=True,
            timeout=600,
        )


def main(arguments: Sequence[str] | None = None) -> int:
    try:
        return run_storage_spike(
            tuple(sys.argv[1:] if arguments is None else arguments)
        ).returncode
    except (OSError, subprocess.TimeoutExpired, StorageSpikeLauncherError) as error:
        print(f"Sprint 3 storage-spike launcher failed: {error}", file=sys.stderr)
        return 70
```

- [ ] **Step 4: Verify unit tests and CLI failure are GREEN and bounded**

```bash
PYTHONPATH=tests/codex python3 -m unittest tests/codex/test_sprint3_storage_spike.py
python3 tests/codex/sprint3_storage_spike.py unknown-command
```

Expected: unit tests pass; the second command exits non-zero with the storage-spike usage message and without a Python traceback.

- [ ] **Step 5: Commit the launcher**

```bash
git add tests/codex/sprint3_storage_spike.py tests/codex/test_sprint3_storage_spike.py
git commit -m "feat(codex): refresh Sprint 3 validator in isolation"
```

---

### Task 3: Current index model compatibility

**Files:**
- Modify: `tests/codex/storage_spike/src/dataset.rs:46-78`
- Modify: `tests/codex/storage_spike/src/dataset.rs:306-338`
- Modify: `tests/codex/storage_spike/Cargo.lock:145-153`
- Test: `tests/codex/test_sprint3_storage_spike.py`

**Interfaces:**
- Consumes: current `godot_codex_index_store::IndexGeneration` with `scene` and `script` domains.
- Produces: storage-spike generations with explicit empty `script` domains and a currently usable tracked developer lock.

- [ ] **Step 1: Add a failing real-build regression test**

```python
def test_current_storage_spike_compiles_through_isolated_launcher(self) -> None:
    completed = launcher.run_storage_spike(
        ["validate", str(launcher.CANONICAL_STORAGE_EVIDENCE)],
        capture_output=True,
    )
    self.assertEqual(completed.returncode, 0, completed.stderr)
```

- [ ] **Step 2: Run the real-build test and verify RED**

```bash
PYTHONPATH=tests/codex python3 -m unittest tests.codex.test_sprint3_storage_spike.Sprint3StorageSpikeTests.test_current_storage_spike_compiles_through_isolated_launcher
```

Expected: Cargo compilation fails because both `IndexGeneration` initializers lack `script`.

- [ ] **Step 3: Add explicit script-domain compatibility and refresh the tracked path version**

Add this field immediately after the existing `scene` field in both initializers:

```rust
scene: Default::default(),
script: Default::default(),
validation_digest: String::new(),
```

Update only the `godot-codex-index-store` path package version in the tracked nested lock from `0.1.0` to the current workspace version `0.1.19`.

- [ ] **Step 4: Verify real build and native crate tests are GREEN**

```bash
PYTHONPATH=tests/codex python3 -m unittest tests.codex.test_sprint3_storage_spike.Sprint3StorageSpikeTests.test_current_storage_spike_compiles_through_isolated_launcher
cargo +1.94.1 test --locked --offline --manifest-path tests/codex/storage_spike/Cargo.toml
```

Expected: both commands pass.

- [ ] **Step 5: Commit compatibility update**

```bash
git add tests/codex/storage_spike/src/dataset.rs tests/codex/storage_spike/Cargo.lock tests/codex/test_sprint3_storage_spike.py
git commit -m "fix(codex): keep Sprint 3 spike compatible"
```

---

### Task 4: Acceptance integration and honest diagnostics

**Files:**
- Modify: `tests/codex/sprint3_acceptance.py:535-580`
- Modify: `tests/codex/test_sprint3_acceptance.py:570-610`
- Modify: `tests/codex/test_sprint3_acceptance.py:720-750`

**Interfaces:**
- Consumes: `run_storage_spike(...)` and `StorageSpikeLauncherError` from Task 2.
- Produces: `bounded_child_error(value: str, *, byte_limit: int) -> str` and `validate_storage_canonical_receipt(...)` that retain canonical rejection semantics and report bounded launcher/build diagnostics separately.

- [ ] **Step 1: Write failing diagnostic classification tests**

```python
def test_storage_validator_reports_build_failure_instead_of_scoring_mismatch(self) -> None:
    failed = subprocess.CompletedProcess(
        args=["cargo"], returncode=101, stdout="", stderr="error: lock mismatch\n"
    )
    with mock.patch.object(acceptance, "run_storage_spike", return_value=failed):
        with self.assertRaisesRegex(
            AcceptanceError,
            "Rust D-05 validator failed: error: lock mismatch",
        ):
            acceptance.validate_storage_canonical_receipt(b"{}", "segment")

def test_storage_validator_preserves_canonical_scoring_rejection(self) -> None:
    failed = subprocess.CompletedProcess(
        args=["cargo"],
        returncode=1,
        stdout="",
        stderr=(
            "storage spike failed: validation failed: combined D-05 evidence "
            "differs from canonical raw-sample scoring\n"
        ),
    )
    with mock.patch.object(acceptance, "run_storage_spike", return_value=failed):
        with self.assertRaisesRegex(
            AcceptanceError,
            "storage aggregate differs from canonical Rust raw-sample scoring",
        ):
            acceptance.validate_storage_canonical_receipt(b"{}", "segment")
```

```python
def test_storage_validator_bounds_child_diagnostics(self) -> None:
    failed = subprocess.CompletedProcess(
        args=["cargo"], returncode=101, stdout="", stderr="x" * 4096
    )
    with mock.patch.object(acceptance, "run_storage_spike", return_value=failed):
        with self.assertRaises(AcceptanceError) as raised:
            acceptance.validate_storage_canonical_receipt(b"{}", "segment")
    self.assertLessEqual(len(str(raised.exception).encode("utf-8")), 576)
```

- [ ] **Step 2: Run the diagnostic tests and verify RED**

```bash
python3 tests/codex/test_sprint3_acceptance.py Sprint3AcceptanceTests.test_storage_validator_reports_build_failure_instead_of_scoring_mismatch
python3 tests/codex/test_sprint3_acceptance.py Sprint3AcceptanceTests.test_storage_validator_preserves_canonical_scoring_rejection
```

Expected: tests fail because the validator still constructs Cargo directly and maps every non-zero exit to the generic scoring message.

- [ ] **Step 3: Route validation through the launcher and classify failures**

```python
try:
    completed = run_storage_spike(
        ["validate", str(snapshot)], capture_output=True
    )
except StorageSpikeLauncherError as error:
    raise AcceptanceError(
        f"canonical Rust D-05 validator could not be prepared: {error}"
    ) from error
if completed.returncode != 0:
    summary = bounded_child_error(completed.stderr, byte_limit=512)
    if "combined D-05 evidence differs from canonical raw-sample scoring" in summary:
        raise AcceptanceError(
            "storage aggregate differs from canonical Rust raw-sample scoring"
        )
    raise AcceptanceError(f"canonical Rust D-05 validator failed: {summary}")
```

Keep the existing exact receipt comparison unchanged.

- [ ] **Step 4: Run diagnostics and the full Sprint 3 acceptance module**

```bash
python3 tests/codex/test_sprint3_acceptance.py
```

Expected: 31 tests pass (the original 28 plus three diagnostic tests), including real canonical validation and tampered-score rejection.

- [ ] **Step 5: Commit acceptance integration**

```bash
git add tests/codex/sprint3_acceptance.py tests/codex/test_sprint3_acceptance.py
git commit -m "fix(codex): make Sprint 3 validation version-resilient"
```

---

### Task 5: Route every supported Sprint 3 entry point through the launcher

**Files:**
- Modify: `tests/codex/runners/sprint3_aggregate.sh:226-234`
- Modify: `tests/codex/runners/sprint3_linux_x86_64.sh:194-196`
- Modify: `tests/codex/storage_spike/README.md:8-46`
- Modify: `tests/codex/test_sprint3_storage_spike.py`

**Interfaces:**
- Consumes: `tests/codex/sprint3_storage_spike.py` CLI from Task 2.
- Produces: operator/documented commands that cannot bypass automatic local path-version refresh.

- [ ] **Step 1: Write a failing entry-point coverage test**

```python
def test_supported_entry_points_use_version_resilient_launcher(self) -> None:
    for path in (
        launcher.AGGREGATE_RUNNER,
        launcher.LINUX_RUNNER,
        launcher.SPIKE_README,
    ):
        text = path.read_text(encoding="utf-8")
        self.assertIn("python3 tests/codex/sprint3_storage_spike.py", text)
        self.assertNotIn(
            "--manifest-path tests/codex/storage_spike/Cargo.toml", text
        )
```

- [ ] **Step 2: Run the coverage test and verify RED**

Run the Task 1 command. Expected: failure because all three files still invoke Cargo directly.

- [ ] **Step 3: Replace direct storage-spike Cargo calls**

Use this command form in runners and documentation:

```bash
python3 tests/codex/sprint3_storage_spike.py \
  validate tests/codex/evidence/sprint-3-storage-spike-cross-platform.json
```

Preserve every existing storage-spike argument and the aggregation order exactly.

- [ ] **Step 4: Verify Python tests and shell syntax**

```bash
PYTHONPATH=tests/codex python3 -m unittest tests/codex/test_sprint3_storage_spike.py
bash -n tests/codex/runners/sprint3_aggregate.sh
bash -n tests/codex/runners/sprint3_linux_x86_64.sh
```

Expected: all commands pass.

- [ ] **Step 5: Commit entry-point migration**

```bash
git add tests/codex/runners/sprint3_aggregate.sh tests/codex/runners/sprint3_linux_x86_64.sh tests/codex/storage_spike/README.md tests/codex/test_sprint3_storage_spike.py
git commit -m "docs(codex): route Sprint 3 through refreshed validator"
```

---

### Task 6: Completion verification and scope audit

**Files:**
- Verify: all files changed since `codex/integration`.

**Interfaces:**
- Consumes: Tasks 1-5.
- Produces: fresh evidence that the Sprint 3 objective is complete and Sprint 5 is untouched.

- [ ] **Step 1: Run all Sprint 3 Python tests**

```bash
PYTHONPATH=tests/codex python3 -m unittest \
  tests/codex/test_sprint3_storage_spike.py \
  tests/codex/test_sprint3_acceptance.py
```

Expected: all tests pass with no errors or failures.

- [ ] **Step 2: Run the native storage-spike suite**

```bash
cargo +1.94.1 test --locked --offline \
  --manifest-path tests/codex/storage_spike/Cargo.toml
```

Expected: all unit and documentation tests pass.

- [ ] **Step 3: Verify Python and shell syntax plus diff hygiene**

```bash
python3 -m py_compile \
  tests/codex/sprint3_storage_spike.py \
  tests/codex/sprint3_acceptance.py
bash -n tests/codex/runners/sprint3_aggregate.sh
bash -n tests/codex/runners/sprint3_linux_x86_64.sh
git diff --check codex/integration...HEAD
git status --short
```

Expected: syntax and diff checks pass; the worktree is clean after committed tasks.

- [ ] **Step 4: Prove Sprint 5 remained untouched**

```bash
if git diff --name-only codex/integration...HEAD | rg 'sprint5|sprint-5'; then
  exit 1
fi
```

Expected: no matching path. Do not run or repair the known historical Sprint 5 failure.

- [ ] **Step 5: Review final commit range**

```bash
git log --oneline codex/integration..HEAD
git diff --stat codex/integration...HEAD
```

Expected: only the design, plan, Sprint 3 launcher/tests, storage-spike compatibility, Sprint 3 runners, and Sprint 3 documentation are present.
