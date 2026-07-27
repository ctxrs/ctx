#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${RUNFILES_DIR:-}" ]]; then
  root="${RUNFILES_DIR}/_main"
elif [[ -n "${TEST_SRCDIR:-}" ]]; then
  root="${TEST_SRCDIR}/_main"
else
  root="${BUILD_WORKSPACE_DIRECTORY:-$(cd "$(dirname "$0")/../.." && pwd)}"
fi

release_script="${root}/scripts/build-public-cli-artifact.sh"
tmp="$(mktemp -d)"
cleanup() {
  rm -rf -- "$tmp"
}
trap cleanup EXIT

cleanup_harness="${tmp}/cleanup-harness.sh"
awk '
  /^inspector_roots=\(\)$/ { capture = 1 }
  capture { print }
  /^trap cleanup_release_temps EXIT$/ { exit }
' "$release_script" > "$cleanup_harness"

grep -F 'if (( ${#inspector_roots[@]} > 0 )); then' "$cleanup_harness" >/dev/null

# This is the stock Bash 3.2 failure shape on non-Linux release paths: the
# inspector array is initialized but remains empty when the EXIT trap runs.
bash "$cleanup_harness"
set +e
bash -c 'set -euo pipefail; source "$1"; exit 23' _ "$cleanup_harness"
status=$?
set -e
[[ "$status" -eq 23 ]]

# Linux stages an inspector root. Preserve cleanup of a real, non-symlinked
# root inside the exact bounded temporary-directory namespace.
inspector_root="${tmp}/ctx-public-inspector.control"
mkdir "$inspector_root"
TMPDIR="$tmp" bash -c '
  set -euo pipefail
  source "$1"
  inspector_roots+=("$2")
  cleanup_release_temps
  [[ ! -e "$2" ]]
  inspector_roots=()
' _ "$cleanup_harness" "$inspector_root"

# An empty inspector array must not skip the independent FreeBSD Cargo home
# cleanup.
freebsd_cargo_home="${tmp}/ctx-freebsd-release-cargo.control"
mkdir "$freebsd_cargo_home"
TMPDIR="$tmp" bash -c '
  set -euo pipefail
  source "$1"
  freebsd_release_cargo_home="$2"
  cleanup_release_temps
  [[ ! -e "$2" ]]
  freebsd_release_cargo_home=""
' _ "$cleanup_harness" "$freebsd_cargo_home"

printf 'public CLI release temp cleanup tests passed\n'
