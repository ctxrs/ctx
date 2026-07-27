#!/bin/sh
set -eu

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/run-native-candidate-smoke.sh BINARY FIXTURE EXPECTED_VERSION RESULT_PATH

Runs a bounded exact-byte ctx candidate smoke on native Linux, macOS, or
FreeBSD. The fixture must be ctx-history-jsonl-v1. RESULT_PATH is written only
after every step passes.
USAGE
}

if [ "$#" -ne 4 ] || [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 2
fi

absolute_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "${PWD}" "$1" ;;
  esac
}

binary="$(absolute_path "$1")"
fixture="$(absolute_path "$2")"
expected_version="$3"
result_path="$(absolute_path "$4")"
command_timeout_seconds="${CTX_NATIVE_CANDIDATE_COMMAND_TIMEOUT_SECONDS:-60}"
script_dir="$(CDPATH= cd -- "$(dirname "$0")" && pwd)"
control_inventory="${script_dir}/../contracts/public-control-surface-v1.json"

case "${command_timeout_seconds}" in
  ''|*[!0-9]*|0)
    printf 'candidate smoke timeout must be a positive whole number of seconds\n' >&2
    exit 2
    ;;
esac
if [ "${command_timeout_seconds}" -gt 900 ]; then
  printf 'candidate smoke timeout must not exceed 900 seconds\n' >&2
  exit 2
fi

if [ ! -f "${binary}" ] || [ ! -x "${binary}" ]; then
  printf 'candidate smoke binary is missing or not executable: %s\n' "${binary}" >&2
  exit 1
fi
if [ ! -f "${fixture}" ]; then
  printf 'candidate smoke fixture is missing: %s\n' "${fixture}" >&2
  exit 1
fi
if [ ! -f "${control_inventory}" ]; then
  printf 'candidate smoke control inventory is missing: %s\n' \
    "${control_inventory}" >&2
  exit 1
fi
if ! command -v ps >/dev/null 2>&1; then
  printf 'candidate smoke requires ps for survivor detection\n' >&2
  exit 127
fi
if ! printf '%s\n' "${expected_version}" \
  | grep -Eq '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$'; then
  printf 'candidate smoke expected version is invalid: %s\n' "${expected_version}" >&2
  exit 1
fi

result_dir="$(dirname "${result_path}")"
mkdir -p "${result_dir}"
rm -f "${result_path}"
result_tmp="${result_path}.tmp.$$"
root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-native-candidate-smoke.XXXXXX")"
cleanup() {
  rm -f "${result_tmp}"
  rm -rf "${root}"
}
trap cleanup 0
trap 'exit 1' 1 2 15

profile="${root}/profile"
data_root="${root}/data"
config_root="${root}/config"
cache_root="${root}/cache"
state_root="${root}/state"
tmp_root="${root}/tmp"
work_root="${root}/work"
mkdir -p "${profile}" "${data_root}" "${config_root}" "${cache_root}" \
  "${state_root}" "${tmp_root}" "${work_root}"

# Start from an empty environment so provider overrides and user configuration
# cannot escape the isolated roots. Individual operational commands opt out of
# analytics and upgrades below. The released-default probes instead redirect
# analytics to an isolated file and use status, which cannot schedule work.
clean_env() {
  env -i \
    PATH="${PATH:-/usr/bin:/bin}" \
    HOME="${profile}" \
    USER="${USER:-ctx-smoke}" \
    LOGNAME="${LOGNAME:-ctx-smoke}" \
    TMPDIR="${tmp_root}" \
    XDG_CONFIG_HOME="${config_root}" \
    XDG_CACHE_HOME="${cache_root}" \
    XDG_DATA_HOME="${root}/xdg-data" \
    XDG_STATE_HOME="${state_root}" \
    CTX_DATA_ROOT="${data_root}" \
    CTX_DAEMON_AUTOSTART_OFF=1 \
    CTX_SEMANTIC_CACHE_DIR="${root}/semantic-cache" \
    HF_HOME="${root}/huggingface" \
    HF_HUB_OFFLINE=1 \
    TRANSFORMERS_OFFLINE=1 \
    "$@"
}

ctx() {
  clean_env \
    CTX_ANALYTICS_ENABLED=false \
    CTX_UPGRADE_AUTO=off \
    CTX_DAEMON_ENABLED=false \
    CTX_SEARCH_SEMANTIC=0 \
    "${binary}" "$@"
}

inventory_default_field() {
  inventory_behavior="$1"
  inventory_field="$2"
  awk -v behavior="${inventory_behavior}" -v field="${inventory_field}" '
    index($0, "\"behavior\": \"" behavior "\"") { in_control = 1 }
    in_control && /"released_default"[[:space:]]*:/ { in_default = 1 }
    in_default && index($0, "\"" field "\"") {
      value = $0
      sub(/^[^:]*:[[:space:]]*/, "", value)
      sub(/,[[:space:]]*$/, "", value)
      gsub(/^"|"$/, "", value)
      print value
      exit
    }
    in_control && /^    }[,]?$/ { exit }
  ' "${control_inventory}"
}

status_top_level_bool() {
  status_object="$1"
  status_field="$2"
  status_expected="$3"
  status_file="$4"
  awk -v object="${status_object}" -v field="${status_field}" \
    -v expected="${status_expected}" '
    $0 == "  \"" object "\": {" { in_object = 1; next }
    in_object && /^  },?$/ { exit found ? 0 : 1 }
    in_object {
      expected_line = "    \"" field "\": " expected
      if ($0 == expected_line || $0 == expected_line ",") {
        found = 1
      }
    }
    END { exit found ? 0 : 1 }
  ' "${status_file}"
}

run_bounded() {
  bounded_stdout="$1"
  bounded_stderr="$2"
  shift 2
  bounded_timeout_marker="${root}/command-timeout.$$"
  rm -f "${bounded_timeout_marker}"
  ( "$@" ) >"${bounded_stdout}" 2>"${bounded_stderr}" &
  bounded_pid=$!
  (
    sleep "${command_timeout_seconds}"
    if kill -0 "${bounded_pid}" 2>/dev/null; then
      : > "${bounded_timeout_marker}"
      kill -TERM "${bounded_pid}" 2>/dev/null || true
      sleep 2
      kill -KILL "${bounded_pid}" 2>/dev/null || true
    fi
  ) &
  bounded_watcher=$!
  bounded_status=0
  wait "${bounded_pid}" || bounded_status=$?
  kill "${bounded_watcher}" 2>/dev/null || true
  wait "${bounded_watcher}" 2>/dev/null || true
  if [ -e "${bounded_timeout_marker}" ]; then
    rm -f "${bounded_timeout_marker}"
    printf 'candidate command exceeded %s seconds: %s\n' \
      "${command_timeout_seconds}" "$*" >&2
    return 124
  fi
  return "${bounded_status}"
}

pro_status_reports_absent_local_runtime() {
  status_file="$1"
  status_compact="$(tr '\r\n' '  ' < "${status_file}")"

  for expected in \
    '"schema_version"[[:space:]]*:[[:space:]]*2' \
    '"payload_type"[[:space:]]*:[[:space:]]*"pro_status"' \
    '"state"[[:space:]]*:[[:space:]]*"not_setup"' \
    '"installed"[[:space:]]*:[[:space:]]*false' \
    '"ready"[[:space:]]*:[[:space:]]*false' \
    '"materialized"[[:space:]]*:[[:space:]]*false' \
    '"helper_version"[[:space:]]*:[[:space:]]*null' \
    '"protocol_version"[[:space:]]*:[[:space:]]*1' \
    '"capabilities"[[:space:]]*:[[:space:]]*\[[[:space:]]*\]' \
    '"command"[[:space:]]*:[[:space:]]*"ctx pro"' \
    '"reason"[[:space:]]*:[[:space:]]*"helper_missing"'
  do
    if ! printf '%s\n' "${status_compact}" | grep -Eq "${expected}"; then
      return 1
    fi
  done

  # The public status shape is path-safe. Commercial access/error fields are
  # intentionally independent of whether the signed helper and graph exist.
  ! printf '%s\n' "${status_compact}" \
    | grep -Eq '"helper_path"[[:space:]]*:'
}

process_ids_for_binary() {
  ps -axo pid=,command= 2>/dev/null \
    | awk -v executable="${binary}" '$2 == executable { print $1 }' \
    | LC_ALL=C sort -n
}

baseline_processes="${root}/baseline-processes"
final_processes="${root}/final-processes"
process_ids_for_binary > "${baseline_processes}"

cd "${work_root}"

if ! run_bounded "${root}/version.out" "${root}/version.err" ctx --version; then
  cat "${root}/version.err" >&2
  printf 'candidate version command failed\n' >&2
  exit 1
fi
version_output="$(cat "${root}/version.out")"
if [ "${version_output}" != "ctx ${expected_version}" ]; then
  printf 'candidate version mismatch: expected ctx %s, got %s\n' \
    "${expected_version}" "${version_output}" >&2
  exit 1
fi

run_bounded "${root}/setup.out" "${root}/setup.err" \
  ctx setup --catalog-only --no-daemon --progress none || {
  cat "${root}/setup.err" >&2
  exit 1
}
run_bounded "${root}/import.json" "${root}/import.err" ctx import \
  --input-format ctx-history-jsonl-v1 \
  --path "${fixture}" \
  --no-daemon \
  --format json \
  --progress none || {
  cat "${root}/import.err" >&2
  exit 1
}
grep -Eq '"imported_events"[[:space:]]*:[[:space:]]*[1-9][0-9]*' "${root}/import.json" || {
  printf 'candidate fixture import did not import events\n' >&2
  exit 1
}

run_bounded "${root}/search.json" "${root}/search.err" ctx search "parser test" \
  --backend lexical \
  --refresh off \
  --format json || {
  cat "${root}/search.err" >&2
  exit 1
}
grep -Eq '"requested_mode"[[:space:]]*:[[:space:]]*"lexical"' "${root}/search.json" \
  || { printf 'candidate search did not request lexical mode\n' >&2; exit 1; }
grep -Eq '"effective_mode"[[:space:]]*:[[:space:]]*"lexical"' "${root}/search.json" \
  || { printf 'candidate search did not remain lexical\n' >&2; exit 1; }
grep -Fq 'Add a parser test.' "${root}/search.json" \
  || { printf 'candidate search did not return the fixture event\n' >&2; exit 1; }

analytics_default="$(inventory_default_field "analytics delivery" "value")"
upgrade_default="$(inventory_default_field "automatic upgrade mode" "value")"
daemon_default="$(inventory_default_field "daemon maintenance" "value")"
semantic_default="$(inventory_default_field "semantic search" "value")"
if [ "${analytics_default}" != true ] \
  || [ "${upgrade_default}" != apply ] \
  || [ "${daemon_default}" != true ] \
  || [ "${semantic_default}" != false ]; then
  printf 'candidate smoke control inventory has unexpected released defaults\n' >&2
  exit 1
fi

# This is the public empty-config runtime-default gate. The analytics endpoint
# is a local file transport, so the probe exercises the default without using
# the network or the user's real state.
analytics_default_events="${root}/analytics-default.jsonl"
run_bounded "${root}/status.json" "${root}/status.err" clean_env \
  CTX_ANALYTICS_ENDPOINT="file://${analytics_default_events}" \
  "${binary}" status --format json || {
  cat "${root}/status.err" >&2
  exit 1
}
grep -Eq '"read_only"[[:space:]]*:[[:space:]]*true' "${root}/status.json" || {
  printf 'candidate read-only status command returned an unexpected payload\n' >&2
  exit 1
}
if ! status_top_level_bool daemon enabled true "${root}/status.json"; then
  printf 'candidate does not report daemon maintenance as enabled by default\n' >&2
  exit 1
fi
status_compact="$(tr '\r\n' '  ' < "${root}/status.json")"
if ! printf '%s\n' "${status_compact}" \
  | grep -Eq '"upgrade"[[:space:]]*:[[:space:]]*\{[^}]*"auto"[[:space:]]*:[[:space:]]*"apply"[^}]*"auto_enabled"[[:space:]]*:[[:space:]]*true'; then
  printf 'candidate does not report managed auto-upgrade apply as the default\n' >&2
  exit 1
fi
if [ ! -s "${analytics_default_events}" ]; then
  printf 'candidate did not exercise default-on analytics through the local endpoint\n' >&2
  exit 1
fi

analytics_opt_out_events="${root}/analytics-opt-out.jsonl"
run_bounded "${root}/status-opt-out.json" "${root}/status-opt-out.err" clean_env \
  CTX_ANALYTICS_ENABLED=false \
  CTX_ANALYTICS_ENDPOINT="file://${analytics_opt_out_events}" \
  CTX_UPGRADE_AUTO=off \
  CTX_DAEMON_ENABLED=false \
  "${binary}" status --format json || {
  cat "${root}/status-opt-out.err" >&2
  exit 1
}
if ! status_top_level_bool daemon enabled false "${root}/status-opt-out.json"; then
  printf 'candidate daemon opt-out did not override the released default\n' >&2
  exit 1
fi
status_opt_out_compact="$(tr '\r\n' '  ' < "${root}/status-opt-out.json")"
if ! printf '%s\n' "${status_opt_out_compact}" \
  | grep -Eq '"upgrade"[[:space:]]*:[[:space:]]*\{[^}]*"auto"[[:space:]]*:[[:space:]]*"off"[^}]*"auto_enabled"[[:space:]]*:[[:space:]]*false'; then
  printf 'candidate upgrade opt-out did not override the released default\n' >&2
  exit 1
fi
if [ -e "${analytics_opt_out_events}" ]; then
  printf 'candidate analytics opt-out did not override the released default\n' >&2
  exit 1
fi

# A distributable ctx must select only the signed helper pair installed below
# its data root. Prove that an ambient developer override cannot execute an
# arbitrary helper in the exact candidate artifact.
untrusted_helper="${root}/untrusted-ctx-pro"
override_marker="${root}/untrusted-helper-executed"
cat > "${untrusted_helper}" <<EOF
#!/bin/sh
: > "${override_marker}"
exit 99
EOF
chmod 0700 "${untrusted_helper}"
run_bounded "${root}/pro-status.json" "${root}/pro-status.err" \
  clean_env CTX_ANALYTICS_ENABLED=false CTX_UPGRADE_AUTO=off \
  CTX_DAEMON_ENABLED=false \
  CTX_PRO_HELPER="${untrusted_helper}" \
  "${binary}" status --format json || {
  cat "${root}/pro-status.err" >&2
  printf 'candidate Pro status query failed while testing helper selection\n' >&2
  exit 1
}
if [ -e "${override_marker}" ]; then
  printf 'candidate executed CTX_PRO_HELPER in a distributable build\n' >&2
  exit 1
fi
if ! pro_status_reports_absent_local_runtime "${root}/pro-status.json"; then
  printf 'candidate Pro status did not report an absent helper and graph\n' >&2
  exit 1
fi
if [ -e "${data_root}/ctx-pro.db" ]; then
  printf 'candidate Pro status created a graph while reporting no local runtime\n' >&2
  exit 1
fi
if grep -Fq "${untrusted_helper}" "${root}/pro-status.json" \
  || grep -Fq "${untrusted_helper}" "${root}/pro-status.err"; then
  printf 'candidate exposed the rejected Pro helper override path\n' >&2
  exit 1
fi

# Semantic search is supported but opt-in on every public release target. Prove
# that the default remains disabled, then that an explicit offline request with
# no provisioned model fails closed without fallback, state, or download.
if ! grep -Eq '"config_source"[[:space:]]*:[[:space:]]*"default"' "${root}/status.json" \
  || ! grep -Eq '"reason"[[:space:]]*:[[:space:]]*"semantic_disabled"' "${root}/status.json"; then
  printf 'native candidate does not report semantic search as disabled by default\n' >&2
  exit 1
fi
if grep -Eq '"source"[[:space:]]*:[[:space:]]*"unsupported"' "${root}/status.json"; then
  printf 'native candidate unexpectedly reports semantic search as unsupported\n' >&2
  exit 1
fi
if run_bounded "${root}/semantic.out" "${root}/semantic.err" clean_env \
  CTX_ANALYTICS_ENABLED=false \
  CTX_UPGRADE_AUTO=off \
  CTX_DAEMON_ENABLED=1 \
  CTX_SEARCH_SEMANTIC=1 \
  "${binary}" search "parser test" --backend semantic --refresh off --format json; then
  printf 'semantic-only search unexpectedly succeeded\n' >&2
  exit 1
fi
if ! grep -Fq 'semantic-only search will not initialize or download' \
  "${root}/semantic.err"; then
  printf 'semantic-only search did not report the fail-closed capability contract\n' >&2
  exit 1
fi
if grep -Eq '"effective_mode"[[:space:]]*:[[:space:]]*"lexical"' \
  "${root}/semantic.out"; then
  printf 'semantic-only search silently fell back to lexical\n' >&2
  exit 1
fi
if [ -e "${root}/semantic-cache" ] || [ -e "${root}/huggingface" ] \
  || [ -e "${data_root}/vectors.sqlite" ] || [ -e "${data_root}/daemon" ]; then
  printf 'semantic-only search created semantic or daemon state\n' >&2
  exit 1
fi

process_ids_for_binary > "${final_processes}"
survivors="$(comm -13 "${baseline_processes}" "${final_processes}")"
if [ -n "${survivors}" ]; then
  printf 'candidate left a background process running: %s\n' "${survivors}" >&2
  exit 1
fi
if [ -e "${data_root}/daemon/daemon.lock" ]; then
  printf 'candidate left a daemon lock behind\n' >&2
  exit 1
fi

printf '%s\n' '{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","released_defaults":"passed","explicit_opt_outs":"passed","pro_helper_override_ignored":"passed","semantic_offline_fail_closed":"passed"}}' \
  > "${result_tmp}"
mv "${result_tmp}" "${result_path}"
printf 'native candidate smoke passed: %s %s\n' "$(uname -s)" "$(uname -m)"
