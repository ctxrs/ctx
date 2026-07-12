#!/usr/bin/env bash
set -euo pipefail

host_system="$(uname -s)"
host_arch="$(uname -m)"
sysctl_bin="/usr/sbin/sysctl"

while (($# > 0)); do
  case "$1" in
    --host-system)
      shift
      host_system="${1:-}"
      ;;
    --host-arch)
      shift
      host_arch="${1:-}"
      ;;
    --sysctl)
      shift
      sysctl_bin="${1:-}"
      ;;
    *)
      echo "unsupported host evidence argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

host_native_arch="${host_arch}"
process_translated=0
native_arch_probe=uname

if [[ "${host_system}" == "Darwin" ]]; then
  host_native_arch=unknown
  process_translated=unknown
  native_arch_probe=sysctl
  if [[ -x "${sysctl_bin}" ]]; then
    translated_probe="$("${sysctl_bin}" -in sysctl.proc_translated 2>/dev/null || true)"
    arm64_probe="$("${sysctl_bin}" -in hw.optional.arm64 2>/dev/null || true)"
    case "${host_arch}:${arm64_probe}:${translated_probe}" in
      arm64:1:|arm64:1:0)
        host_native_arch=arm64
        process_translated=0
        ;;
      x86_64:1:1)
        host_native_arch=arm64
        process_translated=1
        ;;
      x86_64:0:|x86_64:0:0)
        host_native_arch=x86_64
        process_translated=0
        ;;
    esac
  fi
fi

printf '%s\t%s\t%s\t%s\t%s\n' \
  "${host_system}" "${host_arch}" "${host_native_arch}" \
  "${process_translated}" "${native_arch_probe}"
