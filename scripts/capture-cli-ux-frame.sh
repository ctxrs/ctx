#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/capture-cli-ux-frame.sh --ctx PATH [--keep-root] -- COMMAND [ARG...]

Runs one ctx CLI capture command in an isolated Linux data root with an
owner-safe copy of the supplied binary. Before the root or binary can be
removed, cleanup disables the task-owned daemon and verifies through /proc
that its process, advisory lock, and source-refresh endpoint have all been
released.
USAGE
}

ctx_source=""
keep_root=0
while (($# > 0)); do
  case "$1" in
    --ctx)
      shift
      ctx_source="${1:-}"
      ;;
    --keep-root)
      keep_root=1
      ;;
    --)
      shift
      break
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      exit 2
      ;;
  esac
  shift
done

if [[ -z "${ctx_source}" || ! -f "${ctx_source}" || ! -x "${ctx_source}" ]]; then
  echo "error: --ctx must name an executable ctx binary" >&2
  exit 2
fi
if (($# == 0)); then
  echo "error: a ctx command is required after --" >&2
  exit 2
fi
if [[ ! -d "/proc/$$" ]]; then
  echo "error: CLI UX capture teardown requires Linux /proc process identity" >&2
  exit 2
fi

ctx_source="$(cd -- "$(dirname -- "${ctx_source}")" && pwd -P)/$(basename -- "${ctx_source}")"
run_parent="${TMPDIR:-/tmp}"
mkdir -p -- "${run_parent}"
run_parent="$(cd -- "${run_parent}" && pwd -P)"
run_root="$(mktemp -d "${run_parent%/}/ctx-cli-ux-frame.XXXXXX")"
chmod 700 "${run_root}"
run_root="$(cd -- "${run_root}" && pwd -P)"
ctx_bin="${run_root}/ctx"
workspace="${run_root}/workspace"
mkdir -p -- "${workspace}"
cp -- "${ctx_source}" "${ctx_bin}"
chmod 700 "${ctx_bin}"

isolated_ctx() {
  env \
    -u CODEX_HOME \
    -u CLAUDE_CONFIG_DIR \
    -u COPILOT_HOME \
    -u HERMES_HOME \
    -u XDG_CONFIG_HOME \
    -u XDG_DATA_HOME \
    -u XDG_STATE_HOME \
    CTX_DATA_ROOT="${run_root}" \
    HOME="${run_root}" \
    CTX_ANALYTICS_ENABLED=false \
    CTX_LOCAL_USAGE_ENABLED=false \
    "${ctx_bin}" "$@"
}

task_owned_pids() {
  local command_line
  local proc
  local pid
  for proc in /proc/[0-9]*/cmdline; do
    [[ -r "${proc}" ]] || continue
    command_line="$(tr '\0' ' ' < "${proc}" 2>/dev/null || true)"
    [[ "${command_line}" == *"${ctx_bin}"* ]] || continue
    [[ "${command_line}" == *"${run_root}"* ]] || continue
    pid="${proc#/proc/}"
    printf '%s\n' "${pid%/cmdline}"
  done
}

lock_released() {
  local lock_path="${run_root}/daemon/daemon.lock"
  [[ ! -e "${lock_path}" ]] && return 0
  grep -Eq '"released"[[:space:]]*:[[:space:]]*true' "${lock_path}" 2>/dev/null
}

endpoint_released() {
  local endpoint_metadata="${run_root}/daemon/source-refresh-endpoint.json"
  local endpoint_path
  [[ ! -e "${endpoint_metadata}" ]] && return 0
  endpoint_path="$(
    sed -n 's/.*"path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      "${endpoint_metadata}" | head -1
  )"
  [[ -z "${endpoint_path}" || ! -e "${endpoint_path}" ]]
}

daemon_released() {
  [[ -z "$(task_owned_pids)" ]] && lock_released && endpoint_released
}

wait_for_release() {
  local attempt
  for attempt in {1..100}; do
    if daemon_released; then
      return 0
    fi
    sleep 0.05
  done
  return 1
}

stop_and_verify_daemon() {
  local pid
  isolated_ctx daemon disable --format=json >/dev/null 2>&1 || true
  if wait_for_release; then
    return 0
  fi

  # The normal CLI shutdown path is authoritative. This bounded fallback is
  # restricted to processes whose command line contains both the copied binary
  # and this exact task root.
  while IFS= read -r pid; do
    [[ -n "${pid}" ]] || continue
    kill -TERM "${pid}" >/dev/null 2>&1 || true
  done < <(task_owned_pids)

  if wait_for_release; then
    return 0
  fi

  echo "error: task-owned ctx daemon did not release before capture cleanup" >&2
  while IFS= read -r pid; do
    [[ -n "${pid}" ]] && echo "  residual pid ${pid}" >&2
  done < <(task_owned_pids)
  echo "  retained root ${run_root}" >&2
  return 1
}

capture_status=0
cleanup() {
  local cleanup_status=0
  trap - EXIT INT TERM

  if ! stop_and_verify_daemon; then
    cleanup_status=1
    keep_root=1
  fi
  if [[ -n "$(task_owned_pids)" ]]; then
    echo "error: post-capture orphan assertion found a task-owned process" >&2
    cleanup_status=1
    keep_root=1
  fi

  if [[ "${keep_root}" == "1" ]]; then
    echo "ctx CLI UX capture root: ${run_root}" >&2
  else
    case "$(basename -- "${run_root}")" in
      ctx-cli-ux-frame.*) rm -rf -- "${run_root}" ;;
      *)
        echo "error: refusing to remove unexpected capture root ${run_root}" >&2
        cleanup_status=1
        ;;
    esac
  fi

  if [[ -n "$(task_owned_pids)" ]]; then
    echo "error: post-suite orphan assertion found a task-owned process" >&2
    cleanup_status=1
  fi
  if ((cleanup_status != 0)); then
    exit "${cleanup_status}"
  fi
  exit "${capture_status}"
}
trap cleanup EXIT INT TERM

set +e
(
  cd -- "${workspace}"
  isolated_ctx "$@"
)
capture_status=$?
set -e
exit "${capture_status}"
