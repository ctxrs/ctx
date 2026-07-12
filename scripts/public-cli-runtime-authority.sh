#!/usr/bin/env bash
set -euo pipefail

platform="${1:-}"
host_system="${2:-}"
host_arch="${3:-}"
runtime_status="${4:-}"
host_native_arch="${5:-unknown}"
process_translated="${6:-unknown}"

case "${runtime_status}" in
  not_run) printf 'not_run\n' ;;
  passed)
    case "${process_translated}" in
      0) ;;
      1)
        printf 'non_authoritative\n'
        exit 0
        ;;
      unknown)
        printf 'non_authoritative\n'
        exit 0
        ;;
      *)
        echo "process translation status must be 0, 1, or unknown" >&2
        exit 2
        ;;
    esac
    case "${platform}:${host_system}:${host_arch}:${host_native_arch}" in
      linux-x64:Linux:x86_64:x86_64|\
      linux-aarch64:Linux:aarch64:aarch64|\
      macos-arm64:Darwin:arm64:arm64|\
      macos-x64:Darwin:x86_64:x86_64|\
      windows-x64:Windows_NT:AMD64:X64|\
      freebsd-x64:FreeBSD:amd64:amd64)
        printf 'authoritative\n'
        ;;
      *)
        printf 'non_authoritative\n'
        ;;
    esac
    ;;
  *)
    echo "runtime status must be passed or not_run" >&2
    exit 2
    ;;
esac
