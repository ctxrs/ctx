#!/usr/bin/env bash
set -euo pipefail

PACKAGE="opencode-ai"
VERSION="1.17.15"
NATIVE_PACKAGE="opencode-linux-x64"
MODEL="test-model"

fail() {
  printf 'real OpenCode MCP E2E failed: %s\n' "$*" >&2
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

ensure_opencode() {
  local cache_root install_root tmp_root marker native_bin
  cache_root="${CTX_REAL_HARNESS_CACHE:-${PWD}/target-local/real-harness-cache}"
  install_root="${cache_root}/npm/$(cache_key)"
  marker="${install_root}/.ctx-installed-package"
  native_bin="${install_root}/node_modules/${NATIVE_PACKAGE}/bin/opencode"
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
  if [[ ! -x "${tmp_root}/node_modules/${NATIVE_PACKAGE}/bin/opencode" ]]; then
    fail "expected pinned native OpenCode package ${NATIVE_PACKAGE}@${VERSION} to install without lifecycle scripts"
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
  local opencode_bin ctx_bin run_root home xdg_config_home project data_root port_file log_file server_pid port
  local stdout_file stderr_file install_json config_json

  opencode_bin="$(ensure_opencode)"
  run "${opencode_bin}" --version
  ctx_bin="$(resolve_ctx_bin)"

  run_root="${CTX_REAL_HARNESS_RUN_ROOT:-${PWD}/target-local/real-harness-runs}/opencode-mcp-$$"
  rm -rf "${run_root}"
  mkdir -p "${run_root}"
  home="${run_root}/home"
  xdg_config_home="${run_root}/xdg-config"
  project="${run_root}/project"
  data_root="${run_root}/ctx-data"
  mkdir -p "${home}" "${xdg_config_home}/opencode" "${project}" "${data_root}"

  install_json="${run_root}/mcp-install.json"
  config_json="${xdg_config_home}/opencode/opencode.json"
  stdout_file="${run_root}/opencode.stdout"
  stderr_file="${run_root}/opencode.stderr"
  port_file="${run_root}/fixture.port"
  log_file="${run_root}/fixture-requests.jsonl"

  python3 - "${config_json}" <<'PY'
import json
import sys

with open(sys.argv[1], "w", encoding="utf-8") as handle:
    json.dump(
        {
            "autoupdate": False,
            "provider": {
                "mock": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "Mock Provider",
                    "options": {
                        "baseURL": "http://127.0.0.1:1/v1",
                        "apiKey": "test",
                    },
                    "models": {
                        "test-model": {
                            "name": "Test Model",
                            "limit": {"context": 128000, "output": 4096},
                        }
                    },
                }
            },
        },
        handle,
    )
PY

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    HOME="${home}" \
    XDG_CONFIG_HOME="${xdg_config_home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    run "${ctx_bin}" integrations install mcp --agent opencode --json > "${install_json}"
  require_contains "${install_json}" '"agent":"opencode"'
  require_contains "${install_json}" '"status":"current"'
  require_contains "${config_json}" '"mcp"'
  require_contains "${config_json}" '"ctx"'
  require_contains "${config_json}" '"command"'
  require_contains "${config_json}" '"provider"'

  run python3 scripts/real-harness-opencode-mcp-fixture-server.py "${port_file}" "${log_file}" &
  server_pid=$!
  for _ in {1..100}; do
    [[ -s "${port_file}" ]] && break
    sleep 0.05
  done
  [[ -s "${port_file}" ]] || fail 'fixture OpenAI-compatible server did not publish a port'
  port="$(cat "${port_file}")"

  python3 - "${config_json}" "${port}" <<'PY'
import json
import sys

config_path, port = sys.argv[1:]
with open(config_path, encoding="utf-8") as handle:
    config = json.load(handle)
config["provider"]["mock"]["options"]["baseURL"] = f"http://127.0.0.1:{port}/v1"
with open(config_path, "w", encoding="utf-8") as handle:
    json.dump(config, handle)
PY

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    HOME="${home}" \
    XDG_CONFIG_HOME="${xdg_config_home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    run "${opencode_bin}" run \
      'Discover ctx MCP tools, call ctx status, then report success.' \
      --model "mock/${MODEL}" \
      --format json \
      --auto \
      > "${stdout_file}" 2> "${stderr_file}"

  wait "${server_pid}"

  require_contains "${stdout_file}" 'fixture-opencode-mcp-ok'
  require_contains "${log_file}" '"ctx_status"'
  require_contains "${log_file}" '"call_ctx_status"'
  require_contains "${log_file}" 'initialized: false'
  require_contains "${log_file}" 'read_only: true'

  printf 'real OpenCode MCP harness E2E passed: %s\n' "${run_root}"
}

main "$@"
