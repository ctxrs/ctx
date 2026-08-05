#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  repo_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
stage="${repo_root}/scripts/stage-semantic-release-handoff.sh"
publisher="${repo_root}/scripts/release/publish-linux-bazel-release.py"
source_commit="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-semantic-handoff-test.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT

fake_bin="${tmp_dir}/bin"
matrix="${tmp_dir}/matrix"
mkdir -p "${fake_bin}" "${matrix}"
cat >"${fake_bin}/bash" <<'SH'
#!/bin/sh
if [ -n "${CTX_FAKE_SUBSTITUTE_LEAF:-}" ] \
  && [ ! -e "${CTX_FAKE_SUBSTITUTION_FLAG:?}" ]; then
  mv "${CTX_FAKE_SUBSTITUTE_LEAF}" "${CTX_FAKE_SUBSTITUTE_LEAF}.original"
  mv "${CTX_FAKE_SUBSTITUTE_FOREIGN:?}" "${CTX_FAKE_SUBSTITUTE_LEAF}"
  : >"${CTX_FAKE_SUBSTITUTION_FLAG}"
fi
case "${1:-}" in
  *construct-semantic-release-catalog.sh)
    : >"$3"
    exit 0
    ;;
esac
exec /bin/bash "$@"
SH
chmod +x "${fake_bin}/bash"

semantic_assets=(
  ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz
  ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz
  ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz
  ctx-onnxruntime-linux-x64.tar.zst
  ctx-onnxruntime-linux-aarch64.tar.zst
  ctx-onnxruntime-macos-arm64.tar.zst
  ctx-onnxruntime-macos-x64.tar.zst
  ctx-windowsml-windows-x64.zip
  ctx-onnxruntime-freebsd-x64.tar.zst
  ctx-onnxruntime-linux-x64-cuda12.tar.zst
)
for asset in "${semantic_assets[@]}"; do
  printf 'original semantic asset %s\n' "${asset}" >"${matrix}/${asset}"
  sha256sum "${matrix}/${asset}" | awk '{print $1}' \
    >"${matrix}/${asset}.sha256"
  printf '{}\n' >"${matrix}/${asset}.asset.json"
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
  mkdir "${candidate}"
  for leaf in "${leaves[@]}"; do
    if [[ -f "${matrix}/${leaf}" ]]; then
      cp "${matrix}/${leaf}" "${candidate}/${leaf}"
    else
      printf 'completed candidate leaf %s\n' "${leaf}" >"${candidate}/${leaf}"
    fi
    chmod 0755 "${candidate}/${leaf}"
  done
  python3 -I "${publisher}" seal \
    --candidate-dir "${candidate}" \
    --platform "${platform}" \
    --source-commit "${source_commit}" >/dev/null
  cp -a "${candidate}/." "${matrix}/"
}
seal_linux_fixture linux-x64 ctx
seal_linux_fixture linux-aarch64 ctx-linux-aarch64

run_stage() {
  local source="$1"
  local output="$2"
  local commit="$3"
  CTX_PUBLIC_RELEASE_SOURCE_COMMIT="${commit}" \
    PATH="${fake_bin}:${PATH}" \
    /bin/bash "${stage}" "${source}" "${output}"
}

success_output="${tmp_dir}/success"
run_stage "${matrix}" "${success_output}" "${source_commit}"
test "$(find "${success_output}" -maxdepth 1 -type f | wc -l)" -eq 32
test "$(wc -l <"${success_output}/SHA256SUMS")" -eq 10
for asset in "${semantic_assets[@]}"; do
  cmp "${matrix}/${asset}" "${success_output}/${asset}"
  cmp "${matrix}/${asset}.sha256" "${success_output}/${asset}.sha256"
  cmp "${matrix}/${asset}.asset.json" "${success_output}/${asset}.asset.json"
done

missing="${tmp_dir}/missing-marker"
cp -a "${matrix}" "${missing}"
rm "${missing}/ctx-linux-x64.release-complete.json"
if run_stage "${missing}" "${tmp_dir}/missing-output" "${source_commit}" \
  >"${tmp_dir}/missing.out" 2>"${tmp_dir}/missing.err"; then
  printf 'Semantic handoff accepted a missing completion manifest\n' >&2
  exit 1
fi
grep -Fq 'completed release marker is missing' "${tmp_dir}/missing.err"

if run_stage "${matrix}" "${tmp_dir}/wrong-source-output" \
  bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb \
  >"${tmp_dir}/wrong-source.out" 2>"${tmp_dir}/wrong-source.err"; then
  printf 'Semantic handoff accepted the wrong source commit\n' >&2
  exit 1
fi
grep -Fq 'completed release identity is invalid' "${tmp_dir}/wrong-source.err"

wrong_platform="${tmp_dir}/wrong-platform"
cp -a "${matrix}" "${wrong_platform}"
cp \
  "${wrong_platform}/ctx-linux-aarch64.release-complete.json" \
  "${wrong_platform}/ctx-linux-x64.release-complete.json"
if run_stage "${wrong_platform}" "${tmp_dir}/wrong-platform-output" \
  "${source_commit}" >"${tmp_dir}/wrong-platform.out" \
  2>"${tmp_dir}/wrong-platform.err"; then
  printf 'Semantic handoff accepted the wrong completion platform\n' >&2
  exit 1
fi
grep -Fq 'completed release identity is invalid' "${tmp_dir}/wrong-platform.err"

linked_parent="${tmp_dir}/linked-parent"
mkdir "${linked_parent}"
ln -s "${matrix}" "${linked_parent}/candidate"
if run_stage "${linked_parent}/candidate" "${tmp_dir}/linked-parent-output" \
  "${source_commit}" >/dev/null 2>&1; then
  printf 'Semantic handoff followed a candidate ancestor symlink\n' >&2
  exit 1
fi

linked_leaf="${tmp_dir}/linked-leaf"
cp -a "${matrix}" "${linked_leaf}"
sentinel="${tmp_dir}/sentinel"
printf 'sentinel\n' >"${sentinel}"
rm "${linked_leaf}/ctx-onnxruntime-linux-x64.tar.zst"
ln -s "${sentinel}" "${linked_leaf}/ctx-onnxruntime-linux-x64.tar.zst"
if run_stage "${linked_leaf}" "${tmp_dir}/linked-leaf-output" \
  "${source_commit}" >/dev/null 2>&1; then
  printf 'Semantic handoff followed a candidate leaf symlink\n' >&2
  exit 1
fi
grep -Fqx sentinel "${sentinel}"

runtime_race="${tmp_dir}/runtime-race"
cp -a "${matrix}" "${runtime_race}"
runtime_leaf="${runtime_race}/ctx-onnxruntime-linux-x64.tar.zst"
printf 'foreign runtime bytes\n' >"${tmp_dir}/foreign-runtime"
if CTX_FAKE_SUBSTITUTE_LEAF="${runtime_leaf}" \
  CTX_FAKE_SUBSTITUTE_FOREIGN="${tmp_dir}/foreign-runtime" \
  CTX_FAKE_SUBSTITUTION_FLAG="${tmp_dir}/runtime-race.flag" \
  run_stage "${runtime_race}" "${tmp_dir}/runtime-race-output" \
  "${source_commit}" >"${tmp_dir}/runtime-race.out" \
  2>"${tmp_dir}/runtime-race.err"; then
  printf 'Semantic handoff ignored a runtime source substitution\n' >&2
  exit 1
fi
grep -Eq 'changed|substituted' "${tmp_dir}/runtime-race.err"
if [[ -e "${tmp_dir}/runtime-race-output/ctx-onnxruntime-linux-x64.tar.zst" ]]; then
  cmp \
    "${runtime_leaf}.original" \
    "${tmp_dir}/runtime-race-output/ctx-onnxruntime-linux-x64.tar.zst"
  ! grep -Fq 'foreign runtime bytes' \
    "${tmp_dir}/runtime-race-output/ctx-onnxruntime-linux-x64.tar.zst"
fi
grep -Fqx sentinel "${sentinel}"

printf 'Semantic release handoff snapshot contracts passed\n'
