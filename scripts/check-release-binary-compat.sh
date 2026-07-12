#!/usr/bin/env bash
set -euo pipefail

# The release image owns the LLVM version. Tests inject small stand-in tools by
# path so parser failures can be exercised without manufacturing six binaries.
LLVM_READOBJ="${CTX_LLVM_READOBJ:-llvm-readobj}"
LLVM_OBJDUMP="${CTX_LLVM_OBJDUMP:-llvm-objdump}"

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/check-release-binary-compat.sh PLATFORM BINARY

Checks the executable format, architecture, loader, shared-library, ABI, and
minimum-OS contract for one public ctx release binary.
Platforms: linux-x64, linux-aarch64, macos-arm64, macos-x64, windows-x64,
freebsd-x64.
USAGE
}

platform="${1:-}"
binary="${2:-}"
if [[ -z "${platform}" || -z "${binary}" || "${platform}" == "-h" || "${platform}" == "--help" ]]; then
  usage
  exit 2
fi
if [[ ! -f "${binary}" ]]; then
  printf 'release binary missing: %s\n' "${binary}" >&2
  exit 1
fi

case "${platform}" in
  linux-x64|linux-aarch64|macos-arm64|macos-x64|windows-x64|freebsd-x64) ;;
  *) usage; exit 2 ;;
esac

require_tool() {
  local tool="$1"
  if [[ "${tool}" == */* ]]; then
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

elf_interpreter() {
  printf '%s\n' "${readobj_output}" | sed -nE \
    -e 's/.*Requesting program interpreter:[[:space:]]*([^]]+)\].*/\1/p' \
    -e 's/^[[:space:]]*Interpreter:[[:space:]]*([^[:space:]]+).*/\1/p' \
    -e "s/^\[[[:space:]]*[0-9]+\][[:space:]]+(\/[^[:space:]]+).*/\1/p" \
    | head -n 1
}

check_linux() {
  local expected_machine expected_interpreter expected_needed
  if [[ "${platform}" == "linux-x64" ]]; then
    expected_machine="EM_X86_64"
    expected_interpreter="/lib64/ld-linux-x86-64.so.2"
    # The reviewed lexical-only x64 artifact names the loader in DT_NEEDED as
    # well as PT_INTERP; keep that fact explicit instead of normalizing it away.
    expected_needed="ld-linux-x86-64.so.2
libc.so.6
libgcc_s.so.1
libm.so.6"
  else
    expected_machine="EM_AARCH64"
    expected_interpreter="/lib/ld-linux-aarch64.so.1"
    expected_needed="ld-linux-aarch64.so.1
libc.so.6
libgcc_s.so.1
libm.so.6"
  fi

  grep -Eq 'Format:[[:space:]]+ELF64-|Class:[[:space:]]+(ELFCLASS64|64-bit)' <<<"${readobj_output}" \
    || fail "expected ELF64"
  grep -Eq 'DataEncoding:[[:space:]]+(LittleEndian|LittleEndianHex|2.s complement, little endian)' <<<"${readobj_output}" \
    || fail "expected little-endian ELF"
  grep -Eq 'Type:[[:space:]]+SharedObject([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected a position-independent ELF executable"
  grep -Eq "Machine:[[:space:]]+${expected_machine}([^[:alnum:]_]|$)" <<<"${readobj_output}" \
    || fail "expected ${expected_machine}"

  local interpreter
  interpreter="$(elf_interpreter)"
  [[ "${interpreter}" == "${expected_interpreter}" ]] \
    || fail "expected interpreter ${expected_interpreter}, got ${interpreter:-none}"
  assert_exact_lines "DT_NEEDED libraries" "$(elf_needed_libraries)" "${expected_needed}"
  check_no_elf_search_path

  [[ -n "$(max_symbol_version GLIBC)" ]] || fail "no GLIBC requirement found"
  check_symbol_ceiling GLIBC 2.35
  check_symbol_ceiling GCC 4.2.0
  if [[ "${platform}" == "linux-x64" ]]; then
    if grep -Eq 'GLIBCXX_[0-9]|CXXABI_[0-9]' <<<"${readobj_output}"; then
      fail "GLIBCXX and CXXABI requirements are forbidden on lexical-only Linux x64"
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
  # Core ML support is compiled into both macOS artifacts. Keep the complete
  # reviewed system-library set exact so an accidental third-party dylib or
  # new framework dependency still fails release construction.
  assert_exact_lines "Mach-O dylibs" "$(macho_dylibs)" "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation
/System/Library/Frameworks/CoreGraphics.framework/Versions/A/CoreGraphics
/System/Library/Frameworks/CoreML.framework/Versions/A/CoreML
/System/Library/Frameworks/CoreVideo.framework/Versions/A/CoreVideo
/System/Library/Frameworks/Foundation.framework/Versions/C/Foundation
/System/Library/Frameworks/ImageIO.framework/Versions/A/ImageIO
/System/Library/Frameworks/Metal.framework/Versions/A/Metal
/usr/lib/libSystem.B.dylib
/usr/lib/libc++.1.dylib
/usr/lib/libiconv.2.dylib
/usr/lib/libobjc.A.dylib"
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
api-ms-win-core-synch-l1-2-0.dll
bcrypt.dll
bcryptprimitives.dll
kernel32.dll
msvcrt.dll
ntdll.dll
ole32.dll
shell32.dll
userenv.dll
ws2_32.dll"
}

check_freebsd() {
  grep -Eq 'Format:[[:space:]]+ELF64-|Class:[[:space:]]+(ELFCLASS64|64-bit)' <<<"${readobj_output}" \
    || fail "expected ELF64"
  grep -Eq 'DataEncoding:[[:space:]]+(LittleEndian|LittleEndianHex|2.s complement, little endian)' <<<"${readobj_output}" \
    || fail "expected little-endian ELF"
  grep -Eq 'Type:[[:space:]]+SharedObject([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected a position-independent FreeBSD executable"
  grep -Eq 'Machine:[[:space:]]+EM_X86_64([^[:alnum:]_]|$)' <<<"${readobj_output}" \
    || fail "expected EM_X86_64"
  grep -Eq 'OS/ABI:[[:space:]]+(FreeBSD|UNIX - FreeBSD)' <<<"${readobj_output}" \
    || fail "expected FreeBSD ELF ABI"
  assert_exact_lines "DT_NEEDED libraries" "$(elf_needed_libraries)" "libc.so.7
libgcc_s.so.1
libm.so.5
libthr.so.3"
  check_no_elf_search_path
}

require_tool "${LLVM_READOBJ}"
case "${platform}" in
  linux-x64|linux-aarch64|freebsd-x64)
    readobj_output="$("${LLVM_READOBJ}" \
      --file-headers \
      --program-headers \
      --dynamic-table \
      --needed-libs \
      --version-info \
      --notes \
      --string-dump=.interp \
      "${binary}")" || fail "llvm-readobj could not inspect the binary"
    ;;
  macos-arm64|macos-x64)
    readobj_output="$("${LLVM_READOBJ}" --file-headers "${binary}")" \
      || fail "llvm-readobj could not inspect the binary"
    ;;
  windows-x64)
    readobj_output="$("${LLVM_READOBJ}" --file-headers --coff-imports "${binary}")" \
      || fail "llvm-readobj could not inspect the binary"
    ;;
esac

objdump_output=""
if [[ "${platform}" == macos-* ]]; then
  require_tool "${LLVM_OBJDUMP}"
  objdump_output="$("${LLVM_OBJDUMP}" --macho --private-headers "${binary}")" \
    || fail "llvm-objdump could not inspect Mach-O load commands"
fi

case "${platform}" in
  linux-x64|linux-aarch64) check_linux ;;
  macos-arm64|macos-x64) check_macos ;;
  windows-x64) check_windows ;;
  freebsd-x64) check_freebsd ;;
esac

printf 'release binary compatibility ok: %s %s\n' "${platform}" "${binary}"
