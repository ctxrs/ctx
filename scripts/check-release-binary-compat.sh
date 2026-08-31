#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${CTX_LLVM_READOBJ+x}" ]]; then
  echo "error: forbidden public release environment variable: CTX_LLVM_READOBJ" >&2
  exit 1
fi
if [[ -n "${CTX_LLVM_OBJDUMP+x}" ]]; then
  echo "error: forbidden public release environment variable: CTX_LLVM_OBJDUMP" >&2
  exit 1
fi

authoritative_llvm_root() {
  case "$(/usr/bin/uname -s):$(/usr/bin/uname -m)" in
    Linux:*)
      printf '%s\n' /usr/bin
      ;;
    Darwin:arm64|Darwin:aarch64)
      printf '%s\n' /opt/homebrew/opt/llvm/bin
      ;;
    Darwin:x86_64|Darwin:amd64)
      printf '%s\n' /usr/local/opt/llvm/bin
      ;;
    *)
      echo "error: no authoritative LLVM release tool root for this host" >&2
      exit 127
      ;;
  esac
}

# Release construction owns the default package roots. An explicit macOS tool
# pair is accepted only through the packager's validated task-local root;
# caller PATH and environment variables never select an ABI parser.
declared_llvm_readobj="${3:-}"
declared_llvm_objdump="${4:-}"
declared_macos_llvm=0
declared_linux_llvm=0
if [[ -n "${declared_llvm_readobj}" || -n "${declared_llvm_objdump}" ]]; then
  case "${1:-}" in
    windows-x64)
      [[ -n "${declared_llvm_readobj}" && -z "${declared_llvm_objdump}" ]] || {
        echo "error: windows-x64 requires exactly one declared LLVM reader" >&2
        exit 2
      }
      ;;
    macos-x64)
      [[ -n "${declared_llvm_readobj}" && -n "${declared_llvm_objdump}" ]] || {
        echo "error: macos-x64 requires a declared LLVM reader and objdump pair" >&2
        exit 2
      }
      if [[ "$(/usr/bin/uname -s)" == "Linux" \
        && "${declared_llvm_readobj}" == "/usr/bin/llvm-readobj" \
        && "${declared_llvm_objdump}" == "/usr/bin/llvm-objdump" ]]; then
        declared_linux_llvm=1
      else
        declared_macos_llvm=1
      fi
      ;;
    *)
      echo "error: declared LLVM tools are supported only for macos-x64 and windows-x64" >&2
      exit 2
      ;;
  esac
  LLVM_READOBJ="${declared_llvm_readobj}"
  LLVM_OBJDUMP="${declared_llvm_objdump}"
else
  if [[ "${1:-}" == "macos-x64" ]]; then
    echo "error: macos-x64 requires a declared LLVM reader and objdump pair" >&2
    exit 2
  fi
  LLVM_TOOL_ROOT="$(authoritative_llvm_root)"
  LLVM_READOBJ="${LLVM_TOOL_ROOT}/llvm-readobj"
  LLVM_OBJDUMP="${LLVM_TOOL_ROOT}/llvm-objdump"
fi

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-release-binary-compat.sh PLATFORM BINARY [DECLARED_LLVM_READOBJ [DECLARED_LLVM_OBJDUMP]]

Checks the executable format, architecture, loader, shared-library, ABI,
minimum-OS, exploit-mitigation, and stripped-symbol contract for one public
ctx release binary.
Platforms: linux-x64, linux-aarch64, macos-arm64, macos-x64, windows-x64.
USAGE
}

platform="${1:-}"
binary="${2:-}"
if [[ $# -gt 4 || -z "${platform}" || -z "${binary}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi
if [[ ! -f "${binary}" ]]; then
  printf 'release binary missing: %s\n' "${binary}" >&2
  exit 1
fi

case "${platform}" in
  linux-x64|linux-aarch64|macos-arm64|macos-x64|windows-x64) ;;
  *) usage; exit 2 ;;
esac

require_tool() {
  local tool="$1"
  if [[ "${tool}" == */* || "${tool}" == *\\* ]]; then
    [[ -x "${tool}" ]] || {
      printf 'release compatibility tool is not executable: %s\n' "${tool}" >&2
      exit 127
    }
  elif ! command -v "${tool}" >/dev/null 2>&1; then
    printf '%s is required for release compatibility checks\n' "${tool}" >&2
    exit 127
  fi
}

fail() {
  printf 'release binary compatibility failed for %s: %s\n' "${platform}" "$1" >&2
  exit 1
}

macos_llvm_helper="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/release/macos_llvm_authority.py"
macos_llvm_snapshot_root=""

llvm_tool_identity_version() {
  local kind="$1"
  local tool="$2"
  local name version_output version
  name="$(basename "${tool}")"
  case "${kind}:${name}" in
    readobj:llvm-readobj|readobj:llvm-readobj.exe|objdump:llvm-objdump) ;;
    *) fail "declared LLVM ${kind} has the wrong executable identity: ${tool}" ;;
  esac
  version_output="$(run_llvm_tool "${kind}" "${tool}" --version 2>&1)" \
    || fail "LLVM ${kind} identity command failed: ${tool}"
  version="$(sed -nE \
    's/^[[:space:]]*([^[:space:]]+[[:space:]]+)?LLVM version ([0-9]+(\.[0-9]+){1,3})([^0-9].*)?$/\2/p' \
    <<<"${version_output}")"
  [[ "${version}" =~ ^[0-9]+(\.[0-9]+){1,3}$ ]] \
    || fail "LLVM ${kind} did not report one parseable LLVM version: ${tool}"
  printf '%s\n' "${version}"
}

bind_approved_macos_llvm_snapshot() {
  local readobj_suffix="/bin/llvm-readobj"
  [[ "${LLVM_READOBJ}" == /*"${readobj_suffix}" ]] \
    || fail "declared macOS LLVM reader is not in the approved snapshot layout"
  macos_llvm_snapshot_root="${LLVM_READOBJ%${readobj_suffix}}"
  [[ "${LLVM_OBJDUMP}" == "${macos_llvm_snapshot_root}/bin/llvm-objdump" ]] \
    || fail "declared macOS LLVM objdump is not in the approved snapshot layout"
}

run_llvm_tool() {
  local kind="$1"
  local tool="$2"
  shift 2
  if [[ "${declared_macos_llvm}" == "1" ]]; then
    python3 -B "${macos_llvm_helper}" run-verified \
      --snapshot-root "${macos_llvm_snapshot_root}" \
      --tool "${kind}" -- "$@"
  else
    "${tool}" "$@"
  fi
}

if [[ "${declared_macos_llvm}" == "1" ]]; then
  bind_approved_macos_llvm_snapshot
  binary="$(cd "$(dirname "${binary}")" && pwd -P)/$(basename "${binary}")"
fi

version_le() {
  local lhs="$1"
  local rhs="$2"
  [[ "$(printf '%s\n%s\n' "${lhs}" "${rhs}" | sort -V | tail -n 1)" == "${rhs}" ]]
}

max_symbol_version() {
  local prefix="$1"
  printf '%s\n' "${readobj_output}" \
    | { grep -oE "${prefix}_[0-9]+(\.[0-9]+)+" || true; } \
    | sed "s/^${prefix}_//" \
    | sort -Vu \
    | tail -n 1
}

check_symbol_ceiling() {
  local prefix="$1"
  local maximum="$2"
  local required
  required="$(max_symbol_version "${prefix}")"
  if [[ -n "${required}" ]] && ! version_le "${required}" "${maximum}"; then
    fail "requires ${prefix}_${required}, above allowed ${prefix}_${maximum}"
  fi
}

sorted_lines() {
  sed '/^[[:space:]]*$/d' | LC_ALL=C sort -u
}

assert_exact_lines() {
  local label="$1"
  local actual="$2"
  local expected="$3"
  local actual_sorted expected_sorted
  actual_sorted="$(printf '%s\n' "${actual}" | sorted_lines)"
  expected_sorted="$(printf '%s\n' "${expected}" | sorted_lines)"
  if [[ "${actual_sorted}" != "${expected_sorted}" ]]; then
    printf 'expected %s:\n%s\nactual %s:\n%s\n' \
      "${label}" "${expected_sorted}" "${label}" "${actual_sorted}" >&2
    fail "unexpected ${label}"
  fi
}

assert_allowed_required_lines() {
  local label="$1"
  local actual="$2"
  local allowed="$3"
  local required="$4"
  local actual_sorted allowed_sorted required_sorted unexpected missing
  actual_sorted="$(printf '%s\n' "${actual}" | sorted_lines)"
  allowed_sorted="$(printf '%s\n' "${allowed}" | sorted_lines)"
  required_sorted="$(printf '%s\n' "${required}" | sorted_lines)"
  unexpected="$(comm -23 <(printf '%s\n' "${actual_sorted}") <(printf '%s\n' "${allowed_sorted}"))"
  missing="$(comm -23 <(printf '%s\n' "${required_sorted}") <(printf '%s\n' "${actual_sorted}"))"
  if [[ -n "${unexpected}" || -n "${missing}" ]]; then
    [[ -z "${unexpected}" ]] || printf 'unexpected %s:\n%s\n' "${label}" "${unexpected}" >&2
    [[ -z "${missing}" ]] || printf 'missing required %s:\n%s\n' "${label}" "${missing}" >&2
    fail "unapproved ${label}"
  fi
}

elf_needed_libraries() {
  printf '%s\n' "${readobj_output}" | awk '
    /^NeededLibraries \[/ { in_needed=1; next }
    in_needed && /^]/ { in_needed=0; next }
    in_needed {
      value=$0
      sub(/^[[:space:]]+/, "", value)
      sub(/[[:space:]]+$/, "", value)
      if (value != "") print value
    }
  '
}

check_no_elf_search_path() {
  if printf '%s\n' "${readobj_output}" | grep -Eq '(^|[^[:alnum:]_])(RPATH|RUNPATH)([^[:alnum:]_]|$)'; then
    fail "RPATH or RUNPATH is forbidden"
  fi
}

check_stripped_symbols() {
  for inventory in Sections Symbols; do
    if ! awk -v header="${inventory} [" '
      $0 == header {
        opened++
        if (in_inventory) malformed=1
        in_inventory=1
        next
      }
      in_inventory && ($0 == "Sections [" || $0 == "Symbols [") {
        malformed=1
        next
      }
      in_inventory && $0 == "]" { closed=1; in_inventory=0 }
      END {
        exit(opened == 1 && closed && !in_inventory && !malformed ? 0 : 1)
      }
    ' <<<"${readobj_output}"; then
      fail "${inventory} inspection output is missing or truncated"
    fi
  done

  if grep -Eq \
    '^[[:space:]]*Name:[[:space:]]+(\.symtab|\.debug[^[:space:] (]*|\.zdebug[^[:space:] (]*|__debug[^[:space:] (]*)([[:space:] (]|$)' \
    <<<"${readobj_output}"; then
    fail "debug or static symbol section is present"
  fi

  if [[ "${platform}" == macos-* ]]; then
    if awk '
      /^Symbols \[$/ { in_table=1; next }
      in_table && /^\]$/ {
        if (in_symbol) malformed=1
        in_table=0
        next
      }
      in_table && /^[[:space:]]*Symbol[[:space:]]*\{/ {
        if (in_symbol) malformed=1
        in_symbol=1
        external=0
        next
      }
      in_table && in_symbol && /^[[:space:]]*Extern([[:space:]]|$)/ { external=1; next }
      in_table && in_symbol && /^[[:space:]]*\}[[:space:]]*$/ {
        if (!external) local_symbol=1
        in_symbol=0
      }
      END { exit(local_symbol || malformed || in_symbol ? 0 : 1) }
    ' <<<"${readobj_output}"; then
      fail "local symbol table entry is present"
    fi
  elif awk '
    /^Symbols \[$/ { in_table=1; next }
    in_table && /^\]$/ { in_table=0; next }
    in_table && /^[[:space:]]*Symbol[[:space:]]*\{/ { symbol=1 }
    END { exit(symbol ? 0 : 1) }
  ' <<<"${readobj_output}"; then
    fail "static symbol table is present"
  fi
}

elf_interpreter() {
  printf '%s\n' "${readobj_output}" | sed -nE \
    -e 's/.*Requesting program interpreter:[[:space:]]*([^]]+)\].*/\1/p' \
    -e 's/^[[:space:]]*Interpreter:[[:space:]]*([^[:space:]]+).*/\1/p' \
    -e "s/^\[[[:space:]]*[0-9]+\][[:space:]]+(\/[^[:space:]]+).*/\1/p" \
    | head -n 1
}

elf_program_header_block() {
  local wanted_type="$1"
  printf '%s\n' "${readobj_output}" | awk -v wanted_type="${wanted_type}" '
    /^[[:space:]]*ProgramHeader[[:space:]]*\{/ {
      in_header=1
      matches=0
      block=$0 ORS
      next
    }
    in_header {
      block=block $0 ORS
      if ($0 ~ "^[[:space:]]*Type:[[:space:]]*" wanted_type "([^[:alnum:]_]|$)") {
        matches=1
      }
      if ($0 ~ /^[[:space:]]*\}[[:space:]]*$/) {
        if (matches) printf "%s", block
        in_header=0
      }
    }
  '
}

check_elf_hardening() {
  local relro_count stack_count stack_header
  relro_count="$(printf '%s\n' "${readobj_output}" | awk '
    /^[[:space:]]*Type:[[:space:]]*PT_GNU_RELRO([^[:alnum:]_]|$)/ { count++ }
    END { print count + 0 }
  ')"
  [[ "${relro_count}" == "1" ]] \
    || fail "expected exactly one GNU_RELRO program header"

  stack_count="$(printf '%s\n' "${readobj_output}" | awk '
    /^[[:space:]]*Type:[[:space:]]*PT_GNU_STACK([^[:alnum:]_]|$)/ { count++ }
    END { print count + 0 }
  ')"
  [[ "${stack_count}" == "1" ]] \
    || fail "expected exactly one GNU_STACK program header"
  stack_header="$(elf_program_header_block PT_GNU_STACK)"
  grep -Eq '^[[:space:]]*PF_R([^[:alnum:]_]|$)' <<<"${stack_header}" \
    || fail "GNU_STACK is missing read permission"
  grep -Eq '^[[:space:]]*PF_W([^[:alnum:]_]|$)' <<<"${stack_header}" \
    || fail "GNU_STACK is missing write permission"
  if grep -Eq '^[[:space:]]*PF_X([^[:alnum:]_]|$)' <<<"${stack_header}"; then
    fail "GNU_STACK is executable"
  fi

  grep -Eq \
    '^[[:space:]]*0x[0-9A-Fa-f]+[[:space:]]+FLAGS[[:space:]]+.*[[:space:]]BIND_NOW([[:space:]]|$)' \
    <<<"${readobj_output}" || fail "missing BIND_NOW dynamic flag"
  grep -Eq \
    '^[[:space:]]*0x[0-9A-Fa-f]+[[:space:]]+FLAGS_1[[:space:]]+.*[[:space:]]PIE([[:space:]]|$)' \
    <<<"${readobj_output}" || fail "missing PIE dynamic flag"
}

check_linux() {
  local expected_machine expected_interpreter allowed_needed required_needed target_id
  if [[ "${platform}" == "linux-x64" ]]; then
    target_id="linux-x64"
    expected_machine="EM_X86_64"
    expected_interpreter="/lib64/ld-linux-x86-64.so.2"
    # The GLIBC 2.28 sysroot predates the libpthread/libdl merge into libc, so
    # those component DSOs are required. The loader is independently bound by
    # PT_INTERP; x86_64 also emits it in DT_NEEDED. libgcc_s remains an allowed
    # GNU runtime only when its symbol versions satisfy the ceiling below.
    allowed_needed="libc.so.6
ld-linux-x86-64.so.2
libdl.so.2
libgcc_s.so.1
libm.so.6
libpthread.so.0"
    required_needed="libc.so.6
ld-linux-x86-64.so.2
libdl.so.2
libm.so.6
libpthread.so.0"
  else
    target_id="linux-arm64"
    expected_machine="EM_AARCH64"
    expected_interpreter="/lib/ld-linux-aarch64.so.1"
    allowed_needed="libc.so.6
ld-linux-aarch64.so.1
libdl.so.2
libgcc_s.so.1
libm.so.6
libpthread.so.0"
    required_needed="libc.so.6
libdl.so.2
libm.so.6
libpthread.so.0"
  fi

  local CTX_PUBLIC_TARGET_GLIBC_MAX
  eval "$(python3 "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/public-cli-release-targets.py" shell "${target_id}")" \
    || fail "release target matrix is unavailable for ${platform}"
  [[ "${CTX_PUBLIC_TARGET_GLIBC_MAX}" =~ ^[0-9]+\.[0-9]+$ ]] \
    || fail "release target matrix has no valid GLIBC ceiling for ${platform}"

  grep -Eq 'Format:[[:space:]]+ELF64-|Class:[[:space:]]+(ELFCLASS64|64-bit)' <<<"${readobj_output}" \
    || fail "expected ELF64"
  grep -Eq 'DataEncoding:[[:space:]]+(LittleEndian|LittleEndianHex|2.s complement, little endian)' <<<"${readobj_output}" \
    || fail "expected little-endian ELF"
  grep -Eq 'Type:[[:space:]]+SharedObject([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected a position-independent ELF executable"
  grep -Eq "Machine:[[:space:]]+${expected_machine}([^[:alnum:]_]|$)" <<<"${readobj_output}" \
    || fail "expected ${expected_machine}"
  check_elf_hardening

  local interpreter
  interpreter="$(elf_interpreter)"
  [[ "${interpreter}" == "${expected_interpreter}" ]] \
    || fail "expected interpreter ${expected_interpreter}, got ${interpreter:-none}"
  assert_allowed_required_lines "DT_NEEDED libraries" "$(elf_needed_libraries)" \
    "${allowed_needed}" "${required_needed}"
  check_no_elf_search_path

  [[ -n "$(max_symbol_version GLIBC)" ]] || fail "no GLIBC requirement found"
  check_symbol_ceiling GLIBC "${CTX_PUBLIC_TARGET_GLIBC_MAX}"
  check_symbol_ceiling GCC 4.2.0
  if [[ "${platform}" == "linux-x64" ]]; then
    if grep -Eq 'GLIBCXX_[0-9]|CXXABI_[0-9]' <<<"${readobj_output}"; then
      fail "GLIBCXX and CXXABI requirements are forbidden on Linux x64"
    fi
    if grep -Eqi 'x86-64-v[234]|x86-64-v1[^[:alnum:]].*(x86-64-v[234])|ISA_1_(NEEDED|USED).*(AVX|AVX2|AVX512|SSE3|SSE4)' <<<"${readobj_output}"; then
      fail "advertises an x86 ISA requirement above x86-64-v1"
    fi
  elif grep -Eq 'GLIBCXX_[0-9]|CXXABI_[0-9]' <<<"${readobj_output}"; then
    fail "GLIBCXX and CXXABI requirements are forbidden on Linux ARM64"
  fi
}

macho_dylibs() {
  printf '%s\n' "${objdump_output}" | awk '
    /^[[:space:]]*cmd LC_(LOAD|LOAD_WEAK|REEXPORT|LAZY_LOAD|LOAD_UPWARD)_DYLIB/ { want_name=1; next }
    want_name && /^[[:space:]]*name / {
      value=$0
      sub(/^[[:space:]]*name[[:space:]]+/, "", value)
      sub(/[[:space:]]+\(offset .*/, "", value)
      print value
      want_name=0
    }
  '
}

macho_min_version() {
  printf '%s\n' "${objdump_output}" | awk '
    /^[[:space:]]*cmd LC_BUILD_VERSION/ { build=1; next }
    build && /^[[:space:]]*minos / { print $2; exit }
    /^[[:space:]]*cmd LC_VERSION_MIN_MACOSX/ { legacy=1; next }
    legacy && /^[[:space:]]*version / { print $2; exit }
  '
}

check_macos() {
  local expected_format expected_arch
  if [[ "${platform}" == "macos-arm64" ]]; then
    expected_format="Mach-O arm64"
    expected_arch="(aarch64|arm64)"
  else
    expected_format="Mach-O 64-bit x86-64"
    expected_arch="x86_64"
  fi
  grep -Fq "Format: ${expected_format}" <<<"${readobj_output}" \
    || fail "expected ${expected_format}"
  grep -Eq "Arch:[[:space:]]+${expected_arch}([^[:alnum:]_]|$)" <<<"${readobj_output}" \
    || fail "expected ${expected_arch} Mach-O architecture"
  grep -Eq 'FileType:[[:space:]]+Executable([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected a Mach-O executable"
  if grep -Eq '^[[:space:]]*cmd LC_RPATH([[:space:]]|$)' <<<"${objdump_output}"; then
    fail "LC_RPATH is forbidden"
  fi
  local minimum
  minimum="$(macho_min_version)"
  [[ -n "${minimum}" ]] || fail "missing macOS minimum version load command"
  version_le "${minimum}" 13.0 || fail "minimum macOS ${minimum} is newer than 13.0"
  # Core ML support, the locked platform TLS verifier, and the persistent
  # daemon's FSEvents watcher are compiled into both macOS artifacts.
  # CoreServices.framework and Security.framework are their exact native API
  # dependencies. The C++ runtime is retained when a native linker records it,
  # but Zig can correctly omit that unused load command after linking the same
  # static esaxx object. Keep every other system-library entry exact so an
  # accidental third-party dylib still fails.
  local expected_macos_dylibs expected_macos_dylibs_with_cxx actual_macos_dylibs
  expected_macos_dylibs="/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation
/System/Library/Frameworks/CoreGraphics.framework/Versions/A/CoreGraphics
/System/Library/Frameworks/CoreML.framework/Versions/A/CoreML
/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices
/System/Library/Frameworks/CoreVideo.framework/Versions/A/CoreVideo
/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation
/System/Library/Frameworks/ImageIO.framework/Versions/A/ImageIO
/System/Library/Frameworks/Metal.framework/Versions/A/Metal
/System/Library/Frameworks/Security.framework/Versions/A/Security
/usr/lib/libSystem.B.dylib
/usr/lib/libcharset.1.dylib
/usr/lib/libiconv.2.dylib
/usr/lib/libobjc.A.dylib"
  expected_macos_dylibs_with_cxx="${expected_macos_dylibs}
/usr/lib/libc++.1.dylib"
  actual_macos_dylibs="$(macho_dylibs)"
  actual_macos_dylibs="$(printf '%s\n' "${actual_macos_dylibs}" | sorted_lines)"
  if [[ "${actual_macos_dylibs}" != "$(printf '%s\n' "${expected_macos_dylibs}" | sorted_lines)" \
    && "${actual_macos_dylibs}" != "$(printf '%s\n' "${expected_macos_dylibs_with_cxx}" | sorted_lines)" ]]; then
    printf 'expected Mach-O dylibs (with optional libc++):\n%s\nactual Mach-O dylibs:\n%s\n' \
      "${expected_macos_dylibs_with_cxx}" "${actual_macos_dylibs}" >&2
    fail "unexpected Mach-O dylibs"
  fi
}

pe_imports() {
  printf '%s\n' "${readobj_output}" | awk '
    /^Import \{/ { in_import=1; next }
    in_import && /^}/ { in_import=0; next }
    in_import && /^[[:space:]]*Name:/ {
      value=$0
      sub(/^[[:space:]]*Name:[[:space:]]*/, "", value)
      print tolower(value)
    }
  '
}

pe_header_version() {
  local major_field="$1"
  local minor_field="$2"
  local major minor
  major="$(printf '%s\n' "${readobj_output}" | sed -nE "s/^[[:space:]]*${major_field}:[[:space:]]*([0-9]+).*/\\1/p" | head -n 1)"
  minor="$(printf '%s\n' "${readobj_output}" | sed -nE "s/^[[:space:]]*${minor_field}:[[:space:]]*([0-9]+).*/\\1/p" | head -n 1)"
  [[ -n "${major}" && -n "${minor}" ]] || return 1
  printf '%s.%s\n' "${major}" "${minor}"
}

check_windows() {
  grep -Eq 'Format:[[:space:]]+(COFF-x86-64|PE32\+)' <<<"${readobj_output}" \
    || fail "expected PE32+ x86-64"
  grep -Eq 'Machine:[[:space:]]+IMAGE_FILE_MACHINE_AMD64([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected IMAGE_FILE_MACHINE_AMD64"
  grep -Eq 'Magic:[[:space:]]+(PE32\+|0x20B)' <<<"${readobj_output}" \
    || fail "expected PE32+ optional header"
  grep -Eq 'IMAGE_FILE_EXECUTABLE_IMAGE([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected a PE executable image"
  for mitigation in \
    IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE \
    IMAGE_DLL_CHARACTERISTICS_NX_COMPAT \
    IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA; do
    grep -Eq "^[[:space:]]*${mitigation}([^[:alnum:]_]|$)" <<<"${readobj_output}" \
      || fail "missing PE mitigation ${mitigation}"
  done
  grep -Eq 'Subsystem:[[:space:]]+IMAGE_SUBSYSTEM_WINDOWS_CUI([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected Windows console subsystem"

  local os_version subsystem_version
  os_version="$(pe_header_version MajorOperatingSystemVersion MinorOperatingSystemVersion)" \
    || fail "missing Windows header OS version"
  subsystem_version="$(pe_header_version MajorSubsystemVersion MinorSubsystemVersion)" \
    || fail "missing Windows subsystem version"
  version_le "${os_version}" 10.0 || fail "Windows header OS version ${os_version} is newer than 10.0"
  version_le "${subsystem_version}" 10.0 || fail "Windows subsystem version ${subsystem_version} is newer than 10.0"

  assert_exact_lines "PE imported DLLs" "$(pe_imports)" "advapi32.dll
api-ms-win-crt-environment-l1-1-0.dll
api-ms-win-crt-heap-l1-1-0.dll
api-ms-win-crt-math-l1-1-0.dll
api-ms-win-crt-private-l1-1-0.dll
api-ms-win-crt-runtime-l1-1-0.dll
api-ms-win-crt-stdio-l1-1-0.dll
api-ms-win-crt-string-l1-1-0.dll
api-ms-win-crt-time-l1-1-0.dll
api-ms-win-crt-utility-l1-1-0.dll
api-ms-win-core-synch-l1-2-0.dll
bcrypt.dll
bcryptprimitives.dll
kernel32.dll
ntdll.dll
ole32.dll
psapi.dll
rstrtmgr.dll
shell32.dll
userenv.dll
ws2_32.dll"
}

if [[ "${declared_macos_llvm}" != "1" ]]; then
  require_tool "${LLVM_READOBJ}"
fi
llvm_readobj_version=""
if [[ "${declared_macos_llvm}" == "1" ]]; then
  llvm_readobj_version="$(llvm_tool_identity_version readobj "${LLVM_READOBJ}")"
  [[ "${llvm_readobj_version}" == "22.1.8" ]] \
    || fail "approved macOS LLVM reader did not report version 22.1.8"
fi
case "${platform}" in
  linux-x64|linux-aarch64)
    readobj_output="$(run_llvm_tool readobj "${LLVM_READOBJ}" \
      --file-headers \
      --program-headers \
      --dynamic-table \
      --needed-libs \
      --version-info \
      --notes \
      --string-dump=.interp \
      --sections \
      --symbols \
      "${binary}")" || fail "llvm-readobj could not inspect the binary"
    ;;
  macos-arm64|macos-x64)
    readobj_output="$(run_llvm_tool readobj "${LLVM_READOBJ}" \
      --file-headers --sections --symbols "${binary}")" \
      || fail "llvm-readobj could not inspect the binary"
    ;;
  windows-x64)
    readobj_output="$(run_llvm_tool readobj "${LLVM_READOBJ}" \
      --file-headers --coff-imports --sections --symbols "${binary}")" \
      || fail "llvm-readobj could not inspect the binary"
    ;;
esac

check_stripped_symbols

objdump_output=""
llvm_objdump_version=""
if [[ "${platform}" == macos-* ]]; then
  if [[ "${declared_macos_llvm}" != "1" ]]; then
    require_tool "${LLVM_OBJDUMP}"
  fi
  if [[ "${declared_macos_llvm}" == "1" ]]; then
    llvm_objdump_version="$(llvm_tool_identity_version objdump "${LLVM_OBJDUMP}")"
    [[ "${llvm_objdump_version}" == "22.1.8" ]] \
      || fail "approved macOS LLVM objdump did not report version 22.1.8"
  fi
  objdump_output="$(run_llvm_tool objdump "${LLVM_OBJDUMP}" \
    --macho --private-headers "${binary}")" \
    || fail "llvm-objdump could not inspect Mach-O load commands"
fi

case "${platform}" in
  linux-x64|linux-aarch64) check_linux ;;
  macos-arm64|macos-x64) check_macos ;;
  windows-x64) check_windows ;;
esac

scanner_authority="authoritative-package-root:${LLVM_TOOL_ROOT:-declared-release-runfile}"
if [[ "${declared_macos_llvm}" == "1" ]]; then
  scanner_authority="approved-task-snapshot:homebrew-core/llvm-22.1.8-sonoma-x86_64@sha256:2f07536754d0854565f9ac37436681bb3d04a4fbb15c45c51896933262df5e48"
elif [[ "${declared_linux_llvm}" == "1" ]]; then
  scanner_authority="authoritative-package-root:/usr/bin"
fi
scanner_inputs="llvm-readobj=${LLVM_READOBJ}"
if [[ -n "${LLVM_OBJDUMP}" ]]; then
  scanner_inputs="${scanner_inputs},llvm-objdump=${LLVM_OBJDUMP}"
fi
printf 'release binary compatibility ok: %s %s scanner-inputs=%s scanner-authority=%s\n' \
  "${platform}" "${binary}" "${scanner_inputs}" "${scanner_authority}"
