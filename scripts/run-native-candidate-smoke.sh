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

json_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v sha256 >/dev/null 2>&1; then
    sha256 -q "$1"
  else
    printf 'candidate smoke requires a SHA-256 command\n' >&2
    exit 127
  fi
}

make_private_directories() {
  chmod 0700 "$@"
  chmod u-s,g-s,o-t "$@"
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
if ! command -v python3 >/dev/null 2>&1; then
  printf 'candidate smoke requires python3 for exact analytics evidence\n' >&2
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
make_private_directories "${root}"
analytics_daemon_pid=""
process_ids_for_binary() {
  [ -n "${candidate_binary:-}" ] || return 0

  candidate_process_snapshot="${root}/candidate-processes.snapshot"
  ps -axo pid=,command= > "${candidate_process_snapshot}" 2>/dev/null || return 0
  awk -v executable="${candidate_binary}" '
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
  ' "${candidate_process_snapshot}" | LC_ALL=C sort -n
}

candidate_pid_reapable() {
  candidate_pid_state="$(
    ps -o stat= -p "$1" 2>/dev/null | awk 'NR == 1 { print $1 }'
  )"
  case "${candidate_pid_state}" in
    ''|Z*) return 0 ;;
    *) return 1 ;;
  esac
}

terminate_and_reap_analytics_daemon() {
  known_analytics_pid="${analytics_daemon_pid:-}"
  [ -n "${known_analytics_pid}" ] || return 0
  analytics_daemon_pid=""

  if ! candidate_pid_reapable "${known_analytics_pid}"; then
    kill -TERM "${known_analytics_pid}" 2>/dev/null || true
  fi
  termination_waited=0
  while ! candidate_pid_reapable "${known_analytics_pid}" \
    && [ "${termination_waited}" -lt 3 ]; do
    sleep 1
    termination_waited=$((termination_waited + 1))
  done

  if ! candidate_pid_reapable "${known_analytics_pid}"; then
    kill -KILL "${known_analytics_pid}" 2>/dev/null || true
  fi
  termination_waited=0
  while ! candidate_pid_reapable "${known_analytics_pid}" \
    && [ "${termination_waited}" -lt 2 ]; do
    sleep 1
    termination_waited=$((termination_waited + 1))
  done

  if ! candidate_pid_reapable "${known_analytics_pid}"; then
    printf 'candidate cleanup could not terminate analytics daemon PID: %s\n' \
      "${known_analytics_pid}" >&2
    return 1
  fi
  wait "${known_analytics_pid}" 2>/dev/null || true
}

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
cleanup_analytics_processes() {
  analytics_cleanup_status=0
  terminate_and_reap_analytics_daemon || analytics_cleanup_status=1
  cleanup_candidate_processes || analytics_cleanup_status=1
  return "${analytics_cleanup_status}"
}
cleanup() {
  cleanup_status=$?
  trap - 0
  trap '' 1 2 15
  if ! cleanup_analytics_processes; then
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
make_private_directories \
  "${profile}" "${data_root}" "${config_root}" "${cache_root}" \
  "${state_root}" "${tmp_root}" "${work_root}"
candidate_dir="${root}/candidate"
if [ "${pair_mode}" = true ]; then
  case "$(uname -s):$(uname -m)" in
    Linux:x86_64|Linux:amd64) pair_platform=linux-x64 ;;
    Linux:aarch64|Linux:arm64) pair_platform=linux-aarch64 ;;
    Darwin:x86_64|Darwin:amd64) pair_platform=macos-x64 ;;
    Darwin:aarch64|Darwin:arm64) pair_platform=macos-arm64 ;;
    *) printf 'candidate smoke signed pairs are unsupported on this host\n' >&2; exit 1 ;;
  esac
  pair_channel="${CTX_MANAGED_PAIR_CHANNEL:-stable}"
  case "${pair_channel}" in
    stable) staging_dogfood=false ;;
    staging) staging_dogfood=true ;;
    *) printf 'candidate smoke managed-pair channel must be stable or staging\n' >&2; exit 1 ;;
  esac
  install_root="${root}/installation"
  candidate_dir="${install_root}/bin"
  candidate_binary=""
  marker_source="${root}/ctx.install.json"
  binary_sha256="$(sha256_file "${binary}")"
  mkdir -p "${candidate_dir}"
  make_private_directories "${install_root}" "${candidate_dir}"
  cat > "${marker_source}" <<EOF
{"schema_version":1,"manager":"ctx-hosted-installer","managed_pair":true,"install_attempt_id":"ia_native_smoke_$$","install_path":"$(json_escape "${candidate_dir}/ctx")","platform":"${pair_platform}","channel":"${pair_channel}","version":"$(json_escape "${expected_version}")","sha256":"${binary_sha256}","staging_dogfood":${staging_dogfood},"metadata_url":"native-candidate-smoke","artifact_url":"native-candidate-smoke","installed_at":"1970-01-01T00:00:00Z"}
EOF
else
  candidate_binary="${candidate_dir}/${binary##*/}"
  mkdir -p "${candidate_dir}"
  make_private_directories "${candidate_dir}"
  if ! cp "${binary}" "${candidate_binary}" \
    || [ ! -f "${candidate_binary}" ] \
    || [ -L "${candidate_binary}" ]; then
    printf 'candidate smoke could not create a regular private candidate copy\n' >&2
    exit 1
  fi
  chmod 0700 "${candidate_binary}"
  if ! cmp -s "${binary}" "${candidate_binary}"; then
    printf 'candidate smoke private candidate copy does not match the supplied binary\n' >&2
    exit 1
  fi
fi

# Start from an empty environment so provider overrides and user configuration
# cannot escape the isolated roots. Individual operational commands opt out of
# analytics and upgrades below. The released-default probes instead redirect
# analytics to an isolated file and use status, which cannot schedule work.
clean_env() {
  exec env -i \
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

if [ "${pair_mode}" = true ]; then
  pair_receipt="${root}/managed-pair-apply.json"
  run_bounded "${pair_receipt}" "${root}/managed-pair-apply.err" clean_env \
    "${binary}" --ctx-core-managed-pair-apply-v1 "${install_root}" - \
    "${pair_envelope}" "${binary}" "${companion}" "${marker_source}" || {
    cat "${root}/managed-pair-apply.err" >&2
    printf 'candidate Core could not apply the signed managed pair\n' >&2
    exit 1
  }
  if [ -s "${root}/managed-pair-apply.err" ] || ! printf '%s\n' \
    '{"schema_version":1,"command":"managed_pair_apply","ok":true,"status":"committed"}' \
    | cmp -s - "${pair_receipt}"; then
    printf 'candidate Core returned an invalid managed-pair apply receipt\n' >&2
    exit 1
  fi
  candidate_binary="${candidate_dir}/ctx"
  if [ ! -x "${candidate_binary}" ] || ! cmp -s "${binary}" "${candidate_binary}"; then
    printf 'candidate Core did not publish its exact executable bytes\n' >&2
    exit 1
  fi
fi

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
  ctx setup --no-daemon --progress none || {
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

# This is the public empty-config runtime-default gate. Foreground commands
# must append durably without delivery; an isolated persistent daemon then owns
# delivery to the local file transport without using the network or user state.
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
if [ "${pair_mode}" = true ]; then
  if ! printf '%s\n' "${status_compact}" \
    | grep -Eq '"upgrade"[[:space:]]*:[[:space:]]*\{[^}]*"auto"[[:space:]]*:[[:space:]]*"apply"[^}]*"auto_enabled"[[:space:]]*:[[:space:]]*true'; then
    printf 'candidate does not enable managed auto-upgrade by default\n' >&2
    exit 1
  fi
else
  # A Core-only candidate has no hosted marker, so the released `apply`
  # default must fail safe without attempting an unmanaged self-upgrade.
  if ! printf '%s\n' "${status_compact}" \
    | grep -Eq '"upgrade"[[:space:]]*:[[:space:]]*\{[^}]*"auto"[[:space:]]*:[[:space:]]*"off"[^}]*"auto_enabled"[[:space:]]*:[[:space:]]*false'; then
    printf 'candidate does not disable auto-upgrade in the unmanaged validation layout\n' >&2
    exit 1
  fi
fi
if [ -e "${analytics_default_events}" ]; then
  printf 'candidate foreground CLI delivered analytics before daemon ownership\n' >&2
  exit 1
fi
analytics_outbox_paths="${root}/analytics-outbox.paths"
find "${root}" -type f -name analytics-outbox-v1.json -print \
  > "${analytics_outbox_paths}"
if [ "$(awk 'END { print NR + 0 }' "${analytics_outbox_paths}")" -ne 1 ]; then
  printf 'candidate did not create exactly one durable analytics outbox\n' >&2
  exit 1
fi
analytics_outbox="$(sed -n '1p' "${analytics_outbox_paths}")"
analytics_outbox_before="${root}/analytics-outbox-before-daemon.json"
cp "${analytics_outbox}" "${analytics_outbox_before}"

clean_env \
  CTX_ANALYTICS_ENDPOINT="file://${analytics_default_events}" \
  CTX_UPGRADE_AUTO=off \
  CTX_DAEMON_ENABLED=true \
  CTX_DAEMON_MODE=source-refresh-only \
  CTX_SEARCH_SEMANTIC=0 \
  "${candidate_binary}" daemon run --force --loop-interval-seconds 600 \
  --format json > "${root}/analytics-daemon.out" \
  2> "${root}/analytics-daemon.err" &
analytics_daemon_pid=$!
analytics_waited=0
while [ ! -s "${analytics_default_events}" ] \
  && [ "${analytics_waited}" -lt "${command_timeout_seconds}" ]; do
  if ! kill -0 "${analytics_daemon_pid}" 2>/dev/null; then
    break
  fi
  sleep 1
  analytics_waited=$((analytics_waited + 1))
done
if [ ! -s "${analytics_default_events}" ]; then
  cat "${root}/analytics-daemon.err" >&2
  printf 'candidate daemon did not deliver queued status analytics within %s seconds\n' \
    "${command_timeout_seconds}" >&2
  exit 1
fi
if ! cleanup_analytics_processes; then
  printf 'candidate analytics daemon did not stop and reap after bounded delivery\n' >&2
  exit 1
fi
if ! python3 -I - "${analytics_outbox_before}" \
  "${analytics_default_events}" <<'PY'
import json
from pathlib import Path
import sys
import uuid

outbox = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
if outbox.get("schema_version") != 3 or not isinstance(outbox.get("entries"), list):
    raise SystemExit("analytics outbox has an unexpected schema")
queued = [
    json.loads(entry["payload"])
    for entry in outbox["entries"]
    if isinstance(entry, dict)
    and entry.get("kind") == "ordinary"
    and isinstance(entry.get("payload"), str)
]
delivered = [
    json.loads(line)
    for line in Path(sys.argv[2]).read_text(encoding="utf-8").splitlines()
    if line.strip()
]

def status_ids(payloads):
    return [
        event.get("event_id")
        for payload in payloads
        if isinstance(payload, dict) and isinstance(payload.get("events"), list)
        for event in payload["events"]
        if isinstance(event, dict)
        and event.get("event_name") == "operation_completed"
        and event.get("event_version") == 1
        and event.get("surface") == "cli"
        and event.get("operation") == "status"
        and event.get("outcome") == "success"
    ]

queued_ids = status_ids(queued)
delivered_ids = status_ids(delivered)
if len(queued_ids) != 1 or delivered_ids.count(queued_ids[0]) != 1:
    raise SystemExit("queued status analytics was not delivered exactly once")
if uuid.UUID(queued_ids[0]).version != 4:
    raise SystemExit("status analytics event does not have a UUIDv4")
PY
then
  printf 'candidate did not preserve exact status analytics across daemon delivery\n' >&2
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
  printf '%s\n' '{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"managed_pair_apply":"passed","companion_selection":"passed","version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","released_defaults":"passed","explicit_opt_outs":"passed","semantic_offline_fail_closed":"passed"}}' \
    > "${result_tmp}"
else
  printf '%s\n' '{"schema_version":1,"kind":"ctx-native-candidate-smoke","status":"passed","steps":{"version":"passed","setup":"passed","import":"passed","search":"passed","read_only":"passed","released_defaults":"passed","explicit_opt_outs":"passed","semantic_offline_fail_closed":"passed"}}' \
    > "${result_tmp}"
fi
mv "${result_tmp}" "${result_path}"
printf 'native candidate smoke passed: %s %s\n' "$(uname -s)" "$(uname -m)"
