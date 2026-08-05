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
real_python3="$(command -v python3)"
publisher="${repo_root}/scripts/release/publish-linux-bazel-release.py"

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
  *publish-linux-bazel-release.py*)
    exec "${CTX_REAL_PYTHON3:?}" "$@"
    ;;
esac
case "$*" in
  *release-sbom.py\ verify-bundle*)
    printf '%s\n' "$*" >> "${CTX_FAKE_SBOM_LOG:?}"
    ;;
  *check-public-cli-build-info.py*)
    if [ -n "${CTX_FAKE_SUBSTITUTE_LEAF:-}" ] \
      && [ ! -e "${CTX_FAKE_SUBSTITUTION_FLAG:?}" ]; then
      mv "${CTX_FAKE_SUBSTITUTE_LEAF}" \
        "${CTX_FAKE_SUBSTITUTE_LEAF}.original"
      mv "${CTX_FAKE_SUBSTITUTE_FOREIGN:?}" \
        "${CTX_FAKE_SUBSTITUTE_LEAF}"
      : >"${CTX_FAKE_SUBSTITUTION_FLAG}"
    fi
    if [ -n "${CTX_FAKE_SUBSTITUTE_CANDIDATE:-}" ] \
      && [ ! -e "${CTX_FAKE_SUBSTITUTION_FLAG:?}" ]; then
      mv "${CTX_FAKE_SUBSTITUTE_CANDIDATE}" \
        "${CTX_FAKE_SUBSTITUTE_CANDIDATE}.verified"
      ln -s "${CTX_FAKE_SUBSTITUTE_EXTERNAL:?}" \
        "${CTX_FAKE_SUBSTITUTE_CANDIDATE}"
      : >"${CTX_FAKE_SUBSTITUTION_FLAG}"
    fi
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
for binary in ctx ctx-linux-aarch64; do
  printf 'ctx 1.0.0\n' > "${matrix}/${binary}.version"
  printf '{"status":"clean"}\n' \
    > "${matrix}/${binary}.dependency-advisory.json"
done
for platform in linux-x64 linux-aarch64 macos-arm64 macos-x64 freebsd-x64; do
  printf '{}\n' \
    > "${matrix}/ctx-onnxruntime-${platform}.tar.zst.asset.json"
done
for platform in macos-arm64 macos-x64; do
  printf '{}\n' > "${matrix}/ctx-${platform}.signing.json"
  printf '{}\n' > "${matrix}/ctx-onnxruntime-${platform}.signing.json"
done

seal_linux_fixture() {
  local platform="$1"
  local binary="$2"
  local runtime="ctx-onnxruntime-${platform}"
  local candidate="${tmp_dir}/candidate-${platform}"
  local leaf
  local leaves=(
    "${binary}"
    "${binary}.build-info.json"
    "${binary}.candidate.json"
    "${binary}.cdx.json"
    "${binary}.cdx.json.sha256"
    "${binary}.dependency-advisory.json"
    "${binary}.sha256"
    "${binary}.size.json"
    "${binary}.third-party-notices.txt"
    "${binary}.third-party-notices.txt.sha256"
    "${binary}.version"
    "${runtime}.tar.gz"
    "${runtime}.tar.gz.sha256"
    "${runtime}.tar.zst"
    "${runtime}.tar.zst.asset.json"
    "${runtime}.tar.zst.sha256"
  )
  mkdir -p "${candidate}"
  for leaf in "${leaves[@]}"; do
    cp "${matrix}/${leaf}" "${candidate}/${leaf}"
  done
  "${real_python3}" -I "${publisher}" seal \
    --candidate-dir "${candidate}" \
    --platform "${platform}" \
    --source-commit "${CTX_PUBLIC_RELEASE_SOURCE_COMMIT}" >/dev/null
  cp "${candidate}/ctx-${platform}.release-complete.json" "${matrix}/"
}
seal_linux_fixture linux-x64 ctx
seal_linux_fixture linux-aarch64 ctx-linux-aarch64

completed_fixture="${tmp_dir}/candidate-linux-x64"
completed_before="$(
  sha256sum \
    "${completed_fixture}/ctx-onnxruntime-linux-x64.tar.gz" \
    "${completed_fixture}/ctx-onnxruntime-linux-x64.tar.zst" \
    "${completed_fixture}/ctx-linux-x64.release-complete.json"
)"
if /bin/bash "${stage}" --transcode-runtime linux-x64 \
  "${completed_fixture}" \
  >"${tmp_dir}/completed-transcode.out" \
  2>"${tmp_dir}/completed-transcode.err"; then
  printf 'runtime transcode modified a completed public candidate\n' >&2
  exit 1
fi
grep -Fq 'completed public candidate cannot be modified' \
  "${tmp_dir}/completed-transcode.err"
test "${completed_before}" = "$(
  sha256sum \
    "${completed_fixture}/ctx-onnxruntime-linux-x64.tar.gz" \
    "${completed_fixture}/ctx-onnxruntime-linux-x64.tar.zst" \
    "${completed_fixture}/ctx-linux-x64.release-complete.json"
)"

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
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${default_output}"
assert_exact_assets "${default_output}" 24 "${default_assets[@]}"
test "$(wc -l < "${default_sbom_log}")" -eq 6
test "$(wc -l < "${default_build_info_log}")" -eq 6
test "$(grep -Fc -- "--source-commit ${CTX_PUBLIC_RELEASE_SOURCE_COMMIT}" "${default_build_info_log}")" -eq 6

semantic_output="${tmp_dir}/semantic"
CTX_FAKE_SBOM_LOG="${tmp_dir}/semantic-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/semantic-build-info.log" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  --with-semantic "${matrix}" "${semantic_output}"
assert_exact_assets "${semantic_output}" 34 "${semantic_assets[@]}"

printf 'retired proof payload\n' > "${matrix}/ctx-linux-x64.native-runtime-proof.txt"
ignored_proof_output="${tmp_dir}/ignored-proof"
CTX_FAKE_SBOM_LOG="${tmp_dir}/ignored-proof-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/ignored-proof-build-info.log" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${ignored_proof_output}"
assert_exact_assets "${ignored_proof_output}" 24 "${default_assets[@]}"
test ! -e "${ignored_proof_output}/ctx-linux-x64.native-runtime-proof.txt"

printf 'mutated runtime bytes\n' >> "${matrix}/ctx-onnxruntime-linux-x64.tar.gz"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/checksum-mutation-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/checksum-mutation-build-info.log" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" \
  "${matrix}" "${tmp_dir}/checksum-mutation" \
  >"${tmp_dir}/checksum-mutation.out" 2>"${tmp_dir}/checksum-mutation.err"
then
  printf 'release staging accepted runtime bytes that differ from the checksum\n' >&2
  exit 1
fi
grep -Fq 'completed release leaf does not match marker' \
  "${tmp_dir}/checksum-mutation.err"

printf 'synthetic %s\n' ctx-onnxruntime-linux-x64.tar.gz \
  > "${matrix}/ctx-onnxruntime-linux-x64.tar.gz"
cp -a "${matrix}" "${tmp_dir}/runtime-race"
runtime_race_candidate="${tmp_dir}/runtime-race"
runtime_race_leaf="${runtime_race_candidate}/ctx-onnxruntime-linux-x64.tar.gz"
printf 'foreign runtime bytes\n' >"${tmp_dir}/foreign-runtime"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/runtime-race-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/runtime-race-build-info.log" \
  CTX_FAKE_SUBSTITUTE_LEAF="${runtime_race_leaf}" \
  CTX_FAKE_SUBSTITUTE_FOREIGN="${tmp_dir}/foreign-runtime" \
  CTX_FAKE_SUBSTITUTION_FLAG="${tmp_dir}/runtime-race.flag" \
  CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${runtime_race_candidate}" \
  "${tmp_dir}/runtime-race-output" \
  >"${tmp_dir}/runtime-race.out" 2>"${tmp_dir}/runtime-race.err"
then
  printf 'GitHub stager ignored a source runtime name substitution\n' >&2
  exit 1
fi
grep -Eq 'changed|substituted' "${tmp_dir}/runtime-race.err"
if [[ -e "${tmp_dir}/runtime-race-output/ctx-onnxruntime-linux-x64.tar.gz" ]]; then
  cmp \
    "${runtime_race_leaf}.original" \
    "${tmp_dir}/runtime-race-output/ctx-onnxruntime-linux-x64.tar.gz"
  ! grep -Fq 'foreign runtime bytes' \
    "${tmp_dir}/runtime-race-output/ctx-onnxruntime-linux-x64.tar.gz"
fi

cp -a "${matrix}" "${tmp_dir}/missing-marker"
rm "${tmp_dir}/missing-marker/ctx-linux-x64.release-complete.json"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/missing-marker" \
  "${tmp_dir}/missing-marker-output" \
  >"${tmp_dir}/missing-marker.out" 2>"${tmp_dir}/missing-marker.err"; then
  printf 'GitHub stager accepted a candidate without completion identity\n' >&2
  exit 1
fi
grep -Fq 'completed release marker is missing or invalid' \
  "${tmp_dir}/missing-marker.err"

cp -a "${matrix}" "${tmp_dir}/partial-candidate"
rm "${tmp_dir}/partial-candidate/ctx.size.json"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/partial-candidate" \
  "${tmp_dir}/partial-output" \
  >"${tmp_dir}/partial.out" 2>"${tmp_dir}/partial.err"; then
  printf 'GitHub stager accepted a partial completed candidate\n' >&2
  exit 1
fi
test -s "${tmp_dir}/partial.err"
test ! -e "${tmp_dir}/partial-output"

cp -a "${matrix}" "${tmp_dir}/linked-leaf"
printf 'sentinel\n' >"${tmp_dir}/leaf-sentinel"
rm "${tmp_dir}/linked-leaf/ctx.sha256"
ln -s "${tmp_dir}/leaf-sentinel" "${tmp_dir}/linked-leaf/ctx.sha256"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/linked-leaf" \
  "${tmp_dir}/linked-output" >/dev/null 2>&1; then
  printf 'GitHub stager followed a completed candidate leaf link\n' >&2
  exit 1
fi
grep -Fqx sentinel "${tmp_dir}/leaf-sentinel"

mkdir "${tmp_dir}/linked-parent"
ln -s "${matrix}" "${tmp_dir}/linked-parent/candidate"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/linked-parent/candidate" \
  "${tmp_dir}/linked-parent-output" >/dev/null 2>&1; then
  printf 'GitHub stager followed a candidate ancestor link\n' >&2
  exit 1
fi

# Restore the runtime mutation so a deterministic substitution reaches the
# validators after the descriptor-anchored snapshot has completed.
printf 'synthetic %s\n' ctx-onnxruntime-linux-x64.tar.gz \
  > "${matrix}/ctx-onnxruntime-linux-x64.tar.gz"
substitution_external="${tmp_dir}/substitution-external"
mkdir "${substitution_external}"
printf 'sentinel\n' >"${substitution_external}/sentinel"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/substitution-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/substitution-build-info.log" \
  CTX_FAKE_SUBSTITUTE_CANDIDATE="${matrix}" \
  CTX_FAKE_SUBSTITUTE_EXTERNAL="${substitution_external}" \
  CTX_FAKE_SUBSTITUTION_FLAG="${tmp_dir}/substitution.flag" \
  CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${tmp_dir}/substitution-output" \
  >"${tmp_dir}/substitution.out" 2>"${tmp_dir}/substitution.err"; then
  printf 'GitHub stager reported success after candidate parent substitution\n' >&2
  exit 1
fi
grep -Eq 'substituted|changed while pinned' "${tmp_dir}/substitution.err" || {
  cat "${tmp_dir}/substitution.err" >&2
  exit 1
}
grep -Fqx sentinel "${substitution_external}/sentinel"

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
