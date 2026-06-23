#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/check.sh [all|fmt|docs|check|clippy|test|examples|bazel|platform-smoke|product-decisions|provider-fixtures|rich-search-context|dashboard-report-artifact-review|pr-publish-dry-run|security-archive-fixtures|jj-e2e-blocker-status|installer-dry-run-smoke|completion-certificate|completion-certificate-self-test]...

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

sha256_file() {
  local path="$1"

  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${path}" | awk '{ print $1 }'
    return 0
  fi

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "${path}" | awk '{ print $1 }'
    return 0
  fi

  printf 'sha256sum or shasum is required\n' >&2
  return 1
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

  data_root="$(mktemp -d "${TMPDIR}/ctx-records-smoke.XXXXXX")"
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

product_decision_failures=0

fail_product_decision() {
  product_decision_failures=$(( product_decision_failures + 1 ))
  printf 'product decision regression: %s\n' "$*" >&2
}

scan_public_product_text() {
  local search_root="$1"
  local description="$2"
  local pattern="$3"
  local output="$4"

  [[ -e "${search_root}" ]] || return 0
  if rg -n --glob '!**/node_modules/**' --glob '!**/dist/**' --glob '!target/**' --glob '!Cargo.lock' \
    --glob '!docs/provider-support-matrix.json' \
    -e "${pattern}" "${search_root}" > "${output}" 2>/dev/null; then
    fail_product_decision "${description}; see ${output}"
  else
    rm -f "${output}"
  fi
}

require_repo_pattern() {
  local description="$1"
  local pattern="$2"
  shift 2

  if rg -n --glob '!**/node_modules/**' --glob '!target/**' -e "${pattern}" "$@" > /dev/null 2>&1; then
    return 0
  fi
  fail_product_decision "${description} (${pattern})"
}

run_product_decisions() {
  local out_dir="${CTX_ARTIFACT_DIR}/product-decisions"
  local public_roots=()
  local root

  product_decision_failures=0
  mkdir -p "${out_dir}"

  for root in README.md docs apps/work-recorder-dashboard/src release .buildkite; do
    [[ -e "${root}" ]] && public_roots+=("${root}")
  done

  for root in "${public_roots[@]}"; do
    scan_public_product_text "${root}" \
      "public/release text must not brand commands, paths, URLs, or copy around work-record" \
      'work-record' \
      "${out_dir}/stale-work-record-branding-$(printf '%s' "${root}" | tr '/.' '__').txt"
    scan_public_product_text "${root}" \
      "public/release text must not claim the stale canonical ~/.ctx/work-record data root" \
      '~/.ctx/work-record' \
      "${out_dir}/stale-work-record-root-$(printf '%s' "${root}" | tr '/.' '__').txt"
    scan_public_product_text "${root}" \
      "public/release text must not expose canonical blobs layout wording" \
      '(^|[^[:alnum:]_])blobs?/' \
      "${out_dir}/stale-blobs-layout-$(printf '%s' "${root}" | tr '/.' '__').txt"
    scan_public_product_text "${root}" \
      "public/release text must not expose canonical inbox layout wording" \
      '(^|[^[:alnum:]_])inbox(/|[[:space:][:punct:]])' \
      "${out_dir}/stale-inbox-layout-$(printf '%s' "${root}" | tr '/.' '__').txt"
  done

  require_repo_pattern "public copy uses ctx naming plus work records language" 'ctx records agent work|work records' "${public_roots[@]}"
  require_repo_pattern "release metadata uses ctx/records artifact paths" 'ctx/records/release-candidate/v0\.1\.0' release scripts docs
  require_repo_pattern "release metadata avoids work-recorder release artifact prefixes" 'CTX_RELEASE_R2_PREFIX=ctx/records' release scripts
  require_repo_pattern "flat data-root layout is asserted" 'work\.sqlite' crates tests docs README.md
  require_repo_pattern "objects layout is asserted" 'objects/' crates tests docs README.md
  require_repo_pattern "spool layout is asserted" 'spool/' crates tests docs README.md
  require_repo_pattern "setup golden output coverage is present" 'setup.*golden|golden.*setup|setup_output' crates tests
  require_repo_pattern "status golden output coverage is present" 'status.*golden|golden.*status|status_output' crates tests
  require_repo_pattern "dashboard golden output coverage is present" 'dashboard.*golden|golden.*dashboard|dashboard_output' crates tests
  require_repo_pattern "uninstall golden output coverage is present" 'uninstall.*golden|golden.*uninstall|uninstall_output' crates tests
  require_repo_pattern "setup idempotency coverage is present" 'idempotent.*setup|setup.*idempotent' crates tests
  require_repo_pattern "old ADE state ignore coverage is present" 'old.*ADE|ADE.*ignore|ade.*ignore' crates tests
  require_repo_pattern "dashboard interactive open coverage is present" 'dashboard.*interactive|interactive.*dashboard' crates tests
  require_repo_pattern "dashboard headless coverage is present" 'dashboard.*headless|headless.*dashboard' crates tests
  require_repo_pattern "dashboard --no-open coverage is present" 'no-open' crates tests
  require_repo_pattern "status reports database path" 'database path|database_path|db_path' crates tests docs README.md
  require_repo_pattern "status reports shim state" 'shim status|shim_status|shims' crates tests docs README.md
  require_repo_pattern "status reports dashboard URL/running state" 'dashboard.*URL|dashboard_url|dashboard.*running' crates tests docs README.md
  require_repo_pattern "status reports spool pending count" 'spool.*pending|pending.*spool|spool_pending' crates tests docs README.md

  if (( product_decision_failures > 0 )); then
    return 1
  fi

  write_mode_summary "product-decisions" "passed" "verified ctx naming, flat data root, objects/spool layout, setup/status/dashboard/uninstall golden coverage, dashboard open behavior, and stale public path regressions"
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

  ctx_ensure_rust_toolchain
  mkdir -p "${CTX_ARTIFACT_DIR}"
  count=0
  for fixture in tests/fixtures/provider/*.jsonl; do
    test -f "${fixture}"
    case "$(basename "${fixture}")" in
      malformed-*)
        continue
        ;;
    esac
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
  local bin data_root record_json record_id report_json dashboard_index dogfood_dir fake_browser fake_firefox

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
  dogfood_dir="${CTX_ARTIFACT_DIR}/dogfood-sanitizer"
  fake_browser="${TMPDIR}/ctx-fake-browser"
  fake_firefox="${TMPDIR}/ctx-fake-firefox"
  cat >"${fake_browser}" <<EOF
#!/usr/bin/env bash
printf 'fake browser failed from %s and %s\n' "${repo_root}" "${HOME:-}" >&2
exit 1
EOF
  cat >"${fake_firefox}" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
  chmod +x "${fake_browser}" "${fake_firefox}"
  rm -rf "${dogfood_dir}" "${CTX_ARTIFACT_DIR}/dogfood-data"
  ctx_run_timed "dashboard-dogfood-sanitizer" env \
    CTX_BIN="${bin}" \
    CTX_DASHBOARD_REVIEW_BROWSER="${fake_browser}" \
    CTX_DASHBOARD_REVIEW_FIREFOX="${fake_firefox}" \
    TMPDIR="${TMPDIR}" \
    scripts/dashboard-review-dogfood.sh \
      --seed-live \
      --output "${dogfood_dir}" \
      --data-root "${CTX_ARTIFACT_DIR}/dogfood-data"
  file_contains "${dogfood_dir}/manifest.json" '"raw_archive_exported": false'
  file_contains "${dogfood_dir}/manifest.json" '"artifact_dir": "."'
  file_contains "${dogfood_dir}/manifest.json" '"data_root": "[LOCAL_DATA_ROOT]"'
  file_contains "${dogfood_dir}/manifest.json" '"dashboard": "dashboard/index.html"'
  if grep -R -F -q -- "${repo_root}" "${dogfood_dir}"; then
    printf 'dogfood sanitizer leaked repo root into default artifacts\n' >&2
    return 1
  fi
  if [[ -n "${HOME:-}" ]] && grep -R -F -q -- "${HOME}" "${dogfood_dir}"; then
    printf 'dogfood sanitizer leaked home path into default artifacts\n' >&2
    return 1
  fi
  if grep -R -F -q -- "${CTX_ARTIFACT_DIR}/dogfood-data" "${dogfood_dir}"; then
    printf 'dogfood sanitizer leaked raw data-root path into default artifacts\n' >&2
    return 1
  fi
  if grep -R -E -q 'ghp_1234567890abcdef|hunter2|fake_secret_value|chrome-profile|firefox-tmp' "${dogfood_dir}"; then
    printf 'dogfood sanitizer leaked secret marker or browser scratch path\n' >&2
    return 1
  fi
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
  mkdir -p "${CTX_ARTIFACT_DIR}"
  ctx_run_timed "pr-link" capture_output "${CTX_ARTIFACT_DIR}/linked-pr.json" env CTX_DATA_ROOT="${data_root}" "${bin}" link-pr "${record_id}" "https://github.com/example/project/pull/42" --json
  markdown="${CTX_ARTIFACT_DIR}/pr-comment-dry-run.md"
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
  local metadata placeholder_metadata unsafe_metadata insecure_output placeholder_output unsafe_output

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

  insecure_output="${CTX_ARTIFACT_DIR}/install-insecure-metadata.txt"
  if bash scripts/install.sh --metadata http://example.invalid/ctx-release-metadata.env --platform linux-x64 --bin-dir "${CTX_ARTIFACT_DIR}/bin" --dry-run >"${insecure_output}" 2>&1; then
    printf 'installer unexpectedly accepted insecure metadata URL\n' >&2
    return 1
  fi
  file_contains "${insecure_output}" "refusing insecure metadata URL"

  placeholder_metadata="${CTX_ARTIFACT_DIR}/ctx-release-placeholder.env"
  cp "${metadata}" "${placeholder_metadata}"
  sed -i.bak 's/^CTX_RELEASE_SHA256_linux_x64=.*/CTX_RELEASE_SHA256_linux_x64=0000000000000000000000000000000000000000000000000000000000000000/' "${placeholder_metadata}"
  rm -f "${placeholder_metadata}.bak"
  placeholder_output="${CTX_ARTIFACT_DIR}/install-placeholder-checksum.txt"
  if bash scripts/install.sh --metadata "${placeholder_metadata}" --platform linux-x64 --bin-dir "${CTX_ARTIFACT_DIR}/bin" --dry-run >"${placeholder_output}" 2>&1; then
    printf 'installer unexpectedly accepted placeholder checksum\n' >&2
    return 1
  fi
  file_contains "${placeholder_output}" "checksum for linux-x64 is a placeholder"

  unsafe_metadata="${CTX_ARTIFACT_DIR}/ctx-release-unsafe-artifact.env"
  cp "${metadata}" "${unsafe_metadata}"
  sed -i.bak 's/^CTX_RELEASE_ARTIFACT_linux_x64=.*/CTX_RELEASE_ARTIFACT_linux_x64=..\/ctx/' "${unsafe_metadata}"
  rm -f "${unsafe_metadata}.bak"
  unsafe_output="${CTX_ARTIFACT_DIR}/install-unsafe-artifact.txt"
  if bash scripts/install.sh --metadata "${unsafe_metadata}" --platform linux-x64 --bin-dir "${CTX_ARTIFACT_DIR}/bin" --dry-run >"${unsafe_output}" 2>&1; then
    printf 'installer unexpectedly accepted unsafe artifact name\n' >&2
    return 1
  fi
  file_contains "${unsafe_output}" "unsafe artifact name"

  write_mode_summary "installer-dry-run-smoke" "passed" "validated installer dry-run plus insecure metadata, placeholder checksum, and unsafe artifact refusals"
}

write_release_evidence_self_test_fixture() {
  local root="$1"
  local platform="$2"
  local target="$3"
  local platform_key="${platform//-/_}"
  local dir="${root}/artifacts/buildkite/release-dry-run/${platform}"
  local suffix=""
  local artifact
  local artifact_path
  local checksum bytes
  local line_end=$'\n'

  if [[ "${platform}" == "windows-x64" ]]; then
    suffix=".exe"
    line_end=$'\r\n'
  fi
  artifact="ctx-0.1.0-${target}${suffix}"
  artifact_path="artifacts/buildkite/release-dry-run/${platform}/${artifact}"
  mkdir -p "${dir}"
  printf 'ctx dry-run artifact self-test fixture for %s\n' "${platform}" > "${root}/${artifact_path}"
  chmod 0755 "${root}/${artifact_path}" 2>/dev/null || true
  checksum="$(sha256_file "${root}/${artifact_path}")"
  bytes="$(wc -c < "${root}/${artifact_path}" | tr -d '[:space:]')"
  printf '%s  %s%s' "${checksum}" "${artifact}" "${line_end}" > "${dir}/checksums.sha256"
  cat > "${dir}/manifest.json" <<EOF
{
  "schema_version": 1,
  "dry_run": true,
  "upload": false,
  "package": "ctx",
  "version": "0.1.0",
  "platform": "${platform}",
  "target_triple": "${target}",
  "host_triple": "${target}",
  "expected_host_triple": "${target}",
  "git_commit": "$(git rev-parse HEAD)",
  "git_branch": "$(git branch --show-current)",
  "generated_at_unix_s": 1,
  "self_test_fixture": true,
  "artifacts": [
    {
      "path": "${artifact_path}",
      "sha256": "${checksum}",
      "bytes": ${bytes}
    }
  ]
}
EOF
  {
    printf 'CTX_RELEASE_SCHEMA_VERSION=1%s' "${line_end}"
    printf 'CTX_RELEASE_CHANNEL=dry-run%s' "${line_end}"
    printf 'CTX_RELEASE_VERSION=0.1.0%s' "${line_end}"
    printf 'CTX_RELEASE_BASE_URL=https://example.invalid/ctx%s' "${line_end}"
    printf 'CTX_RELEASE_ARTIFACT_%s=%s%s' "${platform_key}" "${artifact}" "${line_end}"
    printf 'CTX_RELEASE_SHA256_%s=%s%s' "${platform_key}" "${checksum}" "${line_end}"
  } > "${dir}/ctx-release-metadata.env"
}

run_completion_certificate_prerequisites() {
  local root="$1"

  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/pipeline-contract" bash scripts/check-buildkite-pipeline.sh
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-blockers/freebsd-x64" bash scripts/release-platform-blocker.sh freebsd-x64
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/provider-live-e2e-lanes" bash scripts/release-provider-live-e2e-lanes.sh definitions

  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/product-decisions" run_product_decisions
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/provider-fixtures" run_provider_fixtures
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/rich-search-context" run_rich_search_context
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/dashboard-report-artifact-review" run_dashboard_report_artifact_review
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/pr-publish-dry-run" run_pr_publish_dry_run
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/security-archive-fixtures" run_security_archive_fixtures
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/jj-e2e-blocker-status" run_jj_e2e_blocker_status
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/finished-product/installer-dry-run-smoke" run_installer_dry_run_smoke
}

run_completion_certificate_release_prerequisites() {
  local root="$1"
  local host_triple

  if ! command -v rustc >/dev/null 2>&1; then
    return 0
  fi
  host_triple="$(ctx_detect_host_triple)"
  case "${host_triple}" in
    x86_64-unknown-linux-gnu)
      CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-dry-run/linux-x64" \
        CTX_RELEASE_PLATFORM=linux-x64 \
        CTX_RELEASE_TARGET_TRIPLE=x86_64-unknown-linux-gnu \
        CTX_EXPECT_HOST_TRIPLE=x86_64-unknown-linux-gnu \
        bash scripts/release-dry-run.sh
      ;;
    aarch64-apple-darwin)
      CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-dry-run/macos-arm64" \
        CTX_RELEASE_PLATFORM=macos-arm64 \
        CTX_RELEASE_TARGET_TRIPLE=aarch64-apple-darwin \
        CTX_EXPECT_HOST_TRIPLE=aarch64-apple-darwin \
        bash scripts/release-dry-run.sh
      ;;
    x86_64-apple-darwin)
      CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-dry-run/macos-x64" \
        CTX_RELEASE_PLATFORM=macos-x64 \
        CTX_RELEASE_TARGET_TRIPLE=x86_64-apple-darwin \
        CTX_EXPECT_HOST_TRIPLE=x86_64-apple-darwin \
        bash scripts/release-dry-run.sh
      ;;
    x86_64-pc-windows-gnu)
      ;;
  esac
}

run_completion_certificate_candidate_metadata_if_possible() {
  local root="$1"

  if [[ -s "${root}/artifacts/buildkite/release-dry-run/linux-x64/ctx-release-metadata.env" \
    && -s "${root}/artifacts/buildkite/release-dry-run/macos-arm64/ctx-release-metadata.env" \
    && -s "${root}/artifacts/buildkite/release-dry-run/macos-x64/ctx-release-metadata.env" \
    && -s "${root}/artifacts/buildkite/release-dry-run/windows-x64/ctx-release-metadata.env" ]]; then
    CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-candidate" \
      bash scripts/release-candidate-metadata.sh \
        "${root}/artifacts/buildkite/release-dry-run" \
        "${root}/artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json"
  fi
}

run_completion_certificate() {
  local root="${CTX_ARTIFACT_DIR}/completion-evidence-root"

  rm -rf "${root}"
  mkdir -p "${root}"
  run_completion_certificate_prerequisites "${root}"
  run_completion_certificate_release_prerequisites "${root}"
  run_completion_certificate_candidate_metadata_if_possible "${root}"

  CTX_COMPLETION_EVIDENCE_ROOT="${root}" bash scripts/release-completion-certificate.sh
}

run_completion_certificate_self_test() {
  local root="${CTX_ARTIFACT_DIR}/completion-certificate-self-test-evidence-root"
  local empty_root="${CTX_ARTIFACT_DIR}/completion-certificate-empty-evidence-root"
  local negative_output="${CTX_ARTIFACT_DIR}/completion-certificate-empty-evidence.txt"

  rm -rf "${root}"
  mkdir -p "${root}"
  run_completion_certificate_prerequisites "${root}"

  write_release_evidence_self_test_fixture "${root}" "linux-x64" "x86_64-unknown-linux-gnu"
  write_release_evidence_self_test_fixture "${root}" "macos-arm64" "aarch64-apple-darwin"
  write_release_evidence_self_test_fixture "${root}" "macos-x64" "x86_64-apple-darwin"
  write_release_evidence_self_test_fixture "${root}" "windows-x64" "x86_64-pc-windows-gnu"
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-candidate" \
    bash scripts/release-candidate-metadata.sh \
      "${root}/artifacts/buildkite/release-dry-run" \
      "${root}/artifacts/buildkite/release-blockers/freebsd-x64/freebsd-x64-blocker.json"

  CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES=1 \
    CTX_COMPLETION_EVIDENCE_ROOT="${root}" \
    bash scripts/release-completion-certificate.sh

  rm -rf "${empty_root}"
  mkdir -p "${empty_root}"
  if CTX_COMPLETION_EVIDENCE_ROOT="${empty_root}" bash scripts/release-completion-certificate.sh >"${negative_output}" 2>&1; then
    printf 'completion certificate unexpectedly passed with empty evidence root\n' >&2
    return 1
  fi
  file_contains "${negative_output}" "required evidence is missing or empty"

  run_completion_certificate_negative_cases "${root}"
}

mutate_completion_certificate_negative_fixture() {
  local root="$1"
  local case_name="$2"
  local linux_dir="${root}/artifacts/buildkite/release-dry-run/linux-x64"
  local manifest="${linux_dir}/manifest.json"
  local metadata="${linux_dir}/ctx-release-metadata.env"
  local artifact

  case "${case_name}" in
    checksum-mismatch)
      artifact="$(sed -n 's/.*"path": "\([^"]*\)".*/\1/p' "${manifest}" | head -n 1)"
      printf 'tampered artifact content\n' > "${root}/${artifact}"
      ;;
    metadata-mismatch)
      sed -i.bak 's/^CTX_RELEASE_SHA256_linux_x64=.*/CTX_RELEASE_SHA256_linux_x64=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/' "${metadata}"
      rm -f "${metadata}.bak"
      ;;
    missing-checksum-file)
      rm -f "${linux_dir}/checksums.sha256"
      ;;
    unsafe-artifact-path)
      perl -0pi -e 's/"path": "[^"]+"/"path": "..\/ctx"/' "${manifest}"
      ;;
    bad-artifact-count)
      perl -0pi -e 's/"artifacts": \[\s*\{.*?\}\s*\]/"artifacts": []/s' "${manifest}"
      ;;
    failing-finished-product-summary)
      sed -i.bak 's/"status":"passed"/"status":"failed"/' \
        "${root}/artifacts/buildkite/finished-product/provider-fixtures/provider-fixtures.json"
      rm -f "${root}/artifacts/buildkite/finished-product/provider-fixtures/provider-fixtures.json.bak"
      ;;
    missing-product-decision-summary)
      rm -f "${root}/artifacts/buildkite/finished-product/product-decisions/product-decisions.json"
      ;;
    stale-release-commit)
      sed -i.bak 's/"git_commit": "[^"]*"/"git_commit": "0000000000000000000000000000000000000000"/' "${manifest}"
      rm -f "${manifest}.bak"
      ;;
    *)
      printf 'unknown completion certificate negative fixture: %s\n' "${case_name}" >&2
      return 2
      ;;
  esac
}

run_completion_certificate_negative_case() {
  local source_root="$1"
  local case_name="$2"
  local expected="$3"
  local case_root="${CTX_ARTIFACT_DIR}/completion-certificate-negative-${case_name}-evidence-root"
  local output="${CTX_ARTIFACT_DIR}/completion-certificate-negative-${case_name}.txt"

  rm -rf "${case_root}"
  cp -R "${source_root}" "${case_root}"
  mutate_completion_certificate_negative_fixture "${case_root}" "${case_name}"

  if CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES=1 \
    CTX_COMPLETION_EVIDENCE_ROOT="${case_root}" \
    bash scripts/release-completion-certificate.sh >"${output}" 2>&1; then
    printf 'completion certificate unexpectedly passed negative case %s\n' "${case_name}" >&2
    return 1
  fi
  file_contains "${output}" "${expected}"
}

run_completion_certificate_self_test_fixture_rejection_case() {
  local source_root="$1"
  local output="${CTX_ARTIFACT_DIR}/completion-certificate-negative-self-test-fixture-without-allow.txt"

  if CTX_COMPLETION_EVIDENCE_ROOT="${source_root}" bash scripts/release-completion-certificate.sh >"${output}" 2>&1; then
    printf 'completion certificate unexpectedly accepted self-test release fixtures without explicit allow\n' >&2
    return 1
  fi
  file_contains "${output}" "is a self-test fixture and cannot satisfy real completion evidence"
}

run_completion_certificate_negative_cases() {
  local source_root="$1"
  local case_name expected

  while IFS='|' read -r case_name expected; do
    [[ -z "${case_name}" ]] && continue
    run_completion_certificate_negative_case "${source_root}" "${case_name}" "${expected}"
  done <<'EOF'
checksum-mismatch|artifact file checksum must equal manifest checksum
metadata-mismatch|metadata checksum must equal manifest artifact checksum
missing-checksum-file|required evidence is missing or empty: artifacts/buildkite/release-dry-run/linux-x64/checksums.sha256
unsafe-artifact-path|manifest must record a safe relative artifact path
bad-artifact-count|manifest must record exactly one release artifact
failing-finished-product-summary|provider-fixtures summary records passing status
stale-release-commit|git_commit must match current HEAD
missing-product-decision-summary|required evidence is missing or empty: artifacts/buildkite/finished-product/product-decisions/product-decisions.json
EOF

  run_completion_certificate_self_test_fixture_rejection_case "${source_root}"
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
    product-decisions)
      run_product_decisions
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
      run_completion_certificate
      ;;
    completion-certificate-self-test)
      run_completion_certificate_self_test
      ;;
    all)
      run_mode fmt
      run_mode docs
      run_mode check
      run_mode clippy
      run_mode test
      run_mode examples
      run_mode bazel
      run_mode product-decisions
      run_mode provider-fixtures
      run_mode rich-search-context
      run_mode dashboard-report-artifact-review
      run_mode pr-publish-dry-run
      run_mode security-archive-fixtures
      run_mode jj-e2e-blocker-status
      run_mode installer-dry-run-smoke
      run_mode completion-certificate-self-test
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
