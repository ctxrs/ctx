#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE:-_main}"
else
  repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)"
fi
test_root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-cli-ux-frame-test.XXXXXX")"
trap 'rm -rf -- "${test_root}"' EXIT

fake_ctx="${test_root}/fake-ctx"
cp -- "${repo_root}/scripts/tests/fixtures/cli-ux-capture-fake-ctx.sh" "${fake_ctx}"
chmod 700 "${fake_ctx}"
capture="${repo_root}/scripts/capture-cli-ux-frame.sh"

assert_stopped() {
  local pid_file="$1"
  local pid
  pid="$(cat "${pid_file}")"
  if kill -0 "${pid}" >/dev/null 2>&1; then
    echo "capture harness leaked fake daemon ${pid}" >&2
    exit 1
  fi
}

pid_file="${test_root}/success.pid"
CTX_UX_CAPTURE_TEST_PID_FILE="${pid_file}" \
  "${capture}" --ctx "${fake_ctx}" -- import
assert_stopped "${pid_file}"

pid_file="${test_root}/failure.pid"
set +e
CTX_UX_CAPTURE_TEST_PID_FILE="${pid_file}" \
  "${capture}" --ctx "${fake_ctx}" -- fail
failure_status=$?
set -e
[[ "${failure_status}" == "1" ]]
assert_stopped "${pid_file}"

shopt -s nullglob
residual_roots=("${TMPDIR:-/tmp}"/ctx-cli-ux-frame.*)
if ((${#residual_roots[@]} > 0)); then
  echo "capture harness left a task-owned root after the suite" >&2
  exit 1
fi
