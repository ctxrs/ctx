#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/ci-common.sh
source "${script_dir}/ci-common.sh"

usage() {
  cat <<'USAGE'
usage: scripts/check.sh [all|fmt|docs|check|clippy|test|examples|bazel|platform-smoke]...

Runs resource-capped local checks sequentially. Defaults to "all".
Environment overrides:
  CARGO_BUILD_JOBS     Cargo build parallelism, default min(cpu, memory_gb / 3)
  RUST_TEST_THREADS    Rust test threads, default CARGO_BUILD_JOBS
  BAZEL_JOBS           Bazel job count, default CARGO_BUILD_JOBS
  CTX_REQUIRE_BAZEL    If 1, bootstrap Bazelisk when Bazel is missing
  CTX_ARTIFACT_DIR     Timing artifact directory, default target/ctx-artifacts/check
  CLIPPY_FLAGS         Extra clippy flags, default "-D warnings"
USAGE
}

cargo_locked_args=()

setup_cargo_args() {
  cargo_locked_args=()
  if [[ "${CTX_CARGO_LOCKED:-1}" != "0" && -f Cargo.lock ]]; then
    cargo_locked_args+=(--locked)
  fi
}

run_fmt() {
  ctx_ensure_rust_toolchain
  cargo fmt --all -- --check
}

run_docs() {
  bash scripts/check-docs.sh
}

run_check() {
  ctx_ensure_rust_toolchain
  cargo check --workspace --all-targets "${cargo_locked_args[@]}"
}

run_clippy() {
  ctx_ensure_rust_toolchain
  if [[ -n "${CLIPPY_FLAGS:-}" ]]; then
    cargo clippy --workspace --all-targets "${cargo_locked_args[@]}" -- ${CLIPPY_FLAGS}
  else
    cargo clippy --workspace --all-targets "${cargo_locked_args[@]}" -- -D warnings
  fi
}

run_test() {
  ctx_ensure_rust_toolchain
  cargo test --workspace --all-targets "${cargo_locked_args[@]}" -- --test-threads "${RUST_TEST_THREADS}"
}

run_examples() {
  local suffix example example_name example_bin

  ctx_ensure_rust_toolchain
  ctx_run_timed "examples-build" cargo build -p ctx --bins "${cargo_locked_args[@]}"

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
  ctx_run_timed "platform-smoke-build" cargo build -p ctx --bin ctx "${cargo_locked_args[@]}"

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
    all)
      run_mode fmt
      run_mode docs
      run_mode check
      run_mode clippy
      run_mode test
      run_mode examples
      run_mode bazel
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
