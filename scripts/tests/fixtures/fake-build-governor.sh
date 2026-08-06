#!/usr/bin/env bash
set -euo pipefail

mode="${1:-}"
command_name="${2:-}"
shift 2
[[ "${1:-}" == "--" ]] || {
  echo 'fake governor: missing separator' >&2
  exit 2
}
shift
{
  printf 'mode=%s\n' "${mode}"
  printf 'command=%s\n' "${command_name}"
  printf 'jobs=%s\n' "${BAZEL_JOBS:-}"
  printf 'cpu=%s\n' "${BAZEL_LOCAL_CPU_RESOURCES:-}"
  printf 'ram=%s\n' "${BAZEL_LOCAL_RAM_RESOURCES:-}"
  for argument in "$@"; do
    printf 'argv=%s\n' "${argument}"
  done
} >>"${CTX_FAKE_GOVERNOR_LOG:?}"
exec "$@"
