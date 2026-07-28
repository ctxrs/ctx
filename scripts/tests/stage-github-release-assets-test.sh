#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
stage="${repo_root}/scripts/stage-github-release-assets.sh"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-stage-assets-test.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

fake_bin="${tmp_dir}/bin"
matrix="${tmp_dir}/matrix"
mkdir -p "${fake_bin}" "${matrix}"

cat > "${fake_bin}/bash" <<'SH'
#!/bin/sh
exit 0
SH
cat > "${fake_bin}/python3" <<'SH'
#!/bin/sh
case "$*" in
  *release-sbom.py\ verify-bundle*)
    printf '%s\n' "$*" >> "${CTX_FAKE_SBOM_LOG:?}"
    ;;
  *check-public-cli-build-info.py*)
    printf '%064d\n' 0
    ;;
  -\ *.build-info.json\ *)
    printf '%040d\n' 0
    ;;
esac
SH
chmod +x "${fake_bin}/bash" "${fake_bin}/python3"

cli_sources=(
  ctx
  ctx-linux-aarch64
  ctx-macos-arm64
  ctx-macos-x64
  ctx.exe
  ctx-freebsd-x64
)
legacy_runtimes=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
  ctx-onnxruntime-freebsd-x64.tar.gz
)
native_runtimes=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-windowsml-windows-x64.zip
  ctx-onnxruntime-freebsd-x64.tar.gz
)
semantic_runtimes=(
  ctx-onnxruntime-linux-x64.tar.zst
  ctx-onnxruntime-linux-aarch64.tar.zst
  ctx-onnxruntime-macos-arm64.tar.zst
  ctx-onnxruntime-macos-x64.tar.zst
  ctx-windowsml-windows-x64.zip
  ctx-onnxruntime-freebsd-x64.tar.zst
)
extra_semantic_assets=(
  ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz
  ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz
  ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz
  ctx-onnxruntime-linux-x64-cuda12.tar.zst
)

for asset in \
  "${cli_sources[@]}" \
  "${legacy_runtimes[@]}" \
  "${native_runtimes[@]}" \
  "${semantic_runtimes[@]}" \
  "${extra_semantic_assets[@]}"
do
  printf 'synthetic %s\n' "${asset}" > "${matrix}/${asset}"
  sha256sum "${matrix}/${asset}" | awk '{print $1}' > "${matrix}/${asset}.sha256"
done

for binary in "${cli_sources[@]}"; do
  printf '{}\n' > "${matrix}/${binary}.build-info.json"
  printf '{}\n' > "${matrix}/${binary}.cdx.json"
  sha256sum "${matrix}/${binary}.cdx.json" \
    | awk '{print $1}' > "${matrix}/${binary}.cdx.json.sha256"
  printf 'third-party notices\n' \
    > "${matrix}/${binary}.third-party-notices.txt"
  sha256sum "${matrix}/${binary}.third-party-notices.txt" \
    | awk '{print $1}' > "${matrix}/${binary}.third-party-notices.txt.sha256"
  printf '{}\n' > "${matrix}/${binary}.size.json"
  printf '{}\n' > "${matrix}/${binary}.candidate.json"
done
for platform in macos-arm64 macos-x64; do
  printf '{}\n' > "${matrix}/ctx-${platform}.signing.json"
  printf '{}\n' > "${matrix}/ctx-onnxruntime-${platform}.signing.json"
done

write_proof() {
  local platform="$1"
  local binary="$2"
  local proof="$3"
  local host_system="$4"
  local host_arch="$5"
  local runtime_asset="$6"
  local native_probe="$7"
  local runtime="${8:-onnxruntime}"
  local embedding_backend="${9:-cpu}"
  local canary_line="${10:-}"
  local binary_sha runtime_sha build_info_line=""

  binary_sha="$(sha256sum "${matrix}/${binary}" | awk '{print $1}')"
  runtime_sha="$(sha256sum "${matrix}/${runtime_asset}" | awk '{print $1}')"
  case "${platform}" in
    linux-*|windows-x64|freebsd-x64)
      build_info_line="build_info_sha256=$(printf '%064d' 0)"
      ;;
  esac
  cat > "${matrix}/${proof}" <<EOF
runtime=${runtime}
embedding_backend=${embedding_backend}
platform=${platform}
host_system=${host_system}
host_arch=${host_arch}
host_native_arch=${host_arch}
process_translated=0
native_arch_probe=${native_probe}
runtime_authority=authoritative
artifact_sha256=${binary_sha}
${build_info_line}
runtime_archive_sha256=${runtime_sha}
${canary_line}
semantic_search=passed
EOF
}

write_proof linux-x64 ctx ctx-linux-x64.native-runtime-proof.txt \
  Linux x86_64 ctx-onnxruntime-linux-x64.tar.gz uname
write_proof linux-aarch64 ctx-linux-aarch64 \
  ctx-linux-aarch64.native-runtime-proof.txt \
  Linux aarch64 ctx-onnxruntime-linux-aarch64.tar.gz uname
write_proof macos-arm64 ctx-macos-arm64 \
  ctx-macos-arm64.native-runtime-proof.txt \
  Darwin arm64 ctx-onnxruntime-macos-arm64.tar.gz sysctl
write_proof macos-x64 ctx-macos-x64 ctx-macos-x64.native-runtime-proof.txt \
  Darwin x86_64 ctx-onnxruntime-macos-x64.tar.gz sysctl
write_proof freebsd-x64 ctx-freebsd-x64 \
  ctx-freebsd-x64.native-runtime-proof.txt \
  FreeBSD amd64 ctx-onnxruntime-freebsd-x64.tar.gz uname

write_legacy_windows_proof() {
  write_proof windows-x64 ctx.exe ctx-windows-x64.native-runtime-proof.txt \
    Windows_NT AMD64 ctx-onnxruntime-windows-x64.zip iswow64process2
  cat >> "${matrix}/ctx-windows-x64.native-runtime-proof.txt" <<'EOF'
runtime_dylib=C:\ctx-runtime\onnxruntime\1.27.0\windows-x64\lib\onnxruntime.dll
runtime_dependency_msvcp140=C:\ctx-runtime\onnxruntime\1.27.0\windows-x64\lib\msvcp140.dll
runtime_dependency_msvcp140_1=C:\ctx-runtime\onnxruntime\1.27.0\windows-x64\lib\msvcp140_1.dll
runtime_dependency_vcruntime140=C:\ctx-runtime\onnxruntime\1.27.0\windows-x64\lib\vcruntime140.dll
runtime_dependency_vcruntime140_1=C:\ctx-runtime\onnxruntime\1.27.0\windows-x64\lib\vcruntime140_1.dll
EOF
}

write_windowsml_proof() {
  write_proof windows-x64 ctx.exe \
    ctx-windows-x64.windowsml-native-runtime-proof.txt \
    Windows_NT AMD64 ctx-windowsml-windows-x64.zip iswow64process2 \
    windows-ml windows-ml semantic_contract_canary=passed
}

write_legacy_windows_proof

default_assets=(
  ctx-freebsd-x64
  ctx-linux-aarch64
  ctx-linux-x64
  ctx-macos-arm64
  ctx-macos-x64
  ctx-onnxruntime-freebsd-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
  ctx-windows-x64.exe
)
cli_evidence_assets=(
  ctx-freebsd-x64.cdx.json
  ctx-freebsd-x64.third-party-notices.txt
  ctx-linux-aarch64.cdx.json
  ctx-linux-aarch64.third-party-notices.txt
  ctx-linux-x64.cdx.json
  ctx-linux-x64.third-party-notices.txt
  ctx-macos-arm64.cdx.json
  ctx-macos-arm64.third-party-notices.txt
  ctx-macos-x64.cdx.json
  ctx-macos-x64.third-party-notices.txt
  ctx-windows-x64.exe.cdx.json
  ctx-windows-x64.exe.third-party-notices.txt
)
default_assets+=("${cli_evidence_assets[@]}")
native_assets=(
  ctx-freebsd-x64
  ctx-linux-aarch64
  ctx-linux-x64
  ctx-macos-arm64
  ctx-macos-x64
  ctx-onnxruntime-freebsd-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-windows-x64.exe
  ctx-windowsml-windows-x64.zip
)
native_assets+=("${cli_evidence_assets[@]}")
semantic_assets=(
  "${default_assets[@]}"
  "${semantic_runtimes[@]}"
  "${extra_semantic_assets[@]}"
)

assert_exact_assets() {
  local output="$1"
  local expected_count="$2"
  shift 2
  local expected="${tmp_dir}/expected.txt"
  local actual="${tmp_dir}/actual.txt"

  printf '%s\n' "$@" | sort > "${expected}"
  awk '{print $2}' "${output}/SHA256SUMS" | sort > "${actual}"
  test "$(wc -l < "${actual}")" -eq "${expected_count}"
  cmp "${expected}" "${actual}"
  find "${output}" -maxdepth 1 -type f ! -name SHA256SUMS \
    -printf '%f\n' | sort > "${actual}"
  cmp "${expected}" "${actual}"
}

default_output="${tmp_dir}/default"
default_sbom_log="${tmp_dir}/default-sbom.log"
CTX_FAKE_SBOM_LOG="${default_sbom_log}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${default_output}"
assert_exact_assets "${default_output}" 24 "${default_assets[@]}"
test "$(wc -l < "${default_sbom_log}")" -eq 6

semantic_output="${tmp_dir}/semantic"
CTX_FAKE_SBOM_LOG="${tmp_dir}/semantic-sbom.log" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  --with-semantic "${matrix}" "${semantic_output}"
assert_exact_assets "${semantic_output}" 34 "${semantic_assets[@]}"

native_output="${tmp_dir}/native"
cp "${matrix}/ctx-windows-x64.native-runtime-proof.txt" \
  "${matrix}/ctx-windows-x64.windowsml-native-runtime-proof.txt"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/native-invalid-sbom.log" \
  PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  --native-candidate "${matrix}" "${native_output}-legacy-proof" \
  >"${tmp_dir}/native-legacy-proof.out" 2>"${tmp_dir}/native-legacy-proof.err"
then
  printf 'native staging accepted the legacy Windows runtime proof\n' >&2
  exit 1
fi
grep -Fq 'runtime proof has wrong runtime' "${tmp_dir}/native-legacy-proof.err"
rm "${matrix}/ctx-windows-x64.windowsml-native-runtime-proof.txt"

write_windowsml_proof
CTX_FAKE_SBOM_LOG="${tmp_dir}/native-sbom.log" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  --native-candidate "${matrix}" "${native_output}"
assert_exact_assets "${native_output}" 24 "${native_assets[@]}"
test ! -e "${native_output}/ctx-onnxruntime-windows-x64.zip"

semantic_with_both_output="${tmp_dir}/semantic-with-both-proofs"
CTX_FAKE_SBOM_LOG="${tmp_dir}/semantic-both-sbom.log" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  --with-semantic "${matrix}" "${semantic_with_both_output}"
assert_exact_assets "${semantic_with_both_output}" 34 "${semantic_assets[@]}"

cp "${matrix}/ctx-windows-x64.windowsml-native-runtime-proof.txt" \
  "${tmp_dir}/valid-windowsml-proof.txt"
sed -i 's/^semantic_contract_canary=passed$/semantic_contract_canary=failed/' \
  "${matrix}/ctx-windows-x64.windowsml-native-runtime-proof.txt"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/native-mutation-sbom.log" \
  PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  --native-candidate "${matrix}" "${native_output}-canary-mutation" \
  >"${tmp_dir}/native-canary-mutation.out" 2>"${tmp_dir}/native-canary-mutation.err"
then
  printf 'native staging accepted a failed Windows ML contract canary\n' >&2
  exit 1
fi
grep -Fq 'did not pass the semantic contract canary' \
  "${tmp_dir}/native-canary-mutation.err"
cp "${tmp_dir}/valid-windowsml-proof.txt" \
  "${matrix}/ctx-windows-x64.windowsml-native-runtime-proof.txt"

printf 'mutated Windows ML bytes\n' >> "${matrix}/ctx-windowsml-windows-x64.zip"
sha256sum "${matrix}/ctx-windowsml-windows-x64.zip" | awk '{print $1}' \
  > "${matrix}/ctx-windowsml-windows-x64.zip.sha256"
if PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  --native-candidate "${matrix}" "${native_output}-archive-mutation" \
  >"${tmp_dir}/native-archive-mutation.out" 2>"${tmp_dir}/native-archive-mutation.err"
then
  printf 'native staging accepted Windows ML bytes not bound by the proof\n' >&2
  exit 1
fi
grep -Fq 'runtime proof does not match the exact runtime sidecar' \
  "${tmp_dir}/native-archive-mutation.err"

printf 'synthetic %s\n' ctx-windowsml-windows-x64.zip \
  > "${matrix}/ctx-windowsml-windows-x64.zip"
sha256sum "${matrix}/ctx-windowsml-windows-x64.zip" | awk '{print $1}' \
  > "${matrix}/ctx-windowsml-windows-x64.zip.sha256"

if PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  --native-candidate --with-semantic "${tmp_dir}/invalid" \
  >"${tmp_dir}/combined.out" 2>"${tmp_dir}/combined.err"
then
  printf 'combined staging modes unexpectedly succeeded\n' >&2
  exit 1
fi
grep -Fq 'staging modes cannot be combined' "${tmp_dir}/combined.err"

if PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" --unknown \
  >"${tmp_dir}/unknown.out" 2>"${tmp_dir}/unknown.err"
then
  printf 'unknown staging mode unexpectedly succeeded\n' >&2
  exit 1
fi
grep -Fq 'unknown staging mode: --unknown' "${tmp_dir}/unknown.err"

printf 'GitHub release staging mode contracts passed\n'
