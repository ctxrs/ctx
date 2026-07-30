#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
export CTX_PUBLIC_RELEASE_SOURCE_COMMIT
CTX_PUBLIC_RELEASE_SOURCE_COMMIT="ffffffffffffffffffffffffffffffffffffffff"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

release_contract_root="${tmp_dir}/release-contract-root"
mkdir -p "${release_contract_root}/contracts" "${release_contract_root}/scripts"
install -m 0755 \
  scripts/check-public-cli-build-info.py \
  scripts/stage-github-release-assets.sh \
  "${release_contract_root}/scripts"
cp -L contracts/release-targets-v1.json \
  "${release_contract_root}/contracts/release-targets-v1.json"
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
  --source-commit "${CTX_PUBLIC_RELEASE_SOURCE_COMMIT}" \
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
test "$(scripts/public-cli-runtime-authority.sh linux-aarch64 Linux aarch64 passed aarch64 0 generic none present 1)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-aarch64 Linux aarch64 passed aarch64 0 generic qemu-user present 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Linux x86_64 passed x86_64 0 generic none absent 1)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh windows-x64 Windows_NT AMD64 passed X64 0 generic none present 1)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh freebsd-x64 FreeBSD amd64 passed amd64 0 generic none present 1)" = authoritative
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
printf '1\n'
EOF
chmod +x \
  "${tmp_dir}/native-sysctl" \
  "${tmp_dir}/rosetta-sysctl" \
  "${tmp_dir}/inconsistent-sysctl" \
  "${tmp_dir}/blank-sysctl" \
  "${tmp_dir}/fixture-ioreg" \
  "${tmp_dir}/fixture-kvm-ioreg" \
  "${tmp_dir}/fixture-system-profiler" \
  "${tmp_dir}/fixture-powershell"
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
  'required ONNX Runtime sidecar missing:' \
  "${tmp_dir}/partial-runtime.err"
grep -Fq \
  'ctx-onnxruntime-macos-x64.tar.gz' \
  "${tmp_dir}/partial-runtime.err"

multiline_cross_output='cross 0.2.5
rustup 1.28.2
cargo 1.97.1'
test "$(printf '%s\n' "${multiline_cross_output}" | sed -n '1p')" = 'cross 0.2.5'
test "$(printf '%s\n' 'cross 0.2.4' 'rustup 1.28.2' | sed -n '1p')" != 'cross 0.2.5'
bash scripts/tests/public-cli-freebsd-build-strategy-test.sh
python3 scripts/tests/onnxruntime-sidecar-tools-test.py

for platform in windows-x64 freebsd-x64; do
  if env CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD=1 \
    scripts/build-public-cli-artifact.sh "${platform}" \
    >"${tmp_dir}/${platform}-dirty-override.out" \
    2>"${tmp_dir}/${platform}-dirty-override.err"; then
    printf '%s construction accepted the dirty-build override\n' "${platform}" >&2
    exit 1
  fi
  grep -Fq \
    'forbidden public release environment variable: CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD' \
    "${tmp_dir}/${platform}-dirty-override.err"
done

mkdir -p "${tmp_dir}/dirty-path"
cat > "${tmp_dir}/dirty-path/git" <<'EOF'
#!/bin/sh
case "${1:-}" in
  rev-parse) printf '%s\n' 0123456789abcdef0123456789abcdef01234567 ;;
  status) printf '%s\n' '?? synthetic-dirty-file' ;;
  *) exit 2 ;;
esac
EOF
chmod +x "${tmp_dir}/dirty-path/git"
dirty_out="target/ctx-release-dirty-test.$$"
hostile_tool_out="target/ctx-release-tool-override-test.$$"
trap 'rm -rf "${tmp_dir}" "${dirty_out}" "${hostile_tool_out}"' EXIT
mkdir -p "${dirty_out}"
printf 'stale evidence\n' > "${dirty_out}/ctx.exe.build-info.json"
if PATH="${tmp_dir}/dirty-path:${PATH}" \
  CTX_PUBLIC_CLI_ARTIFACT_DIR="${dirty_out}" \
  scripts/build-public-cli-artifact.sh windows-x64 \
  >"${tmp_dir}/dirty.out" 2>"${tmp_dir}/dirty.err"; then
  echo "non-Linux construction accepted a dirty source tree" >&2
  exit 1
fi
grep -Fq 'public release construction requires a clean checkout' "${tmp_dir}/dirty.err"
grep -Fxq 'stale evidence' "${dirty_out}/ctx.exe.build-info.json"

for override in CTX_LLVM_READOBJ CTX_LLVM_OBJDUMP; do
  for platform in \
    linux-x64 linux-aarch64 macos-arm64 macos-x64 windows-x64 freebsd-x64; do
    hostile_output="${hostile_tool_out}/${override}/${platform}"
    if env \
      "${override}=${tmp_dir}/forged-llvm-tool" \
      CTX_PUBLIC_CLI_ARTIFACT_DIR="${hostile_output}" \
      scripts/build-public-cli-artifact.sh "${platform}" \
      >"${tmp_dir}/${override}-${platform}.out" \
      2>"${tmp_dir}/${override}-${platform}.err"; then
      printf '%s construction accepted %s\n' "${platform}" "${override}" >&2
      exit 1
    fi
    grep -Fq \
      "forbidden public release environment variable: ${override}" \
      "${tmp_dir}/${override}-${platform}.err"
    if [[ -e "${hostile_output}" ]]; then
      printf '%s construction created output before rejecting %s\n' \
        "${platform}" "${override}" >&2
      exit 1
    fi
  done
done

inspector_parent="${tmp_dir}/mode-0700-parent"
inspector_source="${inspector_parent}/source"
inspector_artifacts="${inspector_source}/target/public-cli-artifacts"
mkdir -p \
  "${inspector_source}/contracts" \
  "${inspector_source}/scripts" \
  "${inspector_source}/tests/fixtures/custom-history-jsonl" \
  "${inspector_artifacts}"
chmod 0700 "${inspector_parent}" "${inspector_source}"
for source in check-public-cli-artifact.sh check-release-binary-compat.sh run-native-candidate-smoke.sh; do
  printf '#!/bin/sh\nexit 0\n' >"${inspector_source}/scripts/${source}"
done
printf '{}\n' >"${inspector_source}/contracts/public-control-surface-v1.json"
printf '{}\n' >"${inspector_source}/tests/fixtures/custom-history-jsonl/basic.jsonl"
printf 'candidate\n' >"${inspector_artifacts}/ctx"
printf '%064d\n' 0 >"${inspector_artifacts}/ctx.sha256"
printf 'ctx 1.0.0\n' >"${inspector_artifacts}/ctx.version"
inspector_output="${tmp_dir}/inspector-output"
mkdir "${inspector_output}"
scripts/stage-public-cli-inspector-inputs.sh \
  "${inspector_source}" "${inspector_artifacts}" ctx "${inspector_output}" >/dev/null
test -x "${inspector_output}/artifacts/ctx"
test "$(stat -c '%a' "${inspector_output}")" = 755
test "$(stat -c '%a' "${inspector_output}/contracts/public-control-surface-v1.json")" = 444
test "$(stat -c '%a' "${inspector_output}/tests/fixtures/custom-history-jsonl/basic.jsonl")" = 444

mv "${inspector_artifacts}/ctx" "${inspector_artifacts}/real-ctx"
ln -s real-ctx "${inspector_artifacts}/ctx"
mkdir "${tmp_dir}/inspector-symlink-output"
if scripts/stage-public-cli-inspector-inputs.sh \
  "${inspector_source}" "${inspector_artifacts}" ctx "${tmp_dir}/inspector-symlink-output" \
  >"${tmp_dir}/inspector-symlink.out" 2>"${tmp_dir}/inspector-symlink.err"; then
  echo "inspector staging accepted a symlink artifact" >&2
  exit 1
fi
grep -Fq 'non-symlink' "${tmp_dir}/inspector-symlink.err"
rm "${inspector_artifacts}/ctx"
mv "${inspector_artifacts}/real-ctx" "${inspector_artifacts}/ctx"

mkdir -p "${tmp_dir}/outside-artifacts" "${tmp_dir}/inspector-escape-output"
if scripts/stage-public-cli-inspector-inputs.sh \
  "${inspector_source}" "${tmp_dir}/outside-artifacts" ctx "${tmp_dir}/inspector-escape-output" \
  >"${tmp_dir}/inspector-escape.out" 2>"${tmp_dir}/inspector-escape.err"; then
  echo "inspector staging accepted an artifact root escape" >&2
  exit 1
fi
grep -Fq 'escapes the source snapshot' "${tmp_dir}/inspector-escape.err"

ln -s "${tmp_dir}/outside-artifacts" "${tmp_dir}/inspector-output-link"
if scripts/stage-public-cli-inspector-inputs.sh \
  "${inspector_source}" "${inspector_artifacts}" ctx "${tmp_dir}/inspector-output-link" \
  >"${tmp_dir}/inspector-output-link.out" 2>"${tmp_dir}/inspector-output-link.err"; then
  echo "inspector staging accepted a symlink output root" >&2
  exit 1
fi
grep -Fq 'empty non-symlink' "${tmp_dir}/inspector-output-link.err"

grep -F '20260701T000000Z' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'ubuntu:22.04@sha256:' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'GLIBC_BASELINE="2.35"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'org.ctx.release.glibc-baseline="${GLIBC_BASELINE}"' \
  scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'RUSTUP_VERSION="1.28.2"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'LINUX_GLIBC_BASELINE="2.35"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'LINUX_RELEASE_IMAGE_UBUNTU="22.04"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'LINUX_RELEASE_UBUNTU_DIGEST="sha256:0e0a0fc6d18feda9db1590da249ac93e8d5abfea8f4c3c0c849ce512b5ef8982"' \
  scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'LINUX_RELEASE_UBUNTU_SNAPSHOT="20260701T000000Z"' \
  scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'RUST_TOOLCHAIN_VERSION="1.97.1"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'RUST_TOOLCHAIN_COMMIT="8bab26f4f68e0e26f0bb7960be334d5b520ea452"' \
  scripts/build-public-cli-artifact.sh >/dev/null
grep -A5 -F '[profile.release]' Cargo.toml | grep -F 'strip = "symbols"' >/dev/null
grep -F 'rustup target add --toolchain "${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'cargo "+${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '-e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'public release construction requires a clean checkout' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'source commit changed during public release construction' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'linux-*' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--network none' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'scripts/run-native-candidate-smoke.sh' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'CTX_PRO_HELPER="${untrusted_helper}"' scripts/run-native-candidate-smoke.sh >/dev/null
grep -F 'pro_helper_override_ignored' scripts/run-native-candidate-smoke.sh >/dev/null
grep -F 'LINUX_X64_QEMU_CPU_PROFILE="qemu64"' scripts/build-public-cli-artifact.sh >/dev/null
if grep -Fq 'CTX_TEST_ONLY_ALLOW_EMULATED_LINUX_BUILD' \
  scripts/build-public-cli-artifact.sh; then
  echo "production Linux builder still contains an emulation override" >&2
  exit 1
fi
if sed -n '/^run_linux_container_build()/,/^}/p' \
  scripts/build-public-cli-artifact.sh \
  | grep -Fq 'CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD'; then
  echo "production Linux builder still contains a dirty-source override" >&2
  exit 1
fi
test "$(grep -Fc 'CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD' \
  scripts/build-public-cli-artifact.sh)" = 2
dirty_override_guard_line="$(grep -n \
  'CTX_TEST_ONLY_ALLOW_DIRTY_RELEASE_BUILD+x' \
  scripts/build-public-cli-artifact.sh | cut -d: -f1)"
platform_dispatch_line="$(grep -n '^case "${platform}" in' \
  scripts/build-public-cli-artifact.sh | head -n 1 | cut -d: -f1)"
test "${dirty_override_guard_line}" -lt "${platform_dispatch_line}"
for override in CTX_LLVM_READOBJ CTX_LLVM_OBJDUMP; do
  override_guard_line="$(grep -n "${override}+x" \
    scripts/build-public-cli-artifact.sh | cut -d: -f1)"
  test "${override_guard_line}" -lt "${platform_dispatch_line}"
done
grep -F 'LLVM_TOOL_ROOT="$(authoritative_llvm_root)"' \
  scripts/check-release-binary-compat.sh >/dev/null
if grep -Eq 'CTX_LLVM_(READOBJ|OBJDUMP):-' \
  scripts/check-release-binary-compat.sh; then
  echo "production release compatibility checker retains a tool override" >&2
  exit 1
fi
grep -F 'flock -n' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'local_runtime_authority' scripts/write-public-cli-build-info.py >/dev/null
grep -F 'linux-*|freebsd-x64)' scripts/smoke-daemon-semantic-release.sh >/dev/null
grep -F 'require_authoritative=1' scripts/smoke-daemon-semantic-release.sh >/dev/null
grep -F 'semantic smoke requires authoritative native' \
  scripts/smoke-daemon-semantic-release.sh >/dev/null
grep -F -- '--source-commit "${source_commit}"' \
  scripts/stage-github-release-assets.sh >/dev/null
grep -F 'verify_and_stage_cli_evidence ctx-freebsd-x64 ctx-freebsd-x64 freebsd-x64' \
  scripts/stage-github-release-assets.sh >/dev/null
grep -F 'required ONNX Runtime sidecar missing' scripts/stage-github-release-assets.sh >/dev/null
grep -F 'ctx-onnxruntime-freebsd-x64.tar.gz' scripts/check-github-release-assets.sh >/dev/null
grep -F 'ctx-onnxruntime-macos-x64.tar.gz' scripts/check-github-release-assets.sh >/dev/null
test "$(grep -Fc -- '--skip_tests --skip_submodule_sync' \
  scripts/onnxruntime-sidecar/build_macos_x64.sh)" = 1
grep -F -- '--expected-builder-base "${LINUX_RELEASE_UBUNTU_DIGEST}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--actual-builder-base "${actual_base_digest}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--runtime-image-id "${runtime_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--inspector-image-id "${inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--inspector-image-id "${artifact_inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'build-info.json' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--locked --offline' scripts/build-linux-release-offline.sh >/dev/null
grep -F 'bash scripts/check-linux-release-environment.sh' \
  scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'bash scripts/check-linux-release-environment.sh' \
  scripts/build-linux-release-offline.sh >/dev/null
grep -F 'bash scripts/check-linux-release-builder.sh "${target}"' \
  scripts/build-linux-release-offline.sh >/dev/null
grep -F '/usr/bin/python3 -I scripts/check-linux-release-network-isolation.py' \
  scripts/build-linux-release-offline.sh >/dev/null
grep -F 'bash scripts/build-linux-release-offline.sh "${platform}" "${target}"' \
  scripts/build-public-cli-artifact.sh >/dev/null
bash scripts/tests/check-linux-release-builder-test.sh
python3 scripts/tests/check-linux-release-network-isolation-test.py \
  scripts/check-linux-release-network-isolation.py \
  scripts/tests/fixtures/linux-release-network-isolation.json
grep -F "cross --version | sed -n '1p'" scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'native-freebsd)' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'release_cargo build -p ctx --release --target "${target}" --locked' \
  scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'linux-cross)' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'RUSTUP_TOOLCHAIN="${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F "cargo-zigbuild --version | sed -n '1p'" scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'run_host_artifact_check' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'stage-public-cli-inspector-inputs.sh' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--target runtime' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--target inspector' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'org.ctx.release.role="runtime"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'runtime tool missing' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F '"${runtime_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F '"${inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'timeout --signal=KILL 120s' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'x86_64-unknown-freebsd:0.2.5@sha256:' Cross.toml >/dev/null
grep -F '[System.IO.File]::WriteAllText(' scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F 'function Get-BoundWindowsBuildInfoSha256' \
  scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F 'if ($RequireAuthoritative -and $runtimeAuthority -cne "authoritative")' \
  scripts/smoke-daemon-semantic-release.ps1 >/dev/null
if grep -Eq 'ProofOutput|native-runtime-proof|packaged-runtime-proof' \
  scripts/smoke-daemon-semantic-release.ps1 \
  scripts/smoke-daemon-semantic-release.sh \
  scripts/stage-github-release-assets.sh; then
  echo 'retired proof output remains in native release scripts' >&2
  exit 1
fi
grep -F 'param([string[]]$CommandArgs)' scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F '@CommandArgs' scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F 'scripts/test-windows-semantic-smoke-contract.ps1' .buildkite/pipeline.yml >/dev/null
grep -F 'scripts/test-windows-runtime-upgrade-extractor.ps1' .buildkite/pipeline.yml >/dev/null
grep -F 'scripts/tests/run-native-candidate-smoke-test.ps1' .buildkite/pipeline.yml >/dev/null
grep -F 'scripts/buildkite-public-ci.sh --mode=ci' .buildkite/pipeline.yml >/dev/null
sed -n '/key: "public-cli-windows-x64-native-smoke"/,/timeout_in_minutes:/p' \
  .buildkite/pipeline.yml \
  | grep -F 'queue: "windows-x64"' >/dev/null
test -f scripts/test-windows-semantic-smoke-contract.ps1
test -f scripts/test-windows-runtime-upgrade-extractor.ps1
if grep -Fq 'param([string[]]$Args)' scripts/smoke-daemon-semantic-release.ps1; then
  echo 'Windows semantic smoke reused the reserved PowerShell $Args variable' >&2
  exit 1
fi

printf 'Linux release construction self-test passed\n'
