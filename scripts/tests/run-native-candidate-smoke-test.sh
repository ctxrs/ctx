#!/usr/bin/env bash
set -euo pipefail
umask 077

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
smoke="${repo_root}/scripts/run-native-candidate-smoke.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-native-smoke-test.XXXXXX")"
chmod 0700 "${tmp}"
chmod u-s,g-s,o-t "${tmp}"

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
  python3 -I - "${fake_template}" \
    "${repo_root}/scripts/tests/native-candidate-outbox.json" \
    "${destination}" "${2:-current}" <<'PY'
import json
from pathlib import Path
import sys

outbox = json.loads(Path(sys.argv[2]).read_text())
payload = json.loads(outbox["entries"][0]["payload"])
case = sys.argv[4]
if case == "legacy":
    outbox["schema_version"] = outbox["entries"][0]["schema_version"] = 2
elif case == "malformed-entries":
    outbox["entries"] = {}
elif case == "event-mismatch":
    payload["events"][0]["event_id"] = "33333333-3333-4333-8333-333333333333"
elif case == "duplicate-delivery":
    payload["events"] *= 2
elif case == "non-v4":
    payload["events"][0]["event_id"] = "11111111-1111-1111-8111-111111111111"
    outbox["entries"][0]["payload"] = json.dumps(payload)
elif case == "wrong-operation":
    payload["events"][0]["operation"] = "search"
    outbox["entries"][0]["payload"] = json.dumps(payload)
elif case != "current":
    raise SystemExit("unknown analytics fixture case")
source = Path(sys.argv[1]).read_text()
source = source.replace("@ANALYTICS_OUTBOX@", json.dumps(outbox))
source = source.replace("@ANALYTICS_PAYLOAD@", json.dumps(payload))
Path(sys.argv[3]).write_text(source)
PY
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
  ps -axo pid=,command= > "${tmp}/processes.snapshot" 2>/dev/null
  awk -v executable="$1" '
    {
      pid = $1
      sub(/^[[:space:]]*[0-9]+[[:space:]]+/, "", $0)
      start = index($0, executable)
      after = start + length(executable)
      if (start > 0 \
          && (start == 1 || substr($0, start - 1, 1) == " ") \
          && (after > length($0) || substr($0, after, 1) == " ")) {
        print pid
      }
    }
  ' "${tmp}/processes.snapshot" | LC_ALL=C sort -n
}

assert_no_candidate_processes_in_tmpdir() {
  local candidate_root_fragment="$1"
  local remaining
  ps -axo pid=,command= > "${tmp}/processes.snapshot" 2>/dev/null
  remaining="$(
    awk -v fragment="${candidate_root_fragment}" \
      'index($0, fragment) { print $1 }' "${tmp}/processes.snapshot" \
      | LC_ALL=C sort -n
  )"
  if [[ -n "${remaining}" ]]; then
    kill -KILL ${remaining} 2>/dev/null || true
    printf 'candidate smoke left space-path processes running: %s\n' \
      "${remaining}" >&2
    exit 1
  fi
}

cat > "${fake_template}" <<'EOF'
#!/bin/sh
set -eu

if test "${1:-}" = --ctx-core-managed-pair-apply-v1; then
  test "$#" = 7
  test "$3" = -
  install_root="$2"
  mkdir -p "${install_root}/bin" "${install_root}/libexec" "${install_root}/share/ctx"
  cp "$5" "${install_root}/bin/ctx"
  chmod 0700 "${install_root}/bin/ctx"
  cp "$6" "${install_root}/libexec/ctx-pro"
  chmod 0700 "${install_root}/libexec/ctx-pro"
  cp "$4" "${install_root}/share/ctx/managed-pair-envelope.json"
  cp "$7" "${install_root}/bin/ctx.install.json"
  printf '%s\n' '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}'
  case "${0##*/}" in
    *extra-receipt*) printf '%s\n' 'unexpected output' ;;
  esac
  exit 0
fi

test "${CTX_DAEMON_AUTOSTART_OFF:-}" = 1
test -n "${CTX_DATA_ROOT:-}"
test -n "${HOME:-}"
test -n "${XDG_CONFIG_HOME:-}"
test -n "${XDG_CACHE_HOME:-}"
test "${HOME}" != "${ORIGINAL_HOME:-not-in-clean-env}"
for private_dir in "${CTX_DATA_ROOT%/*}" "${CTX_DATA_ROOT}" "${HOME}" \
  "${XDG_CONFIG_HOME}" "${XDG_CACHE_HOME}" "${XDG_STATE_HOME}" "${TMPDIR}" "${PWD}"; do
  if private_mode="$(stat -c '%a' "${private_dir}" 2>/dev/null)"; then
    :
  else
    private_mode="$(stat -f '%Lp' "${private_dir}")"
  fi
  if test "${private_mode}" != 700; then
    printf 'private candidate directory is mode %s, not 700: %s\n' \
      "${private_mode}" "${private_dir}" >&2
    exit 1
  fi
done

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
  *" daemon run "*)
    test "${CTX_ANALYTICS_ENABLED+x}" != x
    test "${CTX_UPGRADE_AUTO:-}" = off
    test "${CTX_DAEMON_ENABLED:-}" = true
    test "${CTX_DAEMON_MODE:-}" = source-refresh-only
    test "${CTX_SEARCH_SEMANTIC:-}" = 0
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
  pro)
    test "${2:-}" = --help
    ;;
  status)
    if test "${CTX_ANALYTICS_ENABLED+x}" != x; then
      analytics_path="${CTX_ANALYTICS_ENDPOINT#file://}"
      analytics_payload='@ANALYTICS_PAYLOAD@'
      case "${0##*/}" in
        *foreground-analytics*)
          printf '%s\n' "${analytics_payload}" > "${analytics_path}"
          ;;
        *)
          analytics_outbox="${XDG_STATE_HOME}/ctx/analytics-outbox-v1.json"
          mkdir -p "${analytics_outbox%/*}"
          chmod 0700 "${analytics_outbox%/*}"
          printf '%s\n' '@ANALYTICS_OUTBOX@' \
            > "${analytics_outbox}"
          chmod 0600 "${analytics_outbox}"
          ;;
      esac
      upgrade_auto=off
      upgrade_enabled=false
      if test -f "$0.install.json"; then
        upgrade_auto=apply
        upgrade_enabled=true
      fi
      cat <<JSON
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
    "auto": "${upgrade_auto}",
    "auto_enabled": ${upgrade_enabled}
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
  daemon)
    test "${2:-}" = run
    analytics_outbox="${XDG_STATE_HOME}/ctx/analytics-outbox-v1.json"
    test -s "${analytics_outbox}"
    case "${0##*/}" in
      *no-analytics-delivery*)
        trap '' 1 2 15
        while :; do :; done
        ;;
      *)
        trap 'exit 0' 1 2 15
        analytics_path="${CTX_ANALYTICS_ENDPOINT#file://}"
        printf '%s\n' '@ANALYTICS_PAYLOAD@' \
          > "${analytics_path}"
        ;;
    esac
    while :; do sleep 1; done
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

for analytics_case in legacy malformed-entries event-mismatch duplicate-delivery non-v4 wrong-operation; do
  negative_fake="${tmp}/ctx-${analytics_case}"
  make_fake "${negative_fake}" "${analytics_case}"
  negative_result="${tmp}/${analytics_case}-result.json"
  if "${smoke}" "${negative_fake}" "${tmp}/fixture.jsonl" 0.25.0 \
    "${negative_result}" >"${tmp}/${analytics_case}.out" 2>"${tmp}/${analytics_case}.err"; then
    printf 'candidate smoke accepted invalid analytics: %s\n' "${analytics_case}" >&2
    exit 1
  fi
  grep -Fq 'candidate did not preserve exact status analytics across daemon delivery' \
    "${tmp}/${analytics_case}.err"
  test ! -e "${negative_result}"
done

space_tmp="${tmp}/setgid task parent"
mkdir -p "${space_tmp}"
chmod 2700 "${space_tmp}"
space_result="${tmp}/space-result.json"
TMPDIR="${space_tmp}" "${smoke}" \
  "${fake}" "${tmp}/fixture.jsonl" 0.25.0 "${space_result}" >/dev/null
assert_passed_result "${space_result}"
assert_no_candidate_processes_in_tmpdir \
  "${space_tmp}/ctx-native-candidate-smoke."

foreground_analytics_fake="${tmp}/ctx-foreground-analytics"
make_fake "${foreground_analytics_fake}"
foreground_analytics_result="${tmp}/foreground-analytics-result.json"
if "${smoke}" "${foreground_analytics_fake}" "${tmp}/fixture.jsonl" 0.25.0 \
  "${foreground_analytics_result}" >"${tmp}/foreground-analytics.out" \
  2>"${tmp}/foreground-analytics.err"; then
  printf 'candidate smoke accepted foreground analytics delivery\n' >&2
  exit 1
fi
grep -Fq 'foreground CLI delivered analytics before daemon ownership' \
  "${tmp}/foreground-analytics.err"
test ! -e "${foreground_analytics_result}"

no_analytics_delivery_fake="${tmp}/ctx-no-analytics-delivery"
make_fake "${no_analytics_delivery_fake}"
no_analytics_delivery_result="${tmp}/no-analytics-delivery-result.json"
started="$(date +%s)"
if TMPDIR="${space_tmp}" CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS=1 \
  "${smoke}" \
  "${no_analytics_delivery_fake}" "${tmp}/fixture.jsonl" 0.25.0 \
  "${no_analytics_delivery_result}" >"${tmp}/no-analytics-delivery.out" \
  2>"${tmp}/no-analytics-delivery.err"; then
  printf 'candidate smoke accepted an analytics daemon that did not deliver\n' >&2
  exit 1
fi
elapsed="$(( $(date +%s) - started ))"
[[ "${elapsed}" -lt 10 ]] || {
  printf 'candidate analytics delivery timeout was not bounded: %ss\n' "${elapsed}" >&2
  exit 1
}
grep -Fq 'daemon did not deliver queued status analytics within 1 seconds' \
  "${tmp}/no-analytics-delivery.err"
test ! -e "${no_analytics_delivery_result}"
assert_no_candidate_processes_in_tmpdir \
  "${space_tmp}/ctx-native-candidate-smoke."

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

pair_companion="${tmp}/ctx-pro"
printf '%s\n' '#!/bin/sh' 'exit 0' > "${pair_companion}"
chmod +x "${pair_companion}"
pair_envelope="${tmp}/pair-envelope.json"
printf '%s\n' '{}' > "${pair_envelope}"
pair_result="${tmp}/result-pair.json"
"${smoke}" "${fake}" "${pair_companion}" "${pair_envelope}" \
  "${tmp}/fixture.jsonl" 0.25.0 "${pair_result}" >/dev/null
assert_passed_result "${pair_result}"
grep -Fq '"managed_pair_apply":"passed"' "${pair_result}"
grep -Fq '"companion_selection":"passed"' "${pair_result}"
test "$(grep -Fc -- '--ctx-core-managed-pair-apply-v1' "${smoke}")" = 1
extra_receipt_fake="${tmp}/ctx-extra-receipt"
make_fake "${extra_receipt_fake}"
extra_receipt_result="${tmp}/result-pair-extra-receipt.json"
if "${smoke}" "${extra_receipt_fake}" "${pair_companion}" "${pair_envelope}" \
  "${tmp}/fixture.jsonl" 0.25.0 "${extra_receipt_result}" \
  >"${tmp}/extra-receipt.out" 2>"${tmp}/extra-receipt.err"; then
  printf 'candidate smoke accepted extra managed-pair receipt output\n' >&2
  exit 1
fi
grep -Fq 'invalid managed-pair apply receipt' "${tmp}/extra-receipt.err"
test ! -e "${extra_receipt_result}"

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
