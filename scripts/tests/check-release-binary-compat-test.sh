#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
checker="${repo_root}/scripts/check-release-binary-compat.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-binary-compat-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

for symbol_setting in \
  'build:release --strip=never' \
  'build:release --@rules_rust//rust/settings:extra_rustc_flag=-Cdebuginfo=1'; do
  if [[ "$(grep -Fxc "${symbol_setting}" "${repo_root}/.bazelrc")" != 1 ]]; then
    printf 'Bazel release configuration must contain exactly: %s\n' \
      "${symbol_setting}" >&2
    exit 1
  fi
done
grep -Fq 'detached-debug-symbols.py" prepare' \
  "${repo_root}/scripts/package-public-cli-bazel-release.sh" || {
  echo "release packaging does not extract and strip detached symbols" >&2
  exit 1
}
macos_deployment_setting='build:release --action_env=MACOSX_DEPLOYMENT_TARGET=13.0'
if [[ "$(grep -Fxc "${macos_deployment_setting}" "${repo_root}/.bazelrc")" != 1 ]]; then
  printf 'Bazel release configuration must contain exactly: %s\n' \
    "${macos_deployment_setting}" >&2
  exit 1
fi
macos_x64_triple='        "x86_64-apple-darwin",'
if [[ "$(grep -Fxc "${macos_x64_triple}" "${repo_root}/MODULE.bazel")" != 1 ]]; then
  printf 'crate-universe macOS x64 target is missing or duplicated\n' >&2
  exit 1
fi

cat > "${tmp}/llvm-readobj" <<'EOF'
#!/bin/sh
case " $* " in *' --sections '*) ;; *) exit 64 ;; esac
case " $* " in *' --symbols '*) ;; *) exit 64 ;; esac
cat "$FAKE_READOBJ_OUTPUT"
EOF
printf '#!/bin/sh\ncat "$FAKE_OBJDUMP_OUTPUT"\n' > "${tmp}/llvm-objdump"
chmod +x "${tmp}/llvm-readobj" "${tmp}/llvm-objdump"
printf 'not a real binary\n' > "${tmp}/candidate"
: > "${tmp}/empty"

# Parser fixtures execute only through this disposable checker copy. The
# production checker retains its fixed package-root resolution and has no
# caller-selected tool path.
fixture_checker="${tmp}/check-release-binary-compat-fixture.sh"
sed \
  "s#^  LLVM_TOOL_ROOT=.*#  LLVM_TOOL_ROOT=\"${tmp}\"#" \
  "${checker}" > "${fixture_checker}"
chmod 700 "${fixture_checker}"

run_check() {
  local platform="$1"
  local readobj="$2"
  local objdump="${3:-${tmp}/empty}"
  FAKE_READOBJ_OUTPUT="${readobj}" \
    FAKE_OBJDUMP_OUTPUT="${objdump}" \
    "${fixture_checker}" "${platform}" "${tmp}/candidate"
}

run_declared_windows_check() {
  local readobj="$1"
  FAKE_READOBJ_OUTPUT="${readobj}" \
    "${checker}" windows-x64 "${tmp}/candidate" "${tmp}/llvm-readobj"
}

expect_pass() {
  local name="$1"
  shift
  if ! "$@" >"${tmp}/${name}.out" 2>"${tmp}/${name}.err"; then
    printf 'expected pass: %s\n' "${name}" >&2
    cat "${tmp}/${name}.err" >&2
    exit 1
  fi
}

expect_fail() {
  local name="$1"
  shift
  if "$@" >"${tmp}/${name}.out" 2>"${tmp}/${name}.err"; then
    printf 'expected failure: %s\n' "${name}" >&2
    exit 1
  fi
  grep -Fq 'release binary compatibility failed' "${tmp}/${name}.err" || {
    printf 'failure was not fail-closed: %s\n' "${name}" >&2
    cat "${tmp}/${name}.err" >&2
    exit 1
  }
}

for override in CTX_LLVM_READOBJ CTX_LLVM_OBJDUMP; do
  if env "${override}=${tmp}/forged-tool" \
    "${checker}" linux-x64 "${tmp}/candidate" \
    >"${tmp}/${override}.out" 2>"${tmp}/${override}.err"; then
    printf 'production compatibility checker accepted %s\n' "${override}" >&2
    exit 1
  fi
  grep -Fq \
    "forbidden public release environment variable: ${override}" \
    "${tmp}/${override}.err"
done

mkdir "${tmp}/forged-path"
cat > "${tmp}/forged-path/llvm-readobj" <<'EOF'
#!/bin/sh
touch "$FORGED_TOOL_MARKER"
exit 0
EOF
printf '#!/bin/sh\ntouch "$FORGED_TOOL_MARKER"\nexit 0\n' \
  > "${tmp}/forged-path/llvm-objdump"
chmod +x "${tmp}/forged-path/llvm-readobj" "${tmp}/forged-path/llvm-objdump"
if PATH="${tmp}/forged-path:${PATH}" \
  FORGED_TOOL_MARKER="${tmp}/forged-tool-ran" \
  "${checker}" linux-x64 "${tmp}/candidate" \
  >"${tmp}/forged-path.out" 2>"${tmp}/forged-path.err"; then
  echo "production compatibility checker accepted a forged PATH tool" >&2
  exit 1
fi
if [[ -e "${tmp}/forged-tool-ran" ]]; then
  echo "production compatibility checker executed a caller-PATH LLVM tool" >&2
  exit 1
fi

linux_x64="${tmp}/linux-x64.txt"
cat > "${linux_x64}" <<'EOF'
Format: elf64-x86-64
Arch: x86_64
Class: 64-bit
DataEncoding: LittleEndian
Type: SharedObject (0x3)
Machine: EM_X86_64 (0x3E)
Interpreter: /lib64/ld-linux-x86-64.so.2
NeededLibraries [
  libgcc_s.so.1
  libm.so.6
  libc.so.6
  ld-linux-x86-64.so.2
]
Name: GLIBC_2.35
Name: GCC_4.2.0
GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline
Sections [
Section {
  Name: .dynsym (1)
}
]
Symbols [
]
EOF

linux_arm64="${tmp}/linux-arm64.txt"
cat > "${linux_arm64}" <<'EOF'
Format: elf64-littleaarch64
Arch: aarch64
Class: 64-bit
DataEncoding: LittleEndian
Type: SharedObject (0x3)
Machine: EM_AARCH64 (0xB7)
Interpreter: /lib/ld-linux-aarch64.so.1
NeededLibraries [
  libgcc_s.so.1
  libm.so.6
  libc.so.6
  ld-linux-aarch64.so.1
]
Name: GLIBC_2.35
Name: GCC_4.2.0
Sections [
]
Symbols [
]
EOF

mac_arm_readobj="${tmp}/mac-arm-readobj.txt"
cat > "${mac_arm_readobj}" <<'EOF'
Format: Mach-O arm64
Arch: aarch64
AddressSize: 64bit
FileType: Executable (0x2)
Sections [
]
Symbols [
  Symbol {
    Name: _abort (1)
    Extern
    Type: Undef (0x0)
  }
]
EOF
mac_x64_readobj="${tmp}/mac-x64-readobj.txt"
cat > "${mac_x64_readobj}" <<'EOF'
Format: Mach-O 64-bit x86-64
Arch: x86_64
AddressSize: 64bit
FileType: Executable (0x2)
Sections [
]
Symbols [
]
EOF
mac_objdump="${tmp}/mac-objdump.txt"
cat > "${mac_objdump}" <<'EOF'
Load command 0
      cmd LC_BUILD_VERSION
    minos 13.0
Load command 1
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation (offset 24)
Load command 2
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/CoreGraphics.framework/Versions/A/CoreGraphics (offset 24)
Load command 3
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/CoreML.framework/Versions/A/CoreML (offset 24)
Load command 4
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices (offset 24)
Load command 5
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/CoreVideo.framework/Versions/A/CoreVideo (offset 24)
Load command 6
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/Foundation.framework/Versions/C/Foundation (offset 24)
Load command 7
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/ImageIO.framework/Versions/A/ImageIO (offset 24)
Load command 8
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/Metal.framework/Versions/A/Metal (offset 24)
Load command 9
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/Security.framework/Versions/A/Security (offset 24)
Load command 10
      cmd LC_LOAD_DYLIB
     name /usr/lib/libSystem.B.dylib (offset 24)
Load command 11
      cmd LC_LOAD_DYLIB
     name /usr/lib/libc++.1.dylib (offset 24)
Load command 12
      cmd LC_LOAD_DYLIB
     name /usr/lib/libiconv.2.dylib (offset 24)
Load command 13
      cmd LC_LOAD_DYLIB
     name /usr/lib/libobjc.A.dylib (offset 24)
EOF

windows="${tmp}/windows.txt"
cat > "${windows}" <<'EOF'
Format: COFF-x86-64
Arch: x86_64
Machine: IMAGE_FILE_MACHINE_AMD64 (0x8664)
IMAGE_FILE_EXECUTABLE_IMAGE (0x2)
Magic: 0x20B
MajorOperatingSystemVersion: 10
MinorOperatingSystemVersion: 0
MajorSubsystemVersion: 6
MinorSubsystemVersion: 2
Subsystem: IMAGE_SUBSYSTEM_WINDOWS_CUI (0x3)
Sections [
]
Symbols [
]
Import {
  Name: ADVAPI32.dll
}
Import {
  Name: api-ms-win-core-synch-l1-2-0.dll
}
Import {
  Name: bcrypt.dll
}
Import {
  Name: bcryptprimitives.dll
}
Import {
  Name: combase.dll
}
Import {
  Name: KERNEL32.dll
}
Import {
  Name: msvcrt.dll
}
Import {
  Name: ntdll.dll
}
Import {
  Name: shell32.dll
}
Import {
  Name: userenv.dll
}
Import {
  Name: ws2_32.dll
}
EOF

freebsd="${tmp}/freebsd.txt"
cat > "${freebsd}" <<'EOF'
Format: elf64-x86-64
Arch: x86_64
Class: 64-bit
DataEncoding: LittleEndian
OS/ABI: FreeBSD (0x9)
Type: SharedObject (0x3)
Machine: EM_X86_64 (0x3E)
NeededLibraries [
  libc.so.7
  libgcc_s.so.1
  libm.so.5
  libthr.so.3
]
Sections [
]
Symbols [
]
EOF

expect_pass linux_x64 run_check linux-x64 "${linux_x64}"
expect_pass linux_arm64 run_check linux-aarch64 "${linux_arm64}"
expect_pass mac_arm64_security_framework run_check macos-arm64 "${mac_arm_readobj}" "${mac_objdump}"
expect_pass mac_x64_security_framework run_check macos-x64 "${mac_x64_readobj}" "${mac_objdump}"
expect_pass windows run_check windows-x64 "${windows}"
expect_pass windows_declared_tool run_declared_windows_check "${windows}"
expect_pass freebsd run_check freebsd-x64 "${freebsd}"
expect_fail malformed run_check linux-x64 "${tmp}/empty"

if "${checker}" linux-x64 "${tmp}/candidate" "${tmp}/llvm-readobj" \
  >"${tmp}/declared-linux.out" 2>"${tmp}/declared-linux.err"; then
  echo "non-Windows checker accepted a declared LLVM reader" >&2
  exit 1
fi
grep -Fq 'a declared LLVM reader is supported only for windows-x64' \
  "${tmp}/declared-linux.err"

missing_symbols="${tmp}/missing-symbols.txt"
sed '/^Symbols \[$/,/^\]$/d' "${linux_x64}" > "${missing_symbols}"
expect_fail missing_symbols run_check linux-x64 "${missing_symbols}"
truncated_symbols="${tmp}/truncated-symbols.txt"
sed '$d' "${linux_x64}" > "${truncated_symbols}"
expect_fail truncated_symbols run_check linux-x64 "${truncated_symbols}"
truncated_sections="${tmp}/truncated-sections.txt"
awk '
  /^Sections \[$/ { in_sections=1 }
  in_sections && /^\]$/ { in_sections=0; next }
  { print }
' "${linux_x64}" > "${truncated_sections}"
expect_fail truncated_sections run_check linux-x64 "${truncated_sections}"

mutate_and_fail() {
  local name="$1"
  local platform="$2"
  local source="$3"
  local expression="$4"
  local mutated="${tmp}/${name}.txt"
  sed "${expression}" "${source}" > "${mutated}"
  expect_fail "${name}" run_check "${platform}" "${mutated}"
}

mutate_and_fail linux_wrong_arch linux-x64 "${linux_x64}" 's/EM_X86_64/EM_AARCH64/'
mutate_and_fail linux_endian linux-x64 "${linux_x64}" 's/DataEncoding: LittleEndian/DataEncoding: BigEndian/'
mutate_and_fail linux_type linux-x64 "${linux_x64}" 's/Type: SharedObject/Type: Relocatable/'
mutate_and_fail linux_interpreter linux-x64 "${linux_x64}" 's#/lib64/ld-linux-x86-64.so.2#/lib/ld-linux.so.2#'
mutate_and_fail linux_glibc linux-x64 "${linux_x64}" 's/GLIBC_2.35/GLIBC_2.36/'
grep -Fq 'requires GLIBC_2.36, above allowed GLIBC_2.35' "${tmp}/linux_glibc.err"
mutate_and_fail arm_glibc linux-aarch64 "${linux_arm64}" 's/GLIBC_2.35/GLIBC_2.36/'
grep -Fq 'requires GLIBC_2.36, above allowed GLIBC_2.35' "${tmp}/arm_glibc.err"
mutate_and_fail linux_glibcxx linux-x64 "${linux_x64}" 's/Name: GLIBC_2.35/Name: GLIBC_2.35\nName: GLIBCXX_3.4.30/'
mutate_and_fail linux_cxxabi linux-x64 "${linux_x64}" 's/Name: GLIBC_2.35/Name: GLIBC_2.35\nName: CXXABI_1.3.11/'
mutate_and_fail linux_gcc linux-x64 "${linux_x64}" 's/GCC_4.2.0/GCC_4.3.0/'
mutate_and_fail linux_needed linux-x64 "${linux_x64}" 's/libm.so.6/libz.so.1/'
mutate_and_fail linux_rpath linux-x64 "${linux_x64}" 's/Name: GLIBC_2.35/RUNPATH: \/tmp\nName: GLIBC_2.35/'
mutate_and_fail linux_isa linux-x64 "${linux_x64}" 's/x86-64-baseline/x86-64-v3/'
mutate_and_fail linux_avx linux-x64 "${linux_x64}" 's/GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline/GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline AVX/'
mutate_and_fail arm_glibcxx linux-aarch64 "${linux_arm64}" 's/Name: GLIBC_2.35/Name: GLIBC_2.35\nName: GLIBCXX_3.4.30/'
mutate_and_fail linux_static_symbols linux-x64 "${linux_x64}" 's/Section {/Symbols [\n  Symbol {\n    Name: main (1)\n  }\n]\nSection {/'
mutate_and_fail linux_debug_section linux-x64 "${linux_x64}" 's/Name: .dynsym/Name: .debug_info/'

bad_mac_dylib="${tmp}/bad-mac-dylib.txt"
sed 's#/System/Library/Frameworks/CoreML.framework/Versions/A/CoreML#/opt/local/libCoreML.dylib#' "${mac_objdump}" > "${bad_mac_dylib}"
expect_fail mac_dylib run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_dylib}"
bad_mac_framework="${tmp}/bad-mac-framework.txt"
sed 's#/System/Library/Frameworks/Security.framework/Versions/A/Security#/System/Library/Frameworks/Contacts.framework/Versions/A/Contacts#' \
  "${mac_objdump}" > "${bad_mac_framework}"
expect_fail mac_arbitrary_framework run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_framework}"
expect_fail mac_x64_arbitrary_framework run_check macos-x64 "${mac_x64_readobj}" "${bad_mac_framework}"
bad_mac_framework_path="${tmp}/bad-mac-framework-path.txt"
sed 's#/System/Library/Frameworks/Security.framework/Versions/A/Security#/opt/ctx/Security.framework/Versions/A/Security#' \
  "${mac_objdump}" > "${bad_mac_framework_path}"
expect_fail mac_arbitrary_framework_path run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_framework_path}"
expect_fail mac_x64_arbitrary_framework_path run_check macos-x64 "${mac_x64_readobj}" "${bad_mac_framework_path}"
missing_mac_security="${tmp}/missing-mac-security.txt"
sed '/Security.framework\/Versions\/A\/Security/d' "${mac_objdump}" > "${missing_mac_security}"
expect_fail mac_missing_security_framework run_check macos-arm64 "${mac_arm_readobj}" "${missing_mac_security}"
expect_fail mac_x64_missing_security_framework run_check macos-x64 "${mac_x64_readobj}" "${missing_mac_security}"
missing_mac_core_services="${tmp}/missing-mac-core-services.txt"
sed '/CoreServices.framework\/Versions\/A\/CoreServices/d' "${mac_objdump}" > "${missing_mac_core_services}"
expect_fail mac_missing_core_services_framework run_check macos-arm64 "${mac_arm_readobj}" "${missing_mac_core_services}"
expect_fail mac_x64_missing_core_services_framework run_check macos-x64 "${mac_x64_readobj}" "${missing_mac_core_services}"
injected_mac_dylib="${tmp}/injected-mac-dylib.txt"
sed '/name \/usr\/lib\/libobjc.A.dylib/a\
Load command 14\
      cmd LC_LOAD_DYLIB\
     name /usr/local/lib/libctx-injected.dylib (offset 24)' \
  "${mac_objdump}" > "${injected_mac_dylib}"
expect_fail mac_injected_dylib run_check macos-arm64 "${mac_arm_readobj}" "${injected_mac_dylib}"
expect_fail mac_x64_injected_dylib run_check macos-x64 "${mac_x64_readobj}" "${injected_mac_dylib}"
bad_mac_version="${tmp}/bad-mac-version.txt"
sed 's/minos 13.0/minos 14.0/' "${mac_objdump}" > "${bad_mac_version}"
expect_fail mac_version run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_version}"
host_sdk_mac_version="${tmp}/host-sdk-mac-version.txt"
sed 's/minos 13.0/minos 26.2/' "${mac_objdump}" > "${host_sdk_mac_version}"
expect_fail mac_arm_host_sdk_minos run_check macos-arm64 \
  "${mac_arm_readobj}" "${host_sdk_mac_version}"
expect_fail mac_x64_host_sdk_minos run_check macos-x64 \
  "${mac_x64_readobj}" "${host_sdk_mac_version}"
bad_mac_rpath="${tmp}/bad-mac-rpath.txt"
sed 's/cmd LC_BUILD_VERSION/cmd LC_RPATH\nLoad command 1\n      cmd LC_BUILD_VERSION/' "${mac_objdump}" > "${bad_mac_rpath}"
expect_fail mac_rpath run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_rpath}"
bad_mac_arch="${tmp}/bad-mac-arch.txt"
sed 's/Arch: aarch64/Arch: x86_64/' "${mac_arm_readobj}" > "${bad_mac_arch}"
expect_fail mac_arch run_check macos-arm64 "${bad_mac_arch}" "${mac_objdump}"
bad_mac_type="${tmp}/bad-mac-type.txt"
sed 's/FileType: Executable/FileType: Dylib/' "${mac_arm_readobj}" > "${bad_mac_type}"
expect_fail mac_type run_check macos-arm64 "${bad_mac_type}" "${mac_objdump}"
bad_mac_local_symbol="${tmp}/bad-mac-local-symbol.txt"
sed '/^[[:space:]]*Extern$/d' "${mac_arm_readobj}" > "${bad_mac_local_symbol}"
expect_fail mac_arm_unstripped_local_symbol run_check macos-arm64 "${bad_mac_local_symbol}" "${mac_objdump}"
bad_mac_x64_local_symbol="${tmp}/bad-mac-x64-local-symbol.txt"
sed '/^Symbols \[$/a\
  Symbol {\
    Name: core::ptr::drop_in_place::h0123456789abcdef (1)\
    Type: Section (0xE)\
  }' "${mac_x64_readobj}" > "${bad_mac_x64_local_symbol}"
expect_fail mac_x64_unstripped_local_symbol run_check macos-x64 \
  "${bad_mac_x64_local_symbol}" "${mac_objdump}"
bad_mac_truncated_symbol="${tmp}/bad-mac-truncated-symbol.txt"
sed '/^  }$/d' "${mac_arm_readobj}" > "${bad_mac_truncated_symbol}"
expect_fail mac_truncated_symbol run_check macos-arm64 "${bad_mac_truncated_symbol}" "${mac_objdump}"
bad_mac_debug_section="${tmp}/bad-mac-debug-section.txt"
sed 's/AddressSize: 64bit/AddressSize: 64bit\nSection {\n  Name: __debug_info (1)\n}/' \
  "${mac_arm_readobj}" > "${bad_mac_debug_section}"
expect_fail mac_debug_section run_check macos-arm64 "${bad_mac_debug_section}" "${mac_objdump}"

mutate_and_fail windows_machine windows-x64 "${windows}" 's/IMAGE_FILE_MACHINE_AMD64/IMAGE_FILE_MACHINE_ARM64/'
mutate_and_fail windows_magic windows-x64 "${windows}" 's/Magic: 0x20B/Magic: 0x10B/'
mutate_and_fail windows_type windows-x64 "${windows}" 's/IMAGE_FILE_EXECUTABLE_IMAGE/IMAGE_FILE_DLL/'
mutate_and_fail windows_subsystem windows-x64 "${windows}" 's/IMAGE_SUBSYSTEM_WINDOWS_CUI/IMAGE_SUBSYSTEM_WINDOWS_GUI/'
mutate_and_fail windows_version windows-x64 "${windows}" 's/MajorOperatingSystemVersion: 10/MajorOperatingSystemVersion: 11/'
mutate_and_fail windows_subsystem_version windows-x64 "${windows}" 's/MajorSubsystemVersion: 6/MajorSubsystemVersion: 11/'
mutate_and_fail windows_dll windows-x64 "${windows}" 's/ws2_32.dll/winhttp.dll/'
mutate_and_fail windows_static_symbols windows-x64 "${windows}" 's/Import {/Symbols [\n  Symbol {\n    Name: main (1)\n  }\n]\nImport {/'
mutate_and_fail freebsd_abi freebsd-x64 "${freebsd}" 's/OS\/ABI: FreeBSD/OS\/ABI: UNIX - System V/'
mutate_and_fail freebsd_arch freebsd-x64 "${freebsd}" 's/EM_X86_64/EM_AARCH64/'
mutate_and_fail freebsd_type freebsd-x64 "${freebsd}" 's/Type: SharedObject/Type: Relocatable/'
mutate_and_fail freebsd_needed freebsd-x64 "${freebsd}" 's/libthr.so.3/libutil.so.9/'
mutate_and_fail freebsd_rpath freebsd-x64 "${freebsd}" 's/NeededLibraries \[/RUNPATH: \/tmp\nNeededLibraries [/'

printf 'release binary compatibility tests passed\n'
