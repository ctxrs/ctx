#!/usr/bin/env bash
set -euo pipefail

PACKAGE="@qwen-code/qwen-code"
VERSION="0.19.7"
BIN_NAME="qwen"
MODEL="test-model"
QUERY="needle topic with spaces"

server_pid=""

fail() {
  printf 'real Qwen slash E2E failed: %s\n' "$*" >&2
  exit 1
}

run() {
  printf '==>'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

cleanup() {
  if [[ -n "${server_pid}" ]] && kill -0 "${server_pid}" >/dev/null 2>&1; then
    kill "${server_pid}" >/dev/null 2>&1 || true
    wait "${server_pid}" >/dev/null 2>&1 || true
  fi
}

trap cleanup EXIT

find_repo_root() {
  local candidate
  for candidate in "${BUILD_WORKSPACE_DIRECTORY:-}" "$(pwd)" "$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; do
    if [[ -n "${candidate}" && -f "${candidate}/Cargo.toml" ]]; then
      cd "${candidate}"
      return 0
    fi
  done
  fail 'could not locate repo root containing Cargo.toml'
}

cache_key() {
  printf '%s@%s' "${PACKAGE//@/__}" "${VERSION}" | tr '/:' '__'
}

ensure_npm_binary() {
  local cache_root install_root tmp_root marker
  cache_root="${CTX_REAL_HARNESS_CACHE:-${PWD}/target-local/real-harness-cache}"
  install_root="${cache_root}/npm/$(cache_key)"
  marker="${install_root}/.ctx-installed-package"
  if [[ -x "${install_root}/node_modules/.bin/${BIN_NAME}" ]] && [[ -f "${marker}" ]] && [[ "$(cat "${marker}")" == "${PACKAGE}@${VERSION}" ]]; then
    printf '%s\n' "${install_root}/node_modules/.bin/${BIN_NAME}"
    return 0
  fi

  command -v npm >/dev/null 2>&1 || fail 'npm is required for pinned real harness installs'
  mkdir -p "${cache_root}/npm" "${cache_root}/npm-cache"
  tmp_root="${install_root}.tmp.$$"
  rm -rf "${tmp_root}"
  mkdir -p "${tmp_root}"
  run npm install \
    --prefix "${tmp_root}" \
    --cache "${cache_root}/npm-cache" \
    --ignore-scripts \
    --no-audit \
    --no-fund \
    "${PACKAGE}@${VERSION}" >&2
  printf '%s\n' "${PACKAGE}@${VERSION}" > "${tmp_root}/.ctx-installed-package"
  rm -rf "${install_root}"
  mv "${tmp_root}" "${install_root}"
  printf '%s\n' "${install_root}/node_modules/.bin/${BIN_NAME}"
}

require_contains() {
  local path="$1"
  local needle="$2"
  if ! grep -F -- "${needle}" "${path}" >/dev/null; then
    printf '%s\n' "--- ${path} ---" >&2
    sed -n '1,220p' "${path}" >&2
    fail "expected ${path} to contain: ${needle}"
  fi
}

require_not_contains() {
  local path="$1"
  local needle="$2"
  if grep -F -- "${needle}" "${path}" >/dev/null; then
    printf '%s\n' "--- ${path} ---" >&2
    sed -n '1,220p' "${path}" >&2
    fail "expected ${path} not to contain: ${needle}"
  fi
}

wait_for_port() {
  local port_file="$1"
  for _ in {1..100}; do
    [[ -s "${port_file}" ]] && return 0
    sleep 0.05
  done
  fail 'fixture Qwen server did not publish a port'
}

main() {
  find_repo_root
  local repo_root qwen_bin run_root home xdg qwen_home project data_root port_file log_file
  local stdout_file stderr_file install_json command_path port ctx_bin

  repo_root="${PWD}"
  qwen_bin="$(ensure_npm_binary)"
  run "${qwen_bin}" --version
  export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
  run cargo build --locked -p ctx
  ctx_bin="${CARGO_TARGET_DIR:-${repo_root}/target}/debug/ctx"
  [[ -x "${ctx_bin}" ]] || fail "built ctx binary not found at ${ctx_bin}"

  run_root="${CTX_REAL_HARNESS_RUN_ROOT:-${PWD}/target-local/real-harness-runs}/qwen-slash-$$"
  rm -rf "${run_root}"
  mkdir -p "${run_root}"
  home="${run_root}/home"
  xdg="${run_root}/xdg-config"
  qwen_home="${run_root}/qwen-home"
  project="${run_root}/project"
  data_root="${run_root}/ctx-data"
  mkdir -p "${home}" "${xdg}" "${qwen_home}" "${project}" "${data_root}"

  install_json="${run_root}/slash-install.json"
  stdout_file="${run_root}/qwen.stdout"
  stderr_file="${run_root}/qwen.stderr"
  port_file="${run_root}/fixture.port"
  log_file="${run_root}/fixture-requests.jsonl"
  command_path="${project}/.qwen/commands/ctx-history.md"

  (
    cd "${project}"
    PATH="$(dirname "${ctx_bin}"):${PATH}" \
      HOME="${home}" \
      XDG_CONFIG_HOME="${xdg}" \
      QWEN_HOME="${qwen_home}" \
      CTX_DATA_ROOT="${data_root}" \
      CTX_ANALYTICS_OFF=1 \
      run "${ctx_bin}" integrations install slash-commands --agent qwen-code --project --json
  ) > "${install_json}"
  require_contains "${install_json}" '"agent": "qwen-code"'
  require_contains "${install_json}" '"status": "current"'
  require_contains "${command_path}" 'User request: {{args}}'

  CTX_SLASH_EXPECTED_QUERY="${QUERY}" \
    run python3 scripts/real-harness-slash-fixture-server.py qwen "${port_file}" "${log_file}" &
  server_pid=$!
  wait_for_port "${port_file}"
  port="$(cat "${port_file}")"

  (
    cd "${project}"
    HOME="${home}" \
      XDG_CONFIG_HOME="${xdg}" \
      QWEN_HOME="${qwen_home}" \
      CTX_DATA_ROOT="${data_root}" \
      CTX_ANALYTICS_OFF=1 \
      PATH="$(dirname "${ctx_bin}"):${PATH}" \
      OPENAI_API_KEY="sk-ctx-real-harness-fixture" \
      OPENAI_BASE_URL="http://127.0.0.1:${port}/v1" \
      run "${qwen_bin}" \
        --auth-type openai \
        --model "${MODEL}" \
        --prompt "/ctx-history ${QUERY}" \
        --output-format text \
        --sandbox=false
  ) > "${stdout_file}" 2> "${stderr_file}"

  require_contains "${stdout_file}" 'fixture-qwen-slash-ok'
  require_contains "${log_file}" '"provider":"qwen"'
  require_contains "${log_file}" '"has_ctx_history_expansion":true'
  require_contains "${log_file}" '"has_expected_user_request":true'
  require_contains "${log_file}" '"has_ctx_citations_instruction":true'
  require_not_contains "${log_file}" '"has_raw_slash_invocation":true'

  printf 'real Qwen slash harness E2E passed: %s\n' "${run_root}"
}

main "$@"
