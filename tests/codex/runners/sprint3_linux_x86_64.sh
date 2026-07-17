#!/usr/bin/env bash
set -euo pipefail

# This post-freeze wrapper must remain outside the clean checkout passed as $1.
readonly FREEZE_COMMIT="75364c2cc5fe50de5a508315c41cc43200b90024"
readonly EXPECTED_SOURCE_SHA256="sha256:0540dc092e2a5d6c94ac8e23d84bd2bc7224ddfb2c78456f09443d14e80950d2"
readonly EXPECTED_ORACLE_SHA256="sha256:a41ddfc642f653ad88866aeeefe7852d7742b8701e469aec821aef5b9c046f3b"
export FREEZE_COMMIT EXPECTED_SOURCE_SHA256 EXPECTED_ORACLE_SHA256

if [[ $# -ne 2 ]]; then
  echo "usage: $0 <clean-source-freeze-checkout> <output-json-outside-repository>" >&2
  exit 64
fi

readonly REPOSITORY="$(cd "$1" && pwd -P)"
[[ "$2" == /* ]] || { echo "output must be an absolute path outside the repository" >&2; exit 64; }
readonly OUTPUT="$(python3 - "$2" <<'PY'
import sys
from pathlib import Path

print(Path(sys.argv[1]).resolve())
PY
)"
export OUTPUT
case "$OUTPUT" in
  "$REPOSITORY" | "$REPOSITORY"/*)
    echo "output must be outside the repository" >&2
    exit 64
    ;;
esac
mkdir -p "$(dirname "$OUTPUT")"
cd "$REPOSITORY"

[[ "$(uname -s)" == "Linux" ]] || { echo "Linux host required" >&2; exit 65; }
[[ "$(uname -m)" == "x86_64" ]] || { echo "native x86_64 host required" >&2; exit 65; }
if [[ -e /.dockerenv || -e /run/.containerenv ]] || \
  grep -Eaq '(docker|containerd|kubepods|podman|lxc)' /proc/1/cgroup 2>/dev/null; then
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

cargo +1.94.1 test --locked --workspace --all-targets \
  --manifest-path godot-codex-mcp/Cargo.toml
cargo +1.94.1 run --locked --release \
  --manifest-path tests/codex/storage_spike/Cargo.toml -- \
  run --backend all --dataset all --repo-root "$REPOSITORY" --output "$OUTPUT"

python3 - <<'PY'
import json
import os
from pathlib import Path

report = json.loads(Path(os.environ["OUTPUT"]).read_text(encoding="utf-8"))
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

echo "Linux Sprint 3 storage evidence passed: $OUTPUT"
