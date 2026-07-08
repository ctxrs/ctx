#!/usr/bin/env bash
set -euo pipefail

PACKAGE="@openai/codex"
VERSION="0.143.0"
MODEL="gpt-5.5"

fail() {
  printf 'real Codex skill E2E failed: %s\n' "$*" >&2
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

ensure_codex() {
  local cache_root install_root tmp_root marker
  cache_root="${CTX_REAL_HARNESS_CACHE:-${PWD}/target-local/real-harness-cache}"
  install_root="${cache_root}/npm/$(cache_key)"
  marker="${install_root}/.ctx-installed-package"
  if [[ -x "${install_root}/node_modules/.bin/codex" ]] && [[ -f "${marker}" ]] && [[ "$(cat "${marker}")" == "${PACKAGE}@${VERSION}" ]]; then
    printf '%s\n' "${install_root}/node_modules/.bin/codex"
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
  printf '%s\n' "${install_root}/node_modules/.bin/codex"
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
  local codex_bin ctx_bin run_root home codex_home project data_root port_file log_file server_pid port
  local stdout_file stderr_file install_json skill_path

  codex_bin="$(ensure_codex)"
  run "${codex_bin}" --version
  export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-stable}"
  run cargo build --locked -p ctx
  ctx_bin="${CARGO_TARGET_DIR:-${PWD}/target}/debug/ctx"
  [[ -x "${ctx_bin}" ]] || fail "built ctx binary not found at ${ctx_bin}"

  run_root="${CTX_REAL_HARNESS_RUN_ROOT:-${PWD}/target-local/real-harness-runs}/codex-skill-$$"
  rm -rf "${run_root}"
  mkdir -p "${run_root}"
  home="${run_root}/home"
  codex_home="${home}/.codex"
  project="${run_root}/project"
  data_root="${run_root}/ctx-data"
  mkdir -p "${codex_home}" "${project}" "${data_root}"

  install_json="${run_root}/skill-install.json"
  stdout_file="${run_root}/codex.stdout"
  stderr_file="${run_root}/codex.stderr"
  port_file="${run_root}/fixture.port"
  log_file="${run_root}/fixture-requests.jsonl"
  skill_path="${codex_home}/skills/ctx-agent-history-search/SKILL.md"

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    CODEX_HOME="${codex_home}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    run "${ctx_bin}" skill install --agent codex --json > "${install_json}"
  require_contains "${install_json}" '"agent":"codex"'
  require_contains "${install_json}" '"status":"current"'
  require_contains "${skill_path}" 'name: ctx-agent-history-search'
  require_contains "${skill_path}" 'Use ctx to search local coding-agent history'

  run python3 scripts/real-harness-codex-skill-fixture-server.py "${port_file}" "${log_file}" &
  server_pid=$!
  for _ in {1..100}; do
    [[ -s "${port_file}" ]] && break
    sleep 0.05
  done
  [[ -s "${port_file}" ]] || fail 'fixture Responses server did not publish a port'
  port="$(cat "${port_file}")"

  PATH="$(dirname "${ctx_bin}"):${PATH}" \
    CODEX_HOME="${codex_home}" \
    HOME="${home}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_ANALYTICS_OFF=1 \
    OPENAI_API_KEY="sk-ctx-real-harness-fixture" \
    run "${codex_bin}" exec \
      --skip-git-repo-check \
      --sandbox read-only \
      --color never \
      -m "${MODEL}" \
      -c 'model_provider="fixture"' \
      -c 'model_providers.fixture.name="Fixture"' \
      -c "model_providers.fixture.base_url=\"http://127.0.0.1:${port}/v1\"" \
      -c 'model_providers.fixture.env_key="OPENAI_API_KEY"' \
      -c 'model_providers.fixture.wire_api="responses"' \
      -C "${project}" \
      'Use the ctx-agent-history-search skill and verify ctx is available.' \
      > "${stdout_file}" 2> "${stderr_file}"

  wait "${server_pid}"

  require_contains "${stdout_file}" 'fixture-ctx-skill-ok'
  require_contains "${stderr_file}" "/bin/bash -lc 'ctx --version'"
  require_contains "${stderr_file}" 'ctx 0.21.0'
  require_contains "${log_file}" '"has_ctx_skill":true'
  require_contains "${log_file}" '"has_ctx_skill_description":true'
  require_contains "${log_file}" '"has_ctx_skill_path":true'
  require_contains "${log_file}" '"call_id":"call_ctx_version"'
  require_contains "${log_file}" 'ctx 0.21.0'

  printf 'real Codex skill harness E2E passed: %s\n' "${run_root}"
}

main "$@"
