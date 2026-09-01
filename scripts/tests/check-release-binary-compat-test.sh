#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
checker="${repo_root}/scripts/check-release-binary-compat.sh"
artifact_checker="${repo_root}/scripts/check-public-cli-artifact.sh"
published_checker="${repo_root}/scripts/check-github-release-assets.sh"
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
if [ "${1:-}" = --version ]; then
  printf 'Homebrew LLVM version %s\n' "${FAKE_LLVM_VERSION:-22.1.8}"
  exit 0
fi
case " $* " in *' --sections '*) ;; *) exit 64 ;; esac
case " $* " in *' --symbols '*) ;; *) exit 64 ;; esac
cat "$FAKE_READOBJ_OUTPUT"
EOF
cat > "${tmp}/llvm-objdump" <<'EOF'
#!/bin/sh
if [ "${1:-}" = --version ]; then
  printf 'Homebrew LLVM version %s\n' "${FAKE_LLVM_OBJDUMP_VERSION:-${FAKE_LLVM_VERSION:-22.1.8}}"
  exit 0
fi
cat "$FAKE_OBJDUMP_OUTPUT"
EOF
chmod +x "${tmp}/llvm-readobj" "${tmp}/llvm-objdump"
printf 'not a real binary\n' > "${tmp}/candidate"
: > "${tmp}/empty"

# Parser fixtures execute only through this disposable checker copy. The
# production checker retains fixed package-root resolution unless the release
# packager passes an explicit platform-constrained tool declaration.
fixture_root="${tmp}/fixture"
mkdir -p "${fixture_root}/scripts/release" "${fixture_root}/contracts"
fixture_checker="${fixture_root}/scripts/check-release-binary-compat-fixture.sh"
sed \
  "s#^  LLVM_TOOL_ROOT=.*#  LLVM_TOOL_ROOT=\"${tmp}\"#" \
  "${checker}" > "${fixture_checker}"
chmod 700 "${fixture_checker}"
cp "${repo_root}/scripts/public-cli-release-targets.py" \
  "${repo_root}/scripts/check-release-target-matrix.py" "${fixture_root}/scripts/"
cp "${repo_root}/contracts/release-targets-v1.json" "${fixture_root}/contracts/"
fixture_snapshot="${tmp}/approved-snapshot"
mkdir -p "${fixture_snapshot}/bin"
cp "${tmp}/llvm-readobj" "${fixture_snapshot}/bin/llvm-readobj"
cp "${tmp}/llvm-objdump" "${fixture_snapshot}/bin/llvm-objdump"
cat >"${fixture_root}/scripts/release/macos_llvm_authority.py" <<'PY'
#!/usr/bin/env python3
import os
from pathlib import Path
import sys

root = Path(sys.argv[sys.argv.index("--snapshot-root") + 1])
tool = sys.argv[sys.argv.index("--tool") + 1]
separator = sys.argv.index("--")
executable = root / "bin" / ("llvm-readobj" if tool == "readobj" else "llvm-objdump")
os.execv(executable, [str(executable), *sys.argv[separator + 1 :]])
PY

run_check() {
  local platform="$1"
  local readobj="$2"
  local objdump="${3:-${tmp}/empty}"
  if [[ "${platform}" == "macos-x64" ]]; then
    FAKE_READOBJ_OUTPUT="${readobj}" \
      FAKE_OBJDUMP_OUTPUT="${objdump}" \
      "${fixture_checker}" "${platform}" "${tmp}/candidate" \
      "${fixture_snapshot}/bin/llvm-readobj" \
      "${fixture_snapshot}/bin/llvm-objdump"
  else
    FAKE_READOBJ_OUTPUT="${readobj}" \
      FAKE_OBJDUMP_OUTPUT="${objdump}" \
      "${fixture_checker}" "${platform}" "${tmp}/candidate"
  fi
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
ProgramHeaders [
  ProgramHeader {
    Type: PT_GNU_STACK (0x6474E551)
    Flags [ (0x6)
      PF_R (0x4)
      PF_W (0x2)
    ]
  }
  ProgramHeader {
    Type: PT_GNU_RELRO (0x6474E552)
    Flags [ (0x4)
      PF_R (0x4)
    ]
  }
]
DynamicSection [
  0x000000000000001E FLAGS        BIND_NOW
  0x000000006FFFFFFB FLAGS_1      NOW PIE
]
Interpreter: /lib64/ld-linux-x86-64.so.2
NeededLibraries [
  ld-linux-x86-64.so.2
  libdl.so.2
  libpthread.so.0
  libm.so.6
  libc.so.6
]
Name: GLIBC_2.28
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
ProgramHeaders [
  ProgramHeader {
    Type: PT_GNU_STACK (0x6474E551)
    Flags [ (0x6)
      PF_R (0x4)
      PF_W (0x2)
    ]
  }
  ProgramHeader {
    Type: PT_GNU_RELRO (0x6474E552)
    Flags [ (0x4)
      PF_R (0x4)
    ]
  }
]
DynamicSection [
  0x000000000000001E FLAGS        BIND_NOW
  0x000000006FFFFFFB FLAGS_1      NOW PIE
]
Interpreter: /lib/ld-linux-aarch64.so.1
NeededLibraries [
  libdl.so.2
  libpthread.so.0
  libm.so.6
  libc.so.6
]
Name: GLIBC_2.28
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
     name /usr/lib/libcharset.1.dylib (offset 24)
Load command 13
      cmd LC_LOAD_DYLIB
     name /usr/lib/libiconv.2.dylib (offset 24)
Load command 14
      cmd LC_LOAD_DYLIB
     name /usr/lib/libobjc.A.dylib (offset 24)
EOF

windows="${tmp}/windows.txt"
cat > "${windows}" <<'EOF'
Format: COFF-x86-64
Arch: x86_64
Machine: IMAGE_FILE_MACHINE_AMD64 (0x8664)
IMAGE_FILE_EXECUTABLE_IMAGE (0x2)
Characteristics [ (0x160)
  IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE (0x40)
  IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA (0x20)
  IMAGE_DLL_CHARACTERISTICS_NX_COMPAT (0x100)
]
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
  Name: api-ms-win-crt-environment-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-heap-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-math-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-private-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-runtime-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-stdio-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-string-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-time-l1-1-0.dll
}
Import {
  Name: api-ms-win-crt-utility-l1-1-0.dll
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
  Name: crypt32.dll
}
Import {
  Name: KERNEL32.dll
}
Import {
  Name: ntdll.dll
}
Import {
  Name: ole32.dll
}
Import {
  Name: psapi.dll
}
Import {
  Name: rstrtmgr.dll
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

expect_pass linux_x64 run_check linux-x64 "${linux_x64}"
expect_pass linux_arm64 run_check linux-aarch64 "${linux_arm64}"
linux_x64_libgcc="${tmp}/linux-x64-libgcc.txt"
sed '/  libm\.so\.6/a\  libgcc_s.so.1' "${linux_x64}" >"${linux_x64_libgcc}"
expect_pass linux_x64_optional_libgcc run_check linux-x64 "${linux_x64_libgcc}"
linux_arm64_gnu_runtime="${tmp}/linux-arm64-gnu-runtime.txt"
sed '/NeededLibraries \[/a\  ld-linux-aarch64.so.1\
  libgcc_s.so.1' "${linux_arm64}" >"${linux_arm64_gnu_runtime}"
expect_pass linux_arm64_optional_gnu_runtime run_check linux-aarch64 \
  "${linux_arm64_gnu_runtime}"
expect_pass mac_arm64_security_framework run_check macos-arm64 "${mac_arm_readobj}" "${mac_objdump}"
expect_pass mac_x64_security_framework run_check macos-x64 "${mac_x64_readobj}" "${mac_objdump}"
expect_pass windows_native_trust_store run_check windows-x64 "${windows}"
expect_pass windows_declared_tool run_declared_windows_check "${windows}"
expect_fail malformed run_check linux-x64 "${tmp}/empty"
grep -Fq "scanner-inputs=llvm-readobj=${tmp}/llvm-readobj" \
  "${tmp}/linux_x64.out"
grep -Fq "scanner-authority=authoritative-package-root:${tmp}" \
  "${tmp}/linux_x64.out"

real_unhardened_source="${tmp}/real-unhardened.c"
real_unhardened="${tmp}/real-unhardened"
real_unhardened_readobj="${tmp}/real-unhardened.txt"
printf 'int main(void) { return 0; }\n' >"${real_unhardened_source}"
cc -g -fPIE -pie -Wl,-z,norelro -Wl,-z,lazy -Wl,-z,execstack \
  -o "${real_unhardened}" "${real_unhardened_source}"
strip --strip-all "${real_unhardened}"
/usr/bin/llvm-readobj \
  --file-headers --program-headers --dynamic-table --needed-libs \
  --version-info --notes --sections --symbols \
  "${real_unhardened}" >"${real_unhardened_readobj}"
expect_fail real_unhardened run_check linux-x64 "${real_unhardened_readobj}"
grep -Fq 'expected exactly one GNU_RELRO program header' \
  "${tmp}/real_unhardened.err"

if "${checker}" linux-x64 "${tmp}/candidate" "${tmp}/llvm-readobj" \
  >"${tmp}/declared-linux.out" 2>"${tmp}/declared-linux.err"; then
  echo "non-macOS/non-Windows checker accepted declared LLVM tools" >&2
  exit 1
fi
grep -Fq 'declared LLVM tools are supported only for macos-x64 and windows-x64' \
  "${tmp}/declared-linux.err"

if "${checker}" macos-arm64 "${tmp}/candidate" \
  "${tmp}/llvm-readobj" "${tmp}/llvm-objdump" \
  >"${tmp}/declared-macos-arm64.out" \
  2>"${tmp}/declared-macos-arm64.err"; then
  echo "macos-arm64 checker accepted the x64 LLVM bottle authority" >&2
  exit 1
fi
grep -Fq 'declared LLVM tools are supported only for macos-x64 and windows-x64' \
  "${tmp}/declared-macos-arm64.err"

if "${checker}" macos-x64 "${tmp}/candidate" \
  >"${tmp}/macos-omitted.out" 2>"${tmp}/macos-omitted.err"; then
  echo "macos-x64 compatibility checker accepted omitted pinned LLVM tools" >&2
  exit 1
fi
grep -Fq 'macos-x64 requires a declared LLVM reader and objdump pair' \
  "${tmp}/macos-omitted.err"

if "${artifact_checker}" macos-x64 "${tmp}/missing-artifacts" \
  >"${tmp}/artifact-macos-omitted.out" \
  2>"${tmp}/artifact-macos-omitted.err"; then
  echo "macos-x64 public artifact checker accepted omitted pinned LLVM tools" >&2
  exit 1
fi
grep -Fq 'macos-x64 requires a declared LLVM reader and objdump pair' \
  "${tmp}/artifact-macos-omitted.err"

if "${checker}" macos-x64 "${tmp}/candidate" "${tmp}/llvm-readobj" \
  >"${tmp}/declared-macos-incomplete.out" \
  2>"${tmp}/declared-macos-incomplete.err"; then
  echo "macOS checker accepted an incomplete declared LLVM tool pair" >&2
  exit 1
fi
grep -Fq 'macos-x64 requires a declared LLVM reader and objdump pair' \
  "${tmp}/declared-macos-incomplete.err"

if FAKE_READOBJ_OUTPUT="${mac_x64_readobj}" \
  FAKE_OBJDUMP_OUTPUT="${mac_objdump}" \
  "${checker}" macos-x64 "${tmp}/candidate" \
  "${tmp}/llvm-readobj" "${tmp}/llvm-objdump" \
  >"${tmp}/declared-macos-arbitrary.out" \
  2>"${tmp}/declared-macos-arbitrary.err"; then
  echo "macOS checker accepted arbitrary matching-version LLVM tools" >&2
  exit 1
fi
grep -Fq 'not in the approved snapshot layout' \
  "${tmp}/declared-macos-arbitrary.err"

if FAKE_READOBJ_OUTPUT="${mac_x64_readobj}" \
  FAKE_OBJDUMP_OUTPUT="${mac_objdump}" \
  "${checker}" macos-x64 "${tmp}/candidate" \
  "${tmp}/llvm-objdump" "${tmp}/llvm-readobj" \
  >"${tmp}/declared-macos-swapped.out" \
  2>"${tmp}/declared-macos-swapped.err"; then
  echo "macOS checker accepted swapped declared LLVM identities" >&2
  exit 1
fi
grep -Fq 'not in the approved snapshot layout' "${tmp}/declared-macos-swapped.err"

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
  local expected_error="${5:-}"
  local mutated="${tmp}/${name}.txt"
  sed "${expression}" "${source}" > "${mutated}"
  expect_fail "${name}" run_check "${platform}" "${mutated}"
  if [[ -n "${expected_error}" ]]; then
    grep -Fq "${expected_error}" "${tmp}/${name}.err"
  fi
}

mutate_and_fail linux_wrong_arch linux-x64 "${linux_x64}" 's/EM_X86_64/EM_AARCH64/'
mutate_and_fail linux_endian linux-x64 "${linux_x64}" 's/DataEncoding: LittleEndian/DataEncoding: BigEndian/'
mutate_and_fail linux_type linux-x64 "${linux_x64}" 's/Type: SharedObject/Type: Relocatable/'
mutate_and_fail linux_missing_relro linux-x64 "${linux_x64}" \
  '/Type: PT_GNU_RELRO/d' 'expected exactly one GNU_RELRO program header'
mutate_and_fail linux_exec_stack linux-x64 "${linux_x64}" \
  '1,/PF_W (0x2)/s/PF_W (0x2)/PF_W (0x2)\
      PF_X (0x1)/' 'GNU_STACK is executable'
mutate_and_fail linux_missing_bind_now linux-x64 "${linux_x64}" \
  '/FLAGS[[:space:]]*BIND_NOW/d' 'missing BIND_NOW dynamic flag'
mutate_and_fail linux_missing_pie linux-x64 "${linux_x64}" \
  's/FLAGS_1      NOW PIE/FLAGS_1      NOW/' 'missing PIE dynamic flag'
mutate_and_fail linux_interpreter linux-x64 "${linux_x64}" 's#/lib64/ld-linux-x86-64.so.2#/lib/ld-linux.so.2#'
mutate_and_fail linux_glibc linux-x64 "${linux_x64}" 's/GLIBC_2.28/GLIBC_2.29/'
grep -Fq 'requires GLIBC_2.29, above allowed GLIBC_2.28' "${tmp}/linux_glibc.err"
mutate_and_fail arm_glibc linux-aarch64 "${linux_arm64}" 's/GLIBC_2.28/GLIBC_2.29/'
grep -Fq 'requires GLIBC_2.29, above allowed GLIBC_2.28' "${tmp}/arm_glibc.err"
mutate_and_fail linux_glibcxx linux-x64 "${linux_x64}" 's/Name: GLIBC_2.28/Name: GLIBC_2.28\nName: GLIBCXX_3.4.30/'
mutate_and_fail linux_cxxabi linux-x64 "${linux_x64}" 's/Name: GLIBC_2.28/Name: GLIBC_2.28\nName: CXXABI_1.3.11/'
mutate_and_fail linux_gcc linux-x64 "${linux_x64}" 's/GCC_4.2.0/GCC_4.3.0/'
mutate_and_fail linux_needed linux-x64 "${linux_x64}" 's/libm.so.6/libz.so.1/'
mutate_and_fail linux_rpath linux-x64 "${linux_x64}" 's/Name: GLIBC_2.28/RUNPATH: \/tmp\nName: GLIBC_2.28/'
mutate_and_fail linux_isa linux-x64 "${linux_x64}" 's/x86-64-baseline/x86-64-v3/'
mutate_and_fail linux_avx linux-x64 "${linux_x64}" 's/GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline/GNU_PROPERTY_X86_ISA_1_NEEDED: x86-64-baseline AVX/'
mutate_and_fail arm_glibcxx linux-aarch64 "${linux_arm64}" 's/Name: GLIBC_2.28/Name: GLIBC_2.28\nName: GLIBCXX_3.4.30/'
mutate_and_fail linux_static_symbols linux-x64 "${linux_x64}" 's/Section {/Symbols [\n  Symbol {\n    Name: main (1)\n  }\n]\nSection {/'
mutate_and_fail linux_debug_section linux-x64 "${linux_x64}" 's/Name: .dynsym/Name: .debug_info/'
mutate_and_fail linux_rust_gdb_section linux-x64 "${linux_x64}" \
  's/Name: .dynsym/Name: .debug_gdb_scripts/'

bad_mac_dylib="${tmp}/bad-mac-dylib.txt"
sed 's#/System/Library/Frameworks/CoreML.framework/Versions/A/CoreML#/opt/local/libCoreML.dylib#' "${mac_objdump}" > "${bad_mac_dylib}"
expect_fail mac_dylib run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_dylib}"
bad_mac_framework="${tmp}/bad-mac-framework.txt"
sed 's#/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices#/System/Library/Frameworks/Contacts.framework/Versions/A/Contacts#' \
  "${mac_objdump}" > "${bad_mac_framework}"
expect_fail mac_arbitrary_framework run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_framework}"
expect_fail mac_x64_arbitrary_framework run_check macos-x64 "${mac_x64_readobj}" "${bad_mac_framework}"
bad_mac_framework_path="${tmp}/bad-mac-framework-path.txt"
sed 's#/System/Library/Frameworks/CoreServices.framework/Versions/A/CoreServices#/opt/ctx/CoreServices.framework/Versions/A/CoreServices#' \
  "${mac_objdump}" > "${bad_mac_framework_path}"
expect_fail mac_arbitrary_framework_path run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_framework_path}"
expect_fail mac_x64_arbitrary_framework_path run_check macos-x64 "${mac_x64_readobj}" "${bad_mac_framework_path}"
bad_mac_security_sibling="${tmp}/bad-mac-security-sibling.txt"
sed 's#/System/Library/Frameworks/Security.framework/Versions/A/Security#/System/Library/Frameworks/Security.framework/Versions/B/Security#' \
  "${mac_objdump}" > "${bad_mac_security_sibling}"
expect_fail mac_security_sibling run_check macos-arm64 "${mac_arm_readobj}" "${bad_mac_security_sibling}"
expect_fail mac_x64_security_sibling run_check macos-x64 "${mac_x64_readobj}" "${bad_mac_security_sibling}"
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
mutate_and_fail windows_missing_dynamic_base windows-x64 "${windows}" \
  '/IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE/d' \
  'missing PE mitigation IMAGE_DLL_CHARACTERISTICS_DYNAMIC_BASE'
mutate_and_fail windows_missing_nx_compat windows-x64 "${windows}" \
  '/IMAGE_DLL_CHARACTERISTICS_NX_COMPAT/d' \
  'missing PE mitigation IMAGE_DLL_CHARACTERISTICS_NX_COMPAT'
mutate_and_fail windows_missing_high_entropy_va windows-x64 "${windows}" \
  '/IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA/d' \
  'missing PE mitigation IMAGE_DLL_CHARACTERISTICS_HIGH_ENTROPY_VA'
mutate_and_fail windows_subsystem windows-x64 "${windows}" 's/IMAGE_SUBSYSTEM_WINDOWS_CUI/IMAGE_SUBSYSTEM_WINDOWS_GUI/'
mutate_and_fail windows_version windows-x64 "${windows}" 's/MajorOperatingSystemVersion: 10/MajorOperatingSystemVersion: 11/'
mutate_and_fail windows_subsystem_version windows-x64 "${windows}" 's/MajorSubsystemVersion: 6/MajorSubsystemVersion: 11/'
mutate_and_fail windows_missing_restart_manager windows-x64 "${windows}" '/Name: rstrtmgr.dll/d'
mutate_and_fail windows_crypt32_sibling windows-x64 "${windows}" 's/crypt32.dll/cryptnet.dll/'
mutate_and_fail windows_dll windows-x64 "${windows}" 's/ws2_32.dll/winhttp.dll/'
mutate_and_fail windows_static_symbols windows-x64 "${windows}" 's/Import {/Symbols [\n  Symbol {\n    Name: main (1)\n  }\n]\nImport {/'
# The no-Buildkite local runner validates published bytes through this public
# command. Its contract must provision the pinned task root, materialize an
# owner-private snapshot, and pass both approved tools to the macOS x64 check.
published_root="${tmp}/published-checker"
published_bin="${tmp}/published-bin"
published_task_root="${tmp}/published-task-root"
published_calls="${tmp}/published-compat-calls.tsv"
published_snapshot_record="${tmp}/published-snapshot.txt"
published_gh_marker="${tmp}/published-gh-called"
mkdir -p \
  "${published_root}/scripts/release" \
  "${published_bin}" \
  "${published_task_root}"
cp "${published_checker}" "${published_root}/scripts/check-github-release-assets.sh"
cat >"${published_root}/scripts/release/macos_llvm_authority.py" <<'PY'
#!/usr/bin/env python3
from pathlib import Path
import os
import sys

if sys.argv[1:2] != ["snapshot"]:
    raise SystemExit("unexpected authority command")
task_root = Path(sys.argv[sys.argv.index("--task-root") + 1])
snapshot_root = Path(sys.argv[sys.argv.index("--snapshot-root") + 1])
if task_root != Path(os.environ["EXPECTED_TASK_ROOT"]):
    raise SystemExit("published checker passed the wrong task root")
(snapshot_root / "bin").mkdir(parents=True)
for name in ("llvm-readobj", "llvm-objdump"):
    tool = snapshot_root / "bin" / name
    tool.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")
    tool.chmod(0o500)
Path(os.environ["SNAPSHOT_RECORD"]).write_text(str(snapshot_root), encoding="utf-8")
PY
cat >"${published_root}/scripts/check-release-binary-compat.sh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s' "$1" >>"${COMPAT_CALLS}"
for argument in "${@:2}"; do
  printf '\t%s' "${argument}" >>"${COMPAT_CALLS}"
done
printf '\n' >>"${COMPAT_CALLS}"
if [[ "$1" == "macos-x64" ]]; then
  [[ $# == 4 ]]
  snapshot="$(cat "${SNAPSHOT_RECORD}")"
  [[ "$3" == "${snapshot}/bin/llvm-readobj" ]]
  [[ "$4" == "${snapshot}/bin/llvm-objdump" ]]
else
  [[ $# == 2 ]]
fi
EOF
cat >"${published_bin}/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
touch "${GH_CALL_MARKER}"
case "${1:-}:${2:-}" in
  release:view)
    cat <<'ASSETS'
ctx-linux-aarch64
ctx-linux-aarch64.cdx.json
ctx-linux-aarch64.third-party-notices.txt
ctx-linux-x64
ctx-linux-x64.cdx.json
ctx-linux-x64.third-party-notices.txt
ctx-macos-arm64
ctx-macos-arm64.cdx.json
ctx-macos-arm64.third-party-notices.txt
ctx-macos-x64
ctx-macos-x64.cdx.json
ctx-macos-x64.third-party-notices.txt
ctx-onnxruntime-linux-aarch64.tar.gz
ctx-onnxruntime-linux-x64.tar.gz
ctx-onnxruntime-macos-arm64.tar.gz
ctx-onnxruntime-macos-x64.tar.gz
ctx-onnxruntime-windows-x64.zip
ctx-windows-x64.exe
ctx-windows-x64.exe.cdx.json
ctx-windows-x64.exe.third-party-notices.txt
SHA256SUMS
ASSETS
    ;;
  release:download)
    shift 2
    output_dir=""
    pattern=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --dir) output_dir="$2"; shift 2 ;;
        --pattern) pattern="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    [[ -n "${output_dir}" && -n "${pattern}" ]]
    if [[ "${pattern}" == "SHA256SUMS" ]]; then
      (
        cd "${output_dir}"
        for asset in *; do
          [[ "${asset}" == "SHA256SUMS" ]] && continue
          sha256sum "${asset}"
        done
      ) >"${output_dir}/SHA256SUMS"
    else
      printf 'published fixture: %s\n' "${pattern}" >"${output_dir}/${pattern}"
    fi
    ;;
  *)
    exit 64
    ;;
esac
EOF
chmod 700 \
  "${published_root}/scripts/check-github-release-assets.sh" \
  "${published_root}/scripts/check-release-binary-compat.sh" \
  "${published_bin}/gh"

if PATH="${published_bin}:/usr/bin:/bin" \
  GH_CALL_MARKER="${published_gh_marker}" \
  "${published_root}/scripts/check-github-release-assets.sh" \
  v1.0.0 ctxrs/ctx \
  >"${tmp}/published-omitted.out" 2>"${tmp}/published-omitted.err"; then
  echo "published macos-x64 checker accepted an omitted task authority" >&2
  exit 1
fi
grep -Fq \
  'published macos-x64 validation requires --macos-llvm-task-root' \
  "${tmp}/published-omitted.err"
[[ ! -e "${published_gh_marker}" ]] || {
  echo "published checker reached GitHub before rejecting omitted authority" >&2
  exit 1
}

PATH="${published_bin}:/usr/bin:/bin" \
  EXPECTED_TASK_ROOT="${published_task_root}" \
  SNAPSHOT_RECORD="${published_snapshot_record}" \
  COMPAT_CALLS="${published_calls}" \
  GH_CALL_MARKER="${published_gh_marker}" \
  "${published_root}/scripts/check-github-release-assets.sh" \
  --macos-llvm-task-root "${published_task_root}" \
  v1.0.0 ctxrs/ctx \
  >"${tmp}/published.out" 2>"${tmp}/published.err"
grep -Fq 'GitHub release assets ok: ctxrs/ctx v1.0.0' "${tmp}/published.out"
python3 - "${published_calls}" <<'PY'
from pathlib import Path
import sys

calls = [line.split("\t") for line in Path(sys.argv[1]).read_text().splitlines()]
assert [call[0] for call in calls] == [
    "linux-aarch64",
    "linux-x64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
]
macos = calls[3]
assert len(macos) == 4, macos
snapshot = Path(macos[2]).parent.parent
assert snapshot.name == ".macos-llvm-authority"
assert macos[2] == str(snapshot / "bin/llvm-readobj")
assert macos[3] == str(snapshot / "bin/llvm-objdump")
assert not any(
    marker in value
    for call in calls
    for value in call[2:]
    for marker in ("/usr/local", "/opt/homebrew", "/usr/bin/llvm")
)
PY

printf 'release binary compatibility tests passed\n'
