#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

printf 'artifact\n' > "${tmp_dir}/artifact"
printf 'lock\n' > "${tmp_dir}/Cargo.lock"
build_info_args=(
  --output "${tmp_dir}/artifact.build-info.json"
  --artifact "${tmp_dir}/artifact"
  --cargo-lock "${tmp_dir}/Cargo.lock"
  --platform linux-x64
  --target x86_64-unknown-linux-gnu
  --source-commit 0123456789abcdef
  --source-clean true
  --rust-version "rustc test"
  --expected-builder-base sha256:expected
  --actual-builder-base sha256:expected
  --builder-image-id sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  --runtime-image-id sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  --inspector-image-id sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
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
    "actual": "sha256:expected",
    "expected": "sha256:expected",
}
assert document["builder"]["image_id"] == "sha256:" + "a" * 64
assert document["runtime"]["image_id"] == "sha256:" + "b" * 64
assert document["inspector"]["image_id"] == "sha256:" + "c" * 64
PY

python3 scripts/write-public-cli-build-info.py \
  --output "${tmp_dir}/cross-artifact.build-info.json" \
  --artifact "${tmp_dir}/artifact" \
  --cargo-lock "${tmp_dir}/Cargo.lock" \
  --platform windows-x64 \
  --target x86_64-pc-windows-gnu \
  --source-commit 0123456789abcdef \
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
PY

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

test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin arm64 passed arm64 0)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed x86_64 0)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed arm64 1)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh macos-x64 Darwin x86_64 passed unknown unknown)" = non_authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Linux x86_64 passed x86_64 0)" = authoritative
test "$(scripts/public-cli-runtime-authority.sh linux-x64 Darwin arm64 passed arm64 0)" = non_authoritative
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
  *) exit 2 ;;
esac
EOF
cat > "${tmp_dir}/rosetta-sysctl" <<'EOF'
#!/usr/bin/env bash
case "${2:-}" in
  sysctl.proc_translated|hw.optional.arm64) printf '1\n' ;;
  *) exit 2 ;;
esac
EOF
cat > "${tmp_dir}/inconsistent-sysctl" <<'EOF'
#!/usr/bin/env bash
case "${2:-}" in
  sysctl.proc_translated) printf '0\n' ;;
  hw.optional.arm64) printf '1\n' ;;
  *) exit 2 ;;
esac
EOF
chmod +x \
  "${tmp_dir}/native-sysctl" \
  "${tmp_dir}/rosetta-sysctl" \
  "${tmp_dir}/inconsistent-sysctl"
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/native-sysctl")" = \
  $'Darwin\tx86_64\tx86_64\t0\tsysctl'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/rosetta-sysctl")" = \
  $'Darwin\tx86_64\tarm64\t1\tsysctl'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/missing-sysctl")" = \
  $'Darwin\tx86_64\tunknown\tunknown\tsysctl'
test "$(scripts/public-cli-host-runtime-evidence.sh \
  --host-system Darwin --host-arch x86_64 --sysctl "${tmp_dir}/inconsistent-sysctl")" = \
  $'Darwin\tx86_64\tunknown\tunknown\tsysctl'

partial_runtime_matrix="${tmp_dir}/partial-runtime-matrix"
mkdir -p "${partial_runtime_matrix}"
touch \
  "${partial_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz" \
  "${partial_runtime_matrix}/ctx-onnxruntime-linux-aarch64.tar.gz" \
  "${partial_runtime_matrix}/ctx-onnxruntime-macos-arm64.tar.gz" \
  "${partial_runtime_matrix}/ctx-onnxruntime-windows-x64.zip"
if scripts/stage-github-release-assets.sh \
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

complete_runtime_matrix="${tmp_dir}/complete-runtime-matrix"
mkdir -p "${complete_runtime_matrix}"
touch \
  "${complete_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz" \
  "${complete_runtime_matrix}/ctx-onnxruntime-linux-aarch64.tar.gz" \
  "${complete_runtime_matrix}/ctx-onnxruntime-macos-arm64.tar.gz" \
  "${complete_runtime_matrix}/ctx-onnxruntime-macos-x64.tar.gz" \
  "${complete_runtime_matrix}/ctx-onnxruntime-windows-x64.zip" \
  "${complete_runtime_matrix}/ctx-onnxruntime-freebsd-x64.tar.gz"
if scripts/stage-github-release-assets.sh \
  "${complete_runtime_matrix}" "${tmp_dir}/unproven-release" \
  >"${tmp_dir}/unproven-runtime.out" 2>"${tmp_dir}/unproven-runtime.err"; then
  echo "release staging accepted runtimes without native exact-binary proof" >&2
  exit 1
fi
grep -Fq \
  'required authoritative runtime proof missing:' \
  "${tmp_dir}/unproven-runtime.err"
grep -Fq \
  'ctx-linux-x64.native-runtime-proof.txt' \
  "${tmp_dir}/unproven-runtime.err"

mismatched_runtime_matrix="${tmp_dir}/mismatched-runtime-matrix"
cp -R "${complete_runtime_matrix}" "${mismatched_runtime_matrix}"
for binary in ctx; do
  printf 'synthetic %s\n' "${binary}" > "${mismatched_runtime_matrix}/${binary}"
  sha256sum "${mismatched_runtime_matrix}/${binary}" | awk '{ print $1 }' \
    > "${mismatched_runtime_matrix}/${binary}.sha256"
done
linux_binary_sha="$(cat "${mismatched_runtime_matrix}/ctx.sha256")"
linux_runtime_sha="$(sha256sum \
  "${mismatched_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz" | awk '{ print $1 }')"
printf '%s\n' "${linux_runtime_sha}" > \
  "${mismatched_runtime_matrix}/ctx-onnxruntime-linux-x64.tar.gz.sha256"
cat > "${mismatched_runtime_matrix}/ctx-linux-x64.native-runtime-proof.txt" <<EOF
runtime=onnxruntime
embedding_backend=cpu
platform=linux-x64
host_system=Linux
host_arch=x86_64
host_native_arch=x86_64
process_translated=0
native_arch_probe=uname
runtime_authority=authoritative
artifact_sha256=${linux_binary_sha}
runtime_archive_sha256=ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff
semantic_search=passed
EOF
if scripts/stage-github-release-assets.sh \
  "${mismatched_runtime_matrix}" "${tmp_dir}/mismatched-release" \
  >"${tmp_dir}/mismatched-runtime.out" 2>"${tmp_dir}/mismatched-runtime.err"; then
  echo "release staging accepted proof for a different runtime sidecar" >&2
  exit 1
fi
grep -Fq \
  'runtime proof does not match the exact runtime sidecar:' \
  "${tmp_dir}/mismatched-runtime.err"

duplicate_proof_matrix="${tmp_dir}/duplicate-proof-matrix"
cp -R "${mismatched_runtime_matrix}" "${duplicate_proof_matrix}"
printf 'platform=linux-x64\n' >> \
  "${duplicate_proof_matrix}/ctx-linux-x64.native-runtime-proof.txt"
if scripts/stage-github-release-assets.sh \
  "${duplicate_proof_matrix}" "${tmp_dir}/duplicate-proof-release" \
  >"${tmp_dir}/duplicate-proof.out" 2>"${tmp_dir}/duplicate-proof.err"; then
  echo "release staging accepted a proof with duplicate fields" >&2
  exit 1
fi
grep -Fq \
  'runtime proof contains duplicate field platform:' \
  "${tmp_dir}/duplicate-proof.err"

multiline_cross_output='cross 0.2.5
rustup 1.28.2
cargo 1.88.0'
test "$(printf '%s\n' "${multiline_cross_output}" | sed -n '1p')" = 'cross 0.2.5'
test "$(printf '%s\n' 'cross 0.2.4' 'rustup 1.28.2' | sed -n '1p')" != 'cross 0.2.5'

mkdir -p "${tmp_dir}/dirty-path"
cat > "${tmp_dir}/dirty-path/git" <<'EOF'
#!/bin/sh
case "${1:-}" in
  rev-parse) printf '%s\n' 0123456789abcdef ;;
  status) printf '%s\n' '?? synthetic-dirty-file' ;;
  *) exit 2 ;;
esac
EOF
chmod +x "${tmp_dir}/dirty-path/git"
dirty_out="target/ctx-release-dirty-test.$$"
trap 'rm -rf "${tmp_dir}" "${dirty_out}"' EXIT
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
test ! -e "${dirty_out}/ctx.exe.build-info.json"

grep -F '20260701T000000Z' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'ubuntu:22.04@sha256:' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'RUSTUP_VERSION="1.28.2"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'RUST_TOOLCHAIN_VERSION="1.88.0"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'rustup target add --toolchain "${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'cargo "+${RUST_TOOLCHAIN_VERSION}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '-e "CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'public release construction requires a clean checkout' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'source commit changed during public release construction' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'linux-*' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--network none' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'scripts/run-native-candidate-smoke.sh' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'LINUX_X64_QEMU_CPU_PROFILE="qemu64"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'CTX_TEST_ONLY_ALLOW_EMULATED_LINUX_BUILD' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'flock -n' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'local_runtime_authority' scripts/write-public-cli-build-info.py >/dev/null
grep -F 'required ONNX Runtime sidecar missing' scripts/stage-github-release-assets.sh >/dev/null
grep -F 'ctx-onnxruntime-freebsd-x64.tar.gz' scripts/check-github-release-assets.sh >/dev/null
grep -F 'ctx-onnxruntime-macos-x64.tar.gz' scripts/check-github-release-assets.sh >/dev/null
grep -F -- '--expected-builder-base "${LINUX_RELEASE_UBUNTU_DIGEST}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--actual-builder-base "${actual_base_digest}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--runtime-image-id "${runtime_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--inspector-image-id "${inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--inspector-image-id "${artifact_inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'build-info.json' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--locked --offline' scripts/build-linux-release-offline.sh >/dev/null
grep -F "cross --version | sed -n '1p'" scripts/build-public-cli-artifact.sh >/dev/null
grep -F "cargo-zigbuild --version | sed -n '1p'" scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'run_host_artifact_check' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--target runtime' scripts/build-public-cli-artifact.sh >/dev/null
grep -F -- '--target inspector' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'org.ctx.release.role="runtime"' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F 'runtime tool missing' scripts/docker/linux-release.Dockerfile >/dev/null
grep -F '"${runtime_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F '"${inspector_image_id}"' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'timeout --signal=KILL 120s' scripts/build-public-cli-artifact.sh >/dev/null
grep -F 'x86_64-unknown-freebsd:0.2.5@sha256:' Cross.toml >/dev/null
grep -F '[System.IO.File]::WriteAllText(' scripts/smoke-daemon-semantic-release.ps1 >/dev/null
grep -F '($runtimeProofLines -join "`n") + "`n"' scripts/smoke-daemon-semantic-release.ps1 >/dev/null

printf 'Linux release construction self-test passed\n'
