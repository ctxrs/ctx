#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
checker="${repo_root}/scripts/check-release-binary-compat.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-binary-compat-test.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT

printf '#!/bin/sh\ncat "$FAKE_READOBJ_OUTPUT"\n' > "${tmp}/llvm-readobj"
printf '#!/bin/sh\ncat "$FAKE_OBJDUMP_OUTPUT"\n' > "${tmp}/llvm-objdump"
chmod +x "${tmp}/llvm-readobj" "${tmp}/llvm-objdump"
printf 'not a real binary\n' > "${tmp}/candidate"
: > "${tmp}/empty"

run_check() {
  local platform="$1"
  local readobj="$2"
  local objdump="${3:-${tmp}/empty}"
  FAKE_READOBJ_OUTPUT="${readobj}" \
    FAKE_OBJDUMP_OUTPUT="${objdump}" \
    CTX_LLVM_READOBJ="${tmp}/llvm-readobj" \
    CTX_LLVM_OBJDUMP="${tmp}/llvm-objdump" \
    "${checker}" "${platform}" "${tmp}/candidate"
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
EOF

mac_arm_readobj="${tmp}/mac-arm-readobj.txt"
cat > "${mac_arm_readobj}" <<'EOF'
Format: Mach-O arm64
Arch: aarch64
AddressSize: 64bit
FileType: Executable (0x2)
EOF
mac_x64_readobj="${tmp}/mac-x64-readobj.txt"
cat > "${mac_x64_readobj}" <<'EOF'
Format: Mach-O 64-bit x86-64
Arch: x86_64
AddressSize: 64bit
FileType: Executable (0x2)
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
     name /System/Library/Frameworks/CoreVideo.framework/Versions/A/CoreVideo (offset 24)
Load command 5
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/Foundation.framework/Versions/C/Foundation (offset 24)
Load command 6
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/ImageIO.framework/Versions/A/ImageIO (offset 24)
Load command 7
      cmd LC_LOAD_DYLIB
     name /System/Library/Frameworks/Metal.framework/Versions/A/Metal (offset 24)
Load command 8
      cmd LC_LOAD_DYLIB
     name /usr/lib/libSystem.B.dylib (offset 24)
Load command 9
      cmd LC_LOAD_DYLIB
     name /usr/lib/libc++.1.dylib (offset 24)
Load command 10
      cmd LC_LOAD_DYLIB
     name /usr/lib/libiconv.2.dylib (offset 24)
Load command 11
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
  Name: KERNEL32.dll
}
Import {
  Name: msvcrt.dll
}
Import {
  Name: ntdll.dll
}
Import {
  Name: ole32.dll
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
EOF

expect_pass linux_x64 run_check linux-x64 "${linux_x64}"
expect_pass linux_arm64 run_check linux-aarch64 "${linux_arm64}"
expect_pass mac_arm64 run_check macos-arm64 "${mac_arm_readobj}" "${mac_objdump}"
expect_pass mac_x64 run_check macos-x64 "${mac_x64_readobj}" "${mac_objdump}"
expect_pass windows run_check windows-x64 "${windows}"
expect_pass freebsd run_check freebsd-x64 "${freebsd}"
expect_fail malformed run_check linux-x64 "${tmp}/empty"

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
mutate_and_fail linux_glibcxx linux-x64 "${linux_x64}" 's/Name: GLIBC_2.35/Name: GLIBC_2.35\nName: GLIBCXX_3.4.30/'
mutate_and_fail linux_cxxabi linux-x64 "${linux_x64}" 's/Name: GLIBC_2.35/Name: GLIBC_2.35\nName: CXXABI_1.3.11/'
mutate_and_fail linux_gcc linux-x64 "${linux_x64}" 's/GCC_4.2.0/GCC_4.3.0/'
mutate_and_fail linux_needed linux-x64 "${linux_x64}" 's/libm.so.6/libz.so.1/'
mutate_and_fail linux_rpath linux-x64 "${linux_x64}" 's/Name: GLIBC_2.35/RUNPATH: \/tmp\nName: GLIBC_2.35/'
mutate_and_fail linux_isa linux-x64 "${linux_x64}" 's/x86-64-baseline/x86-64-v3/'
mutate_and_fail linux_avx linux-x64 "${linux_x64}" 's/GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline/GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline AVX/'
mutate_and_fail arm_glibcxx linux-aarch64 "${linux_arm64}" 's/Name: GLIBC_2.35/Name: GLIBC_2.35\nName: GLIBCXX_3.4.30/'

bad_mac_dylib="${tmp}/bad-mac-dylib.txt"
sed 's#/System/Library/Frameworks/CoreML.framework/Versions/A/CoreML#/opt/local/libCoreML.dylib#' "${mac_objdump}" > "${bad_mac_dylib}"
expect_fail mac_dylib run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_dylib}"
bad_mac_version="${tmp}/bad-mac-version.txt"
sed 's/minos 13.0/minos 14.0/' "${mac_objdump}" > "${bad_mac_version}"
expect_fail mac_version run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_version}"
bad_mac_rpath="${tmp}/bad-mac-rpath.txt"
sed 's/cmd LC_BUILD_VERSION/cmd LC_RPATH\nLoad command 1\n      cmd LC_BUILD_VERSION/' "${mac_objdump}" > "${bad_mac_rpath}"
expect_fail mac_rpath run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_rpath}"
bad_mac_arch="${tmp}/bad-mac-arch.txt"
sed 's/Arch: aarch64/Arch: x86_64/' "${mac_arm_readobj}" > "${bad_mac_arch}"
expect_fail mac_arch run_check macos-arm64 "${bad_mac_arch}" "${mac_objdump}"
bad_mac_type="${tmp}/bad-mac-type.txt"
sed 's/FileType: Executable/FileType: Dylib/' "${mac_arm_readobj}" > "${bad_mac_type}"
expect_fail mac_type run_check macos-arm64 "${bad_mac_type}" "${mac_objdump}"

mutate_and_fail windows_machine windows-x64 "${windows}" 's/IMAGE_FILE_MACHINE_AMD64/IMAGE_FILE_MACHINE_ARM64/'
mutate_and_fail windows_magic windows-x64 "${windows}" 's/Magic: 0x20B/Magic: 0x10B/'
mutate_and_fail windows_type windows-x64 "${windows}" 's/IMAGE_FILE_EXECUTABLE_IMAGE/IMAGE_FILE_DLL/'
mutate_and_fail windows_subsystem windows-x64 "${windows}" 's/IMAGE_SUBSYSTEM_WINDOWS_CUI/IMAGE_SUBSYSTEM_WINDOWS_GUI/'
mutate_and_fail windows_version windows-x64 "${windows}" 's/MajorOperatingSystemVersion: 10/MajorOperatingSystemVersion: 11/'
mutate_and_fail windows_subsystem_version windows-x64 "${windows}" 's/MajorSubsystemVersion: 6/MajorSubsystemVersion: 11/'
mutate_and_fail windows_dll windows-x64 "${windows}" 's/ws2_32.dll/winhttp.dll/'
mutate_and_fail freebsd_abi freebsd-x64 "${freebsd}" 's/OS\/ABI: FreeBSD/OS\/ABI: UNIX - System V/'
mutate_and_fail freebsd_arch freebsd-x64 "${freebsd}" 's/EM_X86_64/EM_AARCH64/'
mutate_and_fail freebsd_type freebsd-x64 "${freebsd}" 's/Type: SharedObject/Type: Relocatable/'
mutate_and_fail freebsd_needed freebsd-x64 "${freebsd}" 's/libthr.so.3/libutil.so.9/'
mutate_and_fail freebsd_rpath freebsd-x64 "${freebsd}" 's/NeededLibraries \[/RUNPATH: \/tmp\nNeededLibraries [/'

printf 'release binary compatibility tests passed\n'
