#!/usr/bin/env bash
set -euo pipefail

data_root="${CTX_DATA_ROOT:?}"
command="${1:-}"
shift || true

write_state() {
  local pid="$1"
  mkdir -p -- "${data_root}/daemon"
  printf '{"binary":"%s/ctx","data_root":"%s","pid":%s,"released":false}\n' \
    "${data_root}" "${data_root}" "${pid}" > "${data_root}/daemon/daemon.lock"
  mkfifo "${data_root}/daemon/source-refresh.sock"
  printf '{"path":"%s/daemon/source-refresh.sock","pid":%s}\n' \
    "${data_root}" "${pid}" > "${data_root}/daemon/source-refresh-endpoint.json"
}

case "${command}" in
  __daemon)
    trap '
      printf "{\"binary\":\"%s/ctx\",\"data_root\":\"%s\",\"pid\":%s,\"released\":true}\n" \
        "${data_root}" "${data_root}" "$$" > "${data_root}/daemon/daemon.lock"
      rm -f -- "${data_root}/daemon/source-refresh.sock"
      exit 0
    ' TERM INT
    mkdir -p -- "${data_root}/daemon"
    : > "${data_root}/daemon/fake-ready"
    while :; do
      sleep 1
    done
    ;;
  import|fail)
    "${0}" __daemon >/dev/null 2>&1 &
    daemon_pid=$!
    for _ in {1..100}; do
      [[ -e "${data_root}/daemon/fake-ready" ]] && break
      sleep 0.01
    done
    [[ -e "${data_root}/daemon/fake-ready" ]]
    write_state "${daemon_pid}"
    if [[ -n "${CTX_UX_CAPTURE_TEST_PID_FILE:-}" ]]; then
      printf '%s\n' "${daemon_pid}" > "${CTX_UX_CAPTURE_TEST_PID_FILE}"
    fi
    [[ "${command}" != "fail" ]]
    ;;
  daemon)
    [[ "${1:-}" == "disable" ]]
    daemon_pid="$(
      sed -n 's/.*"pid"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
        "${data_root}/daemon/daemon.lock" | head -1
    )"
    if [[ -n "${daemon_pid}" ]]; then
      kill -TERM "${daemon_pid}" >/dev/null 2>&1 || true
    fi
    ;;
  *)
    printf 'unexpected fake ctx command: %s\n' "${command}" >&2
    exit 2
    ;;
esac
