#!/bin/sh
set -eu
umask 077

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/run-native-candidate-smoke.sh BINARY FIXTURE EXPECTED_VERSION RESULT_PATH
       scripts/run-native-candidate-smoke.sh CORE COMPANION PAIR_ENVELOPE FIXTURE EXPECTED_VERSION RESULT_PATH

Runs a bounded exact-byte ctx candidate smoke on native Linux, macOS, or
FreeBSD. The six-argument release form verifies and installs the signed pair in
the fixed layout, then proves that Core selects that companion. The four-
argument form remains for bounded Core-only unit fixtures. The history fixture
must be ctx-history-jsonl-v2. RESULT_PATH is written only after every step passes.
USAGE
}

if { [ "$#" -ne 4 ] && [ "$#" -ne 6 ]; } || [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 2
fi

absolute_path() {
  case "$1" in
    /*) printf '%s\n' "$1" ;;
    *) printf '%s/%s\n' "${PWD}" "$1" ;;
  esac
}

pair_mode=false
binary="$(absolute_path "$1")"
if [ "$#" -eq 6 ]; then
  pair_mode=true
  companion="$(absolute_path "$2")"
  pair_envelope="$(absolute_path "$3")"
  fixture="$(absolute_path "$4")"
  expected_version="$5"
  result_path="$(absolute_path "$6")"
else
  companion=""
  pair_envelope=""
  fixture="$(absolute_path "$2")"
  expected_version="$3"
  result_path="$(absolute_path "$4")"
fi
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
if [ "${pair_mode}" = true ]; then
  if [ ! -f "${companion}" ] || [ ! -x "${companion}" ] || [ -L "${companion}" ]; then
    printf 'candidate smoke companion is missing or not an executable regular file: %s\n' "${companion}" >&2
    exit 1
  fi
  if [ ! -f "${pair_envelope}" ] || [ -L "${pair_envelope}" ]; then
    printf 'candidate smoke signed pair envelope is missing or not a regular file: %s\n' "${pair_envelope}" >&2
    exit 1
  fi
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
version_core="${expected_version%%[-+]*}"
version_major="${version_core%%.*}"
version_remainder="${version_core#*.}"
version_minor="${version_remainder%%.*}"
fresh_epoch_required=false
if [ "${version_major}" -gt 0 ] || [ "${version_minor}" -ge 26 ]; then
  fresh_epoch_required=true
fi

result_dir="$(dirname "${result_path}")"
mkdir -p "${result_dir}"
rm -f "${result_path}"
result_tmp="${result_path}.tmp.$$"
root="$(mktemp -d "${TMPDIR:-/tmp}/ctx-native-candidate-smoke.XXXXXX")"
# macOS exposes /tmp as a symlink to /private/tmp. Resolve the private root
# before passing its descendants to ctx's no-follow directory traversal.
root="$(CDPATH= cd -- "${root}" && pwd -P)"
cleanup_candidate_processes() {
  [ -n "${candidate_binary:-}" ] || return 0

  cleanup_pids="$(process_ids_for_binary)"
  [ -n "${cleanup_pids}" ] || return 0
  kill -TERM ${cleanup_pids} 2>/dev/null || true

  cleanup_waited=0
  while [ "${cleanup_waited}" -lt 3 ]; do
    sleep 1
    cleanup_pids="$(process_ids_for_binary)"
    [ -n "${cleanup_pids}" ] || return 0
    cleanup_waited=$((cleanup_waited + 1))
  done

  kill -KILL ${cleanup_pids} 2>/dev/null || true
  cleanup_waited=0
  while [ "${cleanup_waited}" -lt 2 ]; do
    sleep 1
    cleanup_pids="$(process_ids_for_binary)"
    [ -n "${cleanup_pids}" ] || return 0
    cleanup_waited=$((cleanup_waited + 1))
  done

  printf 'candidate cleanup could not terminate copied-candidate processes: %s\n' \
    "${cleanup_pids}" >&2
  return 1
}
cleanup() {
  cleanup_status=$?
  trap - 0
  trap '' 1 2 15
  if ! cleanup_candidate_processes; then
    printf 'candidate smoke retained private root for survivor diagnosis: %s\n' \
      "${root}" >&2
    rm -f "${result_tmp}" "${result_path}" || true
    if [ "${cleanup_status}" -eq 0 ]; then
      cleanup_status=1
    fi
    exit "${cleanup_status}"
  fi
  rm -f "${result_tmp}" || true
  rm -rf "${root}" || true
  exit "${cleanup_status}"
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
candidate_dir="${root}/candidate"
if [ "${pair_mode}" = true ]; then
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64) pair_target=linux-x64 ;;
    Linux:aarch64|Linux:arm64) pair_target=linux-arm64 ;;
    Darwin:x86_64|Darwin:amd64) pair_target=macos-x64 ;;
    Darwin:aarch64|Darwin:arm64) pair_target=macos-arm64 ;;
    *) printf 'candidate smoke signed pairs are unsupported on this host\n' >&2; exit 1 ;;
  esac
  command -v python3 >/dev/null 2>&1 || {
    printf 'candidate smoke requires Python 3 for signed-pair verification\n' >&2
    exit 127
  }
  candidate_dir="${root}/installation/bin"
  python3 -I "${script_dir}/install-managed-pair.py" install \
    --envelope "${pair_envelope}" --core "${binary}" --companion "${companion}" \
    --install-root "${root}/installation" --target "${pair_target}" >/dev/null
  candidate_binary="${root}/installation/bin/ctx"
else
  candidate_binary="${candidate_dir}/${binary##*/}"
  mkdir -p "${candidate_dir}"
  chmod 0700 "${root}" "${candidate_dir}"
  if ! cp "${binary}" "${candidate_binary}" \
    || [ ! -f "${candidate_binary}" ] \
    || [ -L "${candidate_binary}" ]; then
    printf 'candidate smoke could not create a regular private candidate copy\n' >&2
    exit 1
  fi
  chmod 0700 "${candidate_binary}"
fi
if ! cmp -s "${binary}" "${candidate_binary}"; then
  printf 'candidate smoke private candidate copy does not match the supplied binary\n' >&2
  exit 1
fi

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
    CTX_DAEMON_AUTOSTART_LOOP_INTERVAL_SECONDS=1 \
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
    "${candidate_binary}" "$@"
}

ctx_source_refresh() {
  clean_env \
    CTX_ANALYTICS_ENABLED=false \
    CTX_UPGRADE_AUTO=off \
    CTX_DAEMON_ENABLED=true \
    CTX_SEARCH_SEMANTIC=0 \
    CTX_DAEMON_AUTOSTART_OFF=0 \
    "${candidate_binary}" "$@"
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

process_ids_for_binary() {
  ps -axo pid=,command= 2>/dev/null \
    | awk -v executable="${candidate_binary}" \
      '$2 == executable || $3 == executable { print $1 }' \
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

if [ "${pair_mode}" = true ]; then
  run_bounded "${root}/companion-selection.out" "${root}/companion-selection.err" \
    ctx pro --help || {
    cat "${root}/companion-selection.err" >&2
    printf 'candidate Core did not select its verified fixed companion\n' >&2
    exit 1
  }
fi

run_bounded "${root}/setup.out" "${root}/setup.err" \
  ctx setup --catalog-only --no-daemon --progress none || {
  cat "${root}/setup.err" >&2
  exit 1
}
core_manifest_required="${fresh_epoch_required}"
if ! run_bounded "${root}/import.json" "${root}/import.err" ctx import \
  --input-format ctx-history-jsonl-v2 \
  --path "${fixture}" \
  --no-daemon \
  --format json \
  --progress none; then
  if ! grep -Fq 'no foreground writer was started' "${root}/import.err"; then
    cat "${root}/import.err" >&2
    exit 1
  fi
  core_manifest_required=true
  run_bounded "${root}/import.json" "${root}/import.err" ctx_source_refresh import \
    --input-format ctx-history-jsonl-v2 \
    --path "${fixture}" \
    --format json \
    --progress none || {
    cat "${root}/import.err" >&2
    exit 1
  }
  if ! cleanup_candidate_processes; then
    printf 'candidate import daemon did not stop after bounded teardown\n' >&2
    exit 1
  fi
fi
if [ "${fresh_epoch_required}" = true ]; then
  if ! grep -Eq '"current_source_count"[[:space:]]*:[[:space:]]*[1-9][0-9]*' "${root}/import.json" \
    || ! grep -Eq '"current_indexed_documents"[[:space:]]*:[[:space:]]*[1-9][0-9]*' "${root}/import.json" \
    || ! grep -Eq '"published_generation"[[:space:]]*:[[:space:]]*"[0-9a-f]{64}"' "${root}/import.json"; then
    printf 'candidate fixture import did not publish Core-generation authority\n' >&2
    exit 1
  fi
elif ! grep -Eq '"imported_events"[[:space:]]*:[[:space:]]*[1-9][0-9]*' "${root}/import.json" \
  && { ! grep -Eq '"imported_sources"[[:space:]]*:[[:space:]]*[1-9][0-9]*' "${root}/import.json" \
    || ! grep -Eq '"published_generation"[[:space:]]*:[[:space:]]*"[0-9a-f]{64}"' "${root}/import.json"; }; then
    printf 'candidate fixture import did not report imported data\n' >&2
    exit 1
fi

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
# Import and search execute in separate candidate processes. The expected hit
# plus the absence of the old Store proves that the fresh Core generation, not
# pre-v0.26 SQLite authority, carried the fixture across that boundary.
if [ -e "${data_root}/work.sqlite" ]; then
  printf 'candidate created or opened the pre-v0.26 Store\n' >&2
  exit 1
fi
if [ "${core_manifest_required}" = true ]; then
  if [ ! -f "${data_root}/search/lexical/active-generation.json" ]; then
    printf 'candidate did not publish the fresh lexical generation\n' >&2
    exit 1
  fi
  core_manifest_found=false
  for core_manifest in "${data_root}/search/lexical/ctx-generations/"*.json; do
    if [ -f "${core_manifest}" ]; then
      core_manifest_found=true
      break
    fi
  done
  if [ "${core_manifest_found}" != true ]; then
    printf 'candidate did not publish Core-generation authority\n' >&2
    exit 1
  fi
fi

analytics_default="$(inventory_default_field "analytics delivery" "value")"
upgrade_default="$(inventory_default_field "automatic upgrade mode" "value")"
indexing_default="$(inventory_default_field "indexing mode" "value")"
semantic_default="$(inventory_default_field "semantic search" "value")"
if [ "${analytics_default}" != true ] \
  || [ "${upgrade_default}" != apply ] \
  || [ "${indexing_default}" != auto ] \
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
  "${candidate_binary}" status --format json || {
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
# Both validation layouts deliberately omit the hosted-install marker. The
# control inventory above proves the released `apply` default; this runtime
# probe proves an isolated, unmanaged candidate fails safe instead of trying to
# self-upgrade.
if ! printf '%s\n' "${status_compact}" \
  | grep -Eq '"upgrade"[[:space:]]*:[[:space:]]*\{[^}]*"auto"[[:space:]]*:[[:space:]]*"off"[^}]*"auto_enabled"[[:space:]]*:[[:space:]]*false'; then
  printf 'candidate does not disable auto-upgrade in the unmanaged validation layout\n' >&2
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
  "${candidate_binary}" status --format json || {
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
  "${candidate_binary}" search "parser test" --backend semantic --refresh off --format json; then
  printf 'semantic-only search unexpectedly succeeded\n' >&2
  exit 1
fi
if ! grep -Eq 'semantic_store_missing|semantic-only search will not initialize or download' \
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
  || [ -e "${data_root}/search/semantic" ]; then
  printf 'semantic-only search created semantic state\n' >&2
  exit 1
fi

shutdown_attempts=0
while :; do
  process_ids_for_binary > "${final_processes}"
  survivors="$(comm -13 "${baseline_processes}" "${final_processes}")"
  if [ -z "${survivors}" ] || [ "${shutdown_attempts}" -ge 10 ]; then
    break
  fi
  shutdown_attempts=$((shutdown_attempts + 1))
  sleep 1
done
if [ -n "${survivors}" ]; then
  printf 'candidate left a background process running: %s\n' "${survivors}" >&2
  exit 1
fi

if [ "${pair_mode}" = true ]; then
  printf '%s\n' '{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"signed_pair_install":"passed","companion_selection":"passed","version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","released_defaults":"passed","explicit_opt_outs":"passed","semantic_offline_fail_closed":"passed"}}' \
    > "${result_tmp}"
else
  printf '%s\n' '{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","released_defaults":"passed","explicit_opt_outs":"passed","semantic_offline_fail_closed":"passed"}}' \
    > "${result_tmp}"
fi
mv "${result_tmp}" "${result_path}"
printf 'native candidate smoke passed: %s %s\n' "$(uname -s)" "$(uname -m)"
