#!/usr/bin/env bash
set -euo pipefail

host_system="$(uname -s)"
host_arch="$(uname -m)"
sysctl_bin="/usr/sbin/sysctl"
ioreg_bin="/usr/sbin/ioreg"
system_profiler_bin="/usr/sbin/system_profiler"
cpuinfo_path="/proc/cpuinfo"
process_maps_path="/proc/$$/maps"
platform_facts_path=""
powershell_bin=""

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
    --ioreg)
      shift
      ioreg_bin="${1:-}"
      ;;
    --system-profiler)
      shift
      system_profiler_bin="${1:-}"
      ;;
    --cpuinfo)
      shift
      cpuinfo_path="${1:-}"
      ;;
    --process-maps)
      shift
      process_maps_path="${1:-}"
      ;;
    --platform-facts)
      shift
      platform_facts_path="${1:-}"
      ;;
    --powershell)
      shift
      powershell_bin="${1:-}"
      ;;
    *)
      echo "unsupported host evidence argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

case "${host_system}" in
  MINGW*|MSYS*|CYGWIN*) host_system=Windows_NT ;;
esac
if [[ "${host_system}" == Windows_NT && "${host_arch}" == x86_64 ]]; then
  host_arch=AMD64
fi
if [[ "${host_system}" == FreeBSD && ! -x "${sysctl_bin}" && -x /sbin/sysctl ]]; then
  sysctl_bin=/sbin/sysctl
fi

host_native_arch="${host_arch}"
process_translated=0
native_arch_probe=uname
hardware_identity=unknown
emulation=unknown
hypervisor=unknown
evidence_complete=0

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
      x86_64:1:|x86_64:1:0)
        host_native_arch=arm64
        process_translated=unknown
        ;;
      x86_64::|x86_64::0|x86_64:0:|x86_64:0:0)
        host_native_arch=x86_64
        process_translated=0
        ;;
    esac
  fi
  platform_identity=""
  hardware_summary=""
  device_summary=""
  hypervisor_probe=""
  if [[ -x "${ioreg_bin}" ]]; then
    platform_identity="$("${ioreg_bin}" -rd1 -c IOPlatformExpertDevice 2>/dev/null || true)"
    device_summary="$("${ioreg_bin}" -r -c IOPCIDevice -l -w0 2>/dev/null || true)"
  fi
  if [[ -x "${system_profiler_bin}" ]]; then
    hardware_summary="$("${system_profiler_bin}" SPHardwareDataType 2>/dev/null || true)"
    device_summary+="$("${system_profiler_bin}" SPDisplaysDataType 2>/dev/null || true)"
  fi
  if [[ -x "${sysctl_bin}" ]]; then
    hypervisor_probe="$("${sysctl_bin}" -in kern.hv_vmm_present 2>/dev/null || true)"
  fi
  if grep -F 'Apple Inc.' <<<"${platform_identity}" >/dev/null && \
    grep -E 'Model (Name|Identifier):' <<<"${hardware_summary}" >/dev/null; then
    hardware_identity=apple
  elif [[ -n "${platform_identity}" && -n "${hardware_summary}" ]]; then
    hardware_identity=generic
  fi
  case "${hypervisor_probe}" in
    0) hypervisor=absent ;;
    1) hypervisor=present ;;
  esac
  if [[ "${process_translated}" == "1" ]]; then
    emulation=rosetta-2
  elif grep -Ei 'qemu|(^|[^[:alnum:]_])(kvm|tcg)([^[:alnum:]_]|$)|vmware[ _-]*svga|virtualbox|parallels|bochs|virtio[-_ ]?(net|blk|gpu)' \
    <<<"${platform_identity}"$'\n'"${hardware_summary}"$'\n'"${device_summary}" >/dev/null; then
    emulation=qemu-kvm
  elif [[ -n "${platform_identity}" && -n "${hardware_summary}" && -n "${device_summary}" ]]; then
    emulation=none
  fi
  if [[ "${host_native_arch}" != unknown && "${process_translated}" != unknown && \
    "${hardware_identity}" != unknown && "${emulation}" != unknown && \
    "${hypervisor}" != unknown ]]; then
    evidence_complete=1
  fi
elif [[ "${host_system}" == "Linux" ]]; then
  cpuinfo=""
  process_maps=""
  platform_facts=""
  [[ -r "${cpuinfo_path}" ]] && cpuinfo="$(head -c 262144 "${cpuinfo_path}" 2>/dev/null || true)"
  [[ -r "${process_maps_path}" ]] && process_maps="$(head -c 262144 "${process_maps_path}" 2>/dev/null || true)"
  if [[ -n "${platform_facts_path}" && -r "${platform_facts_path}" ]]; then
    platform_facts="$(head -c 262144 "${platform_facts_path}" 2>/dev/null || true)"
  else
    for fact in \
      /sys/firmware/devicetree/base/compatible \
      /sys/class/dmi/id/sys_vendor \
      /sys/class/dmi/id/product_name \
      /sys/class/dmi/id/board_vendor \
      /sys/class/dmi/id/board_name \
      /sys/hypervisor/type; do
      if [[ -r "${fact}" ]]; then
        platform_facts+="$(head -c 32768 "${fact}" 2>/dev/null | tr '\000' '\n' || true)"
        platform_facts+=$'\n'
      fi
    done
  fi
  hardware_identity=generic
  emulation=none
  if grep -Ei 'qemu-[^/[:space:]]*|binfmt' <<<"${process_maps}" >/dev/null || \
    { [[ "${host_arch}" == aarch64 ]] && grep -Ei 'qemu|(^|[^[:alnum:]_])tcg([^[:alnum:]_]|$)|linux,dummy-virt|genuineintel|authenticamd|x86[_ -]?64|intel\(r\)|amd ryzen|amd epyc' \
      <<<"${cpuinfo}"$'\n'"${platform_facts}" >/dev/null; }; then
    emulation=qemu-user
  fi
  if grep -Ei '(^|[^[:alnum:]_])hypervisor([^[:alnum:]_]|$)' <<<"${cpuinfo}" >/dev/null || \
    grep -Ei 'qemu|(^|[^[:alnum:]_])kvm([^[:alnum:]_]|$)|vmware|virtualbox|parallels|hyper-v' <<<"${platform_facts}" >/dev/null; then
    hypervisor=present
  else
    hypervisor=absent
  fi
  if [[ -n "${cpuinfo}" && -n "${process_maps}" ]]; then
    if [[ "${host_arch}" != aarch64 ]] || \
      { [[ -n "${platform_facts}" ]] && grep -E '^Features[[:space:]]*:' <<<"${cpuinfo}" >/dev/null; }; then
      evidence_complete=1
    fi
  fi
elif [[ "${host_system}" == FreeBSD ]]; then
  hardware_identity=generic
  emulation=none
  vm_guest="$("${sysctl_bin}" -n kern.vm_guest 2>/dev/null || true)"
  case "${vm_guest}" in
    none) hypervisor=absent ;;
    '') hypervisor=unknown ;;
    *) hypervisor=present ;;
  esac
  [[ -n "${vm_guest}" ]] && evidence_complete=1
elif [[ "${host_system}" == Windows_NT ]]; then
  [[ "${host_arch}" == AMD64 ]] && host_native_arch=X64
  hardware_identity=generic
  emulation=none
  if [[ -z "${powershell_bin}" ]]; then
    powershell_bin="$(command -v powershell.exe 2>/dev/null || true)"
  fi
  windows_hypervisor=""
  if [[ -n "${powershell_bin}" && -x "${powershell_bin}" ]]; then
    windows_hypervisor="$("${powershell_bin}" -NoLogo -NoProfile -NonInteractive -Command \
      "if ((Get-CimInstance Win32_ComputerSystem).HypervisorPresent) {'1'} else {'0'}" \
      2>/dev/null || true)"
  fi
  case "${windows_hypervisor}" in
    0) hypervisor=absent ;;
    1) hypervisor=present ;;
  esac
  if [[ "${host_native_arch}" == X64 && "${hypervisor}" != unknown ]]; then
    evidence_complete=1
  fi
fi

printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
  "${host_system}" "${host_arch}" "${host_native_arch}" \
  "${process_translated}" "${native_arch_probe}" "${hardware_identity}" \
  "${emulation}" "${hypervisor}" "${evidence_complete}"
