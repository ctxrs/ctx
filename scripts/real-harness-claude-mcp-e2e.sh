#!/usr/bin/env bash
set -euo pipefail

PACKAGE="@anthropic-ai/claude-code"
VERSION="2.1.204"
NATIVE_PACKAGE="@anthropic-ai/claude-code-linux-x64"
MODEL="mock-model"

fail() {
  printf 'real Claude MCP E2E failed: %s\n' "$*" >&2
  exit 1
}

run() {
  printf '==>'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

# shellcheck source=scripts/real-harness-common.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/real-harness-common.sh"

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

ensure_claude() {
  local cache_root install_root tmp_root marker native_bin
  cache_root="${CTX_REAL_HARNESS_CACHE:-${PWD}/target-local/real-harness-cache}"
  install_root="${cache_root}/npm/$(cache_key)"
  marker="${install_root}/.ctx-installed-package"
  native_bin="${install_root}/node_modules/${NATIVE_PACKAGE}/claude"
  if [[ -x "${native_bin}" ]] && [[ -f "${marker}" ]] && [[ "$(cat "${marker}")" == "${PACKAGE}@${VERSION}" ]]; then
    printf '%s\n' "${native_bin}"
    return 0
  fi

  command -v npm >/dev/null 2>&1 || fail 'npm is required for pinned real harness installs'
  mkdir -p "${cache_root}/npm"
  tmp_root="${install_root}.tmp.$$"
  rm -rf "${tmp_root}"
  mkdir -p "${tmp_root}"
  run npm install --prefix "${tmp_root}" --ignore-scripts --no-audit --no-fund "${PACKAGE}@${VERSION}" >&2
  if [[ ! -x "${tmp_root}/node_modules/${NATIVE_PACKAGE}/claude" ]]; then
    fail "expected pinned native Claude package ${NATIVE_PACKAGE}@${VERSION} to install without lifecycle scripts"
  fi
  printf '%s\n' "${PACKAGE}@${VERSION}" > "${tmp_root}/.ctx-installed-package"
  rm -rf "${install_root}"
  mv "${tmp_root}" "${install_root}"
  printf '%s\n' "${native_bin}"
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
  local claude_bin ctx_bin run_root home claude_config_dir project data_root port_file log_file server_pid port
  local stdout_file stderr_file install_json claude_config_json

  claude_bin="$(ensure_claude)"
  run "${claude_bin}" --version
  ctx_bin="$(resolve_ctx_bin)"

  run_root="${CTX_REAL_HARNESS_RUN_ROOT:-${PWD}/target-local/real-harness-runs}/claude-mcp-$$"
  rm -rf "${run_root}"
  mkdir -p "${run_root}"
  home="${run_root}/home"
  claude_config_dir="${home}/.claude"
  project="${run_root}/project"
  data_root="${run_root}/ctx-data"
  mkdir -p "${claude_config_dir}" "${project}" "${data_root}"

  install_json="${run_root}/mcp-install.json"
  claude_config_json="${claude_config_dir}/.claude.json"
  stdout_file="${run_root}/claude.stdout"
  stderr_file="${run_root}/claude.stderr"
  port_file="${run_root}/fixture.port"
  log_file="${run_root}/fixture-requests.jsonl"

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    CLAUDE_CONFIG_DIR="${claude_config_dir}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    run "${ctx_bin}" integrations install mcp --agent claude-code --json > "${install_json}"
  require_contains "${install_json}" '"agent":"claude-code"'
  require_contains "${install_json}" '"status":"current"'
  require_contains "${claude_config_json}" '"mcpServers"'
  require_contains "${claude_config_json}" '"ctx"'
  require_contains "${claude_config_json}" '"command": "ctx"'

  run python3 scripts/real-harness-claude-mcp-fixture-server.py "${port_file}" "${log_file}" &
  server_pid=$!
  for _ in {1..100}; do
    [[ -s "${port_file}" ]] && break
    sleep 0.05
  done
  [[ -s "${port_file}" ]] || fail 'fixture Anthropic server did not publish a port'
  port="$(cat "${port_file}")"

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    CLAUDE_CONFIG_DIR="${claude_config_dir}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    ANTHROPIC_API_KEY="sk-ctx-real-harness-fixture" \
    ANTHROPIC_BASE_URL="http://127.0.0.1:${port}" \
    CLAUDE_CODE_SKIP_PROMPT_HISTORY=1 \
    CLAUDE_CODE_SIMPLE=1 \
    DISABLE_AUTOUPDATER=1 \
    run "${claude_bin}" \
      --bare \
      -p \
      --output-format stream-json \
      --verbose \
      --model "${MODEL}" \
      --max-turns 2 \
      --permission-mode bypassPermissions \
      --no-session-persistence \
      --strict-mcp-config \
      --mcp-config "${claude_config_json}" \
      -- \
      'Call ctx sources once.' \
      > "${stdout_file}" 2> "${stderr_file}"

  wait "${server_pid}"

  require_contains "${stdout_file}" 'CALLED_CTX_SOURCES'
  require_contains "${stdout_file}" '"name":"ctx"'
  require_contains "${stdout_file}" '"status":"connected"'
  require_contains "${stdout_file}" '"mcp__ctx__sources"'
  require_contains "${stdout_file}" '"tool_result"'
  require_contains "${log_file}" '"mcp__ctx__sources"'
  require_contains "${log_file}" '"toolu_ctx_sources"'
  require_contains "${log_file}" 'read_only'

  printf 'real Claude MCP harness E2E passed: %s\n' "${run_root}"
}

main "$@"
