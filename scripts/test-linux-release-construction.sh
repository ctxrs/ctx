#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
mismatched_source_commit="ffffffffffffffffffffffffffffffffffffffff"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

release_contract_root="${tmp_dir}/release-contract-root"
mkdir -p \
  "${release_contract_root}/contracts" \
  "${release_contract_root}/scripts/release"
install -m 0755 \
  scripts/check-public-cli-build-info.py \
  scripts/stage-github-release-assets.sh \
  "${release_contract_root}/scripts"
install -m 0755 scripts/release/release_bundle.py \
  "${release_contract_root}/scripts/release"
cp -L contracts/release-targets-v1.json \
  "${release_contract_root}/contracts/release-targets-v1.json"
git -C "${release_contract_root}" init -q
git -C "${release_contract_root}" add .
git -C "${release_contract_root}" \
  -c user.name='ctx release test' \
  -c user.email='ctx-release-test@example.invalid' \
  commit -qm 'create release construction fixture'
release_target_matrix="${release_contract_root}/contracts/release-targets-v1.json"
stage_release_assets="${release_contract_root}/scripts/stage-github-release-assets.sh"
test -f "${release_target_matrix}"
test ! -L "${release_target_matrix}"

printf 'artifact\n' > "${tmp_dir}/artifact"
printf 'lock\n' > "${tmp_dir}/Cargo.lock"
build_info_args=(
  --output "${tmp_dir}/artifact.build-info.json"
  --artifact "${tmp_dir}/artifact"
  --cargo-lock "${tmp_dir}/Cargo.lock"
  --platform linux-x64
  --target x86_64-unknown-linux-gnu
  --source-commit 0123456789abcdef0123456789abcdef01234567
  --source-clean true
  --rust-version "rustc 1.97.1 (8bab26f4f 2026-07-14)"
  --expected-builder-base sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982
  --actual-builder-base sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982
  --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  --builder-recipe-sha256 dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd
  --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
  --linux-builder-image docker.io/library/ubuntu:22.04@sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982
  --linux-ubuntu-snapshot 20260701T000000Z
  --linux-glibc-max 2.35
  --linux-rust-toolchain 1.97.1
  --linux-rust-commit 8bab26f4f68e0e26f0bb7960be334d5b520ea452
  --linux-rust-sysroot /opt/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu
  --static-status passed
  --local-runtime-status passed
  --local-runtime-authority authoritative
)
python3 scripts/write-public-cli-build-info.py "${build_info_args[@]}"
first_build_info_sha="$(sha256sum "${tmp_dir}/artifact.build-info.json")"
python3 scripts/write-public-cli-build-info.py "${build_info_args[@]}"
test "${first_build_info_sha}" = "$(sha256sum "${tmp_dir}/artifact.build-info.json")"
python3 - "${tmp_dir}/artifact.build-info.json" <<'PY'
import json
import sys

document = json.load(open(sys.argv[1], encoding="utf-8"))
assert document["builder"]["base_image"] == {
    "actual": "sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982",
    "expected": "sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982",
}
assert document["builder"]["image_id"] == "sha256:" + "a" * 64
assert document["runtime"]["image_id"] == "sha256:" + "b" * 64
assert document["inspector"]["image_id"] == "sha256:" + "c" * 64
assert document["gates"]["static_abi"] == "passed"
assert document["linux_build"]["glibc_max"] == "2.35"
assert document["linux_build"]["rust_sysroot"] == (
    "/opt/rustup/toolchains/1.97.1-x86_64-unknown-linux-gnu"
)
PY
test "$(
  python3 -I scripts/check-public-cli-build-info.py \
    --artifact "${tmp_dir}/artifact" \
    --build-info "${tmp_dir}/artifact.build-info.json" \
    --matrix "${release_target_matrix}" \
    --platform linux-x64
)" = "$(sha256sum "${tmp_dir}/artifact.build-info.json" | awk '{ print $1 }')"
if python3 -I scripts/check-public-cli-build-info.py \
  --artifact "${tmp_dir}/artifact" \
  --build-info "${tmp_dir}/artifact.build-info.json" \
  --matrix "${release_target_matrix}" \
  --platform linux-x64 \
  --source-commit "${mismatched_source_commit}" \
  >"${tmp_dir}/source-mismatch.out" 2>"${tmp_dir}/source-mismatch.err"; then
  echo "build-info validator accepted an artifact from another source commit" >&2
  exit 1
fi
grep -Fq 'build-info does not bind the clean exact artifact' \
  "${tmp_dir}/source-mismatch.err"

python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/cross-artifact.build-info.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform windows-x64 \
  --target x86_64-pc-windows-gnu \
  --source-commit 0123456789abcdef0123456789abcdef01234567 \
  --source-clean true \
  --rust-version "rustc test" \
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --static-status passed \
  --local-runtime-status not_run \
  --local-runtime-authority not_run
python3 - "${tmp_dir}/cross-artifact.build-info.json" <<'PY'
import json
import sys

document = json.load(open(sys.argv[1], encoding="utf-8"))
assert document["builder"]["image_id"] is None
assert document["builder"]["base_image"] == {"actual": None, "expected": None}
assert document["runtime"]["image_id"] is None
assert document["inspector"]["image_id"] == "sha256:" + "c" * 64
assert document["linux_build"] is None
PY
test "$(
  python3 -I scripts/check-public-cli-build-info.py \
    --artifact "${tmp_dir}/artifact" \
    --build-info "${tmp_dir}/cross-artifact.build-info.json" \
    --matrix "${release_target_matrix}" \
    --platform windows-x64
)" = "$(sha256sum "${tmp_dir}/cross-artifact.build-info.json" | awk '{ print $1 }')"

python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/freebsd-artifact.build-info.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform freebsd-x64 \
  --target x86_64-unknown-freebsd \
  --source-commit 0123456789abcdef0123456789abcdef01234567 \
  --source-clean true \
  --rust-version "rustc 1.97.1 (8bab26f4f 2026-07-14)" \
  --static-status passed \
  --local-runtime-status passed \
  --local-runtime-authority authoritative
test "$(
  python3 -I scripts/check-public-cli-build-info.py \
    --artifact "${tmp_dir}/artifact" \
    --build-info "${tmp_dir}/freebsd-artifact.build-info.json" \
    --matrix "${release_target_matrix}" \
    --platform freebsd-x64
)" = "$(sha256sum "${tmp_dir}/freebsd-artifact.build-info.json" | awk '{ print $1 }')"

ln -s "${release_target_matrix}" "${tmp_dir}/release-targets-link.json"
if python3 -I scripts/check-public-cli-build-info.py \
  --artifact "${tmp_dir}/artifact" \
  --build-info "${tmp_dir}/cross-artifact.build-info.json" \
  --matrix "${tmp_dir}/release-targets-link.json" \
  --platform windows-x64 \
  >"${tmp_dir}/matrix-symlink.out" 2>"${tmp_dir}/matrix-symlink.err"; then
  echo "build-info validator accepted a symlink target matrix" >&2
  exit 1
fi
grep -Fq 'release-target matrix is not a regular file' \
  "${tmp_dir}/matrix-symlink.err"

if python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/mismatch.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform linux-x64 \
  --target x86_64-unknown-linux-gnu \
  --source-commit 0123456789abcdef \
  --source-clean true \
  --rust-version "rustc test" \
  --expected-builder-base sha256:expected \
  --actual-builder-base sha256:wrong \
  --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa \
  --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc \
  --static-status passed \
  --local-runtime-status passed \
  --local-runtime-authority authoritative \
  >/dev/null 2>&1; then
  echo "mismatched builder identity unexpectedly produced build evidence" >&2
  exit 1
fi

if python3 scripts/write-public-cli-build-info.py \
  "${build_info_args[@]}" \
  --source-clean false >/dev/null 2>&1; then
  echo "dirty Linux source unexpectedly produced build evidence" >&2
  exit 1
fi

if python3 scripts/write-public-cli-build-info.py \
  "${build_info_args[@]}" \
  --local-runtime-authority non_authoritative >/dev/null 2>&1; then
  echo "non-authoritative Linux runtime unexpectedly produced build evidence" >&2
  exit 1
fi

if python3 scripts/write-public-cli-build-info.py \
  "${build_info_args[@]}" \
  --builder-image-id not-a-digest >/dev/null 2>&1; then
  echo "invalid builder image identity unexpectedly produced build evidence" >&2
  exit 1
fi

if python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/bad-authority.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform linux-x64 \
  --target x86_64-unknown-linux-gnu \
  --source-commit 0123456789abcdef \
  --source-clean true \
  --rust-version "rustc test" \
  --static-status passed \
  --local-runtime-status not_run \
  --local-runtime-authority authoritative >/dev/null 2>&1; then
  echo "inconsistent runtime authority unexpectedly produced build evidence" >&2
  exit 1
fi

test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin arm64 passed arm64 0 apple none absent 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 apple none absent 1)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed arm64 1 apple rosetta-2 absent 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 generic qemu-kvm present 1 ctx-mac-gui-shared-x64)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 generic qemu-kvm present 1 precision-7780-macos-x64-kvm-v1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 generic qemu-kvm present 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 generic qemu-kvm present 1 arbitrary-qemu)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 1 generic qemu-kvm present 1 ctx-mac-gui-shared-x64)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 generic none absent 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed unknown unknown unknown unknown unknown 0)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-arm64 Darwin arm64 passed arm64 0 apple none absent 1)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-aarch64 Linux aarch64 passed aarch64 0 generic none present 1 "" ubuntu 22.04 unknown)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-aarch64 Linux aarch64 passed aarch64 0 generic qemu-user present 1 "" ubuntu 22.04 unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Linux x86_64 passed x86_64 0 generic none absent 1 "" ubuntu 22.04 unknown)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Linux x86_64 passed x86_64 0 generic none absent 1 "" ubuntu 24.04 unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Linux x86_64 passed x86_64 0 generic none absent 1 "" debian 22.04 unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Linux x86_64 passed x86_64 0 generic none absent 1 "" unknown unknown unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 passed X64 0 generic none present 1 "" "Microsoft Windows 11 Pro" 10.0.22631 1)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 passed X64 0 generic none present 1 "" "Microsoft Windows 11 Pro" 10.0.21999 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 passed X64 0 generic none present 1 "" "Microsoft Windows 10 Pro" 10.0.22631 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 passed X64 0 generic none present 1 "" "Microsoft Windows 11 Server" 10.0.26100 3)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 passed X64 0 generic none present 1 "" unknown unknown unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh freebsd-x64 FreeBSD amd64 passed amd64 0 generic none present 1 "" freebsd 14.4-RELEASE unknown)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh freebsd-x64 FreeBSD amd64 passed amd64 0 generic none present 1 "" freebsd 14.4-RELEASE-p3 unknown)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh freebsd-x64 FreeBSD amd64 passed amd64 0 generic none present 1 "" freebsd 14.3-RELEASE unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh freebsd-x64 FreeBSD amd64 passed amd64 0 generic none present 1 "" freebsd 14.4-STABLE unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh freebsd-x64 FreeBSD amd64 passed amd64 0 generic none present 1 "" unknown unknown unknown)" = non_authoritative
test "$(CTX_HARDWARE_IDENTITY=apple CTX_EXECUTION_EMULATION=none scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0 generic qemu-kvm present 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Darwin arm64 passed arm64 0 apple none absent 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 not_run)" = not_run
if scripts/public-cli-runtime-authority.sh macos-x64 Darwin arm64 invalid >/dev/null 2>&1; then
  echo "invalid runtime status unexpectedly produced authority" >&2
  exit 1
fi

cat > "${tmp_dir}/native-sysctl" <<'EOF'
#!/usr/bin/env bash
case "${2:-}" in
  sysctl.proc_translated) exit 1 ;;
  hw.optional.arm64) printf '0\n' ;;
  kern.hv_vmm_present) printf '0\n' ;;
  *) exit 2 ;;
esac
EOF
cat > "${tmp_dir}/rosetta-sysctl" <<'EOF'
#!/usr/bin/env bash
case "${2:-}" in
  sysctl.proc_translated|hw.optional.arm64) printf '1\n' ;;
  kern.hv_vmm_present) printf '0\n' ;;
  *) exit 2 ;;
esac
EOF
cat > "${tmp_dir}/inconsistent-sysctl" <<'EOF'
#!/usr/bin/env bash
case "${2:-}" in
  sysctl.proc_translated) printf '0\n' ;;
  hw.optional.arm64) printf '1\n' ;;
  kern.hv_vmm_present) printf '0\n' ;;
  *) exit 2 ;;
esac
EOF
cat > "${tmp_dir}/blank-sysctl" <<'EOF'
#!/usr/bin/env bash
exit 1
EOF
cat > "${tmp_dir}/fixture-ioreg" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *IOPlatformExpertDevice*) printf '"manufacturer" = <"Apple Inc.">\n' ;;
  *) printf 'Apple internal display\n' ;;
esac
EOF
cat > "${tmp_dir}/fixture-kvm-ioreg" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *IOPlatformExpertDevice*) printf '"manufacturer" = <"Apple Inc.">\n' ;;
  *) printf 'QEMU display\nvirtio-net-pci\n' ;;
esac
EOF
cat > "${tmp_dir}/fixture-system-profiler" <<'EOF'
#!/usr/bin/env bash
case "${1:-}" in
  SPHardwareDataType) printf 'Model Name: Mac Pro\n' ;;
  SPDisplaysDataType) printf 'AMD Radeon Pro\n' ;;
  *) exit 2 ;;
esac
EOF
cat > "${tmp_dir}/fixture-powershell" <<'EOF'
#!/usr/bin/env bash
case "$*" in
  *Win32_OperatingSystem*)
    printf 'Microsoft Windows 11 Pro\t10.0.22631\t1\r\n'
    ;;
  *)
    printf '1\n'
    ;;
esac
EOF
cat > "${tmp_dir}/fixture-freebsd-version" <<'EOF'
#!/usr/bin/env bash
test "${1:-}" = "-u"
printf '14.4-RELEASE-p3\n'
EOF
chmod +x \
  "${tmp_dir}/native-sysctl" \
  "${tmp_dir}/rosetta-sysctl" \
  "${tmp_dir}/inconsistent-sysctl" \
  "${tmp_dir}/blank-sysctl" \
  "${tmp_dir}/fixture-ioreg" \
  "${tmp_dir}/fixture-kvm-ioreg" \
  "${tmp_dir}/fixture-system-profiler" \
  "${tmp_dir}/fixture-powershell" \
  "${tmp_dir}/fixture-freebsd-version"
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/native-sysctl" \
  --ioreg "${tmp_dir}/fixture-ioreg" --system-profiler "${tmp_dir}/fixture-system-profiler")" = \
  $'Darwin\tx86_64\tx86_64\t0\tsysctl\tapple\tnone\tabsent\t1'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/rosetta-sysctl" \
  --ioreg "${tmp_dir}/fixture-ioreg" --system-profiler "${tmp_dir}/fixture-system-profiler")" = \
  $'Darwin\tx86_64\tarm64\t1\tsysctl\tapple\trosetta-2\tabsent\t1'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/missing-sysctl" \
  --ioreg "${tmp_dir}/fixture-ioreg" --system-profiler "${tmp_dir}/fixture-system-profiler")" = \
  $'Darwin\tx86_64\tunknown\tunknown\tsysctl\tapple\tnone\tunknown\t0'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/blank-sysctl" \
  --ioreg "${tmp_dir}/fixture-ioreg" --system-profiler "${tmp_dir}/fixture-system-profiler")" = \
  $'Darwin\tx86_64\tx86_64\t0\tsysctl\tapple\tnone\tunknown\t0'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/inconsistent-sysctl" \
  --ioreg "${tmp_dir}/fixture-ioreg" --system-profiler "${tmp_dir}/fixture-system-profiler")" = \
  $'Darwin\tx86_64\tarm64\tunknown\tsysctl\tapple\tnone\tabsent\t0'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/native-sysctl" \
  --ioreg "${tmp_dir}/fixture-kvm-ioreg" --system-profiler "${tmp_dir}/fixture-system-profiler")" = \
  $'Darwin\tx86_64\tx86_64\t0\tsysctl\tapple\tqemu-kvm\tabsent\t1'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system MINGW64_NT-10.0 --host-arch x86_64 \
  --powershell "${tmp_dir}/fixture-powershell")" = \
  $'Windows_NT\tAMD64\tX64\t0\tuname\tgeneric\tnone\tpresent\t1'

cat > "${tmp_dir}/ubuntu-22.04-os-release" <<'EOF'
NAME="Ubuntu"
ID=ubuntu
VERSION_ID="22.04"
EOF
cat > "${tmp_dir}/ubuntu-24.04-os-release" <<'EOF'
NAME="Ubuntu"
ID=ubuntu
VERSION_ID="24.04"
EOF
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Linux --host-arch x86_64 \
  --os-release "${tmp_dir}/ubuntu-22.04-os-release" --os-baseline-only)" = \
  $'ubuntu\t22.04\tunknown'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Linux --host-arch x86_64 \
  --os-release "${tmp_dir}/ubuntu-24.04-os-release" --os-baseline-only)" = \
  $'ubuntu\t24.04\tunknown'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Linux --host-arch x86_64 \
  --os-release "${tmp_dir}/missing-os-release" --os-baseline-only)" = \
  $'unknown\tunknown\tunknown'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system FreeBSD --host-arch amd64 \
  --freebsd-version "${tmp_dir}/fixture-freebsd-version" --os-baseline-only)" = \
  $'freebsd\t14.4-RELEASE-p3\tunknown'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Windows_NT --host-arch AMD64 \
  --powershell "${tmp_dir}/fixture-powershell" --os-baseline-only)" = \
  $'Microsoft Windows 11 Pro\t10.0.22631\t1'

printf 'processor : 0\nFeatures : fp asimd aes sha2\n' > "${tmp_dir}/arm-cpuinfo"
printf '/usr/bin/ctx-pro\n' > "${tmp_dir}/arm-maps"
printf 'Amazon EC2 Graviton3\n' > "${tmp_dir}/arm-platform"
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Linux --host-arch aarch64 \
  --cpuinfo "${tmp_dir}/arm-cpuinfo" --process-maps "${tmp_dir}/arm-maps" \
  --platform-facts "${tmp_dir}/arm-platform")" = \
  $'Linux\taarch64\taarch64\t0\tuname\tgeneric\tnone\tabsent\t1'
printf '/usr/bin/qemu-aarch64-static\n' > "${tmp_dir}/arm-maps"
printf 'QEMU Virtual Machine\nlinux,dummy-virt\n' > "${tmp_dir}/arm-platform"
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Linux --host-arch aarch64 \
  --cpuinfo "${tmp_dir}/arm-cpuinfo" --process-maps "${tmp_dir}/arm-maps" \
  --platform-facts "${tmp_dir}/arm-platform")" = \
  $'Linux\taarch64\taarch64\t0\tuname\tgeneric\tqemu-user\tpresent\t1'

partial_runtime_matrix="${tmp_dir}/partial-runtime-matrix"
mkdir -p "${partial_runtime_matrix}"
touch \
  "${partial_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz" \
  "${partial_runtime_matrix}/ctx-onnxruntime-linux-aarch64.tar.gz" \
  "${partial_runtime_matrix}/ctx-onnxruntime-macos-arm64.tar.gz" \
  "${partial_runtime_matrix}/ctx-onnxruntime-windows-x64.zip"
if "${stage_release_assets}" \
  "${partial_runtime_matrix}" "${tmp_dir}/partial-release" \
  >"${tmp_dir}/partial-runtime.out" 2>"${tmp_dir}/partial-runtime.err"; then
  echo "release staging accepted an incomplete runtime matrix" >&2
  exit 1
fi
grep -Fq \
  'release completion marker is invalid: ctx-linux-x64.release-complete.json' \
  "${tmp_dir}/partial-runtime.err"

grep -F 'CTX_PRO_HELPER="${untrusted_helper}"' \
  scripts/run-native-candidate-smoke.sh >/dev/null
grep -F 'pro_helper_override_ignored' \
  scripts/run-native-candidate-smoke.sh >/dev/null
grep -F 'local_runtime_authority' scripts/write-public-cli-build-info.py >/dev/null
grep -F 'linux-*|freebsd-x64)' \
  scripts/smoke-daemon-semantic-release.sh >/dev/null
grep -F 'require_authoritative=1' \
  scripts/smoke-daemon-semantic-release.sh >/dev/null
grep -F -- '--source-commit "${source_commit}"' \
  scripts/stage-github-release-assets.sh >/dev/null
grep -F 'validate_staged_cli_evidence ctx-freebsd-x64 ctx-freebsd-x64 freebsd-x64' \
  scripts/stage-github-release-assets.sh >/dev/null
grep -F 'required ONNX Runtime sidecar' \
  scripts/stage-github-release-assets.sh >/dev/null
grep -F 'ctx-onnxruntime-freebsd-x64.tar.gz' \
  scripts/check-github-release-assets.sh >/dev/null
grep -F '[System.IO.File]::WriteAllText(' \
  scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F 'function Get-BoundWindowsBuildInfoSha256' \
  scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F 'scripts/test-windows-semantic-smoke-contract.ps1' \
  .buildkite/pipeline.yml >/dev/null
grep -F 'scripts/buildkite-public-ci.sh --mode=ci' \
  .buildkite/pipeline.yml >/dev/null

bash scripts/tests/linux-bazel-release-controller-test.sh
python3 scripts/tests/linux-bazel-controller-receipt-test.py

printf 'Linux release construction self-test passed\n'
