#!/usr/bin/env bash
set -euo pipefail

mode="${1:-cargo_test_default}"

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
  export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
  export RUSTUP_TOOLCHAIN="${RUSTUP_TOOLCHAIN:-${CTX_RUST_TOOLCHAIN:-stable}}"
  export RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
}

run() {
  printf '==>'
  printf ' %q' "$@"
  printf '\n'
  "$@"
}

run_cargo_test() {
  run cargo test --locked "$@"
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
  cargo_fmt_check)
    run cargo fmt --all -- --check
    ;;
  cargo_check)
    run cargo check --workspace --locked
    ;;
  cargo_clippy)
    run cargo clippy --workspace --all-targets --locked -- -D warnings
    ;;
  cargo_test_default)
    run_cargo_test --workspace
    ;;
  cli_contract_tests)
    run_cargo_test -p ctx --test cli_public_help_docs help_exposes_session_retrieval_commands
    run_cargo_test -p ctx --test cli_public_help_docs public_subcommand_help_is_golden_enough_for_session_retrieval
    run_cargo_test -p ctx --test cli_public_help_docs provider_help_and_errors_do_not_dump_full_provider_list
    run_cargo_test -p ctx --test cli_public_help_docs docs_commands_expose_embedded_docs_and_man_pages
    run_cargo_test -p ctx --test upgrade upgrade_status_check_and_apply_support_managed_installs
    run_cargo_test -p ctx --test upgrade json_commands_do_not_spawn_background_upgrade
    ;;
  slash_command_e2e)
    run_cargo_test -p ctx --test slash_command_e2e
    ;;
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
  mcp_integration_e2e)
    run_cargo_test -p ctx --test mcp_integration_e2e
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
  loc_check)
    run bash scripts/check-loc.sh
    ;;
  source_diff_check)
    run_source_diff_check
    ;;
  package_audit_fast)
    CTX_AUDIT_SKIP_RELEASE_BUILD=1 run bash scripts/audit-search-mvp-package.sh
    ;;
  sdk_contract_checks)
    run bash scripts/check-sdks.sh
    ;;
  sdk_package_dry_run)
    run bash scripts/sdk-package-dry-run.sh
    ;;
  package_audit_release)
    run bash scripts/audit-search-mvp-package.sh
    ;;
  fresh_home_e2e)
    run_cargo_test -p ctx --test search_show_locate_sql fresh_home_search_mvp_flow
    ;;
  provider_fixture_e2e)
    run_cargo_test -p ctx --test search_show_locate_sql codex_cli_provider_oracle_covers_retrieval_and_claimed_fidelity
    run_cargo_test -p ctx --test search_show_locate_sql pi_cli_import_search_flow
    run_cargo_test -p ctx --test native_providers native_provider_cli_flow_imports_supported_provider_paths
    run_cargo_test -p ctx --test native_providers native_provider_cli_requires_existing_history_or_explicit_path
    run_cargo_test -p ctx --test native_providers antigravity_cli_imports_native_transcript_tree
    ;;
  local_transcript_oracle)
    run_cargo_test -p ctx --test search_show_locate_sql local_transcript_oracle_preserves_cli_json_and_sqlite
    ;;
  search_determinism_tests)
    run_cargo_test -p ctx-history-search search_packet_is_deterministic_for_large_history_and_equal_ties_use_record_id
    ;;
  *)
    fail "unknown bazel test mode: ${mode}"
    ;;
esac
