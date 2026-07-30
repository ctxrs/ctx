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
export CTX_PUBLIC_RELEASE_SOURCE_COMMIT="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

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
    printf '%s\n' "$*" >> "${CTX_FAKE_BUILD_INFO_LOG:?}"
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
default_build_info_log="${tmp_dir}/default-build-info.log"
CTX_FAKE_SBOM_LOG="${default_sbom_log}" \
  CTX_FAKE_BUILD_INFO_LOG="${default_build_info_log}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${default_output}"
assert_exact_assets "${default_output}" 24 "${default_assets[@]}"
test "$(wc -l < "${default_sbom_log}")" -eq 6
test "$(wc -l < "${default_build_info_log}")" -eq 6
test "$(grep -Fc -- "--source-commit ${CTX_PUBLIC_RELEASE_SOURCE_COMMIT}" "${default_build_info_log}")" -eq 6

semantic_output="${tmp_dir}/semantic"
CTX_FAKE_SBOM_LOG="${tmp_dir}/semantic-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/semantic-build-info.log" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  --with-semantic "${matrix}" "${semantic_output}"
assert_exact_assets "${semantic_output}" 34 "${semantic_assets[@]}"

printf 'retired proof payload\n' > "${matrix}/ctx-linux-x64.native-runtime-proof.txt"
ignored_proof_output="${tmp_dir}/ignored-proof"
CTX_FAKE_SBOM_LOG="${tmp_dir}/ignored-proof-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/ignored-proof-build-info.log" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${ignored_proof_output}"
assert_exact_assets "${ignored_proof_output}" 24 "${default_assets[@]}"
test ! -e "${ignored_proof_output}/ctx-linux-x64.native-runtime-proof.txt"

printf 'mutated runtime bytes\n' >> "${matrix}/ctx-onnxruntime-linux-x64.tar.gz"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/checksum-mutation-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/checksum-mutation-build-info.log" \
  PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  "${matrix}" "${tmp_dir}/checksum-mutation" \
  >"${tmp_dir}/checksum-mutation.out" 2>"${tmp_dir}/checksum-mutation.err"
then
  printf 'release staging accepted runtime bytes that differ from the checksum\n' >&2
  exit 1
fi
grep -Fq 'public artifact checksum mismatch' "${tmp_dir}/checksum-mutation.err"

if PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  --native-candidate "${tmp_dir}/invalid" \
  >"${tmp_dir}/native-mode.out" 2>"${tmp_dir}/native-mode.err"
then
  printf 'retired native-candidate staging mode unexpectedly succeeded\n' >&2
  exit 1
fi
grep -Fq 'unknown staging mode: --native-candidate' "${tmp_dir}/native-mode.err"

if PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" --unknown \
  >"${tmp_dir}/unknown.out" 2>"${tmp_dir}/unknown.err"
then
  printf 'unknown staging mode unexpectedly succeeded\n' >&2
  exit 1
fi
grep -Fq 'unknown staging mode: --unknown' "${tmp_dir}/unknown.err"

printf 'GitHub release staging mode contracts passed\n'
