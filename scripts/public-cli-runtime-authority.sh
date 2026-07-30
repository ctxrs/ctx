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
runner_id="${11:-}"
argument_count=$#
os_identity="${12:-unknown}"
os_version="${13:-unknown}"
os_product_type="${14:-unknown}"
pinned_macos_x64_kvm_runner="ctx-mac-gui-shared-x64"

os_baseline_matches() {
  local windows_build=""

  case "${platform}" in
    linux-x64|linux-aarch64)
      [[ "${os_identity}" == "ubuntu" && "${os_version}" == "22.04" ]]
      ;;
    freebsd-x64)
      [[ "${os_identity}" == "freebsd" \
        && "${os_version}" =~ ^14\.4-RELEASE(-p[0-9]+)?$ ]]
      ;;
    windows-x64)
      if [[ ! "${os_version}" =~ ^10\.0\.([0-9]+)(\.[0-9]+)?$ ]]; then
        return 1
      fi
      windows_build="${BASH_REMATCH[1]}"
      [[ "${os_identity}" =~ ^Microsoft[[:space:]]+Windows[[:space:]]+11([[:space:]]|$) \
        && "${os_product_type}" == "1" ]] \
        && ((10#${windows_build} >= 22000))
      ;;
    *)
      return 0
      ;;
  esac
}

case "${runtime_status}" in
  not_run) printf 'not_run\n' ;;
  passed)
    if [[ "${platform}" == linux-* || "${platform}" == "freebsd-x64" \
      || "${platform}" == "windows-x64" ]]; then
      if ((argument_count < 14)); then
        script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
        if [[ -x "${script_dir}/public-cli-host-runtime-evidence.sh" ]]; then
          IFS=$'\t' read -r os_identity os_version os_product_type \
            < <(
              "${script_dir}/public-cli-host-runtime-evidence.sh" \
                --host-system "${host_system}" \
                --host-arch "${host_arch}" \
                --os-baseline-only
            )
        fi
      fi
      if ! os_baseline_matches; then
        printf 'non_authoritative\n'
        exit 0
      fi
    fi
    case "${hardware_identity}:${emulation}:${hypervisor}:${evidence_complete}" in
      apple:none:absent:1|generic:none:absent:1|generic:none:present:1|generic:none:unknown:1) ;;
      generic:qemu-kvm:present:1)
        if [[ "${platform}" != "macos-x64" || "${runner_id}" != "${pinned_macos_x64_kvm_runner}" ]]; then
          printf 'non_authoritative\n'
          exit 0
        fi
        ;;
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
    if [[ "${platform}" == "macos-arm64" ]] && \
      [[ "${hardware_identity}:${emulation}:${hypervisor}" != "apple:none:absent" ]]; then
      printf 'non_authoritative\n'
      exit 0
    fi
    if [[ "${platform}" == "macos-x64" ]] && \
      [[ "${hardware_identity}:${emulation}:${hypervisor}" != "apple:none:absent" ]] && \
      [[ "${hardware_identity}:${emulation}:${hypervisor}:${runner_id}" != \
        "generic:qemu-kvm:present:${pinned_macos_x64_kvm_runner}" ]]; then
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
