#!/usr/bin/env bash
set -euo pipefail

mode="${1:-docs_check}"

fail() {
  printf 'bazel gate failed: %s\n' "$*" >&2
  exit 1
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

init_env() {
  find_repo_root
  # shellcheck source=scripts/ci-common.sh
  source "${PWD}/scripts/ci-common.sh"
  ctx_init_bazel_test_env
  ctx_init_resource_env
}

run() {
  printf '==>'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

run_real_harness() {
  local script="$1"
  local ctx_bin="${2:-}"
  if [[ -n "${ctx_bin}" ]]; then
    run env CTX_REAL_HARNESS_CTX_BIN="${ctx_bin}" bash "${script}"
  else
    run bash "${script}"
  fi
}

run_source_diff_check() {
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    run git diff --check
    run git diff --cached --check
    return 0
  fi

  if command -v rg >/dev/null 2>&1; then
    if rg -n '^(<<<<<<<|=======|>>>>>>>)' . \
      --glob '!target/**' \
      --glob '!bazel-*' \
      --glob '!Cargo.lock'; then
      fail 'conflict markers found'
    fi
    return 0
  fi

  if grep -R -n -E '^(<<<<<<<|=======|>>>>>>>)' . \
    --exclude-dir=target \
    --exclude-dir='bazel-*' \
    --exclude='Cargo.lock'; then
    fail 'conflict markers found'
  fi
}

init_env

case "${mode}" in
  real_harness_codex_skill_e2e)
    run_real_harness scripts/real-harness-codex-skill-e2e.sh "${2:-}"
    ;;
  real_harness_gemini_slash_e2e)
    run_real_harness scripts/real-harness-gemini-slash-e2e.sh "${2:-}"
    ;;
  real_harness_qwen_slash_e2e)
    run_real_harness scripts/real-harness-qwen-slash-e2e.sh "${2:-}"
    ;;
  docs_check)
    run bash scripts/check-docs.sh
    ;;
  real_harness_codex_mcp_e2e)
    run_real_harness scripts/real-harness-codex-mcp-e2e.sh "${2:-}"
    ;;
  real_harness_qwen_mcp_e2e)
    run_real_harness scripts/real-harness-qwen-mcp-e2e.sh "${2:-}"
    ;;
  real_harness_claude_mcp_e2e)
    run_real_harness scripts/real-harness-claude-mcp-e2e.sh "${2:-}"
    ;;
  real_harness_gemini_mcp_e2e)
    run_real_harness scripts/real-harness-gemini-mcp-e2e.sh "${2:-}"
    ;;
  real_harness_opencode_mcp_e2e)
    run_real_harness scripts/real-harness-opencode-mcp-e2e.sh "${2:-}"
    ;;
  installer_path_smoke)
    run bash scripts/install-path-smoke.sh
    ;;
  buildkite_pipeline_check)
    run bash scripts/check-buildkite-pipeline.sh
    ;;
  release_binary_compat_tests)
    run bash scripts/tests/check-release-binary-compat-test.sh
    ;;
  native_candidate_smoke_tests)
    run bash scripts/tests/run-native-candidate-smoke-test.sh
    run bash scripts/tests/smoke-daemon-semantic-release-test.sh
    if command -v pwsh >/dev/null 2>&1; then
      run pwsh -NoLogo -NoProfile -File scripts/tests/run-native-candidate-smoke-test.ps1
    fi
    ;;
  linux_release_construction_tests)
    run bash scripts/test-linux-release-construction.sh
    ;;
  macos_release_signing_tests)
    run bash scripts/tests/macos-release-signing-test.sh
    ;;
  loc_check)
    loc_scc="${2:-}"
    loc_manifest="${3:-}"
    [[ -n "${loc_scc}" ]] || fail 'loc_check requires the pinned scc runfile'
    [[ -n "${loc_manifest}" && -f "${loc_manifest}" ]] || \
      fail 'loc_check requires the declared source manifest'
    loc_manifest="$(cd "$(dirname "${loc_manifest}")" && pwd -P)/$(basename "${loc_manifest}")"
    [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]] || \
      fail 'loc_check requires the Bazel runfiles repository root'
    loc_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
    run bash scripts/tests/check_loc_bazel_inputs_test.sh "${loc_scc}" "${loc_manifest}"
    run env CTX_LOC_SCC="${loc_scc}" bash scripts/tests/check_loc_test.sh
    run env \
      CTX_LOC_PATHS_MANIFEST="${loc_manifest}" \
      CTX_LOC_ROOT="${loc_root}" \
      CTX_LOC_SCC="${loc_scc}" \
      bash scripts/check-loc.sh
    ;;
  public_control_surface_check)
    run bash scripts/tests/check-public-control-surface-test.sh
    run python3 scripts/check-public-control-surface.py
    ;;
  source_diff_check)
    run_source_diff_check
    ;;
  package_audit_fast)
    CTX_AUDIT_SKIP_RELEASE_BUILD=1 CTX_AUDIT_CTX_BINARY="${2:-}" run bash scripts/audit-search-mvp-package.sh
    ;;
  sdk_contract_checks)
    run bash scripts/check-sdks.sh
    ;;
  sdk_package_dry_run)
    CTX_SDK_CARGO="${2:-}" \
      CTX_SDK_RUSTC="${3:-}" \
      CTX_SDK_CARGO_VENDOR_MANIFEST="${4:-}" \
      run bash scripts/sdk-package-dry-run.sh
    ;;
  package_audit_release)
    CTX_AUDIT_CTX_BINARY="${2:-}" run bash scripts/audit-search-mvp-package.sh
    ;;
  *)
    fail "unknown bazel test mode: ${mode}"
    ;;
esac
