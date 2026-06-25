#!/usr/bin/env bash
set -euo pipefail

mode="${1:-cargo_test_default}"

fail() {
  printf 'bazel gate failed: %s\n' "$*" >&2
  exit 1
}

json_escape() {
  local value="${1:-}"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '%s' "${value}"
}

positive_int() {
  [[ "${1:-}" =~ ^[0-9]+$ ]] && (( "$1" > 0 ))
}

detect_cpu_count() {
  local cores

  if [[ -r /proc/cpuinfo ]]; then
    cores="$(awk '
      /^physical id[[:space:]]*:/ { physical = $NF }
      /^core id[[:space:]]*:/ {
        if (physical != "") {
          seen[physical ":" $NF] = 1
        }
      }
      END {
        for (core in seen) {
          count++
        }
        if (count > 0) {
          print count
        }
      }
    ' /proc/cpuinfo)"
    if positive_int "${cores}"; then
      printf '%s\n' "${cores}"
      return 0
    fi
  fi

  cores="$(getconf _NPROCESSORS_ONLN 2>/dev/null || true)"
  if positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  cores="$(sysctl -n hw.physicalcpu 2>/dev/null || true)"
  if positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  cores="$(sysctl -n hw.ncpu 2>/dev/null || true)"
  if positive_int "${cores}"; then
    printf '%s\n' "${cores}"
    return 0
  fi

  if positive_int "${NUMBER_OF_PROCESSORS:-}"; then
    printf '%s\n' "${NUMBER_OF_PROCESSORS}"
    return 0
  fi

  printf '2\n'
}

detect_memory_gb() {
  local kb bytes gb

  if [[ -r /proc/meminfo ]]; then
    kb="$(awk '/^MemTotal:/ { print $2; exit }' /proc/meminfo)"
    if positive_int "${kb}"; then
      gb=$(( kb / 1048576 ))
      if (( gb < 1 )); then
        gb=1
      fi
      printf '%s\n' "${gb}"
      return 0
    fi
  fi

  bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
  if positive_int "${bytes}"; then
    gb=$(( bytes / 1073741824 ))
    if (( gb < 1 )); then
      gb=1
    fi
    printf '%s\n' "${gb}"
    return 0
  fi

  printf '4\n'
}

candidate_repo_root() {
  local candidate="$1"
  local real_cargo real_root

  [[ -n "${candidate}" && -f "${candidate}/Cargo.toml" ]] || return 1

  if real_root="$(git -C "${candidate}" rev-parse --show-toplevel 2>/dev/null)"; then
    printf '%s\n' "${real_root}"
    return 0
  fi

  if command -v realpath >/dev/null 2>&1; then
    real_cargo="$(realpath "${candidate}/Cargo.toml" 2>/dev/null || true)"
    if [[ -n "${real_cargo}" ]]; then
      real_root="$(dirname "${real_cargo}")"
      if [[ -f "${real_root}/Cargo.toml" ]]; then
        printf '%s\n' "${real_root}"
        return 0
      fi
    fi
  fi

  printf '%s\n' "${candidate}"
}

find_repo_root() {
  local script_dir candidate root
  local candidates=()

  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  candidates+=("${BUILD_WORKSPACE_DIRECTORY:-}")
  candidates+=("$(pwd)")
  candidates+=("${RUNFILES_DIR:-}/_main")
  candidates+=("${RUNFILES_DIR:-}/ctx")
  candidates+=("${RUNFILES_DIR:-}/ctx_work_record")
  candidates+=("${script_dir}/..")
  candidates+=("${script_dir}/../_main")
  candidates+=("${script_dir}/../ctx")
  candidates+=("${script_dir}/../ctx_work_record")

  for candidate in "${candidates[@]}"; do
    root="$(candidate_repo_root "${candidate}" 2>/dev/null || true)"
    if [[ -n "${root}" ]]; then
      cd "${root}"
      return 0
    fi
  done

  fail 'could not locate repo root containing Cargo.toml for Bazel gate'
}

infer_cargo_home() {
  local cargo_bin cargo_parent

  if [[ -n "${CARGO_HOME:-}" ]]; then
    printf '%s\n' "${CARGO_HOME}"
    return 0
  fi

  cargo_bin="$(command -v cargo 2>/dev/null || true)"
  if [[ -n "${cargo_bin}" ]]; then
    cargo_bin="$(readlink -f "${cargo_bin}" 2>/dev/null || printf '%s' "${cargo_bin}")"
    cargo_parent="$(dirname "$(dirname "${cargo_bin}")")"
    if [[ "$(basename "${cargo_parent}")" == ".cargo" ]]; then
      printf '%s\n' "${cargo_parent}"
      return 0
    fi
  fi

  printf '%s\n' "${HOME}/.cargo"
}

infer_rustup_home() {
  local cargo_home="$1"
  local home_from_cargo

  if [[ -n "${RUSTUP_HOME:-}" ]]; then
    printf '%s\n' "${RUSTUP_HOME}"
    return 0
  fi

  home_from_cargo="$(dirname "${cargo_home}")"
  if [[ -d "${home_from_cargo}/.rustup" ]]; then
    printf '%s\n' "${home_from_cargo}/.rustup"
    return 0
  fi

  printf '%s\n' "${HOME}/.rustup"
}

init_bazel_gate_env() {
  local cpu_count memory_gb memory_jobs default_jobs cargo_home rustup_home

  find_repo_root
  export CTX_REPO_ROOT="$(pwd)"

  if [[ -z "${HOME:-}" ]]; then
    export HOME="${TEST_TMPDIR:-${CTX_REPO_ROOT}/target/tmp}/home"
  fi
  mkdir -p "${HOME}"

  cargo_home="$(infer_cargo_home)"
  rustup_home="$(infer_rustup_home "${cargo_home}")"
  export CARGO_HOME="${cargo_home}"
  export RUSTUP_HOME="${rustup_home}"
  export PATH="${CARGO_HOME}/bin:${PATH}"

  if ! command -v cargo >/dev/null 2>&1; then
    fail "cargo is required for ${mode} but was not found on PATH=${PATH}"
  fi

  cpu_count="${CTX_CPU_COUNT:-$(detect_cpu_count)}"
  memory_gb="${CTX_TOTAL_MEMORY_GB:-$(detect_memory_gb)}"
  if ! positive_int "${cpu_count}"; then
    cpu_count=2
  fi
  if ! positive_int "${memory_gb}"; then
    memory_gb=4
  fi

  memory_jobs=$(( memory_gb / 3 ))
  if (( memory_jobs < 1 )); then
    memory_jobs=1
  fi

  default_jobs="${cpu_count}"
  if (( memory_jobs < default_jobs )); then
    default_jobs="${memory_jobs}"
  fi
  if [[ "${CTX_LOCAL_RESOURCE_CAP:-1}" == "1" && -z "${CI:-}" && -z "${BUILDKITE:-}" && -z "${BUILDKITE_BUILD_ID:-}" ]] \
    && (( default_jobs > 2 )); then
    default_jobs=2
  fi

  export CTX_CPU_COUNT="${cpu_count}"
  export CTX_TOTAL_MEMORY_GB="${memory_gb}"
  if [[ -n "${TEST_TMPDIR:-}" ]]; then
    export TMPDIR="${TEST_TMPDIR}/tmp"
  else
    export TMPDIR="${TMPDIR:-${CTX_REPO_ROOT}/target/tmp}"
  fi
  export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-${TEST_TMPDIR:-${CTX_REPO_ROOT}/target}/cargo-target}"
  export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-${CTX_CARGO_JOBS:-${default_jobs}}}"
  export RUST_TEST_THREADS="${RUST_TEST_THREADS:-${CTX_TEST_THREADS:-${CARGO_BUILD_JOBS}}}"
  export CARGO_TERM_COLOR="${CARGO_TERM_COLOR:-always}"
  export CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR:-${TEST_UNDECLARED_OUTPUTS_DIR:-${CTX_REPO_ROOT}/target/ctx-artifacts}/ctx-artifacts/${mode}}"

  if [[ "${CTX_USE_SCCACHE:-0}" != "1" && "${RUSTC_WRAPPER:-}" == *sccache* ]]; then
    unset RUSTC_WRAPPER
  fi

  mkdir -p "${TMPDIR}" "${CARGO_TARGET_DIR}" "${CTX_ARTIFACT_DIR}"

  CTX_TIMING_FILE="${CTX_ARTIFACT_DIR}/timings.json"
  CTX_TIMING_EVENTS="${CTX_TIMING_FILE}.events"
  : > "${CTX_TIMING_EVENTS}"

  cargo_locked_args=()
  if [[ "${CTX_CARGO_LOCKED:-1}" != "0" && -f Cargo.lock ]]; then
    cargo_locked_args+=(--locked)
  fi

  printf 'bazel gate: mode=%s repo=%s home=%s cargo_home=%s rustup_home=%s target=%s artifacts=%s\n' \
    "${mode}" \
    "${CTX_REPO_ROOT}" \
    "${HOME}" \
    "${CARGO_HOME}" \
    "${RUSTUP_HOME}" \
    "${CARGO_TARGET_DIR}" \
    "${CTX_ARTIFACT_DIR}"
  printf 'resource limits: cpu=%s memory_gb=%s cargo_jobs=%s test_threads=%s tmpdir=%s\n' \
    "${CTX_CPU_COUNT}" \
    "${CTX_TOTAL_MEMORY_GB}" \
    "${CARGO_BUILD_JOBS}" \
    "${RUST_TEST_THREADS}" \
    "${TMPDIR}"
}

finish_timing() {
  if [[ -n "${CTX_TIMING_EVENTS:-}" && -f "${CTX_TIMING_EVENTS}" ]]; then
    {
      printf '[\n'
      awk 'NR == 1 { printf "  %s", $0; next } { printf ",\n  %s", $0 } END { if (NR > 0) printf "\n" }' "${CTX_TIMING_EVENTS}"
      printf ']\n'
    } > "${CTX_TIMING_FILE}"
    rm -f "${CTX_TIMING_EVENTS}"
    printf 'timing artifact: %s\n' "${CTX_TIMING_FILE}"
  fi
}

record_timing() {
  local name="$1"
  local status="$2"
  local started_at="$3"
  local ended_at="$4"
  local duration_s="$5"
  local exit_code="$6"
  local note="${7:-}"

  printf '{"name":"%s","status":"%s","started_at_unix_s":%s,"ended_at_unix_s":%s,"duration_s":%s,"exit_code":%s,"note":"%s"}\n' \
    "$(json_escape "${name}")" \
    "$(json_escape "${status}")" \
    "${started_at}" \
    "${ended_at}" \
    "${duration_s}" \
    "${exit_code}" \
    "$(json_escape "${note}")" >> "${CTX_TIMING_EVENTS}"
}

run_timed() {
  local name="$1"
  shift
  local started_at ended_at duration_s exit_code status command

  command="$*"
  started_at="$(date +%s)"
  printf '==> %s\n' "${name}"
  set +e
  "$@"
  exit_code=$?
  set -e
  ended_at="$(date +%s)"
  duration_s=$(( ended_at - started_at ))

  if (( exit_code == 0 )); then
    status="passed"
  else
    status="failed"
  fi

  record_timing "${name}" "${status}" "${started_at}" "${ended_at}" "${duration_s}" "${exit_code}" "${command}"
  return "${exit_code}"
}

cargo_test_filter() {
  local package="$1"
  local filter="$2"

  run_timed "cargo-test-${package}-${filter}" \
    cargo test -p "${package}" "${cargo_locked_args[@]}" "${filter}" -- --test-threads "${RUST_TEST_THREADS}"
}

cargo_test_filter_ignored() {
  local package="$1"
  local filter="$2"

  run_timed "cargo-test-${package}-${filter}-ignored" \
    cargo test -p "${package}" "${cargo_locked_args[@]}" "${filter}" -- --ignored --test-threads "${RUST_TEST_THREADS}"
}

build_ctx_debug() {
  run_timed "cargo-build-ctx-debug" cargo build -p ctx --bin ctx "${cargo_locked_args[@]}"
}

ctx_debug_bin() {
  local suffix=""
  case "$(uname -s 2>/dev/null || true)" in
    MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s\n' "${CARGO_TARGET_DIR}/debug/ctx${suffix}"
}

run_fresh_home_flow() {
  local data_root fixture ctx_bin list_json record_id

  build_ctx_debug
  ctx_bin="$(ctx_debug_bin)"
  [[ -x "${ctx_bin}" || -f "${ctx_bin}" ]] || fail "ctx debug binary missing: ${ctx_bin}"

  data_root="$(mktemp -d "${TMPDIR}/ctx-fresh-home.XXXXXX")"
  fixture="${CTX_REPO_ROOT}/tests/fixtures/provider-history/codex-sessions"

  run_timed "fresh-home-setup" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" setup
  run_timed "fresh-home-sources" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" sources --json
  run_timed "fresh-home-import" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" import --provider codex --path "${fixture}" --json

  list_json="$(CTX_DATA_ROOT="${data_root}" "${ctx_bin}" list --json)"
  record_id="$(printf '%s\n' "${list_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p' | head -n1)"
  [[ -n "${record_id}" ]] || fail 'fresh-home list did not return a record id'

  run_timed "fresh-home-search" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" search onboarding --json
  run_timed "fresh-home-show" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" show "${record_id}" --json
  run_timed "fresh-home-context" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" context onboarding --json
  run_timed "fresh-home-status" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" status --json
  run_timed "fresh-home-doctor" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" doctor --json
  run_timed "fresh-home-validate" env CTX_DATA_ROOT="${data_root}" "${ctx_bin}" validate --json
}

file_mode() {
  local path="$1"

  stat -c '%a' "${path}" 2>/dev/null || stat -f '%Lp' "${path}"
}

file_size_bytes() {
  local path="$1"

  wc -c < "${path}" | tr -d '[:space:]'
}

write_manifest_entry() {
  local path="$1"
  local mode size checksum target

  if [[ -L "${path}" ]]; then
    mode="$(file_mode "${path}")"
    target="$(readlink "${path}")"
    printf '{"type":"symlink","mode":"%s","target":"%s","path":"%s"}\n' \
      "$(json_escape "${mode}")" \
      "$(json_escape "${target}")" \
      "$(json_escape "${path}")"
    return 0
  fi

  if [[ -f "${path}" ]]; then
    mode="$(file_mode "${path}")"
    size="$(file_size_bytes "${path}")"
    checksum="$(sha256_file "${path}")"
    printf '{"type":"file","mode":"%s","bytes":%s,"sha256":"%s","path":"%s"}\n' \
      "$(json_escape "${mode}")" \
      "${size}" \
      "$(json_escape "${checksum}")" \
      "$(json_escape "${path}")"
    return 0
  fi

  if [[ -d "${path}" ]]; then
    mode="$(file_mode "${path}")"
    printf '{"type":"dir","mode":"%s","path":"%s"}\n' \
      "$(json_escape "${mode}")" \
      "$(json_escape "${path}")"
    return 0
  fi

  printf '{"type":"missing","path":"%s"}\n' "$(json_escape "${path}")"
}

write_side_effect_manifest() {
  local output="$1"
  shift
  local root path

  : > "${output}"
  for root in "$@"; do
    if [[ -d "${root}" ]]; then
      while IFS= read -r -d '' path; do
        write_manifest_entry "${path}" >> "${output}"
      done < <(find "${root}" -print0 | sort -z)
    else
      write_manifest_entry "${root}" >> "${output}"
    fi
  done
}

run_security_ctx_command() {
  local name="$1"
  local workspace="$2"
  local home_dir="$3"
  local data_root="$4"
  local ctx_bin="$5"
  shift 5

  (
    cd "${workspace}"
    run_timed "${name}" \
      env \
        -u OPENAI_API_KEY \
        -u ANTHROPIC_API_KEY \
        -u GEMINI_API_KEY \
        -u GOOGLE_API_KEY \
        -u AZURE_OPENAI_API_KEY \
        HOME="${home_dir}" \
        CTX_DATA_ROOT="${data_root}" \
        "${ctx_bin}" "$@"
  )
}

capture_security_ctx_command() {
  local workspace="$1"
  local home_dir="$2"
  local data_root="$3"
  local ctx_bin="$4"
  shift 4

  (
    cd "${workspace}"
    env \
      -u OPENAI_API_KEY \
      -u ANTHROPIC_API_KEY \
      -u GEMINI_API_KEY \
      -u GOOGLE_API_KEY \
      -u AZURE_OPENAI_API_KEY \
      HOME="${home_dir}" \
      CTX_DATA_ROOT="${data_root}" \
      "${ctx_bin}" "$@"
  )
}

run_security_runtime_flow() {
  local prefix="$1"
  local workspace="$2"
  local home_dir="$3"
  local data_root="$4"
  local ctx_bin="$5"
  local fixture="$6"
  local list_json record_id

  run_security_ctx_command "${prefix}-setup" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" setup
  run_security_ctx_command "${prefix}-sources" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" sources --json
  run_security_ctx_command "${prefix}-import" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" import --provider codex --path "${fixture}" --json

  list_json="$(capture_security_ctx_command "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" list --json)"
  printf '%s\n' "${list_json}" > "${CTX_ARTIFACT_DIR}/${prefix}-list.json"
  record_id="$(printf '%s\n' "${list_json}" | sed -n 's/.*"id": "\([^"]*\)".*/\1/p' | head -n1)"
  [[ -n "${record_id}" ]] || fail "${prefix} list did not return a record id"

  run_security_ctx_command "${prefix}-search" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" search onboarding --json
  run_security_ctx_command "${prefix}-show" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" show "${record_id}" --json
  run_security_ctx_command "${prefix}-context" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" context onboarding --json
  run_security_ctx_command "${prefix}-status" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" status --json
  run_security_ctx_command "${prefix}-doctor" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" doctor --json
  run_security_ctx_command "${prefix}-validate" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" validate --json
}

write_no_network_report() {
  local output="$1"
  local status="$2"
  local reason="$3"
  local tool="${4:-}"
  local trace_dir="${5:-}"

  cat > "${output}" <<EOF
{
  "status": "$(json_escape "${status}")",
  "reason": "$(json_escape "${reason}")",
  "tool": "$(json_escape "${tool}")",
  "trace_dir": "$(json_escape "${trace_dir}")",
  "checked_syscalls": [
    "socket",
    "connect",
    "sendto",
    "sendmsg",
    "recvfrom",
    "recvmsg",
    "accept",
    "accept4",
    "bind",
    "listen"
  ]
}
EOF
}

write_side_effect_report() {
  local output="$1"
  local status="$2"
  local reason="$3"
  local before_manifest="$4"
  local after_manifest="$5"
  local diff_output="${6:-}"

  cat > "${output}" <<EOF
{
  "status": "$(json_escape "${status}")",
  "reason": "$(json_escape "${reason}")",
  "before_manifest": "$(json_escape "${before_manifest}")",
  "after_manifest": "$(json_escape "${after_manifest}")",
  "diff": "$(json_escape "${diff_output}")",
  "monitored_surfaces": [
    "fake_home",
    "fake_repo_files_and_hooks",
    "read_only_provider_fixture"
  ],
  "detects": [
    "content",
    "creation",
    "deletion",
    "mode"
  ]
}
EOF
}

assert_manifest_unchanged() {
  local name="$1"
  local before="$2"
  local after="$3"
  local report="$4"
  local diff_output="${CTX_ARTIFACT_DIR}/${name}-diff.txt"

  if cmp -s "${before}" "${after}"; then
    write_side_effect_report "${report}" "passed" "monitored files unchanged" "${before}" "${after}"
    return 0
  fi

  diff -u "${before}" "${after}" > "${diff_output}" || true
  write_side_effect_report "${report}" "failed" "monitored files changed" "${before}" "${after}" "${diff_output}"
  printf 'side-effect manifest changed for %s:\n' "${name}" >&2
  cat "${diff_output}" >&2
  fail "security side-effect oracle detected modified ${name}"
}

run_straced_security_ctx_command() {
  local name="$1"
  local strace_bin="$2"
  local trace_output="$3"
  local workspace="$4"
  local home_dir="$5"
  local data_root="$6"
  local ctx_bin="$7"
  shift 7

  (
    cd "${workspace}"
    run_timed "${name}" \
      "${strace_bin}" \
        -f \
        -e trace=network \
        -o "${trace_output}" \
        env \
          -u OPENAI_API_KEY \
          -u ANTHROPIC_API_KEY \
          -u GEMINI_API_KEY \
          -u GOOGLE_API_KEY \
          -u AZURE_OPENAI_API_KEY \
          HOME="${home_dir}" \
          CTX_DATA_ROOT="${data_root}" \
          "${ctx_bin}" "$@"
  )
}

run_security_no_network_oracle() {
  local oracle_root="$1"
  local workspace="$2"
  local home_dir="$3"
  local fixture="$4"
  local ctx_bin="$5"
  local report="${CTX_ARTIFACT_DIR}/no-network-oracle.json"
  local strace_bin data_root trace_dir violations grep_status probe_trace probe_stderr probe_status probe_reason

  strace_bin="$(command -v strace 2>/dev/null || true)"
  if [[ -z "${strace_bin}" ]]; then
    write_no_network_report "${report}" "skipped" "strace not found on PATH"
    printf 'no-network oracle skipped: strace not found on PATH\n'
    return 0
  fi

  data_root="${oracle_root}/no network data root"
  trace_dir="${CTX_ARTIFACT_DIR}/strace-network"
  violations="${CTX_ARTIFACT_DIR}/no-network-violations.txt"
  rm -rf "${trace_dir}"
  mkdir -p "${data_root}" "${trace_dir}"

  probe_trace="${trace_dir}/probe.log"
  probe_stderr="${trace_dir}/probe.stderr"
  set +e
  "${strace_bin}" -f -e trace=network -o "${probe_trace}" true >/dev/null 2>"${probe_stderr}"
  probe_status=$?
  set -e
  if (( probe_status != 0 )); then
    probe_reason="$(tr '\n' ' ' < "${probe_stderr}" | sed 's/[[:space:]][[:space:]]*/ /g' | cut -c1-240)"
    if [[ -z "${probe_reason}" ]]; then
      probe_reason="strace probe exited with status ${probe_status}"
    fi
    write_no_network_report "${report}" "skipped" "${probe_reason}" "${strace_bin}" "${trace_dir}"
    printf 'no-network oracle skipped: %s\n' "${probe_reason}"
    return 0
  fi

  run_straced_security_ctx_command "security-no-network-setup" "${strace_bin}" "${trace_dir}/setup.log" \
    "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" setup
  run_straced_security_ctx_command "security-no-network-import" "${strace_bin}" "${trace_dir}/import.log" \
    "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" import --provider codex --path "${fixture}" --json
  run_straced_security_ctx_command "security-no-network-search" "${strace_bin}" "${trace_dir}/search.log" \
    "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" search onboarding --json
  run_straced_security_ctx_command "security-no-network-context" "${strace_bin}" "${trace_dir}/context.log" \
    "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" context onboarding --json

  set +e
  grep -R -n -E 'AF_INET|AF_INET6' "${trace_dir}" > "${violations}"
  grep_status=$?
  set -e

  if (( grep_status == 0 )); then
    write_no_network_report "${report}" "failed" "AF_INET or AF_INET6 network activity observed" "${strace_bin}" "${trace_dir}"
    printf 'no-network oracle detected AF_INET/AF_INET6 activity:\n' >&2
    cat "${violations}" >&2
    fail 'setup/import/search/context attempted network activity'
  fi
  if (( grep_status > 1 )); then
    cat "${violations}" >&2 || true
    fail 'no-network oracle could not scan strace output'
  fi

  write_no_network_report "${report}" "passed" "no traced AF_INET or AF_INET6 activity observed" "${strace_bin}" "${trace_dir}"
  printf 'no-network oracle artifact: %s\n' "${report}"
}

prepare_security_oracle_fixture() {
  local oracle_root="$1"
  local home_dir="$2"
  local workspace="$3"
  local fixture_copy="$4"
  local source_fixture="${CTX_REPO_ROOT}/tests/fixtures/provider-history/codex-sessions"

  mkdir -p "${home_dir}" "${workspace}" "$(dirname "${fixture_copy}")"
  printf '# ctx security sentinel\nCTX_SENTINEL_BASHRC=unchanged\n' > "${home_dir}/.bashrc"
  printf '# ctx security sentinel\nCTX_SENTINEL_ZSHRC=unchanged\n' > "${home_dir}/.zshrc"

  git -C "${workspace}" init -q
  mkdir -p "${workspace}/src" "${workspace}/docs" "${workspace}/.git/hooks"
  printf '# ctx side-effect sentinel repo\n' > "${workspace}/README.md"
  printf 'repo sentinel source\n' > "${workspace}/src/sentinel.txt"
  printf 'repo sentinel docs\n' > "${workspace}/docs/sentinel.txt"
  printf '#!/usr/bin/env bash\nprintf "pre-commit sentinel should not run\\n"\n' > "${workspace}/.git/hooks/pre-commit"
  printf '#!/usr/bin/env bash\nprintf "post-checkout sentinel should not run\\n"\n' > "${workspace}/.git/hooks/post-checkout"
  chmod +x "${workspace}/.git/hooks/pre-commit" "${workspace}/.git/hooks/post-checkout"

  mkdir -p "${fixture_copy}"
  cp -R "${source_fixture}/." "${fixture_copy}/"
  chmod -R a-w "${fixture_copy}"

  printf 'security oracle workspace: %s\n' "${oracle_root}"
  printf 'security oracle provider fixture: %s\n' "${fixture_copy}"
}

run_security_side_effect_oracle() {
  local ctx_bin="$1"
  local oracle_root home_dir workspace data_root fixture_copy before_manifest after_manifest report

  oracle_root="$(mktemp -d "${TMPDIR}/ctx security oracle.XXXXXX")"
  home_dir="${oracle_root}/home with spaces"
  workspace="${oracle_root}/workspace repo"
  data_root="${oracle_root}/data root"
  fixture_copy="${oracle_root}/provider fixtures/codex sessions"
  before_manifest="${CTX_ARTIFACT_DIR}/side-effect-before.jsonl"
  after_manifest="${CTX_ARTIFACT_DIR}/side-effect-after.jsonl"
  report="${CTX_ARTIFACT_DIR}/side-effect-oracle.json"

  mkdir -p "${data_root}"
  prepare_security_oracle_fixture "${oracle_root}" "${home_dir}" "${workspace}" "${fixture_copy}"

  write_side_effect_manifest "${before_manifest}" "${home_dir}" "${workspace}" "${fixture_copy}"
  run_security_runtime_flow "security-side-effect" "${workspace}" "${home_dir}" "${data_root}" "${ctx_bin}" "${fixture_copy}"
  run_security_no_network_oracle "${oracle_root}" "${workspace}" "${home_dir}" "${fixture_copy}" "${ctx_bin}"
  write_side_effect_manifest "${after_manifest}" "${home_dir}" "${workspace}" "${fixture_copy}"

  assert_manifest_unchanged "side-effect" "${before_manifest}" "${after_manifest}" "${report}"
  printf 'side-effect oracle artifact: %s\n' "${report}"
  printf 'side-effect manifests: %s %s\n' "${before_manifest}" "${after_manifest}"
}

run_security_no_repo_writes() {
  local before after ctx_bin

  build_ctx_debug
  ctx_bin="$(ctx_debug_bin)"
  [[ -x "${ctx_bin}" || -f "${ctx_bin}" ]] || fail "ctx debug binary missing: ${ctx_bin}"

  before="$(git status --porcelain=v1 --untracked-files=all)"
  printf '%s\n' "${before}" > "${CTX_ARTIFACT_DIR}/git-status-before.txt"

  run_security_side_effect_oracle "${ctx_bin}"

  after="$(git status --porcelain=v1 --untracked-files=all)"
  printf '%s\n' "${after}" > "${CTX_ARTIFACT_DIR}/git-status-after.txt"

  if [[ "${before}" != "${after}" ]]; then
    printf 'git status before Bazel no-repo-writes flow:\n%s\n' "${before}" >&2
    printf 'git status after Bazel no-repo-writes flow:\n%s\n' "${after}" >&2
    fail 'setup/import/search/context flow modified repo-visible files'
  fi
}

assert_no_matches() {
  local name="$1"
  local pattern="$2"
  shift 2
  local output status

  output="${CTX_ARTIFACT_DIR}/static-audit-${name}.txt"
  set +e
  rg -n --hidden -S -e "${pattern}" "$@" > "${output}"
  status=$?
  set -e

  if (( status == 0 )); then
    printf 'static audit `%s` found forbidden matches:\n' "${name}" >&2
    cat "${output}" >&2
    fail "static audit ${name} failed"
  fi
  if (( status > 1 )); then
    cat "${output}" >&2 || true
    fail "static audit ${name} could not scan requested files"
  fi
}

run_security_static_audit() {
  local ctx_package="ctx"
  local runtime_crates=(
    crates/ctx-cli/src
    crates/work-record-capture/src
    crates/work-record-core/src
    crates/work-record-search/src
    crates/work-record-store/src
  )
  local public_surfaces=(
    README.md
    SECURITY.md
    docs
    skills/ctx-agent-memory/SKILL.md
    scripts/install.sh
    scripts/install.ps1
    crates/ctx-cli/src/main.rs
  )

  cargo_test_filter work-record-capture codex_session_file_rejects_symlinked_jsonl_files
  cargo_test_filter work-record-capture codex_session_tree_rejects_symlinked_jsonl_files

  assert_no_matches \
    rust-network-clients \
    'std::net::|Tcp(Stream|Listener)|UdpSocket|Unix(Stream|Listener)|reqwest|ureq|hyper|tonic|axum|warp|actix_web|tungstenite|tokio::net' \
    "${runtime_crates[@]}" Cargo.toml Cargo.lock
  assert_no_matches \
    rust-subprocess-spawn \
    'std::process::Command|process::Command|Command::new' \
    "${runtime_crates[@]}"
  assert_no_matches \
    rust-browser-daemon \
    'webbrowser|open::that|xdg-open|open_browser|daemonize|start_server|serve_forever' \
    "${runtime_crates[@]}"
  assert_no_matches \
    rust-llm-api-surface \
    'OPENAI_API_KEY|ANTHROPIC_API_KEY|GEMINI_API_KEY|GOOGLE_API_KEY|async_openai|openai::|anthropic::|gemini::' \
    "${runtime_crates[@]}" Cargo.toml Cargo.lock
  assert_no_matches \
    rust-path-mutation \
    'env::set_var\([^)]*["'\'']PATH["'\'']|std::env::set_var\([^)]*["'\'']PATH["'\'']' \
    "${runtime_crates[@]}"
  assert_no_matches \
    public-path-mutation \
    'export[[:space:]]+PATH=|PATH=.*\$[{]?PATH|setx[[:space:]]+PATH|\.bashrc|\.zshrc|fish_user_paths|shell hook' \
    "${public_surfaces[@]}"
  assert_no_matches \
    public-llm-key-requirement \
    'OPENAI_API_KEY|ANTHROPIC_API_KEY|GEMINI_API_KEY|GOOGLE_API_KEY|API key required|required API key' \
    "${public_surfaces[@]}"
}

run_cli_contract_tests() {
  local ctx_package="ctx"

  cargo_test_filter "${ctx_package}" help_exposes_only_search_mvp_commands
  cargo_test_filter "${ctx_package}" removed_commands_are_rejected
  cargo_test_filter "${ctx_package}" provider_help_matches_implemented_importers
  cargo_test_filter "${ctx_package}" codex_cli_provider_oracle_covers_retrieval_and_claimed_fidelity
  cargo_test_filter "${ctx_package}" pi_cli_import_search_and_context_flow
  cargo_test_filter "${ctx_package}" pi_cli_reports_malformed_partial_and_schema_failures
  cargo_test_filter "${ctx_package}" pi_cli_rejects_directory_import_path
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

  if command -v sha256 >/dev/null 2>&1; then
    sha256 -q "${path}"
    return 0
  fi

  fail 'sha256sum, shasum, or sha256 is required'
}

write_release_fixture_platform() {
  local root="$1"
  local platform="$2"
  local platform_key="$3"
  local target_triple="$4"
  local platform_dir artifact checksum bytes generated_at commit branch

  platform_dir="${root}/${platform}"
  mkdir -p "${platform_dir}"
  artifact="ctx-0.1.0-${target_triple}"
  if [[ "${platform}" == windows-* ]]; then
    artifact="${artifact}.exe"
  fi
  printf 'ctx release fixture for %s\n' "${platform}" > "${platform_dir}/${artifact}"
  chmod 0755 "${platform_dir}/${artifact}" 2>/dev/null || true

  checksum="$(sha256_file "${platform_dir}/${artifact}")"
  bytes="$(wc -c < "${platform_dir}/${artifact}" | tr -d '[:space:]')"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${platform_dir}/ctx-release-metadata.env" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=0.1.0
CTX_RELEASE_CHANNEL=dry-run
CTX_RELEASE_BASE_URL=https://github.com/ctxrs/ctx/releases/download/v0.1.0
CTX_RELEASE_ARTIFACT_${platform_key}=${artifact}
CTX_RELEASE_SHA256_${platform_key}=${checksum}
EOF

  cat > "${platform_dir}/checksums.sha256" <<EOF
${checksum}  ${artifact}
EOF

  cat > "${platform_dir}/manifest.json" <<EOF
{
  "schema_version": 1,
  "evidence_class": "contract",
  "self_test_fixture": true,
  "dry_run": true,
  "upload": false,
  "package": "ctx",
  "version": "0.1.0",
  "platform": "$(json_escape "${platform}")",
  "target_triple": "$(json_escape "${target_triple}")",
  "host_triple": "$(json_escape "${target_triple}")",
  "expected_host_triple": "$(json_escape "${target_triple}")",
  "git_commit": "$(json_escape "${commit}")",
  "git_branch": "$(json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at},
  "artifacts": [
    {
      "path": "$(json_escape "artifacts/buildkite/release-dry-run/${platform}/${artifact}")",
      "sha256": "$(json_escape "${checksum}")",
      "bytes": ${bytes}
    }
  ]
}
EOF
}

write_release_fixture_root() {
  local root="$1"

  rm -rf "${root}"
  mkdir -p "${root}"
  write_release_fixture_platform "${root}" "linux-x64" "linux_x64" "x86_64-unknown-linux-gnu"
  write_release_fixture_platform "${root}" "macos-arm64" "macos_arm64" "aarch64-apple-darwin"
  write_release_fixture_platform "${root}" "macos-x64" "macos_x64" "x86_64-apple-darwin"
  write_release_fixture_platform "${root}" "windows-x64" "windows_x64" "x86_64-pc-windows-gnu"
}

write_release_evidence_platform() {
  local root="$1"
  local platform="$2"
  local platform_key="$3"
  local target_triple="$4"
  local marker="${5:-real}"
  local platform_dir artifact checksum bytes generated_at commit branch marker_json marker_env

  platform_dir="${root}/${platform}"
  mkdir -p "${platform_dir}"
  artifact="ctx-0.1.0-${target_triple}"
  if [[ "${platform}" == windows-* ]]; then
    artifact="${artifact}.exe"
  fi
  printf 'ctx release evidence for %s\n' "${platform}" > "${platform_dir}/${artifact}"
  chmod 0755 "${platform_dir}/${artifact}" 2>/dev/null || true

  checksum="$(sha256_file "${platform_dir}/${artifact}")"
  bytes="$(wc -c < "${platform_dir}/${artifact}" | tr -d '[:space:]')"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  marker_json=""
  marker_env=""
  if [[ "${marker}" == "contract_fixture" ]]; then
    marker_json=$'  "evidence_class": "contract_fixture",\n  "self_test_fixture": true,\n'
    marker_env=$'CTX_RELEASE_EVIDENCE_CLASS=contract_fixture\nCTX_RELEASE_SELF_TEST_FIXTURE=true\n'
  fi

  cat > "${platform_dir}/ctx-release-metadata.env" <<EOF
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_VERSION=0.1.0
CTX_RELEASE_CHANNEL=dry-run
${marker_env}CTX_RELEASE_BASE_URL=https://github.com/ctxrs/ctx/releases/download/v0.1.0
CTX_RELEASE_ARTIFACT_${platform_key}=${artifact}
CTX_RELEASE_SHA256_${platform_key}=${checksum}
EOF

  cat > "${platform_dir}/checksums.sha256" <<EOF
${checksum}  ${artifact}
EOF

  cat > "${platform_dir}/manifest.json" <<EOF
{
  "schema_version": 1,
${marker_json}  "dry_run": true,
  "upload": false,
  "package": "ctx",
  "version": "0.1.0",
  "platform": "$(json_escape "${platform}")",
  "target_triple": "$(json_escape "${target_triple}")",
  "host_triple": "$(json_escape "${target_triple}")",
  "expected_host_triple": "$(json_escape "${target_triple}")",
  "git_commit": "$(json_escape "${commit}")",
  "git_branch": "$(json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at},
  "artifacts": [
    {
      "path": "$(json_escape "artifacts/buildkite/release-dry-run/${platform}/${artifact}")",
      "sha256": "$(json_escape "${checksum}")",
      "bytes": ${bytes}
    }
  ]
}
EOF
}

write_release_evidence_artifact_smoke() {
  local root="$1"
  local platform="$2"
  local platform_key="$3"
  local target_triple="$4"
  local marker="${5:-real}"
  local artifact artifact_rel artifact_full checksum bytes out_dir command_dir generated_at commit branch marker_json

  artifact="ctx-0.1.0-${target_triple}"
  if [[ "${platform}" == windows-* ]]; then
    artifact="${artifact}.exe"
  fi
  artifact_rel="artifacts/buildkite/release-dry-run/${platform}/${artifact}"
  artifact_full="${root}/${artifact_rel}"
  checksum="$(sha256_file "${artifact_full}")"
  bytes="$(wc -c < "${artifact_full}" | tr -d '[:space:]')"
  out_dir="${root}/artifacts/buildkite/release-artifact-smoke/${platform}"
  command_dir="${out_dir}/commands"
  mkdir -p "${command_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"
  marker_json=""
  if [[ "${marker}" == "contract_fixture" ]]; then
    marker_json=$'  "evidence_class": "contract_fixture",\n  "self_test_fixture": true,\n'
  fi

  printf 'release evidence command output for %s\n' "${platform}" > "${command_dir}/version.stdout"

  cat > "${out_dir}/artifact-smoke.json" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_release_artifact_smoke",
${marker_json}  "mode": "release-artifact-smoke",
  "status": "passed",
  "publishing": false,
  "platform": "$(json_escape "${platform}")",
  "platform_key": "$(json_escape "${platform_key}")",
  "target_triple": "$(json_escape "${target_triple}")",
  "host_triple": "$(json_escape "${target_triple}")",
  "release_dry_run_dir": "artifacts/buildkite/release-dry-run/${platform}",
  "release_manifest": "artifacts/buildkite/release-dry-run/${platform}/manifest.json",
  "release_metadata": "artifacts/buildkite/release-dry-run/${platform}/ctx-release-metadata.env",
  "release_artifact": "$(json_escape "${artifact_rel}")",
  "release_artifact_name": "$(json_escape "${artifact}")",
  "release_artifact_sha256": "$(json_escape "${checksum}")",
  "release_artifact_bytes": ${bytes},
  "install_method": "release-evidence-fixture",
  "installed_artifact_runtime": true,
  "fixture": "tests/fixtures/provider-history/codex-sessions",
  "command_output_dir": "artifacts/buildkite/release-artifact-smoke/${platform}/commands",
  "version_output": "ctx 0.1.0",
  "version_status": "passed",
  "setup_status": "passed",
  "import_status": "passed",
  "search_status": "passed",
  "context_status": "passed",
  "doctor_status": "passed",
  "validate_status": "passed",
  "git_commit": "$(json_escape "${commit}")",
  "git_branch": "$(json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF

  cat > "${out_dir}/artifact-smoke.md" <<EOF
# ctx Release Artifact Smoke

- Publishing: false
- Platform: \`${platform}\`
- Target triple: \`${target_triple}\`
- Release artifact: \`${artifact_rel}\`
- Status: passed
EOF
}

write_release_evidence_summary() {
  local root="$1"
  local rel_dir="$2"
  local mode="$3"
  local out_dir="${root}/${rel_dir}"
  local generated_at commit branch

  mkdir -p "${out_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${out_dir}/${mode}.json" <<EOF
{
  "schema_version": 1,
  "kind": "ctx_release_evidence_summary",
  "mode": "$(json_escape "${mode}")",
  "status": "passed",
  "publishing": false,
  "git_commit": "$(json_escape "${commit}")",
  "git_branch": "$(json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at}
}
EOF
}

write_release_evidence_finished_artifacts() {
  local root="$1"

  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/product-decisions" "product-decisions"
  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/provider-fixtures" "provider-fixtures"
  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/rich-search-context" "rich-search-context"
  printf '{"schema_version":1,"kind":"rich_context_evidence"}\n' \
    > "${root}/artifacts/buildkite/finished-product/rich-search-context/rich-context.json"
  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/search-mvp-package-audit" "search-mvp-package-audit"
  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/security-archive-fixtures" "security-archive-fixtures"
  printf '# Security Archive Evidence\n\n- Publishing: false\n' \
    > "${root}/artifacts/buildkite/finished-product/security-archive-fixtures/security-archive-fixtures.md"
  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/jj-e2e-blocker-status" "jj-e2e-blocker-status"
  printf 'jj e2e blocker release evidence\n' \
    > "${root}/artifacts/buildkite/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.txt"
  write_release_evidence_summary "${root}" "artifacts/buildkite/finished-product/installer-dry-run-smoke" "installer-dry-run-smoke"
  printf 'ctx install plan release evidence; publishing false\n' \
    > "${root}/artifacts/buildkite/finished-product/installer-dry-run-smoke/install-dry-run.txt"
}

write_release_evidence_provider_live_lanes() {
  local root="$1"
  local out_dir="${root}/artifacts/buildkite/provider-live-e2e-lanes"
  local generated_at commit branch

  mkdir -p "${out_dir}"
  generated_at="$(date +%s)"
  commit="$(git rev-parse HEAD)"
  branch="$(git branch --show-current)"

  cat > "${out_dir}/provider-live-e2e-lanes.json" <<EOF
{
  "schema_version": 1,
  "kind": "provider_live_e2e_lane_definitions",
  "publishing": false,
  "default_enabled": false,
  "git_commit": "$(json_escape "${commit}")",
  "git_branch": "$(json_escape "${branch}")",
  "generated_at_unix_s": ${generated_at},
  "lanes": []
}
EOF

  cat > "${out_dir}/provider-live-e2e-lanes.md" <<'EOF'
# Provider Live E2E Lane Definitions

- Publishing: false
- Global opt-in: `CTX_LIVE_PROVIDER_E2E=1`
- Providers listed by the release contract include Codex, Claude Code, Gemini CLI, and OpenRouter Generated Harness.
EOF
}

write_release_evidence_supply_chain_r2() {
  local root="$1"

  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/supply-chain" \
    bash scripts/release-supply-chain-proof.sh
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/r2-staging-readback" \
    bash scripts/release-r2-staging-readback-proof.sh "${root}/artifacts/buildkite/release-candidate"
}

write_release_evidence_docs() {
  local root="$1"
  local path

  for path in \
    docs/release-install.md \
    docs/release-supply-chain.md \
    docs/release-r2-layout.md \
    docs/freebsd-release-worker.md; do
    mkdir -p "${root}/$(dirname "${path}")"
    cp "${CTX_REPO_ROOT}/${path}" "${root}/${path}"
  done
}

write_release_evidence_root() {
  local root="$1"
  local freebsd_marker="${2:-real}"
  local release_root

  rm -rf "${root}"
  mkdir -p "${root}/artifacts/buildkite/pipeline-contract"
  printf 'release evidence pipeline contract; publishing false\n' \
    > "${root}/artifacts/buildkite/pipeline-contract/pipeline-contract.txt"

  release_root="${root}/artifacts/buildkite/release-dry-run"
  write_release_evidence_platform "${release_root}" "linux-x64" "linux_x64" "x86_64-unknown-linux-gnu"
  write_release_evidence_artifact_smoke "${root}" "linux-x64" "linux_x64" "x86_64-unknown-linux-gnu"
  write_release_evidence_platform "${release_root}" "macos-arm64" "macos_arm64" "aarch64-apple-darwin"
  write_release_evidence_artifact_smoke "${root}" "macos-arm64" "macos_arm64" "aarch64-apple-darwin"
  write_release_evidence_platform "${release_root}" "macos-x64" "macos_x64" "x86_64-apple-darwin"
  write_release_evidence_artifact_smoke "${root}" "macos-x64" "macos_x64" "x86_64-apple-darwin"
  write_release_evidence_platform "${release_root}" "windows-x64" "windows_x64" "x86_64-pc-windows-gnu"
  write_release_evidence_artifact_smoke "${root}" "windows-x64" "windows_x64" "x86_64-pc-windows-gnu"
  write_release_evidence_platform "${release_root}" "freebsd-x64" "freebsd_x64" "x86_64-unknown-freebsd" "${freebsd_marker}"
  write_release_evidence_artifact_smoke "${root}" "freebsd-x64" "freebsd_x64" "x86_64-unknown-freebsd" "${freebsd_marker}"

  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/release-candidate" \
    bash scripts/release-candidate-metadata.sh "${release_root}"
  CTX_ARTIFACT_DIR="${root}/artifacts/buildkite/r2-staging-smoke" \
    bash scripts/release-r2-staging-smoke.sh "${root}/artifacts/buildkite/release-candidate"
  write_release_evidence_supply_chain_r2 "${root}"
  write_release_evidence_finished_artifacts "${root}"
  write_release_evidence_provider_live_lanes "${root}"
  write_release_evidence_docs "${root}"
}

write_freebsd_blocker_fixture() {
  local out_dir="$1"

  rm -rf "${out_dir}"
  mkdir -p "${out_dir}"
  CTX_ARTIFACT_DIR="${out_dir}" run_timed "release-platform-blocker-freebsd-fixture" \
    bash scripts/release-platform-blocker.sh freebsd-x64
}

run_release_candidate_metadata_contract() {
  local release_root freebsd_dir candidate_dir

  release_root="${TMPDIR}/release-dry-run-fixture"
  freebsd_dir="${TMPDIR}/release-blockers/freebsd-x64"
  candidate_dir="${CTX_ARTIFACT_DIR}/release-candidate"
  write_release_fixture_root "${release_root}"
  write_freebsd_blocker_fixture "${freebsd_dir}"
  rm -rf "${candidate_dir}"
  mkdir -p "${candidate_dir}"
  CTX_ARTIFACT_DIR="${candidate_dir}" run_timed "release-candidate-metadata-contract" \
    bash scripts/release-candidate-metadata.sh \
      "${release_root}" \
      "${freebsd_dir}/freebsd-x64-blocker.json"
}

run_r2_staging_smoke_contract() {
  local candidate_dir r2_dir

  run_release_candidate_metadata_contract
  candidate_dir="${CTX_ARTIFACT_DIR}/release-candidate"
  r2_dir="${CTX_ARTIFACT_DIR}/r2-staging-smoke"
  rm -rf "${r2_dir}"
  mkdir -p "${r2_dir}"
  CTX_ARTIFACT_DIR="${r2_dir}" run_timed "r2-staging-smoke-contract" \
    bash scripts/release-r2-staging-smoke.sh "${candidate_dir}"
}

host_release_artifact_smoke_spec() {
  local host_triple

  host_triple="$(rustc -vV | awk '/^host:/ { print $2; exit }')"
  case "${host_triple}" in
    x86_64-unknown-linux-gnu)
      printf '%s|%s|%s\n' "linux-x64" "linux_x64" "${host_triple}"
      ;;
    aarch64-apple-darwin)
      printf '%s|%s|%s\n' "macos-arm64" "macos_arm64" "${host_triple}"
      ;;
    x86_64-apple-darwin)
      printf '%s|%s|%s\n' "macos-x64" "macos_x64" "${host_triple}"
      ;;
    x86_64-unknown-freebsd)
      printf '%s|%s|%s\n' "freebsd-x64" "freebsd_x64" "${host_triple}"
      ;;
    *)
      fail "release artifact smoke contract does not support host triple ${host_triple}"
      ;;
  esac
}

run_release_artifact_smoke_contract() {
  local spec platform platform_key host_triple contract_root release_dir smoke_dir smoke_json

  spec="$(host_release_artifact_smoke_spec)"
  IFS='|' read -r platform platform_key host_triple <<<"${spec}"
  contract_root="${CTX_ARTIFACT_DIR}/release-artifact-smoke-contract"
  release_dir="${contract_root}/release-dry-run/${platform}"
  smoke_dir="${contract_root}/release-artifact-smoke/${platform}"
  rm -rf "${contract_root}"
  mkdir -p "${release_dir}" "${smoke_dir}"

  CARGO_TARGET_DIR="${CTX_REPO_ROOT}/target" \
  CTX_RELEASE_PLATFORM="${platform}" \
  CTX_RELEASE_TARGET_TRIPLE="${host_triple}" \
  CTX_EXPECT_HOST_TRIPLE="${host_triple}" \
  CTX_ARTIFACT_DIR="${release_dir}" \
    run_timed "release-artifact-smoke-contract-dry-run" bash scripts/release-dry-run.sh

  CTX_RELEASE_PLATFORM="${platform}" \
  CTX_RELEASE_TARGET_TRIPLE="${host_triple}" \
  CTX_EXPECT_HOST_TRIPLE="${host_triple}" \
  CTX_RELEASE_DRY_RUN_DIR="${release_dir}" \
  CTX_ARTIFACT_DIR="${smoke_dir}" \
    run_timed "release-artifact-smoke-contract-runtime" bash scripts/release-artifact-smoke.sh "${platform}" "${release_dir}"

  smoke_json="${smoke_dir}/artifact-smoke.json"
  grep -F '"kind": "ctx_release_artifact_smoke"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not write the expected evidence kind'
  grep -F '"status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record passing status'
  grep -F "\"platform\": \"${platform}\"" "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record the host release platform'
  grep -F "\"platform_key\": \"${platform_key}\"" "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record the platform key'
  grep -F '"installed_artifact_runtime": true' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record installed artifact runtime execution'
  grep -F '"setup_status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record setup status'
  grep -F '"import_status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record import status'
  grep -F '"search_status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record search status'
  grep -F '"context_status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record context status'
  grep -F '"doctor_status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record doctor status'
  grep -F '"validate_status": "passed"' "${smoke_json}" >/dev/null \
    || fail 'release artifact smoke did not record validate status'
}

run_release_supply_chain_r2_contract() {
  local supply_dir r2_dir candidate_dir upload_log upload_status fake_wrangler_dir fake_wrangler_log
  local contract_dir evidence_root certificate_dir log status

  supply_dir="${CTX_ARTIFACT_DIR}/supply-chain"
  rm -rf "${supply_dir}"
  mkdir -p "${supply_dir}"
  CTX_ARTIFACT_DIR="${supply_dir}" run_timed "release-supply-chain-proof-contract" \
    bash scripts/release-supply-chain-proof.sh
  grep -F '"kind": "ctx_dependency_advisory_license_audit"' \
    "${supply_dir}/dependency-advisory-license-audit.json" >/dev/null \
    || fail 'supply-chain proof did not write dependency advisory/license evidence'
  grep -F '"kind": "ctx_sbom_provenance_signature_evidence"' \
    "${supply_dir}/sbom-provenance-signature.json" >/dev/null \
    || fail 'supply-chain proof did not write SBOM/provenance/signature evidence'
  grep -F '"required_before_public_release": true' \
    "${supply_dir}/sbom-provenance-signature.json" >/dev/null \
    || fail 'supply-chain artifact evidence does not block public release without proof'

  run_release_candidate_metadata_contract
  candidate_dir="${CTX_ARTIFACT_DIR}/release-candidate"
  r2_dir="${CTX_ARTIFACT_DIR}/r2-staging-readback"
  rm -rf "${r2_dir}"
  mkdir -p "${r2_dir}"
  CTX_ARTIFACT_DIR="${r2_dir}" run_timed "r2-staging-readback-blocked-contract" \
    bash scripts/release-r2-staging-readback-proof.sh "${candidate_dir}"
  grep -F '"kind": "ctx_r2_staging_readback"' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'R2 readback proof did not write readback evidence'
  grep -F '"status": "blocked_manual_required"' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'R2 readback proof must be blocked without upload/readback credentials'
  grep -F '"upload_performed": false' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'R2 readback proof must not claim upload without credentials'
  grep -F '"readback_performed": false' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'R2 readback proof must not claim readback without credentials'
  grep -F '"no_ctx_rs_cutover": true' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'R2 readback proof must record no ctx.rs cutover'

  upload_log="${CTX_ARTIFACT_DIR}/r2-upload-without-approval.log"
  set +e
  CTX_RELEASE_R2_UPLOAD_READBACK=1 \
  CTX_ARTIFACT_DIR="${CTX_ARTIFACT_DIR}/r2-upload-without-approval" \
    bash scripts/release-r2-staging-readback-proof.sh "${candidate_dir}" > "${upload_log}" 2>&1
  upload_status=$?
  set -e
  (( upload_status != 0 )) || fail 'R2 upload/readback mode ran without manager approval'
  grep -F 'CTX_RELEASE_R2_UPLOAD_READBACK=1 requires CTX_RELEASE_R2_MANAGER_APPROVED=1' "${upload_log}" >/dev/null \
    || fail 'R2 upload/readback mode did not explain missing manager approval'

  fake_wrangler_dir="${CTX_ARTIFACT_DIR}/fake-wrangler-bin"
  fake_wrangler_log="${CTX_ARTIFACT_DIR}/fake-wrangler.log"
  r2_dir="${CTX_ARTIFACT_DIR}/r2-staging-readback-plan-only"
  mkdir -p "${fake_wrangler_dir}" "${r2_dir}"
  cat > "${fake_wrangler_dir}/wrangler" <<'EOF'
#!/usr/bin/env bash
printf 'wrangler invoked: %s\n' "$*" >> "${CTX_FAKE_WRANGLER_LOG}"
exit 99
EOF
  chmod +x "${fake_wrangler_dir}/wrangler"
  (
    export PATH="${fake_wrangler_dir}:${PATH}"
    export CTX_FAKE_WRANGLER_LOG="${fake_wrangler_log}"
    export CTX_RELEASE_R2_UPLOAD_READBACK=1
    export CTX_RELEASE_R2_MANAGER_APPROVED=1
    CTX_RELEASE_R2_UPLOAD_READBACK=0 \
    CTX_RELEASE_R2_MANAGER_APPROVED=0 \
    CTX_ARTIFACT_DIR="${r2_dir}" \
      run_timed "r2-staging-readback-plan-only-env-guard" \
        bash scripts/release-r2-staging-readback-proof.sh "${candidate_dir}"
  )
  if [[ -s "${fake_wrangler_log}" ]]; then
    fail "plan-only R2 readback path invoked wrangler despite forced-off upload env: $(cat "${fake_wrangler_log}")"
  fi
  grep -F '"status": "blocked_manual_required"' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'plan-only R2 readback guard did not remain blocked without upload/readback'
  grep -F '"upload_performed": false' "${r2_dir}/r2-staging-readback.json" >/dev/null \
    || fail 'plan-only R2 readback guard claimed an upload'

  contract_dir="$(mktemp -d "${TMPDIR}/supply-chain-r2-certificate.XXXXXX")"
  evidence_root="${contract_dir}/evidence"
  certificate_dir="${contract_dir}/certificate-output"
  log="${contract_dir}/missing-supply-chain-r2.log"
  write_release_evidence_root "${evidence_root}" "real"
  rm -rf "${evidence_root}/artifacts/buildkite/supply-chain"
  rm -rf "${evidence_root}/artifacts/buildkite/r2-staging-readback"
  mkdir -p "${certificate_dir}"

  set +e
  bash scripts/release-completion-certificate.sh \
    --mode=release-evidence \
    --evidence-root "${evidence_root}" \
    --artifact-dir "${certificate_dir}" > "${log}" 2>&1
  status=$?
  set -e

  (( status != 0 )) || fail 'release artifact evidence accepted missing supply-chain evidence'
  grep -F 'required evidence is missing or empty: artifacts/buildkite/supply-chain/dependency-advisory-license-audit.json' "${log}" >/dev/null \
    || fail 'release artifact evidence did not require dependency advisory/license evidence'
  grep -F 'required evidence is missing or empty: artifacts/buildkite/supply-chain/sbom-provenance-signature.json' "${log}" >/dev/null \
    || fail 'release artifact evidence did not require SBOM/provenance/signature evidence'
  grep -F 'required evidence is missing or empty: artifacts/buildkite/r2-staging-readback/r2-staging-readback.json' "${log}" >/dev/null \
    || fail 'release artifact evidence did not require R2 staging readback evidence'
}

run_release_finished_product_evidence_contract() {
  local contract_root evidence_root release_root candidate_dir artifact_root
  local product_json provider_json rich_json rich_context package_json security_json security_md jj_json jj_txt installer_json installer_txt lanes_json

  contract_root="${CTX_ARTIFACT_DIR}/release-finished-product-evidence-contract"
  evidence_root="${contract_root}/evidence"
  artifact_root="${evidence_root}/artifacts/buildkite"
  release_root="${artifact_root}/release-dry-run"
  candidate_dir="${artifact_root}/release-candidate"
  rm -rf "${contract_root}"
  mkdir -p "${release_root}" "${candidate_dir}"

  write_release_evidence_platform "${release_root}" "linux-x64" "linux_x64" "x86_64-unknown-linux-gnu"
  write_release_evidence_platform "${release_root}" "macos-arm64" "macos_arm64" "aarch64-apple-darwin"
  write_release_evidence_platform "${release_root}" "macos-x64" "macos_x64" "x86_64-apple-darwin"
  write_release_evidence_platform "${release_root}" "windows-x64" "windows_x64" "x86_64-pc-windows-gnu"
  write_release_evidence_platform "${release_root}" "freebsd-x64" "freebsd_x64" "x86_64-unknown-freebsd"

  CTX_ARTIFACT_DIR="${candidate_dir}" run_timed "release-finished-product-candidate-metadata" \
    bash scripts/release-candidate-metadata.sh "${release_root}"
  CTX_ARTIFACT_DIR="${contract_root}/helper-timings" run_timed "release-finished-product-evidence-helper" \
    bash scripts/release-finished-product-evidence.sh "${artifact_root}"

  product_json="${artifact_root}/finished-product/product-decisions/product-decisions.json"
  provider_json="${artifact_root}/finished-product/provider-fixtures/provider-fixtures.json"
  rich_json="${artifact_root}/finished-product/rich-search-context/rich-search-context.json"
  rich_context="${artifact_root}/finished-product/rich-search-context/rich-context.json"
  package_json="${artifact_root}/finished-product/search-mvp-package-audit/search-mvp-package-audit.json"
  security_json="${artifact_root}/finished-product/security-archive-fixtures/security-archive-fixtures.json"
  security_md="${artifact_root}/finished-product/security-archive-fixtures/security-archive-fixtures.md"
  jj_json="${artifact_root}/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.json"
  jj_txt="${artifact_root}/finished-product/jj-e2e-blocker-status/jj-e2e-blocker-status.txt"
  installer_json="${artifact_root}/finished-product/installer-dry-run-smoke/installer-dry-run-smoke.json"
  installer_txt="${artifact_root}/finished-product/installer-dry-run-smoke/install-dry-run.txt"
  lanes_json="${artifact_root}/provider-live-e2e-lanes/provider-live-e2e-lanes.json"

  for required_file in \
    "${product_json}" \
    "${provider_json}" \
    "${rich_json}" \
    "${rich_context}" \
    "${package_json}" \
    "${security_json}" \
    "${security_md}" \
    "${jj_json}" \
    "${jj_txt}" \
    "${installer_json}" \
    "${installer_txt}" \
    "${lanes_json}"; do
    [[ -s "${required_file}" ]] || fail "release finished-product evidence is missing: ${required_file}"
  done

  for json in "${product_json}" "${provider_json}" "${rich_json}" "${package_json}" "${security_json}" "${jj_json}" "${installer_json}"; do
    grep -F '"status": "passed"' "${json}" >/dev/null \
      || fail "release finished-product evidence did not pass: ${json}"
    grep -F '"evidence_class": "release_artifact_evidence"' "${json}" >/dev/null \
      || fail "release finished-product evidence did not record release_artifact_evidence: ${json}"
    grep -F '"self_test_fixture": false' "${json}" >/dev/null \
      || fail "release finished-product evidence was marked as a self-test fixture: ${json}"
  done
  grep -F 'Publishing: false' "${security_md}" >/dev/null \
    || fail 'release finished-product evidence did not record non-publishing security archive status'
  grep -F 'ctx install plan' "${installer_txt}" >/dev/null \
    || fail 'release finished-product evidence did not record installer dry-run plan'
  grep -F '"kind": "provider_live_e2e_lane_definitions"' "${lanes_json}" >/dev/null \
    || fail 'release finished-product evidence did not write provider live lane definitions'
  grep -F '"default_enabled": false' "${lanes_json}" >/dev/null \
    || fail 'provider live lane definitions must remain opt-in by default'
}

run_completion_certificate_contract() {
  local certificate_dir explicit_mode_dir explicit_mode_fixture_dir explicit_mode_output_dir explicit_mode_log explicit_mode_status

  run_timed "completion-certificate-shell-syntax" bash -n scripts/release-completion-certificate.sh
  certificate_dir="${CTX_ARTIFACT_DIR}/completion-certificate"
  mkdir -p "${certificate_dir}"
  run_timed "completion-certificate-fixture-evidence" \
    bash scripts/release-completion-certificate.sh \
      --contract-self-test \
      --artifact-dir "${certificate_dir}"

  explicit_mode_dir="$(mktemp -d "${TMPDIR}/completion-explicit-mode.XXXXXX")"
  explicit_mode_fixture_dir="${explicit_mode_dir}/fixture-certificate"
  explicit_mode_output_dir="${explicit_mode_dir}/explicit-release-evidence"
  explicit_mode_log="${explicit_mode_dir}/explicit-release-evidence.log"
  mkdir -p "${explicit_mode_fixture_dir}" "${explicit_mode_output_dir}"
  bash scripts/release-completion-certificate.sh \
    --contract-self-test \
    --artifact-dir "${explicit_mode_fixture_dir}" >/dev/null

  set +e
  CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES=1 \
    bash scripts/release-completion-certificate.sh \
      --mode=release-evidence \
      --evidence-root "${explicit_mode_fixture_dir}/contract-evidence" \
      --artifact-dir "${explicit_mode_output_dir}" > "${explicit_mode_log}" 2>&1
  explicit_mode_status=$?
  set -e

  (( explicit_mode_status != 0 )) \
    || fail 'explicit --mode=release-evidence was downgraded to contract self-test mode by CTX_COMPLETION_CERTIFICATE_ALLOW_SELF_TEST_FIXTURES'
  grep -F 'is contract self-test evidence' "${explicit_mode_log}" >/dev/null \
    || fail 'explicit release-evidence mode did not reject contract-marked fixture evidence'
  if [[ -f "${explicit_mode_output_dir}/ctx-completion-certificate.json" ]] \
    && grep -F '"evidence_mode": "contract-self-test"' "${explicit_mode_output_dir}/ctx-completion-certificate.json" >/dev/null; then
    fail 'explicit release-evidence mode wrote a contract-self-test completion certificate'
  fi
}

run_release_artifact_evidence_missing_contract() {
  local contract_dir evidence_root artifact_dir log status

  contract_dir="$(mktemp -d "${TMPDIR}/release-evidence-missing.XXXXXX")"
  evidence_root="${contract_dir}/empty-evidence"
  artifact_dir="${contract_dir}/certificate-output"
  log="${contract_dir}/release-evidence.log"
  mkdir -p "${evidence_root}" "${artifact_dir}"

  set +e
  bash scripts/release-completion-certificate.sh \
    --mode=release-evidence \
    --evidence-root "${evidence_root}" \
    --artifact-dir "${artifact_dir}" > "${log}" 2>&1
  status=$?
  set -e

  (( status != 0 )) || fail 'release artifact evidence accepted an empty evidence root'
  grep -F 'required evidence is missing or empty: artifacts/buildkite/release-dry-run/linux-x64/manifest.json' "${log}" >/dev/null \
    || fail 'release artifact evidence did not reject missing linux-x64 artifact proof'
  grep -F 'required evidence is missing or empty: artifacts/buildkite/release-artifact-smoke/linux-x64/artifact-smoke.json' "${log}" >/dev/null \
    || fail 'release artifact evidence did not reject missing linux-x64 artifact smoke proof'
  grep -F 'required evidence is missing or empty: artifacts/buildkite/release-exceptions/freebsd-x64/freebsd-x64-exception.json' "${log}" >/dev/null \
    || fail 'release artifact evidence did not require FreeBSD proof or manager-approved exception'
  grep -F 'FreeBSD release exception records manager approval' "${log}" >/dev/null \
    || fail 'release artifact evidence did not validate FreeBSD manager approval'

  if grep -F 'completion certificate:' "${log}" >/dev/null; then
    fail 'release artifact evidence wrote a certificate despite missing required evidence'
  fi

  printf 'release artifact evidence missing-evidence rejection log: %s\n' "${log}"
}

run_release_artifact_evidence_freebsd_contract() {
  local contract_dir evidence_root certificate_dir certificate_json negative_root negative_dir negative_log status

  contract_dir="$(mktemp -d "${TMPDIR}/release-evidence-freebsd.XXXXXX")"
  evidence_root="${contract_dir}/evidence"
  certificate_dir="${contract_dir}/certificate-output"
  write_release_evidence_root "${evidence_root}" "real"
  mkdir -p "${certificate_dir}"

  run_timed "release-artifact-evidence-freebsd-accepted" \
    bash scripts/release-completion-certificate.sh \
      --mode=release-evidence \
      --evidence-root "${evidence_root}" \
      --artifact-dir "${certificate_dir}"

  certificate_json="${certificate_dir}/ctx-completion-certificate.json"
  grep -F '"status_in_this_certificate": "native_release_artifact_smoke_verified"' "${certificate_json}" >/dev/null \
    || fail 'completion certificate did not record native FreeBSD artifact smoke proof status'
  grep -F '"manager_exception_required_for_public_release_without_proof": false' "${certificate_json}" >/dev/null \
    || fail 'completion certificate still required a FreeBSD manager exception with native proof present'
  grep -F 'CTX_RELEASE_ARTIFACT_freebsd_x64=ctx-0.1.0-x86_64-unknown-freebsd' \
    "${evidence_root}/artifacts/buildkite/release-candidate/ctx-release-metadata.env" >/dev/null \
    || fail 'release candidate metadata did not include the FreeBSD artifact'
  grep -F '"validated_upload_object_count": 10' \
    "${evidence_root}/artifacts/buildkite/r2-staging-smoke/r2-staging-smoke.json" >/dev/null \
    || fail 'R2 staging smoke did not validate the five-platform object count'

  negative_root="${contract_dir}/freebsd-contract-fixture-evidence"
  negative_dir="${contract_dir}/freebsd-contract-fixture-certificate"
  negative_log="${contract_dir}/freebsd-contract-fixture-rejection.log"
  write_release_evidence_root "${negative_root}" "contract_fixture"
  mkdir -p "${negative_dir}"

  set +e
  bash scripts/release-completion-certificate.sh \
    --mode=release-evidence \
    --evidence-root "${negative_root}" \
    --artifact-dir "${negative_dir}" > "${negative_log}" 2>&1
  status=$?
  set -e

  (( status != 0 )) || fail 'release artifact evidence accepted contract-marked FreeBSD evidence'
  grep -F 'freebsd-x64 manifest: artifacts/buildkite/release-dry-run/freebsd-x64/manifest.json is contract self-test evidence' "${negative_log}" >/dev/null \
    || fail 'release artifact evidence did not reject contract-marked FreeBSD proof'
  if grep -F 'completion certificate:' "${negative_log}" >/dev/null; then
    fail 'release artifact evidence wrote a certificate despite contract-marked FreeBSD evidence'
  fi

  printf 'release artifact evidence FreeBSD rejection log: %s\n' "${negative_log}"
}

provider_live_selected_needs_ctx_bin() {
  [[ "${CTX_LIVE_PROVIDER_E2E:-0}" == "1" ]] || return 1

  if [[ "${CTX_LIVE_PROVIDER_OPENROUTER:-0}" == "1" && "${CTX_LIVE_PROVIDER_OPENROUTER_GENERATE:-0}" == "1" ]]; then
    return 0
  fi

  [[ "${CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY:-0}" == "1" ]] || return 1

  if [[ "${CTX_LIVE_PROVIDER_CODEX:-0}" == "1" && -n "${CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH:-}" ]]; then
    return 0
  fi
  if [[ "${CTX_LIVE_PROVIDER_PI:-0}" == "1" && -n "${CTX_LIVE_PROVIDER_PI_SESSIONS_PATH:-}" ]]; then
    return 0
  fi
  return 1
}

provider_live_single_needs_ctx_bin() {
  local provider="$1"

  [[ "${CTX_LIVE_PROVIDER_E2E:-0}" == "1" ]] || return 1

  case "${provider}" in
    openrouter)
      [[ "${CTX_LIVE_PROVIDER_OPENROUTER:-0}" == "1" && "${CTX_LIVE_PROVIDER_OPENROUTER_GENERATE:-0}" == "1" ]]
      ;;
    codex)
      [[ "${CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY:-0}" == "1" ]] || return 1
      [[ "${CTX_LIVE_PROVIDER_CODEX:-0}" == "1" && -n "${CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH:-}" ]]
      ;;
    pi)
      [[ "${CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY:-0}" == "1" ]] || return 1
      [[ "${CTX_LIVE_PROVIDER_PI:-0}" == "1" && -n "${CTX_LIVE_PROVIDER_PI_SESSIONS_PATH:-}" ]]
      ;;
    *)
      return 1
      ;;
  esac
}

with_ctx_bin_if_needed() {
  local needs_ctx="$1"
  local ctx_bin
  shift

  if [[ "${needs_ctx}" == "1" ]]; then
    build_ctx_debug
    ctx_bin="$(ctx_debug_bin)"
    [[ -x "${ctx_bin}" || -f "${ctx_bin}" ]] || fail "ctx debug binary missing: ${ctx_bin}"
    CTX_BIN="${ctx_bin}" "$@"
  else
    "$@"
  fi
}

run_provider_live_stderr_redaction_contract() {
  local contract_dir private_root fake_ctx log status secret_query secret_snippet artifact_dir
  local artifact_json artifact_provider_dir redacted_error_artifact candidate

  contract_dir="$(mktemp -d "${TMPDIR}/provider-live-redaction.XXXXXX")"
  private_root="${contract_dir}/private-local-history"
  fake_ctx="${contract_dir}/ctx"
  artifact_dir="${contract_dir}/artifacts"
  log="${contract_dir}/run.log"
  secret_query="private query should not be logged"
  secret_snippet="PRIVATE_SNIPPET_SHOULD_NOT_LEAK"
  mkdir -p "${private_root}" "${artifact_dir}"

  cat > "${fake_ctx}" <<EOF
#!/usr/bin/env bash
set -euo pipefail
case "\${1:-}" in
  setup)
    printf '{"ok":true}\n'
    ;;
  import)
    printf '{"totals":{"source_files":1,"source_bytes":1,"imported_sessions":1,"imported_events":1,"imported_edges":0,"skipped":0,"failed":0}}\n'
    ;;
  search)
    printf 'raw path %s query %s snippet %s data %s\n' "${private_root}" "\$*" "${secret_snippet}" "\${CTX_DATA_ROOT:-}" >&2
    exit 23
    ;;
  context)
    printf '{"results":[{"id":"redacted"}]}\n'
    ;;
  status)
    printf '{"indexed_items":1,"indexed_sources":1}\n'
    ;;
  doctor)
    printf '{"ok":true}\n'
    ;;
  validate)
    printf '{"valid":true}\n'
    ;;
  *)
    printf 'unexpected ctx command %s\n' "\${1:-}" >&2
    exit 2
    ;;
esac
EOF
  chmod +x "${fake_ctx}"

  set +e
  CTX_ARTIFACT_DIR="${artifact_dir}" \
  CTX_LIVE_PROVIDER_E2E=1 \
  CTX_LIVE_PROVIDER_ACCEPT_LOCAL_HISTORY=1 \
  CTX_LIVE_PROVIDER_CODEX=1 \
  CTX_LIVE_PROVIDER_CODEX_SESSIONS_PATH="${private_root}" \
  CTX_LIVE_PROVIDER_CODEX_QUERY="${secret_query}" \
  CTX_LIVE_PROVIDER_CTX_BIN="${fake_ctx}" \
    bash scripts/release-provider-live-e2e-lanes.sh run codex > "${log}" 2>&1
  status=$?
  set -e

  (( status != 0 )) || fail 'provider live redaction contract expected fake ctx failure'

  artifact_json=""
  for candidate in "${artifact_dir}/codex/live-e2e.json" "${artifact_dir}/live-e2e.json"; do
    if [[ -f "${candidate}" ]]; then
      artifact_json="${candidate}"
      break
    fi
  done
  [[ -n "${artifact_json}" ]] \
    || fail 'provider live redaction contract did not write a Codex failed artifact'
  artifact_provider_dir="$(dirname "${artifact_json}")"
  redacted_error_artifact="${artifact_provider_dir}/live-e2e-error.txt"

  grep -F '"provider": "codex"' "${artifact_json}" >/dev/null \
    || fail "provider live redaction contract wrote unexpected artifact: ${artifact_json}"
  grep -F '"status": "failed"' "${artifact_json}" >/dev/null \
    || fail 'provider live redaction contract did not write failed artifact'
  grep -F '"redacted_error_artifact": "live-e2e-error.txt"' "${artifact_json}" >/dev/null \
    || fail 'provider live redaction contract did not reference redacted error artifact'
  grep -F 'stderr_content: suppressed' "${redacted_error_artifact}" >/dev/null \
    || fail 'provider live redaction contract did not suppress raw stderr content'

  for secret in "${private_root}" "${secret_query}" "${secret_snippet}"; do
    if grep -R -F -- "${secret}" "${artifact_dir}" "${log}" >/dev/null 2>&1; then
      fail 'provider live redaction contract leaked a raw local path, query, or snippet'
    fi
  done
}

run_manual_external_contract() {
  CTX_LIVE_PROVIDER_E2E=1 \
  CTX_LIVE_PROVIDER_CLAUDE_CODE=1 \
  CTX_LIVE_PROVIDER_E2E_ACCEPT_BLOCKERS=1 \
  run_timed "manual-external-provider-blocker-contract" \
    bash scripts/release-provider-live-e2e-lanes.sh run-selected
  grep -F '"providers_blocked": 1' "${CTX_ARTIFACT_DIR}/live-e2e.json" >/dev/null \
    || fail 'manual external contract did not exercise a fixture-only blocker'
  grep -F '"provider": "claude_code"' "${CTX_ARTIFACT_DIR}/claude_code/live-e2e.json" >/dev/null \
    || fail 'manual external contract did not write Claude fixture-only blocker artifact'

  run_timed "manual-external-provider-stderr-redaction-contract" \
    run_provider_live_stderr_redaction_contract
}

run_provider_live_e2e() {
  local provider="$1"
  local needs_ctx=0

  if provider_live_single_needs_ctx_bin "${provider}"; then
    needs_ctx=1
  fi
  run_timed "provider-live-e2e-${provider}" \
    with_ctx_bin_if_needed "${needs_ctx}" \
    bash scripts/release-provider-live-e2e-lanes.sh run "${provider}"
}

run_provider_live_e2e_selected() {
  local needs_ctx=0

  if provider_live_selected_needs_ctx_bin; then
    needs_ctx=1
  fi
  run_timed "provider-live-e2e-selected" \
    with_ctx_bin_if_needed "${needs_ctx}" \
    bash scripts/release-provider-live-e2e-lanes.sh run-selected
}

init_bazel_gate_env
trap finish_timing EXIT

case "${mode}" in
  cargo_fmt_check)
    run_timed "cargo-fmt-check" cargo fmt --all -- --check
    ;;
  cargo_check)
    run_timed "cargo-check" cargo check --workspace --all-targets "${cargo_locked_args[@]}"
    ;;
  cargo_clippy)
    run_timed "cargo-clippy" cargo clippy --workspace --all-targets "${cargo_locked_args[@]}"
    ;;
  cargo_test_default)
    run_timed "cargo-test-default" cargo test --workspace --all-targets "${cargo_locked_args[@]}" -- --test-threads "${RUST_TEST_THREADS}"
    ;;
  cargo_test_all_features)
    run_timed "cargo-test-all-features" cargo test --workspace --all-targets --all-features "${cargo_locked_args[@]}" -- --test-threads "${RUST_TEST_THREADS}"
    ;;
  cli_contract_tests)
    run_cli_contract_tests
    ;;
  docs_check)
    run_timed "docs-check" bash scripts/check-docs.sh
    ;;
  buildkite_pipeline_check)
    run_timed "buildkite-pipeline-check" bash scripts/check-buildkite-pipeline.sh
    ;;
  source_diff_check)
    run_timed "git-diff-check" git diff --check
    ;;
  package_audit_fast)
    CTX_AUDIT_SKIP_RELEASE_BUILD=1 run_timed "package-audit-fast" bash scripts/audit-search-mvp-package.sh
    ;;
  package_audit_release)
    CARGO_TARGET_DIR="${CTX_REPO_ROOT}/target" \
    CTX_AUDIT_SKIP_RELEASE_BUILD=0 \
    run_timed "package-audit-release" bash scripts/audit-search-mvp-package.sh
    ;;
  fresh_home_e2e)
    run_fresh_home_flow
    ;;
  provider_fixture_e2e)
    cargo_test_filter work-record-capture provider_fixture_replay
    cargo_test_filter ctx normalized_provider_cli_flow_covers_all_harness_providers_with_multiple_sessions
    cargo_test_filter ctx normalized_provider_cli_requires_explicit_path_for_non_discovered_providers
    cargo_test_filter ctx normalized_provider_cli_rejects_provider_mismatches
    ;;
  security_static_audit)
    run_security_static_audit
    ;;
  security_no_repo_writes)
    run_security_no_repo_writes
    ;;
  privacy_redaction_oracle)
    ctx_package="ctx"
    cargo_test_filter "${ctx_package}" privacy_redaction_oracle_covers_cli_json_and_sqlite
    cargo_test_filter work-record-core redaction
    cargo_test_filter work-record-search redacts_secret_like_values_in_snippets
    cargo_test_filter work-record-capture provider_fixture_replay_supports_pi_and_redacts_metadata
    ;;
  search_determinism_tests)
    cargo_test_filter work-record-search context_packet_budget_is_deterministic_for_large_history
    cargo_test_filter work-record-store search_records
    ;;
  synthetic_search_smoke)
    cargo_test_filter work-record-search rich_search_matches_typed_context_with_citations_and_redaction
    cargo_test_filter ctx fresh_home_search_mvp_flow
    ;;
  search_perf_bench)
    cargo_test_filter_ignored work-record-search synthetic_search_perf_bench_records_thresholded_evidence
    run_timed "search-perf-bench-artifact" test -s "${CTX_ARTIFACT_DIR}/synthetic-search-perf.json"
    ;;
  release_dry_run_host)
    CARGO_TARGET_DIR="${CTX_REPO_ROOT}/target" \
    run_timed "release-dry-run-host" bash scripts/release-dry-run.sh
    ;;
  release_platform_blocker_freebsd)
    run_timed "release-platform-blocker-freebsd" bash scripts/release-platform-blocker.sh freebsd-x64
    ;;
  provider_live_e2e_lane_definitions)
    run_timed "provider-live-e2e-lane-definitions" bash scripts/release-provider-live-e2e-lanes.sh definitions
    ;;
  provider_live_e2e_codex)
    run_provider_live_e2e codex
    ;;
  provider_live_e2e_pi)
    run_provider_live_e2e pi
    ;;
  provider_live_e2e_openrouter)
    run_provider_live_e2e openrouter
    ;;
  provider_live_e2e_selected)
    run_provider_live_e2e_selected
    ;;
  release_candidate_metadata_contract)
    run_release_candidate_metadata_contract
    ;;
  r2_staging_smoke_contract)
    run_r2_staging_smoke_contract
    ;;
  release_artifact_smoke_contract)
    run_timed "release-artifact-smoke-contract" \
      run_release_artifact_smoke_contract
    ;;
  release_supply_chain_r2_contract)
    run_release_supply_chain_r2_contract
    ;;
  release_finished_product_evidence_contract)
    run_release_finished_product_evidence_contract
    ;;
  completion_certificate_contract)
    run_completion_certificate_contract
    ;;
  release_artifact_evidence_missing_contract)
    run_timed "release-artifact-evidence-missing-contract" \
      run_release_artifact_evidence_missing_contract
    ;;
  release_artifact_evidence_freebsd_contract)
    run_timed "release-artifact-evidence-freebsd-contract" \
      run_release_artifact_evidence_freebsd_contract
    ;;
  manual_external_contract)
    run_manual_external_contract
    ;;
  *)
    fail "unknown Bazel gate mode: ${mode}"
    ;;
esac
