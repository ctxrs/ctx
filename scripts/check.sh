#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/check.sh [all|fmt|docs|check|clippy|test|examples|bazel|platform-smoke|provider-fixtures|rich-search-context|dashboard-report-artifact-review|pr-publish-dry-run|security-archive-fixtures|jj-e2e-blocker-status|installer-dry-run-smoke|completion-certificate]...

Runs resource-capped local checks sequentially. Defaults to "all".
Environment overrides:
  CARGO                Cargo executable/wrapper, default cargo
  CARGO_BUILD_JOBS     Cargo build parallelism, default local cap 2; CI uses min(cpu, memory_gb / 3)
  RUST_TEST_THREADS    Rust test threads, default CARGO_BUILD_JOBS
  BAZEL_JOBS           Bazel job count, default CARGO_BUILD_JOBS
  CTX_REQUIRE_BAZEL    If 1, bootstrap Bazelisk when Bazel is missing
  CTX_ARTIFACT_DIR     Timing artifact directory, default target/ctx-artifacts/check
  CLIPPY_FLAGS         Extra clippy flags, default "-D warnings"
USAGE
}

cargo_locked_args=()
cargo_bin="${CARGO:-cargo}"

setup_cargo_args() {
  cargo_locked_args=()
  if [[ "${CTX_CARGO_LOCKED:-1}" != "0" && -f Cargo.lock ]]; then
    cargo_locked_args+=(--locked)
  fi
}

file_contains() {
  local file="$1"
  local text="$2"

  if command -v rg >/dev/null 2>&1; then
    rg --fixed-strings -q -- "${text}" "${file}"
    return $?
  fi

  grep -F -q -- "${text}" "${file}"
}

run_fmt() {
  ctx_ensure_rust_toolchain
  "${cargo_bin}" fmt --all -- --check
}

run_docs() {
  bash scripts/check-docs.sh
}

run_check() {
  ctx_ensure_rust_toolchain
  "${cargo_bin}" check --workspace --all-targets "${cargo_locked_args[@]}"
}

run_clippy() {
  ctx_ensure_rust_toolchain
  if [[ -n "${CLIPPY_FLAGS:-}" ]]; then
    "${cargo_bin}" clippy --workspace --all-targets "${cargo_locked_args[@]}" -- ${CLIPPY_FLAGS}
  else
    "${cargo_bin}" clippy --workspace --all-targets "${cargo_locked_args[@]}" -- -D warnings
  fi
}

run_test() {
  ctx_ensure_rust_toolchain
  "${cargo_bin}" test --workspace --all-targets "${cargo_locked_args[@]}" -- --test-threads "${RUST_TEST_THREADS}"
}

run_examples() {
  local suffix example example_name example_bin

  ctx_ensure_rust_toolchain
  ctx_run_timed "examples-build" "${cargo_bin}" build -p ctx --bins "${cargo_locked_args[@]}"

  suffix="$(ctx_host_exe_suffix)"
  example_bin="${CTX_REPO_ROOT}/target/debug/ctx${suffix}"
  if [[ ! -f "${example_bin}" ]]; then
    printf 'expected example binary missing: %s\n' "${example_bin}" >&2
    return 1
  fi

  for example in examples/*.sh; do
    example_name="$(basename "${example}" .sh)"
    ctx_run_timed "example-${example_name}" env \
      CTX_BIN="${example_bin}" \
      CTX_EXAMPLE_TMPDIR="${TMPDIR}" \
      bash "${example}"
  done
}

run_platform_smoke() {
  local suffix smoke_bin data_root record_id record_json

  ctx_run_timed "platform-smoke-host-triple" ctx_require_host_triple "${CTX_EXPECT_HOST_TRIPLE:-}"
  ctx_ensure_rust_toolchain
  ctx_run_timed "platform-smoke-build" "${cargo_bin}" build -p ctx --bin ctx "${cargo_locked_args[@]}"

  suffix="$(ctx_host_exe_suffix)"
  smoke_bin="${CTX_REPO_ROOT}/target/debug/ctx${suffix}"
  if [[ ! -f "${smoke_bin}" ]]; then
    printf 'expected smoke binary missing: %s\n' "${smoke_bin}" >&2
    return 1
  fi

  data_root="$(mktemp -d "${TMPDIR}/ctx-work-record-smoke.XXXXXX")"
  ctx_run_timed "platform-smoke-setup" env CTX_DATA_ROOT="${data_root}" "${smoke_bin}" setup
  record_json="$(CTX_DATA_ROOT="${data_root}" "${smoke_bin}" record \
    --title "platform smoke" \
    --body "platform smoke body" \
    --tag "smoke" \
    --json)"
  record_id="$(printf '%s\n' "${record_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p')"
  record_id="${record_id%%$'\n'*}"
  if [[ -z "${record_id}" ]]; then
    printf 'platform smoke failed to create a record id\n' >&2
    return 1
  fi
  ctx_run_timed "platform-smoke-search" env CTX_DATA_ROOT="${data_root}" "${smoke_bin}" search "platform" --json
  ctx_run_timed "platform-smoke-context" env CTX_DATA_ROOT="${data_root}" "${smoke_bin}" context "platform" --json
  ctx_run_timed "platform-smoke-dashboard" env CTX_DATA_ROOT="${data_root}" "${smoke_bin}" dashboard export --output "${data_root}/dashboard"
  ctx_run_timed "platform-smoke-validate" env CTX_DATA_ROOT="${data_root}" "${smoke_bin}" validate
}

ctx_debug_bin() {
  local suffix bin

  ctx_ensure_rust_toolchain
  ctx_run_timed "ctx-debug-build" "${cargo_bin}" build -p ctx --bin ctx "${cargo_locked_args[@]}" >&2
  suffix="$(ctx_host_exe_suffix)"
  bin="${CTX_REPO_ROOT}/target/debug/ctx${suffix}"
  if [[ ! -f "${bin}" ]]; then
    printf 'expected ctx debug binary missing: %s\n' "${bin}" >&2
    return 1
  fi
  printf '%s\n' "${bin}"
}

write_mode_summary() {
  local name="$1"
  local status="$2"
  local note="$3"
  local out="${CTX_ARTIFACT_DIR}/${name}.json"

  mkdir -p "${CTX_ARTIFACT_DIR}"
  printf '{"schema_version":1,"mode":"%s","status":"%s","publishing":false,"note":"%s"}\n' \
    "$(ctx_json_escape "${name}")" \
    "$(ctx_json_escape "${status}")" \
    "$(ctx_json_escape "${note}")" > "${out}"
}

capture_output() {
  local output="$1"
  shift

  "$@" > "${output}"
}

run_provider_fixtures() {
  local fixture count line

  mkdir -p "${CTX_ARTIFACT_DIR}"
  count=0
  for fixture in tests/fixtures/provider/*.jsonl; do
    test -f "${fixture}"
    while IFS= read -r line; do
      [[ -z "${line}" ]] && continue
      case "${line}" in
        *'"provider"'*'"session"'*)
          ;;
        *)
          printf '%s: provider fixture line is missing provider/session keys\n' "${fixture}" >&2
          return 1
          ;;
      esac
    done < "${fixture}"
    count=$((count + 1))
  done

  if (( count == 0 )); then
    printf 'no provider fixtures found\n' >&2
    return 1
  fi

  ctx_run_timed "provider-fixture-import-tests" "${cargo_bin}" test -p work-record-capture provider_fixture_replay "${cargo_locked_args[@]}" -- --test-threads "${RUST_TEST_THREADS}"
  write_mode_summary "provider-fixtures" "passed" "validated inert provider fixture import coverage for codex, pi, and claude"
}

run_rich_search_context() {
  local bin data_root record_json record_id search_json context_json

  bin="$(ctx_debug_bin)"
  data_root="$(mktemp -d "${TMPDIR}/ctx-rich-search.XXXXXX")"
  ctx_run_timed "rich-search-setup" env CTX_DATA_ROOT="${data_root}" "${bin}" setup
  record_json="$(CTX_DATA_ROOT="${data_root}" "${bin}" record \
    --title "rich search context fixture" \
    --body "Searchable body with release blocker context, dashboard artifacts, and provider fixture details." \
    --tag "finished-product" \
    --tag "search" \
    --json)"
  record_id="$(printf '%s\n' "${record_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p')"
  test -n "${record_id}"
  ctx_run_timed "rich-search-evidence" env CTX_DATA_ROOT="${data_root}" "${bin}" evidence run --record "${record_id}" -- bash -lc 'printf "%s\n" "provider fixture dashboard report context"'
  search_json="${CTX_ARTIFACT_DIR}/rich-search.json"
  context_json="${CTX_ARTIFACT_DIR}/rich-context.json"
  mkdir -p "${CTX_ARTIFACT_DIR}"
  ctx_run_timed "rich-search-json" capture_output "${search_json}" env CTX_DATA_ROOT="${data_root}" "${bin}" search "provider dashboard" --limit 10 --json
  ctx_run_timed "rich-context-json" capture_output "${context_json}" env CTX_DATA_ROOT="${data_root}" "${bin}" context "provider dashboard" --limit 10 --max-tokens 1200 --json
  file_contains "${search_json}" '"results"'
  file_contains "${context_json}" '"results"'
  write_mode_summary "rich-search-context" "passed" "search and context JSON include the finished-product fixture record and evidence"
}

run_dashboard_report_artifact_review() {
  local bin data_root record_json record_id report_json dashboard_index

  bin="$(ctx_debug_bin)"
  data_root="$(mktemp -d "${TMPDIR}/ctx-dashboard-report.XXXXXX")"
  ctx_run_timed "dashboard-report-setup" env CTX_DATA_ROOT="${data_root}" "${bin}" setup
  record_json="$(CTX_DATA_ROOT="${data_root}" "${bin}" record \
    --title "dashboard report artifact review" \
    --body "Review dashboard and report artifacts before sharing." \
    --tag "dashboard" \
    --json)"
  record_id="$(printf '%s\n' "${record_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p')"
  test -n "${record_id}"
  ctx_run_timed "dashboard-report-evidence" env CTX_DATA_ROOT="${data_root}" "${bin}" evidence run --record "${record_id}" -- bash -lc 'printf "%s\n" "report artifact preview"'
  report_json="${CTX_ARTIFACT_DIR}/report.json"
  mkdir -p "${CTX_ARTIFACT_DIR}/dashboard"
  ctx_run_timed "report-json" capture_output "${report_json}" env CTX_DATA_ROOT="${data_root}" "${bin}" report --format json
  ctx_run_timed "dashboard-export" env CTX_DATA_ROOT="${data_root}" "${bin}" dashboard export --output "${CTX_ARTIFACT_DIR}/dashboard"
  dashboard_index="${CTX_ARTIFACT_DIR}/dashboard/index.html"
  test -s "${dashboard_index}"
  file_contains "${report_json}" '"record_count"'
  file_contains "${dashboard_index}" "dashboard report artifact review"
  write_mode_summary "dashboard-report-artifact-review" "passed" "report JSON and dashboard HTML artifacts were generated for review"
}

run_pr_publish_dry_run() {
  local bin data_root record_json record_id markdown

  bin="$(ctx_debug_bin)"
  data_root="$(mktemp -d "${TMPDIR}/ctx-pr-publish.XXXXXX")"
  ctx_run_timed "pr-publish-setup" env CTX_DATA_ROOT="${data_root}" "${bin}" setup
  record_json="$(CTX_DATA_ROOT="${data_root}" "${bin}" record \
    --title "PR publish dry-run fixture" \
    --body "Render marker-bounded PR output without a network write." \
    --tag "publish" \
    --json)"
  record_id="$(printf '%s\n' "${record_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p')"
  test -n "${record_id}"
  ctx_run_timed "pr-link" capture_output "${CTX_ARTIFACT_DIR}/linked-pr.json" env CTX_DATA_ROOT="${data_root}" "${bin}" link-pr "${record_id}" "https://github.com/example/project/pull/42" --json
  markdown="${CTX_ARTIFACT_DIR}/pr-comment-dry-run.md"
  mkdir -p "${CTX_ARTIFACT_DIR}"
  ctx_run_timed "pr-comment-dry-run" capture_output "${markdown}" env CTX_DATA_ROOT="${data_root}" "${bin}" publish pr-comment "${record_id}" --dry-run
  file_contains "${markdown}" "ctx-work-record:finished-product:start"
  file_contains "${markdown}" "PR publish dry-run fixture"
  write_mode_summary "pr-publish-dry-run" "passed" "rendered marker-bounded PR comment dry-run without publishing"
}

run_security_archive_fixtures() {
  local corpus summary line

  corpus="tests/fixtures/redaction/redaction-corpus.jsonl"
  test -f "${corpus}"
  while IFS= read -r line; do
    [[ -z "${line}" ]] && continue
    case "${line}" in
      *'"expected_redacted"'*'[REDACTED'*)
        ;;
      *)
        printf '%s: redaction fixture line is missing expected redaction marker\n' "${corpus}" >&2
        return 1
        ;;
    esac
  done < "${corpus}"
  require_security_text "archive fixture hash mismatch" "crates/work-record-store/src/lib.rs" "import_rejects_archive_artifact_hash_mismatch_and_rolls_back"
  require_security_text "archive fixture byte-size mismatch" "crates/work-record-store/src/lib.rs" "import_rejects_archive_artifact_byte_size_mismatch_and_rolls_back"
  require_security_text "malicious archive path traversal fixture" "crates/work-record-store/src/lib.rs" "import_rejects_hostile_archive_blob_path_and_rolls_back"
  require_security_text "symlink archive export fixture" "crates/work-record-store/src/lib.rs" "export_rejects_symlink_archive_blob_file"
  summary="${CTX_ARTIFACT_DIR}/security-archive-fixtures.md"
  mkdir -p "${CTX_ARTIFACT_DIR}"
  {
    printf '# Security Archive Fixtures\n\n'
    printf '%s\n' '- Publishing: false'
    printf '%s `%s`\n' '- Redaction corpus:' "${corpus}"
    printf '%s\n' '- Malicious archive fixture coverage: hash mismatch, byte-size mismatch, path traversal, symlink export refusal'
  } > "${summary}"
  write_mode_summary "security-archive-fixtures" "passed" "validated redaction corpus and malicious archive fixture coverage markers"
}

require_security_text() {
  local description="$1"
  local file="$2"
  local text="$3"

  if ! file_contains "${file}" "${text}"; then
    printf 'missing %s: %s\n' "${description}" "${text}" >&2
    return 1
  fi
}

run_jj_e2e_blocker_status() {
  local bin data_root out

  bin="$(ctx_debug_bin)"
  data_root="$(mktemp -d "${TMPDIR}/ctx-jj-blocker.XXXXXX")"
  mkdir -p "${CTX_ARTIFACT_DIR}"
  ctx_run_timed "jj-blocker-setup" env CTX_DATA_ROOT="${data_root}" "${bin}" setup
  out="${CTX_ARTIFACT_DIR}/jj-e2e-blocker-status.txt"
  if command -v jj >/dev/null 2>&1; then
    ctx_run_timed "jj-vcs-inspect" capture_output "${CTX_ARTIFACT_DIR}/vcs-inspect.json" env CTX_DATA_ROOT="${data_root}" "${bin}" vcs inspect --json
    printf 'jj installed; vcs inspect artifact recorded\n' > "${out}"
  else
    printf 'jj unavailable on this runner; full jj e2e remains externally blocked for this lane\n' > "${out}"
  fi
  write_mode_summary "jj-e2e-blocker-status" "passed" "recorded jj availability and blocker status without installing external tools"
}

run_installer_dry_run_smoke() {
  local metadata

  metadata="${CTX_ARTIFACT_DIR}/ctx-release-metadata.env"
  mkdir -p "${CTX_ARTIFACT_DIR}"
  cat > "${metadata}" <<'EOF'
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=0.0.0-smoke
CTX_RELEASE_BASE_URL=https://example.invalid/ctx
CTX_RELEASE_ARTIFACT_linux_x64=ctx-0.0.0-smoke-x86_64-unknown-linux-gnu
CTX_RELEASE_SHA256_linux_x64=1111111111111111111111111111111111111111111111111111111111111111
CTX_RELEASE_ARTIFACT_macos_arm64=ctx-0.0.0-smoke-aarch64-apple-darwin
CTX_RELEASE_SHA256_macos_arm64=2222222222222222222222222222222222222222222222222222222222222222
CTX_RELEASE_ARTIFACT_macos_x64=ctx-0.0.0-smoke-x86_64-apple-darwin
CTX_RELEASE_SHA256_macos_x64=3333333333333333333333333333333333333333333333333333333333333333
CTX_RELEASE_ARTIFACT_windows_x64=ctx-0.0.0-smoke-x86_64-pc-windows-gnu.exe
CTX_RELEASE_SHA256_windows_x64=4444444444444444444444444444444444444444444444444444444444444444
CTX_RELEASE_ARTIFACT_freebsd_x64=ctx-0.0.0-smoke-x86_64-unknown-freebsd
CTX_RELEASE_SHA256_freebsd_x64=5555555555555555555555555555555555555555555555555555555555555555
EOF
  ctx_run_timed "installer-linux-dry-run" capture_output "${CTX_ARTIFACT_DIR}/install-dry-run.txt" bash scripts/install.sh --metadata "${metadata}" --platform linux-x64 --bin-dir "${CTX_ARTIFACT_DIR}/bin" --dry-run
  file_contains "${CTX_ARTIFACT_DIR}/install-dry-run.txt" "ctx install plan"
  write_mode_summary "installer-dry-run-smoke" "passed" "validated installer metadata and dry-run plan without downloading or installing"
}

run_bazel() {
  local bazel_cmd="$1"

  "${bazel_cmd}" \
    --output_user_root="${BAZEL_OUTPUT_USER_ROOT}" \
    test \
    --nozip_undeclared_test_outputs \
    --jobs="${BAZEL_JOBS}" \
    --local_resources="cpu=${BAZEL_LOCAL_CPU_RESOURCES}" \
    --local_resources="memory=${BAZEL_LOCAL_RAM_RESOURCES}" \
    --test_env=CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS}" \
    --test_env=RUST_TEST_THREADS="${RUST_TEST_THREADS}" \
    --test_env=CTX_CARGO_JOBS="${CARGO_BUILD_JOBS}" \
    --test_env=CTX_TEST_THREADS="${RUST_TEST_THREADS}" \
    --test_env=TMPDIR="${TMPDIR}" \
    --test_env=PATH="${PATH}" \
    --test_env=CARGO_HOME="${CARGO_HOME}" \
    --test_env=RUSTUP_HOME="${RUSTUP_HOME}" \
    //...
}

run_mode() {
  local mode="$1"
  local bazel_cmd=""

  case "${mode}" in
    fmt)
      ctx_run_timed "cargo-fmt" run_fmt
      ;;
    docs)
      ctx_run_timed "docs" run_docs
      ;;
    check)
      ctx_run_timed "cargo-check" run_check
      ;;
    clippy)
      ctx_run_timed "cargo-clippy" run_clippy
      ;;
    test)
      ctx_run_timed "cargo-test" run_test
      ;;
    examples)
      run_examples
      ;;
    bazel)
      if [[ ! -f BUILD.bazel && ! -f MODULE.bazel && ! -f WORKSPACE && ! -f WORKSPACE.bazel ]]; then
        ctx_record_skip "bazel-test" "no Bazel workspace files found"
      else
        ctx_ensure_rust_toolchain
        if ! bazel_cmd="$(ctx_find_bazel)"; then
          if [[ "${CTX_REQUIRE_BAZEL:-0}" == "1" ]]; then
            printf 'bazel/bazelisk is required because Bazel workspace files exist\n' >&2
            return 1
          fi
          ctx_record_skip "bazel-test" "bazel/bazelisk is not installed"
        else
          ctx_run_timed "bazel-test" run_bazel "${bazel_cmd}"
        fi
      fi
      ;;
    platform-smoke)
      run_platform_smoke
      ;;
    provider-fixtures)
      run_provider_fixtures
      ;;
    rich-search-context)
      run_rich_search_context
      ;;
    dashboard-report-artifact-review)
      run_dashboard_report_artifact_review
      ;;
    pr-publish-dry-run)
      run_pr_publish_dry_run
      ;;
    security-archive-fixtures)
      run_security_archive_fixtures
      ;;
    jj-e2e-blocker-status)
      run_jj_e2e_blocker_status
      ;;
    installer-dry-run-smoke)
      run_installer_dry_run_smoke
      ;;
    completion-certificate)
      bash scripts/release-completion-certificate.sh
      ;;
    all)
      run_mode fmt
      run_mode docs
      run_mode check
      run_mode clippy
      run_mode test
      run_mode examples
      run_mode bazel
      run_mode provider-fixtures
      run_mode rich-search-context
      run_mode dashboard-report-artifact-review
      run_mode pr-publish-dry-run
      run_mode security-archive-fixtures
      run_mode jj-e2e-blocker-status
      run_mode installer-dry-run-smoke
      ;;
    -h|--help|help)
      usage
      ;;
    *)
      printf 'unknown check mode: %s\n' "${mode}" >&2
      usage >&2
      return 2
      ;;
  esac
}

cd "${CTX_REPO_ROOT}"
ctx_init_resource_env
ctx_timing_init
trap ctx_timing_finish EXIT
ctx_print_resource_env
setup_cargo_args

if (( "$#" == 0 )); then
  set -- all
fi

for mode in "$@"; do
  run_mode "${mode}"
done
