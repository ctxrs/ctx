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
  export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
  export RUST_TEST_THREADS="${RUST_TEST_THREADS:-2}"
  if [[ -z "${HOME:-}" ]]; then
    local user_home
    user_home="$(getent passwd "$(id -un)" | cut -d: -f6 || true)"
    if [[ -n "${user_home}" && -d "${user_home}" ]]; then
      export HOME="${user_home}"
    else
      export HOME="${PWD}/target/bazel-home"
    fi
  fi
  mkdir -p "${HOME}"
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
  docs_check)
    run bash scripts/check-docs.sh
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
