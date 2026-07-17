#!/usr/bin/env bash
set -euo pipefail

# This post-freeze wrapper must remain outside the clean checkout passed as $1.
readonly FREEZE_COMMIT="a90ddd06c81a6210f44552b46ff533248b93ed90"
readonly EXPECTED_SOURCE_SHA256="sha256:73e99eec9897f8e2b4c6c210a3c56bd57a91ce347aedba030323eb01bf8438eb"
readonly EXPECTED_ORACLE_SHA256="sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"
export FREEZE_COMMIT EXPECTED_SOURCE_SHA256 EXPECTED_ORACLE_SHA256

if [[ "${1:-}" == "--preflight-only" ]]; then
  readonly PREFLIGHT_ONLY=1
  shift
else
  readonly PREFLIGHT_ONLY=0
fi
if [[ $# -ne 2 ]]; then
  echo "usage: $0 [--preflight-only] <clean-source-freeze-checkout> <output-json-outside-repository>" >&2
  exit 64
fi

[[ -f "$0" && ! -L "$0" ]] || { echo "runner must be a regular non-symlink file" >&2; exit 64; }
readonly RUNNER_PATH="$(cd "$(dirname "$0")" && pwd -P)/$(basename "$0")"
readonly MANIFEST_HELPER="$(dirname "$RUNNER_PATH")/sprint3_transfer_manifest.py"
[[ -f "$MANIFEST_HELPER" && ! -L "$MANIFEST_HELPER" ]] || {
  echo "transfer manifest helper is missing beside the runner" >&2
  exit 64
}
command -v python3 >/dev/null 2>&1 || { echo "Python 3 is required" >&2; exit 69; }
readonly REPOSITORY="$(cd "$1" && pwd -P)"
[[ "$2" == /* ]] || { echo "output must be an absolute path outside the repository" >&2; exit 64; }
readonly OUTPUT="$(python3 - "$2" <<'PY'
import sys
from pathlib import Path

print(Path(sys.argv[1]).resolve())
PY
)"
[[ "$(basename "$OUTPUT")" == "sprint-3-storage-spike-linux.json" ]] || {
  echo "output must be named sprint-3-storage-spike-linux.json" >&2
  exit 64
}
case "$OUTPUT" in
  "$REPOSITORY" | "$REPOSITORY"/*)
    echo "output must be outside the repository" >&2
    exit 64
    ;;
esac
readonly OUTPUT_DIRECTORY="$(dirname "$OUTPUT")"
readonly RECEIPT="$OUTPUT_DIRECTORY/sprint-3-linux.receipt.json"
for destination in "$OUTPUT" "$RECEIPT"; do
  [[ ! -e "$destination" && ! -L "$destination" ]] || {
    echo "refusing to reuse existing output: $destination" >&2
    exit 64
  }
done
mkdir -p "$OUTPUT_DIRECTORY"
write_probe="$(mktemp -d "$OUTPUT_DIRECTORY/.sprint3-linux-preflight.XXXXXX")"
rmdir "$write_probe"
cd "$REPOSITORY"

[[ "$(uname -s)" == "Linux" ]] || { echo "Linux host required" >&2; exit 65; }
[[ "$(uname -m)" == "x86_64" ]] || { echo "native x86_64 host required" >&2; exit 65; }
if [[ -e /.dockerenv || -e /run/.containerenv ]] || \
  grep -Eaq '(docker|containerd|kubepods|podman|lxc)' /proc/1/cgroup 2>/dev/null; then
  echo "qualifying evidence requires a real Linux host, not a container" >&2
  exit 65
fi
if command -v systemd-detect-virt >/dev/null 2>&1 && systemd-detect-virt --quiet --container; then
  echo "qualifying evidence requires a real Linux host, not a container" >&2
  exit 65
fi
if [[ -n "${WSL_INTEROP:-}" || -n "${WSL_DISTRO_NAME:-}" ]] || \
  grep -Eiq '(microsoft|wsl)' /proc/sys/kernel/osrelease /proc/version 2>/dev/null; then
  echo "qualifying evidence requires a real Linux host or full VM, not WSL" >&2
  exit 65
fi

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
  printf 'qualifying evidence requires a local host; detected CI markers: %s\n' \
    "${detected_ci_markers[*]}" >&2
  exit 65
fi

for required_command in git python3 cargo rustc cc; do
  command -v "$required_command" >/dev/null 2>&1 || {
    echo "required command is unavailable: $required_command" >&2
    exit 69
  }
done
python3 - <<'PY'
import platform
import struct
import sys

if sys.version_info < (3, 9):
    raise SystemExit("Python 3.9 or newer is required")
if sys.platform != "linux" or struct.calcsize("P") != 8 or platform.machine().lower() not in {"amd64", "x86_64"}:
    raise SystemExit("native Linux x86_64 Python is required")
PY
[[ "$(git rev-parse HEAD)" == "$FREEZE_COMMIT" ]] || { echo "wrong source freeze" >&2; exit 66; }
[[ -z "$(git status --porcelain)" ]] || { echo "checkout is dirty" >&2; exit 67; }
rustc +1.94.1 -vV | grep -Fx 'host: x86_64-unknown-linux-gnu' >/dev/null

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

available_kib() {
  df -Pk "$1" | awk 'NR == 2 { print $4 }'
}

require_free_kib() {
  local path="$1"
  local minimum="$2"
  local label="$3"
  local available
  available="$(available_kib "$path")"
  [[ "$available" =~ ^[0-9]+$ ]] || { echo "unable to measure free space for $label" >&2; exit 69; }
  ((available >= minimum)) || {
    echo "$label requires at least $minimum KiB free; found $available KiB" >&2
    exit 69
  }
  printf '%s' "$available"
}

readonly MINIMUM_REPOSITORY_KIB=$((10 * 1024 * 1024))
readonly MINIMUM_TEMP_KIB=$((10 * 1024 * 1024))
readonly MINIMUM_OUTPUT_KIB=$((1 * 1024 * 1024))
readonly TEMP_DIRECTORY="${TMPDIR:-/tmp}"
[[ -d "$TEMP_DIRECTORY" ]] || { echo "temporary directory is unavailable: $TEMP_DIRECTORY" >&2; exit 69; }
repository_available_kib="$(require_free_kib "$REPOSITORY" "$MINIMUM_REPOSITORY_KIB" "repository volume")"
temp_available_kib="$(require_free_kib "$TEMP_DIRECTORY" "$MINIMUM_TEMP_KIB" "temporary volume")"
output_available_kib="$(require_free_kib "$OUTPUT_DIRECTORY" "$MINIMUM_OUTPUT_KIB" "output volume")"
echo "Linux Sprint 3 preflight passed (repository_kib=$repository_available_kib; temp_kib=$temp_available_kib; output_kib=$output_available_kib)"
if ((PREFLIGHT_ONLY)); then
  exit 0
fi

readonly WORK="$(mktemp -d "$OUTPUT_DIRECTORY/.sprint3-linux-run.XXXXXX")"
readonly WORK_OUTPUT="$WORK/sprint-3-storage-spike-linux.json"
readonly WORK_RECEIPT="$WORK/sprint-3-linux.receipt.json"
run_completed=0
cleanup() {
  status=$?
  trap - EXIT
  rm -rf -- "$WORK"
  if ((run_completed == 0)); then
    rm -f -- "$OUTPUT" "$RECEIPT"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cargo +1.94.1 test --locked --workspace --all-targets \
  --manifest-path godot-codex-mcp/Cargo.toml
cargo +1.94.1 run --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  run --backend all --dataset all --repo-root "$REPOSITORY" --output "$WORK_OUTPUT"

REPORT_PATH="$WORK_OUTPUT" python3 - <<'PY'
import json
import os
from pathlib import Path

report = json.loads(Path(os.environ["REPORT_PATH"]).read_text(encoding="utf-8"))
if report["profile"] != "decision":
    raise SystemExit("Linux storage report is not the decision profile")
if report["os"] != "linux" or report["architecture"] != "x86_64":
    raise SystemExit("Linux storage report has the wrong platform coordinates")
if report["git_commit"] != os.environ["FREEZE_COMMIT"] or report["git_dirty"] is not False:
    raise SystemExit("Linux storage report has the wrong Git coordinates")
if report["source_tree_sha256"] != os.environ["EXPECTED_SOURCE_SHA256"]:
    raise SystemExit("Linux storage report has the wrong source digest")
if report["oracle_sha256"] != os.environ["EXPECTED_ORACLE_SHA256"]:
    raise SystemExit("Linux storage report has the wrong oracle digest")
if report["chosen_backend"] not in {"sqlite", "segment"}:
    raise SystemExit("Linux storage report has no valid chosen backend")
if len(report["backends"]) != 2:
    raise SystemExit("Linux storage report must contain exactly two backends")
if {backend["backend"] for backend in report["backends"]} != {"sqlite", "segment"}:
    raise SystemExit("Linux storage report must contain SQLite and segment backends")
segment = next(backend for backend in report["backends"] if backend["backend"] == "segment")
if segment["qualified"] is not True:
    raise SystemExit("Linux segment backend did not qualify")
if not all(segment["gates"].values()):
    raise SystemExit("Linux segment backend failed a storage gate")
if not all(segment["fault_matrix"].values()):
    raise SystemExit("Linux segment backend failed a fault case")
if segment["errors"]:
    raise SystemExit("Linux segment backend reported errors")
PY

python3 "$MANIFEST_HELPER" create \
  --platform linux-x86_64 \
  --runner "$RUNNER_PATH" \
  --receipt "$WORK_RECEIPT" \
  --file "$WORK_OUTPUT"
python3 "$MANIFEST_HELPER" verify \
  --platform linux-x86_64 \
  --runner "$RUNNER_PATH" \
  --receipt "$WORK_RECEIPT" \
  --file "$WORK_OUTPUT"

mv "$WORK_OUTPUT" "$OUTPUT"
mv "$WORK_RECEIPT" "$RECEIPT"
python3 "$MANIFEST_HELPER" verify \
  --platform linux-x86_64 \
  --runner "$RUNNER_PATH" \
  --receipt "$RECEIPT" \
  --file "$OUTPUT"
run_completed=1

echo "Linux Sprint 3 storage evidence passed: $OUTPUT; receipt: $RECEIPT"
