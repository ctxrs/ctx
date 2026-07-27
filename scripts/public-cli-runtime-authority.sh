#!/usr/bin/env bash
set -euo pipefail

platform="${1:-}"
host_system="${2:-}"
host_arch="${3:-}"
runtime_status="${4:-}"
host_native_arch="${5:-unknown}"
process_translated="${6:-unknown}"
hardware_identity="${7:-unknown}"
emulation="${8:-unknown}"
hypervisor="${9:-unknown}"
evidence_complete="${10:-0}"

case "${runtime_status}" in
  not_run) printf 'not_run\n' ;;
  passed)
    case "${hardware_identity}:${emulation}:${hypervisor}:${evidence_complete}" in
      apple:none:absent:1|generic:none:absent:1|generic:none:present:1|generic:none:unknown:1) ;;
      apple:rosetta-2:absent:1|apple:qemu-kvm:*:1|generic:qemu-user:*:1|*:unknown:*:*|*:*:unknown:0|*:*:*:0)
        printf 'non_authoritative\n'
        exit 0
        ;;
      *)
        printf 'non_authoritative\n'
        exit 0
        ;;
    esac
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
    if [[ "${platform}" == macos-* ]] && \
      [[ "${hardware_identity}:${emulation}:${hypervisor}" != "apple:none:absent" ]]; then
      printf 'non_authoritative\n'
      exit 0
    fi
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
