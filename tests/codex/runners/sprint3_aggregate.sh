#!/usr/bin/env bash
set -euo pipefail

# This post-freeze wrapper installs evidence only after all canonical validators pass.
readonly FREEZE_COMMIT="a90ddd06c81a6210f44552b46ff533248b93ed90"
readonly EXPECTED_SOURCE_SHA256="sha256:73e99eec9897f8e2b4c6c210a3c56bd57a91ce347aedba030323eb01bf8438eb"
readonly EXPECTED_ORACLE_SHA256="sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"
# These hashes pin the qualifying macOS reports regenerated at FREEZE_COMMIT.
# Aggregation fails closed if either tracked input changes.
readonly EXPECTED_MACOS_LIVE_SHA256="bb56c6e7e514a7741ae1f29de5eb1127a22757691b736cd9285e37d3f93f7ef3"
readonly EXPECTED_MACOS_STORAGE_SHA256="7ecffdf270148712a492228de0bef83df6d688891e1b8385bf348682e0ff5311"
export EXPECTED_SOURCE_SHA256

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <clean-aggregation-checkout> <raw-evidence-staging-directory>" >&2
  exit 64
fi

[[ -f "$0" && ! -L "$0" ]] || { echo "runner must be a regular non-symlink file" >&2; exit 64; }
readonly RUNNER_PATH="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
readonly RUNNER_DIRECTORY="$(dirname "$RUNNER_PATH")"
readonly MANIFEST_HELPER="$RUNNER_DIRECTORY/sprint3_transfer_manifest.py"
readonly WINDOWS_RUNNER="$RUNNER_DIRECTORY/sprint3_windows_x86_64.ps1"
for runner_file in "$MANIFEST_HELPER" "$WINDOWS_RUNNER"; do
  [[ -f "$runner_file" && ! -L "$runner_file" ]] || {
    echo "required runner file is missing or is a symlink: $runner_file" >&2
    exit 64
  }
done

canonical_path() {
  python3 - "$1" <<'PY'
import sys
from pathlib import Path

print(Path(sys.argv[1]).resolve(strict=True))
PY
}

readonly REPOSITORY="$(canonical_path "$1")"
readonly STAGING="$(canonical_path "$2")"
case "$STAGING" in
  "$REPOSITORY" | "$REPOSITORY"/*)
    echo "raw evidence staging must be outside the repository" >&2
    exit 64
    ;;
esac

readonly CI_MARKERS=(
  APPVEYOR
  BITBUCKET_BUILD_NUMBER
  BUILDKITE
  CI
  CIRCLECI
  CODEBUILD_BUILD_ID
  CONTINUOUS_INTEGRATION
  DRONE
  GITEA_ACTIONS
  GITHUB_ACTIONS
  GITLAB_CI
  JENKINS_URL
  TEAMCITY_VERSION
  TF_BUILD
  TRAVIS
  WOODPECKER
)
detected_ci_markers=()
for marker in "${CI_MARKERS[@]}"; do
  printenv "$marker" >/dev/null 2>&1 && detected_ci_markers+=("$marker")
done
if ((${#detected_ci_markers[@]} != 0)); then
  printf 'aggregation requires a local checkout; detected CI markers: %s\n' \
    "${detected_ci_markers[*]}" >&2
  exit 65
fi

cd "$REPOSITORY"
git cat-file -e "$FREEZE_COMMIT^{commit}"
git merge-base --is-ancestor "$FREEZE_COMMIT" HEAD || {
  echo "source freeze is not an ancestor of the aggregation checkout" >&2
  exit 66
}
[[ -z "$(git status --porcelain)" ]] || {
  echo "aggregation checkout must be clean before evidence installation" >&2
  exit 67
}

source_json="$(PYTHONPATH=tests/codex python3 - <<'PY'
import json
from sprint3_acceptance import checkout_source_coordinates

print(json.dumps(checkout_source_coordinates("a90ddd06c81a6210f44552b46ff533248b93ed90"), sort_keys=True))
PY
)"
SOURCE_JSON="$source_json" python3 - <<'PY'
import json
import os

source = json.loads(os.environ["SOURCE_JSON"])
if source["git_dirty"] is not False:
    raise SystemExit("source coordinate helper reported a dirty scoped tree")
if source["source_tree_sha256"] != os.environ["EXPECTED_SOURCE_SHA256"]:
    raise SystemExit("source scope differs from the frozen digest")
PY

actual_oracle_sha256="$(python3 - <<'PY'
import hashlib
from pathlib import Path

path = Path("tests/codex/fixtures/resource_graph_oracle/golden-resource-graph.json")
print(f"sha256:{hashlib.sha256(path.read_bytes()).hexdigest()}")
PY
)"
[[ "$actual_oracle_sha256" == "$EXPECTED_ORACLE_SHA256" ]] || {
  echo "oracle differs from the source freeze" >&2
  exit 68
}

readonly MACOS_LIVE_SOURCE="$REPOSITORY/tests/codex/evidence/sprint-3-resource-graph-macos.json"
readonly MACOS_STORAGE_SOURCE="$REPOSITORY/tests/codex/evidence/platform/sprint-3-storage-spike-macos.json"
readonly WINDOWS_LIVE_SOURCE="$STAGING/sprint-3-resource-graph-windows.json"
readonly WINDOWS_STORAGE_SOURCE="$STAGING/sprint-3-storage-spike-windows.json"
readonly WINDOWS_RECEIPT_SOURCE="$STAGING/sprint-3-windows.receipt.json"

for evidence in \
  "$MACOS_LIVE_SOURCE" \
  "$MACOS_STORAGE_SOURCE" \
  "$WINDOWS_LIVE_SOURCE" \
  "$WINDOWS_STORAGE_SOURCE" \
  "$WINDOWS_RECEIPT_SOURCE"; do
  [[ -f "$evidence" && ! -L "$evidence" ]] || {
    echo "required regular evidence file is missing: $evidence" >&2
    exit 69
  }
done

readonly EVIDENCE_DIRECTORY="$REPOSITORY/tests/codex/evidence"
readonly PLATFORM_DIRECTORY="$EVIDENCE_DIRECTORY/platform"
readonly FINAL_WINDOWS_LIVE="$EVIDENCE_DIRECTORY/sprint-3-resource-graph-windows.json"
readonly FINAL_WINDOWS_STORAGE="$PLATFORM_DIRECTORY/sprint-3-storage-spike-windows.json"
readonly FINAL_STORAGE="$EVIDENCE_DIRECTORY/sprint-3-storage-spike-cross-platform.json"
readonly FINAL_ACCEPTANCE="$EVIDENCE_DIRECTORY/sprint-3-acceptance.json"
for destination in \
  "$FINAL_WINDOWS_LIVE" \
  "$FINAL_WINDOWS_STORAGE" \
  "$FINAL_STORAGE" \
  "$FINAL_ACCEPTANCE"; do
  [[ ! -e "$destination" && ! -L "$destination" ]] || {
    echo "refusing to overwrite existing acceptance artifact: $destination" >&2
    exit 70
  }
done

readonly WORK="$(mktemp -d "${TMPDIR:-/tmp}/godotstg-s3-aggregate.XXXXXX")"
INSTALL_STARTED=0
INSTALL_COMPLETE=0
WINDOWS_LIVE_TEMP=""
WINDOWS_STORAGE_TEMP=""
STORAGE_TEMP=""
ACCEPTANCE_TEMP=""
cleanup() {
  status=$?
  trap - EXIT
  rm -rf -- "$WORK"
  for temporary_file in \
    "$WINDOWS_LIVE_TEMP" \
    "$WINDOWS_STORAGE_TEMP" \
    "$STORAGE_TEMP" \
    "$ACCEPTANCE_TEMP"; do
    [[ -z "$temporary_file" ]] || rm -f -- "$temporary_file"
  done
  if ((INSTALL_STARTED == 1 && INSTALL_COMPLETE == 0)); then
    rm -f -- \
      "$FINAL_WINDOWS_LIVE" \
      "$FINAL_WINDOWS_STORAGE" \
      "$FINAL_STORAGE" \
      "$FINAL_ACCEPTANCE"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
mkdir -p "$WORK/input" "$WORK/a" "$WORK/b"

readonly MACOS_LIVE="$WORK/input/sprint-3-resource-graph-macos.json"
readonly MACOS_STORAGE="$WORK/input/sprint-3-storage-spike-macos.json"
readonly WINDOWS_LIVE="$WORK/input/sprint-3-resource-graph-windows.json"
readonly WINDOWS_STORAGE="$WORK/input/sprint-3-storage-spike-windows.json"
readonly WINDOWS_RECEIPT="$WORK/input/sprint-3-windows.receipt.json"

# Snapshot every input once. Validation, aggregation, and installation must use
# the same immutable bytes even if the external staging directory changes.
install -m 0644 "$MACOS_LIVE_SOURCE" "$MACOS_LIVE"
install -m 0644 "$MACOS_STORAGE_SOURCE" "$MACOS_STORAGE"
install -m 0644 "$WINDOWS_LIVE_SOURCE" "$WINDOWS_LIVE"
install -m 0644 "$WINDOWS_STORAGE_SOURCE" "$WINDOWS_STORAGE"
install -m 0644 "$WINDOWS_RECEIPT_SOURCE" "$WINDOWS_RECEIPT"

macos_live_sha256="$(shasum -a 256 "$MACOS_LIVE" | awk '{ print $1 }')"
macos_storage_sha256="$(shasum -a 256 "$MACOS_STORAGE" | awk '{ print $1 }')"
[[ "$macos_live_sha256" == "$EXPECTED_MACOS_LIVE_SHA256" ]] || {
  echo "snapshotted macOS live evidence differs from the approved artifact" >&2
  exit 69
}
[[ "$macos_storage_sha256" == "$EXPECTED_MACOS_STORAGE_SHA256" ]] || {
  echo "snapshotted macOS storage evidence differs from the approved artifact" >&2
  exit 69
}

python3 "$MANIFEST_HELPER" verify \
  --platform windows-x86_64 \
  --runner "$WINDOWS_RUNNER" \
  --receipt "$WINDOWS_RECEIPT" \
  --file "$WINDOWS_LIVE" \
  --file "$WINDOWS_STORAGE" >/dev/null

readonly STORAGE_A="$WORK/a/sprint-3-storage-spike-cross-platform.json"
readonly STORAGE_B="$WORK/b/sprint-3-storage-spike-cross-platform.json"
readonly ACCEPTANCE_A="$WORK/a/sprint-3-acceptance.json"
readonly ACCEPTANCE_B="$WORK/b/sprint-3-acceptance.json"

python3 tests/codex/sprint3_acceptance.py validate-live "$MACOS_LIVE" >/dev/null
python3 tests/codex/sprint3_acceptance.py validate-live "$WINDOWS_LIVE" >/dev/null

cargo +1.94.1 run --quiet --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  merge "$STORAGE_A" "$MACOS_STORAGE" "$WINDOWS_STORAGE"
cargo +1.94.1 run --quiet --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  merge "$STORAGE_B" "$WINDOWS_STORAGE" "$MACOS_STORAGE"
cmp "$STORAGE_A" "$STORAGE_B"
cargo +1.94.1 run --quiet --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- validate "$STORAGE_A"
python3 tests/codex/sprint3_acceptance.py validate-storage "$STORAGE_A" >/dev/null

python3 tests/codex/sprint3_acceptance.py merge \
  --macos-live "$MACOS_LIVE" \
  --windows-live "$WINDOWS_LIVE" \
  --storage "$STORAGE_A" \
  --output "$ACCEPTANCE_A" >/dev/null
python3 tests/codex/sprint3_acceptance.py merge \
  --macos-live "$MACOS_LIVE" \
  --windows-live "$WINDOWS_LIVE" \
  --storage "$STORAGE_A" \
  --output "$ACCEPTANCE_B" >/dev/null
cmp "$ACCEPTANCE_A" "$ACCEPTANCE_B"

[[ -z "$(git status --porcelain)" ]] || {
  echo "aggregation checkout changed while evidence was being validated" >&2
  exit 67
}
mkdir -p "$PLATFORM_DIRECTORY"
WINDOWS_LIVE_TEMP="$(mktemp "$EVIDENCE_DIRECTORY/.sprint-3-resource-graph-windows.XXXXXX")"
WINDOWS_STORAGE_TEMP="$(mktemp "$PLATFORM_DIRECTORY/.sprint-3-storage-spike-windows.XXXXXX")"
STORAGE_TEMP="$(mktemp "$EVIDENCE_DIRECTORY/.sprint-3-storage-spike-cross-platform.XXXXXX")"
ACCEPTANCE_TEMP="$(mktemp "$EVIDENCE_DIRECTORY/.sprint-3-acceptance.XXXXXX")"
install -m 0644 "$WINDOWS_LIVE" "$WINDOWS_LIVE_TEMP"
install -m 0644 "$WINDOWS_STORAGE" "$WINDOWS_STORAGE_TEMP"
install -m 0644 "$STORAGE_A" "$STORAGE_TEMP"
install -m 0644 "$ACCEPTANCE_A" "$ACCEPTANCE_TEMP"

INSTALL_STARTED=1
mv "$WINDOWS_LIVE_TEMP" "$FINAL_WINDOWS_LIVE"
mv "$WINDOWS_STORAGE_TEMP" "$FINAL_WINDOWS_STORAGE"
mv "$STORAGE_TEMP" "$FINAL_STORAGE"
mv "$ACCEPTANCE_TEMP" "$FINAL_ACCEPTANCE"

cmp "$WINDOWS_LIVE" "$FINAL_WINDOWS_LIVE"
cmp "$WINDOWS_STORAGE" "$FINAL_WINDOWS_STORAGE"
cmp "$STORAGE_A" "$FINAL_STORAGE"
cmp "$ACCEPTANCE_A" "$FINAL_ACCEPTANCE"
python3 tests/codex/sprint3_acceptance.py validate-live \
  "$FINAL_WINDOWS_LIVE" >/dev/null
python3 tests/codex/sprint3_acceptance.py validate-storage \
  "$FINAL_STORAGE" >/dev/null

shasum -a 256 \
  "$FINAL_WINDOWS_LIVE" \
  "$FINAL_WINDOWS_STORAGE" \
  "$FINAL_STORAGE" \
  "$FINAL_ACCEPTANCE"
INSTALL_COMPLETE=1
echo "Sprint 3 raw evidence and deterministic aggregates installed"
