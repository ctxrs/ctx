#!/usr/bin/env bash
set -euo pipefail

case "${1:-}" in
  -Pk)
    printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
    printf 'fixture 10000000 1 %s 1%% %s\n' \
      "${CTX_FAKE_DF_FREE_KIB:-9000000}" \
      "${2:-/fixture}"
    ;;
  -Pi)
    printf 'Filesystem Inodes IUsed IFree IUse%% Mounted on\n'
    printf 'fixture 500000 1 %s 1%% %s\n' \
      "${CTX_FAKE_DF_FREE_INODES:-400000}" \
      "${2:-/fixture}"
    ;;
  *)
    printf 'unsupported fake df arguments: %s\n' "$*" >&2
    exit 2
    ;;
esac
