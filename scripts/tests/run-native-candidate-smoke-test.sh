#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
smoke="${repo_root}/scripts/run-native-candidate-smoke.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-native-smoke-test.XXXXXX")"

cleanup_survivor_fixture() {
  local survivor_pids
  [[ -n "${survivor_copy:-}" ]] || return 0
  survivor_pids="$(process_ids_for_command_path "${survivor_copy}")"
  [[ -n "${survivor_pids}" ]] || return 0
  kill -TERM ${survivor_pids} 2>/dev/null || true
  sleep 1
  survivor_pids="$(process_ids_for_command_path "${survivor_copy}")"
  [[ -z "${survivor_pids}" ]] || kill -KILL ${survivor_pids} 2>/dev/null || true
}

cleanup_test() {
  local test_status=$?
  trap - EXIT
  cleanup_survivor_fixture || true
  rm -rf "${tmp}"
  exit "${test_status}"
}
trap cleanup_test EXIT

fake_template="${tmp}/ctx.template"
make_fake() {
  local destination="$1"
  cp "${fake_template}" "${destination}"
  chmod +x "${destination}"
}

file_mode() {
  if mode="$(stat -c '%a' "$1" 2>/dev/null)"; then
    printf '%s\n' "${mode}"
  else
    stat -f '%Lp' "$1"
  fi
}

file_size() {
  if size="$(stat -c '%s' "$1" 2>/dev/null)"; then
    printf '%s\n' "${size}"
  else
    stat -f '%z' "$1"
  fi
}

file_hash() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    sha256 -q "$1"
  fi
}

snapshot_tree() {
  local tree_root="$1"
  (
    cd "${tree_root}"
    while IFS= read -r entry; do
      if [[ -d "${entry}" ]]; then
        printf '%s\tdirectory\t%s\t%s\t-\n' \
          "${entry}" "$(file_mode "${entry}")" "$(file_size "${entry}")"
      elif [[ -f "${entry}" ]]; then
        printf '%s\tfile\t%s\t%s\t%s\n' \
          "${entry}" "$(file_mode "${entry}")" "$(file_size "${entry}")" \
          "$(file_hash "${entry}")"
      else
        printf '%s\tother\t%s\t%s\t-\n' \
          "${entry}" "$(file_mode "${entry}")" "$(file_size "${entry}")"
      fi
    done < <(find . -mindepth 1 -print | LC_ALL=C sort)
  )
}

process_ids_for_command_path() {
  ps -axo pid=,command= 2>/dev/null \
    | awk -v executable="$1" '$2 == executable || $3 == executable { print $1 }' \
    | LC_ALL=C sort -n
}

cat > "${fake_template}" <<'EOF'
#!/bin/sh
set -eu

test "${CTX_DAEMON_AUTOSTART_OFF:-}" = 1
test -n "${CTX_DATA_ROOT:-}"
test -n "${HOME:-}"
test -n "${XDG_CONFIG_HOME:-}"
test -n "${XDG_CACHE_HOME:-}"
test "${HOME}" != "${ORIGINAL_HOME:-not-in-clean-env}"
if data_root_mode="$(stat -c '%a' "${CTX_DATA_ROOT}" 2>/dev/null)"; then
  :
else
  data_root_mode="$(stat -f '%Lp' "${CTX_DATA_ROOT}")"
fi
test "${data_root_mode}" = 700

case "${0##*/}" in
  *ctx-hang*)
    sleep 30
    ;;
  *lifecycle*)
    if test "${1:-}" = status && test "${CTX_ANALYTICS_ENABLED+x}" != x; then
      candidate_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
      : > "${candidate_dir}/.ctx.install.lock"
      : > "${candidate_dir}/.ctx.daemon-quiescence.lock"
      mkdir -p "${candidate_dir}/.ctx.daemon-quiescence-acks"
      printf '%s\n' "${CTX_DATA_ROOT}" > "${candidate_dir}/.ctx.data-root"
      sleep 3
    fi
    ;;
  *ctx-survivor*)
    if test "${1:-}" = --version; then
      "$0" --survivor-child &
      sleep 3
    fi
    ;;
esac

case " $* " in
  *" --backend semantic "*)
    test "${CTX_ANALYTICS_ENABLED:-}" = false
    test "${CTX_UPGRADE_AUTO:-}" = off
    test "${CTX_SEARCH_SEMANTIC:-}" = 1
    test "${CTX_DAEMON_ENABLED:-}" = 1
    printf '%s\n' 'semantic-only search will not initialize or download intfloat/multilingual-e5-small during search' >&2
    exit 1
    ;;
  *" status --format json "*)
    test -z "${CTX_SEARCH_SEMANTIC:-}"
    if test "${CTX_ANALYTICS_ENABLED+x}" != x; then
      test -z "${CTX_UPGRADE_AUTO:-}"
      test -z "${CTX_DAEMON_ENABLED:-}"
    else
      test "${CTX_ANALYTICS_ENABLED:-}" = false
      test "${CTX_UPGRADE_AUTO:-}" = off
      test "${CTX_DAEMON_ENABLED:-}" = false
    fi
    ;;
  *)
    test "${CTX_ANALYTICS_ENABLED:-}" = false
    test "${CTX_UPGRADE_AUTO:-}" = off
    test "${CTX_DAEMON_ENABLED:-}" = false
    test "${CTX_SEARCH_SEMANTIC:-}" = 0
    ;;
esac

case "${1:-}" in
  --version)
    version=0.25.0
    case "${0##*/}" in
      *bad-version*|*ctx-survivor*) version=9.9.9 ;;
      *ctx-v1*) version=1.0.0 ;;
    esac
    printf 'ctx %s\n' "${version}"
    ;;
  setup)
    ;;
  import)
    case "${0##*/}" in
      *ctx-v1*)
        generation_directory=generation-11111111111111111111111111111111
        mkdir -p \
          "${CTX_DATA_ROOT}/search/lexical/ctx-generations" \
          "${CTX_DATA_ROOT}/search/lexical/index-generations/${generation_directory}"
        printf '%s' \
          '{"version":1,"active":{"generation_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","directory":"generation-11111111111111111111111111111111"},"previous":null}' \
          > "${CTX_DATA_ROOT}/search/lexical/active-generation.json"
        : > "${CTX_DATA_ROOT}/search/lexical/index-generations/${generation_directory}/meta.json"
        : > "${CTX_DATA_ROOT}/search/lexical/ctx-generations/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"
        printf '%s\n' '{"totals":{"current_source_count":1,"current_indexed_documents":2},"sources":[{"published_generation":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}]}'
        ;;
      *)
        printf '%s\n' '{"totals":{"imported_events":2}}'
        ;;
    esac
    ;;
  search)
    printf '%s\n' '{"retrieval":{"requested_mode":"lexical","effective_mode":"lexical"},"results":[{"text":"Add a parser test."}]}'
    ;;
  status)
    if test "${CTX_ANALYTICS_ENABLED+x}" != x; then
      analytics_path="${CTX_ANALYTICS_ENDPOINT#file://}"
      printf '%s\n' '{"events":[{"event_name":"operation_completed"}]}' > "${analytics_path}"
      cat <<'JSON'
{
  "read_only": true,
  "daemon": {
    "jobs": {
      "history_refresh": {
        "enabled": false
      }
    },
    "enabled": true
  },
  "upgrade": {
    "auto": "apply",
    "auto_enabled": true
  },
  "semantic": {
    "config_source": "default",
    "enabled": false,
    "reason": "semantic_disabled",
    "embed_policy": {
      "source": "dynamic_quiet"
    }
  }
}
JSON
    else
      cat <<'JSON'
{
  "read_only": true,
  "daemon": {
    "jobs": {
      "history_refresh": {
        "enabled": true
      }
    },
    "enabled": false
  },
  "upgrade": {
    "auto": "off",
    "auto_enabled": false
  },
  "semantic": {
    "config_source": "default",
    "enabled": false,
    "reason": "semantic_disabled",
    "embed_policy": {
      "source": "dynamic_quiet"
    }
  }
}
JSON
    fi
    ;;
  --survivor-child)
    sleep 30
    ;;
  *)
    printf 'unexpected fake ctx arguments: %s\n' "$*" >&2
    exit 1
    ;;
esac
EOF
printf '%s\n' '{"record_type":"manifest","schema_version":"ctx-history-jsonl-v2"}' > "${tmp}/fixture.jsonl"

assert_passed_result() {
  local result_path="$1"
  grep -Fq '"schema_version":1' "${result_path}"
  grep -Fq '"kind":"ctx-native-candidate-smoke"' "${result_path}"
  grep -Fq '"status":"passed"' "${result_path}"
  for step in \
    version setup import search read_only released_defaults explicit_opt_outs \
    semantic_offline_fail_closed; do
    grep -Fq "\"${step}\":\"passed\"" "${result_path}"
  done
}

fake="${tmp}/ctx"
make_fake "${fake}"
result="${tmp}/result.json"
"${smoke}" "${fake}" "${tmp}/fixture.jsonl" 0.25.0 "${result}" >/dev/null
assert_passed_result "${result}"

ctx_v1_parent="${tmp}/ctx-v1-parent"
mkdir -p "${ctx_v1_parent}"
ordinary_fake="${ctx_v1_parent}/ctx"
make_fake "${ordinary_fake}"
ordinary_result="${tmp}/result-ordinary-under-ctx-v1-parent.json"
"${smoke}" "${ordinary_fake}" "${tmp}/fixture.jsonl" 0.25.0 "${ordinary_result}" >/dev/null
assert_passed_result "${ordinary_result}" || {
  printf 'candidate smoke fake matched an ancestor path instead of its basename\n' >&2
  cat "${ordinary_result}" >&2
  exit 1
}

v1_fake="${tmp}/ctx-v1"
make_fake "${v1_fake}"
v1_result="${tmp}/result-v1.json"
"${smoke}" "${v1_fake}" "${tmp}/fixture.jsonl" 1.0.0 "${v1_result}" >/dev/null
assert_passed_result "${v1_result}" || {
  printf 'candidate smoke result schema changed for the fresh epoch\n' >&2
  cat "${v1_result}" >&2
  exit 1
}

lifecycle_parent="${tmp}/lifecycle-candidate"
lifecycle_tmpdir_real="${tmp}/lifecycle-smoke-tmp-real"
lifecycle_tmpdir="${tmp}/lifecycle-smoke-tmp"
mkdir -p "${lifecycle_parent}" "${lifecycle_tmpdir_real}"
ln -s "${lifecycle_tmpdir_real}" "${lifecycle_tmpdir}"
lifecycle_fake="${lifecycle_parent}/ctx-lifecycle"
make_fake "${lifecycle_fake}"
mkdir -p "${lifecycle_parent}/sealed-release-metadata"
printf '%s\n' 'sealed release metadata' > "${lifecycle_parent}/sealed-release-metadata/manifest.txt"
chmod 0750 "${lifecycle_parent}/sealed-release-metadata"
chmod 0640 "${lifecycle_parent}/sealed-release-metadata/manifest.txt"
lifecycle_snapshot_before="${tmp}/lifecycle-before.snapshot"
lifecycle_snapshot_during="${tmp}/lifecycle-during.snapshot"
lifecycle_snapshot_after="${tmp}/lifecycle-after.snapshot"
snapshot_tree "${lifecycle_parent}" > "${lifecycle_snapshot_before}"
lifecycle_result="${tmp}/result-lifecycle.json"
TMPDIR="${lifecycle_tmpdir}" "${smoke}" \
  "${lifecycle_fake}" "${tmp}/fixture.jsonl" 0.25.0 "${lifecycle_result}" \
  >"${tmp}/lifecycle.out" 2>"${tmp}/lifecycle.err" &
lifecycle_smoke_pid=$!
lifecycle_copy=""
for _ in {1..15}; do
  for copy in "${lifecycle_tmpdir}"/ctx-native-candidate-smoke.*/candidate/ctx-lifecycle; do
    if [[ -f "${copy}" \
      && -e "$(dirname "${copy}")/.ctx.install.lock" \
      && -e "$(dirname "${copy}")/.ctx.daemon-quiescence.lock" \
      && -d "$(dirname "${copy}")/.ctx.daemon-quiescence-acks" ]]; then
      lifecycle_copy="${copy}"
      break 2
    fi
  done
  sleep 1
done
[[ -n "${lifecycle_copy}" ]] || {
  wait "${lifecycle_smoke_pid}" || true
  printf 'candidate smoke did not create lifecycle artifacts beside its private copy\n' >&2
  cat "${tmp}/lifecycle.err" >&2
  exit 1
}
[[ "${lifecycle_copy##*/}" == "${lifecycle_fake##*/}" ]]
[[ ! -L "${lifecycle_copy}" ]]
cmp -s "${lifecycle_fake}" "${lifecycle_copy}"
physical_lifecycle_root="$(
  CDPATH= cd -- "$(dirname "$(dirname "${lifecycle_copy}")")" && pwd -P
)"
[[ "$(cat "$(dirname "${lifecycle_copy}")/.ctx.data-root")" \
  == "${physical_lifecycle_root}/data" ]] || {
  printf 'candidate smoke exported a data root through a symlinked TMPDIR\n' >&2
  exit 1
}
snapshot_tree "${lifecycle_parent}" > "${lifecycle_snapshot_during}"
cmp -s "${lifecycle_snapshot_before}" "${lifecycle_snapshot_during}"
lifecycle_root="$(dirname "$(dirname "${lifecycle_copy}")")"
wait "${lifecycle_smoke_pid}" || {
  cat "${tmp}/lifecycle.err" >&2
  exit 1
}
assert_passed_result "${lifecycle_result}"
snapshot_tree "${lifecycle_parent}" > "${lifecycle_snapshot_after}"
cmp -s "${lifecycle_snapshot_before}" "${lifecycle_snapshot_after}"
[[ ! -e "${lifecycle_root}" ]] || {
  printf 'candidate smoke did not clean its private lifecycle artifacts\n' >&2
  exit 1
}

failed_result="${tmp}/failed-result.json"
make_fake "${tmp}/ctx-bad-version"
if "${smoke}" \
  "${tmp}/ctx-bad-version" "${tmp}/fixture.jsonl" 0.25.0 "${failed_result}" \
  >"${tmp}/failure.out" 2>"${tmp}/failure.err"; then
  printf 'candidate smoke accepted a mismatched version\n' >&2
  exit 1
fi
[[ ! -e "${failed_result}" ]] || {
  printf 'candidate smoke wrote passing evidence after failure\n' >&2
  exit 1
}
grep -Fq 'candidate version mismatch' "${tmp}/failure.err"

hung_result="${tmp}/hung-result.json"
make_fake "${tmp}/ctx-hang"
started="$(date +%s)"
if CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS=1 "${smoke}" \
  "${tmp}/ctx-hang" "${tmp}/fixture.jsonl" 0.25.0 "${hung_result}" \
  >"${tmp}/hung.out" 2>"${tmp}/hung.err"; then
  printf 'candidate smoke accepted a hung command\n' >&2
  exit 1
fi
elapsed="$(( $(date +%s) - started ))"
[[ "${elapsed}" -lt 10 ]] || {
  printf 'candidate smoke timeout was not bounded: %ss\n' "${elapsed}" >&2
  exit 1
}
[[ ! -e "${hung_result}" ]]
grep -Fq 'candidate command exceeded 1 seconds' "${tmp}/hung.err"

survivor_tmpdir="${tmp}/survivor-smoke-tmp"
mkdir -p "${survivor_tmpdir}"
survivor_fake="${tmp}/ctx-survivor"
make_fake "${survivor_fake}"
survivor_result="${tmp}/survivor-result.json"
TMPDIR="${survivor_tmpdir}" "${smoke}" \
  "${survivor_fake}" "${tmp}/fixture.jsonl" 0.25.0 "${survivor_result}" \
  >"${tmp}/survivor.out" 2>"${tmp}/survivor.err" &
survivor_smoke_pid=$!
survivor_copy=""
survivor_processes=""
for _ in {1..15}; do
  for copy in "${survivor_tmpdir}"/ctx-native-candidate-smoke.*/candidate/ctx-survivor; do
    if [[ -f "${copy}" ]]; then
      process_ids="$(process_ids_for_command_path "${copy}")"
      if [[ -n "${process_ids}" ]]; then
        survivor_copy="${copy}"
        survivor_processes="${process_ids}"
        break 2
      fi
    fi
  done
  sleep 1
done
[[ -n "${survivor_copy}" && -n "${survivor_processes}" ]] || {
  wait "${survivor_smoke_pid}" || true
  printf 'candidate smoke did not start a copied-candidate survivor\n' >&2
  cat "${tmp}/survivor.err" >&2
  exit 1
}
survivor_root="$(dirname "$(dirname "${survivor_copy}")")"
if wait "${survivor_smoke_pid}"; then
  printf 'candidate smoke accepted a copied-candidate survivor failure fixture\n' >&2
  exit 1
fi
grep -Fq 'candidate version mismatch' "${tmp}/survivor.err"
survivor_remaining="$(process_ids_for_command_path "${survivor_copy}")"
if [[ -n "${survivor_remaining}" ]]; then
  cleanup_survivor_fixture
  printf 'candidate smoke cleanup left copied-candidate survivors running: %s\n' \
    "${survivor_remaining}" >&2
  exit 1
fi
[[ ! -e "${survivor_root}" ]] || {
  printf 'candidate smoke cleanup did not remove its private root after reaping the survivor\n' >&2
  exit 1
}
[[ ! -e "${survivor_result}" ]]

printf 'native candidate smoke tests passed\n'
