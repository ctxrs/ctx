#!/usr/bin/env bash
set -euo pipefail

PACKAGE="@google/gemini-cli"
VERSION="0.49.0"
MODEL="gemini-2.5-flash"

fail() {
  printf 'real Gemini MCP E2E failed: %s\n' "$*" >&2
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

ensure_gemini() {
  local cache_root install_root tmp_root marker
  cache_root="${CTX_REAL_HARNESS_CACHE:-${PWD}/target-local/real-harness-cache}"
  install_root="${cache_root}/npm/$(cache_key)"
  marker="${install_root}/.ctx-installed-package"
  if [[ -x "${install_root}/node_modules/.bin/gemini" ]] && [[ -f "${marker}" ]] && [[ "$(cat "${marker}")" == "${PACKAGE}@${VERSION}" ]]; then
    printf '%s\n' "${install_root}/node_modules/.bin/gemini"
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
  printf '%s\n' "${install_root}/node_modules/.bin/gemini"
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
  local gemini_bin ctx_bin run_root home project data_root port_file log_file server_pid port
  local stdout_file stderr_file install_json settings_json

  gemini_bin="$(ensure_gemini)"
  run "${gemini_bin}" --version
  export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
  run cargo build --locked -p ctx
  ctx_bin="${CARGO_TARGET_DIR:-${PWD}/target}/debug/ctx"
  [[ -x "${ctx_bin}" ]] || fail "built ctx binary not found at ${ctx_bin}"

  run_root="${CTX_REAL_HARNESS_RUN_ROOT:-${PWD}/target-local/real-harness-runs}/gemini-mcp-$$"
  rm -rf "${run_root}"
  mkdir -p "${run_root}"
  home="${run_root}/home"
  project="${run_root}/project"
  data_root="${run_root}/ctx-data"
  mkdir -p "${home}/.gemini" "${project}" "${data_root}"

  install_json="${run_root}/mcp-install.json"
  settings_json="${home}/.gemini/settings.json"
  stdout_file="${run_root}/gemini.stdout"
  stderr_file="${run_root}/gemini.stderr"
  port_file="${run_root}/fixture.port"
  log_file="${run_root}/fixture-requests.jsonl"

  printf '%s\n' '{"security":{"auth":{"selectedType":"gemini-api-key"}}}' > "${settings_json}"
  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    run "${ctx_bin}" integrations install mcp --agent gemini-cli --json > "${install_json}"
  require_contains "${install_json}" '"agent":"gemini-cli"'
  require_contains "${install_json}" '"status":"current"'
  require_contains "${settings_json}" '"mcpServers"'
  require_contains "${settings_json}" '"ctx"'
  require_contains "${settings_json}" '"command": "ctx"'
  require_contains "${settings_json}" '"selectedType"'

  run python3 scripts/real-harness-gemini-mcp-fixture-server.py "${port_file}" "${log_file}" &
  server_pid=$!
  for _ in {1..100}; do
    [[ -s "${port_file}" ]] && break
    sleep 0.05
  done
  [[ -s "${port_file}" ]] || fail 'fixture Google GenAI server did not publish a port'
  port="$(cat "${port_file}")"

  (
    cd "${project}"
    PATH="$(dirname "${ctx_bin}"):${PATH}" \
      HOME="${home}" \
      CTX_DATA_ROOT="${data_root}" \
      CTX_ANALYTICS_OFF=1 \
      GEMINI_API_KEY="sk-ctx-real-harness-fixture" \
      GOOGLE_GEMINI_BASE_URL="http://127.0.0.1:${port}" \
      GEMINI_CLI_TRUST_WORKSPACE=true \
      run "${gemini_bin}" \
        -p 'Discover ctx MCP tools, call ctx status, then report success.' \
        --model "${MODEL}" \
        --output-format json \
        --allowed-mcp-server-names ctx \
        --skip-trust
  ) > "${stdout_file}" 2> "${stderr_file}"

  wait "${server_pid}"

  require_contains "${stdout_file}" 'fixture-gemini-mcp-ok'
  require_contains "${log_file}" '"mcp_ctx_status"'
  require_contains "${log_file}" '"functionResponse"'
  require_contains "${log_file}" 'initialized: false'
  require_contains "${log_file}" 'read_only: true'

  printf 'real Gemini MCP harness E2E passed: %s\n' "${run_root}"
}

main "$@"
