#!/usr/bin/env bash
set -euo pipefail

PACKAGE="@qwen-code/qwen-code"
VERSION="0.19.7"
MODEL="test-model"

fail() {
  printf 'real Qwen MCP E2E failed: %s\n' "$*" >&2
  exit 1
}

run() {
  printf '==>'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

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

ensure_qwen() {
  local cache_root install_root tmp_root marker
  cache_root="${CTX_REAL_HARNESS_CACHE:-${PWD}/target-local/real-harness-cache}"
  install_root="${cache_root}/npm/$(cache_key)"
  marker="${install_root}/.ctx-installed-package"
  if [[ -x "${install_root}/node_modules/.bin/qwen" ]] && [[ -f "${marker}" ]] && [[ "$(cat "${marker}")" == "${PACKAGE}@${VERSION}" ]]; then
    printf '%s\n' "${install_root}/node_modules/.bin/qwen"
    return 0
  fi

  command -v npm >/dev/null 2>&1 || fail 'npm is required for pinned real harness installs'
  mkdir -p "${cache_root}/npm"
  tmp_root="${install_root}.tmp.$$"
  rm -rf "${tmp_root}"
  mkdir -p "${tmp_root}"
  run npm install --prefix "${tmp_root}" --ignore-scripts --no-audit --no-fund "${PACKAGE}@${VERSION}" >&2
  printf '%s\n' "${PACKAGE}@${VERSION}" > "${tmp_root}/.ctx-installed-package"
  rm -rf "${install_root}"
  mv "${tmp_root}" "${install_root}"
  printf '%s\n' "${install_root}/node_modules/.bin/qwen"
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

main() {
  find_repo_root
  local qwen_bin ctx_bin run_root home qwen_home project data_root port_file log_file server_pid port
  local stdout_file stderr_file install_json settings_json

  qwen_bin="$(ensure_qwen)"
  run "${qwen_bin}" --version
  export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
  run cargo build --locked -p ctx
  ctx_bin="${CARGO_TARGET_DIR:-${PWD}/target}/debug/ctx"
  [[ -x "${ctx_bin}" ]] || fail "built ctx binary not found at ${ctx_bin}"

  run_root="${CTX_REAL_HARNESS_RUN_ROOT:-${PWD}/target-local/real-harness-runs}/qwen-mcp-$$"
  rm -rf "${run_root}"
  mkdir -p "${run_root}"
  home="${run_root}/home"
  qwen_home="${home}/.qwen"
  project="${run_root}/project"
  data_root="${run_root}/ctx-data"
  mkdir -p "${qwen_home}" "${project}" "${data_root}"

  install_json="${run_root}/mcp-install.json"
  settings_json="${qwen_home}/settings.json"
  stdout_file="${run_root}/qwen.stdout"
  stderr_file="${run_root}/qwen.stderr"
  port_file="${run_root}/fixture.port"
  log_file="${run_root}/fixture-requests.jsonl"

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    QWEN_HOME="${qwen_home}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    run "${ctx_bin}" integrations install mcp --agent qwen-code --json > "${install_json}"
  require_contains "${install_json}" '"agent":"qwen-code"'
  require_contains "${install_json}" '"status":"current"'
  require_contains "${settings_json}" '"mcpServers"'
  require_contains "${settings_json}" '"ctx"'
  require_contains "${settings_json}" '"command": "ctx"'

  run python3 scripts/real-harness-qwen-mcp-fixture-server.py "${port_file}" "${log_file}" &
  server_pid=$!
  for _ in {1..100}; do
    [[ -s "${port_file}" ]] && break
    sleep 0.05
  done
  [[ -s "${port_file}" ]] || fail 'fixture OpenAI-compatible server did not publish a port'
  port="$(cat "${port_file}")"

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    QWEN_HOME="${qwen_home}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    OPENAI_API_KEY="sk-ctx-real-harness-fixture" \
    OPENAI_BASE_URL="http://127.0.0.1:${port}/v1" \
    OPENAI_MODEL="${MODEL}" \
    QWEN_CODE_SUPPRESS_YOLO_WARNING=1 \
    run "${qwen_bin}" \
      -p 'Discover ctx MCP tools, call ctx status, then report success.' \
      --model "${MODEL}" \
      --output-format json \
      > "${stdout_file}" 2> "${stderr_file}"

  wait "${server_pid}"

  require_contains "${stdout_file}" 'fixture-qwen-mcp-ok'
  require_contains "${log_file}" '"mcp__ctx__status"'
  require_contains "${log_file}" '"call_ctx_status"'
  require_contains "${log_file}" 'initialized: false'
  require_contains "${log_file}" 'read_only: true'

  printf 'real Qwen MCP harness E2E passed: %s\n' "${run_root}"
}

main "$@"
