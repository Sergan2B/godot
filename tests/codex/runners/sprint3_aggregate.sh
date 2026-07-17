#!/usr/bin/env bash
set -euo pipefail

# This post-freeze wrapper installs evidence only after all canonical validators pass.
readonly FREEZE_COMMIT="75364c2cc5fe50de5a508315c41cc43200b90024"
readonly EXPECTED_SOURCE_SHA256="sha256:0540dc092e2a5d6c94ac8e23d84bd2bc7224ddfb2c78456f09443d14e80950d2"
readonly EXPECTED_ORACLE_SHA256="sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"
export EXPECTED_SOURCE_SHA256

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <clean-aggregation-checkout> <raw-evidence-staging-directory>" >&2
  exit 64
fi

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
from sprint3_stage4_index_mcp import source_coordinates

print(json.dumps(source_coordinates(), sort_keys=True))
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
readonly LINUX_STORAGE_SOURCE="$STAGING/sprint-3-storage-spike-linux.json"
readonly WINDOWS_STORAGE_SOURCE="$STAGING/sprint-3-storage-spike-windows.json"

for evidence in \
  "$MACOS_LIVE_SOURCE" \
  "$MACOS_STORAGE_SOURCE" \
  "$WINDOWS_LIVE_SOURCE" \
  "$LINUX_STORAGE_SOURCE" \
  "$WINDOWS_STORAGE_SOURCE"; do
  [[ -f "$evidence" && ! -L "$evidence" ]] || {
    echo "required regular evidence file is missing: $evidence" >&2
    exit 69
  }
done

readonly WORK="$(mktemp -d "${TMPDIR:-/tmp}/godotstg-s3-aggregate.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/input" "$WORK/a" "$WORK/b"

readonly MACOS_LIVE="$WORK/input/sprint-3-resource-graph-macos.json"
readonly MACOS_STORAGE="$WORK/input/sprint-3-storage-spike-macos.json"
readonly WINDOWS_LIVE="$WORK/input/sprint-3-resource-graph-windows.json"
readonly LINUX_STORAGE="$WORK/input/sprint-3-storage-spike-linux.json"
readonly WINDOWS_STORAGE="$WORK/input/sprint-3-storage-spike-windows.json"

# Snapshot every input once. Validation, aggregation, and installation must use
# the same immutable bytes even if the external staging directory changes.
install -m 0644 "$MACOS_LIVE_SOURCE" "$MACOS_LIVE"
install -m 0644 "$MACOS_STORAGE_SOURCE" "$MACOS_STORAGE"
install -m 0644 "$WINDOWS_LIVE_SOURCE" "$WINDOWS_LIVE"
install -m 0644 "$LINUX_STORAGE_SOURCE" "$LINUX_STORAGE"
install -m 0644 "$WINDOWS_STORAGE_SOURCE" "$WINDOWS_STORAGE"

readonly STORAGE_A="$WORK/a/sprint-3-storage-spike-cross-platform.json"
readonly STORAGE_B="$WORK/b/sprint-3-storage-spike-cross-platform.json"
readonly ACCEPTANCE_A="$WORK/a/sprint-3-acceptance.json"
readonly ACCEPTANCE_B="$WORK/b/sprint-3-acceptance.json"

python3 tests/codex/sprint3_acceptance.py validate-live "$MACOS_LIVE" >/dev/null
python3 tests/codex/sprint3_acceptance.py validate-live "$WINDOWS_LIVE" >/dev/null

cargo +1.94.1 run --quiet --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  merge "$STORAGE_A" "$LINUX_STORAGE" "$MACOS_STORAGE" "$WINDOWS_STORAGE"
cargo +1.94.1 run --quiet --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  merge "$STORAGE_B" "$WINDOWS_STORAGE" "$MACOS_STORAGE" "$LINUX_STORAGE"
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

readonly EVIDENCE_DIRECTORY="$REPOSITORY/tests/codex/evidence"
readonly PLATFORM_DIRECTORY="$EVIDENCE_DIRECTORY/platform"
mkdir -p "$PLATFORM_DIRECTORY"
install -m 0644 "$WINDOWS_LIVE" "$EVIDENCE_DIRECTORY/sprint-3-resource-graph-windows.json"
install -m 0644 "$LINUX_STORAGE" "$PLATFORM_DIRECTORY/sprint-3-storage-spike-linux.json"
install -m 0644 "$WINDOWS_STORAGE" "$PLATFORM_DIRECTORY/sprint-3-storage-spike-windows.json"
install -m 0644 "$STORAGE_A" "$EVIDENCE_DIRECTORY/sprint-3-storage-spike-cross-platform.json"
install -m 0644 "$ACCEPTANCE_A" "$EVIDENCE_DIRECTORY/sprint-3-acceptance.json"

cmp "$WINDOWS_LIVE" "$EVIDENCE_DIRECTORY/sprint-3-resource-graph-windows.json"
cmp "$LINUX_STORAGE" "$PLATFORM_DIRECTORY/sprint-3-storage-spike-linux.json"
cmp "$WINDOWS_STORAGE" "$PLATFORM_DIRECTORY/sprint-3-storage-spike-windows.json"
cmp "$STORAGE_A" "$EVIDENCE_DIRECTORY/sprint-3-storage-spike-cross-platform.json"
cmp "$ACCEPTANCE_A" "$EVIDENCE_DIRECTORY/sprint-3-acceptance.json"
python3 tests/codex/sprint3_acceptance.py validate-live \
  "$EVIDENCE_DIRECTORY/sprint-3-resource-graph-windows.json" >/dev/null
python3 tests/codex/sprint3_acceptance.py validate-storage \
  "$EVIDENCE_DIRECTORY/sprint-3-storage-spike-cross-platform.json" >/dev/null

shasum -a 256 \
  "$EVIDENCE_DIRECTORY/sprint-3-resource-graph-windows.json" \
  "$PLATFORM_DIRECTORY/sprint-3-storage-spike-linux.json" \
  "$PLATFORM_DIRECTORY/sprint-3-storage-spike-windows.json" \
  "$EVIDENCE_DIRECTORY/sprint-3-storage-spike-cross-platform.json" \
  "$EVIDENCE_DIRECTORY/sprint-3-acceptance.json"
echo "Sprint 3 raw evidence and deterministic aggregates installed"
