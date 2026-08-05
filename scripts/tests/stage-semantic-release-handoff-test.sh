#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-semantic-handoff-test.XXXXXX")"
trap 'rm -rf -- "${tmp_dir}"' EXIT
real_install="$(command -v install)"
export CTX_REAL_INSTALL="${real_install}"

repo_root="${tmp_dir}/repo"
mkdir -p "${repo_root}/scripts/release"
cp "${source_root}/scripts/stage-semantic-release-handoff.sh" \
  "${repo_root}/scripts/stage-semantic-release-handoff.sh"
cp "${source_root}/scripts/release/release_bundle.py" \
  "${repo_root}/scripts/release/release_bundle.py"
ln -s "${source_root}/scripts/construct-semantic-release-catalog.sh" \
  "${repo_root}/scripts/construct-semantic-release-catalog.sh"
stage="${repo_root}/scripts/stage-semantic-release-handoff.sh"

fake_bin="${tmp_dir}/bin"
matrix="${tmp_dir}/matrix"
mkdir -p "${fake_bin}" "${matrix}"
cat >"${fake_bin}/bash" <<'SH'
#!/bin/sh
case "${1:-}" in
  *construct-semantic-release-catalog.sh)
    if [ -n "${CTX_FAKE_CATALOG_INPUT_LOG:-}" ]; then
      printf '%s\n' "$2" >"${CTX_FAKE_CATALOG_INPUT_LOG}"
    fi
    printf 'SEMANTIC_ASSET_COUNT=10\n' >"$3"
    exit 0
    ;;
esac
exec /bin/bash "$@"
SH
cat >"${fake_bin}/install" <<'SH'
#!/bin/sh
if [ -n "${CTX_FAKE_INSTALL_SUBSTITUTE_LEAF:-}" ] \
  && [ "${3:-}" = "${CTX_FAKE_INSTALL_SUBSTITUTE_LEAF}" ] \
  && [ ! -e "${CTX_FAKE_INSTALL_SUBSTITUTION_FLAG:?}" ]; then
  mv "${CTX_FAKE_INSTALL_SUBSTITUTE_LEAF}" \
    "${CTX_FAKE_INSTALL_SUBSTITUTE_LEAF}.original"
  mv "${CTX_FAKE_INSTALL_SUBSTITUTE_FOREIGN:?}" \
    "${CTX_FAKE_INSTALL_SUBSTITUTE_LEAF}"
  : >"${CTX_FAKE_INSTALL_SUBSTITUTION_FLAG}"
fi
exec "${CTX_REAL_INSTALL:?}" "$@"
SH
chmod +x "${fake_bin}/bash" "${fake_bin}/install"

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

run_stage() {
  PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" "$1" "$2"
}

success_output="${tmp_dir}/success"
CTX_RELEASE_PINNED_CONSUMER=irrelevant \
  CTX_PUBLIC_RELEASE_SOURCE_COMMIT="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" \
  CTX_FAKE_CATALOG_INPUT_LOG="${tmp_dir}/catalog-input.log" \
  run_stage "${matrix}" "${success_output}"
test "$(find "${success_output}" -maxdepth 1 -type f | wc -l)" -eq 32
test "$(wc -l <"${success_output}/SHA256SUMS")" -eq 10
grep -Fx 'SEMANTIC_ASSET_COUNT=10' "${success_output}/semantic-release.env"
test "$(cat "${tmp_dir}/catalog-input.log")" != "${matrix}"
for asset in "${semantic_assets[@]}"; do
  cmp "${matrix}/${asset}" "${success_output}/${asset}"
  cmp "${matrix}/${asset}.sha256" "${success_output}/${asset}.sha256"
  cmp "${matrix}/${asset}.asset.json" "${success_output}/${asset}.asset.json"
  grep -Fqx \
    "$(sha256sum "${success_output}/${asset}" | awk '{print $1}')  ${asset}" \
    "${success_output}/SHA256SUMS"
done
test ! -e "${matrix}/ctx-linux-x64.release-complete.json"
test ! -e "${matrix}/ctx-linux-aarch64.release-complete.json"

missing="${tmp_dir}/missing"
cp -a "${matrix}" "${missing}"
rm "${missing}/${semantic_assets[0]}"
if run_stage "${missing}" "${tmp_dir}/missing-output" \
  >"${tmp_dir}/missing.out" 2>"${tmp_dir}/missing.err"; then
  printf 'Semantic handoff accepted a missing asset\n' >&2
  exit 1
fi
grep -Eq 'incomplete|No such file|regular non-symlink' "${tmp_dir}/missing.err"
test ! -e "${tmp_dir}/missing-output"

bad_checksum="${tmp_dir}/bad-checksum"
cp -a "${matrix}" "${bad_checksum}"
printf 'mutated\n' >>"${bad_checksum}/${semantic_assets[1]}"
if run_stage "${bad_checksum}" "${tmp_dir}/bad-checksum-output" \
  >"${tmp_dir}/bad-checksum.out" 2>"${tmp_dir}/bad-checksum.err"; then
  printf 'Semantic handoff accepted bytes that differ from their checksum\n' >&2
  exit 1
fi
grep -Fq 'checksum mismatch' "${tmp_dir}/bad-checksum.err"
test ! -e "${tmp_dir}/bad-checksum-output"

linked="${tmp_dir}/linked"
cp -a "${matrix}" "${linked}"
sentinel="${tmp_dir}/sentinel"
printf 'sentinel\n' >"${sentinel}"
rm "${linked}/${semantic_assets[2]}"
ln -s "${sentinel}" "${linked}/${semantic_assets[2]}"
if run_stage "${linked}" "${tmp_dir}/linked-output" >/dev/null 2>&1; then
  printf 'Semantic handoff accepted a linked asset\n' >&2
  exit 1
fi
grep -Fx sentinel "${sentinel}"
test ! -e "${tmp_dir}/linked-output"

race="${tmp_dir}/race"
cp -a "${matrix}" "${race}"
race_leaf="${race}/${semantic_assets[3]}"
printf 'foreign runtime bytes\n' >"${tmp_dir}/foreign-runtime"
if CTX_FAKE_INSTALL_SUBSTITUTE_LEAF="${race_leaf}" \
  CTX_FAKE_INSTALL_SUBSTITUTE_FOREIGN="${tmp_dir}/foreign-runtime" \
  CTX_FAKE_INSTALL_SUBSTITUTION_FLAG="${tmp_dir}/race.flag" \
  run_stage "${race}" "${tmp_dir}/race-output" \
  >"${tmp_dir}/race.out" 2>"${tmp_dir}/race.err"; then
  printf 'Semantic handoff ignored a source substitution\n' >&2
  exit 1
fi
grep -Fq 'staged Semantic archive checksum mismatch' "${tmp_dir}/race.err"
test ! -e "${tmp_dir}/race-output"

for linked_suffix in .sha256 .asset.json; do
  linked_sidecar="${tmp_dir}/linked-sidecar-${linked_suffix#.}"
  cp -a "${matrix}" "${linked_sidecar}"
  rm "${linked_sidecar}/${semantic_assets[4]}${linked_suffix}"
  ln -s "${sentinel}" \
    "${linked_sidecar}/${semantic_assets[4]}${linked_suffix}"
  if run_stage \
    "${linked_sidecar}" \
    "${linked_sidecar}-output" >/dev/null 2>&1; then
    printf 'Semantic handoff followed a producer sidecar link: %s\n' \
      "${linked_suffix}" >&2
    exit 1
  fi
  test ! -e "${linked_sidecar}-output"
done

mkdir -p "${tmp_dir}/higher-real/nested"
cp -a "${matrix}" "${tmp_dir}/higher-real/nested/candidate"
ln -s "${tmp_dir}/higher-real" "${tmp_dir}/higher-link"
if run_stage "${tmp_dir}/higher-link/nested/candidate" \
  "${tmp_dir}/higher-link-output" \
  >"${tmp_dir}/higher-link.out" 2>"${tmp_dir}/higher-link.err"; then
  printf 'Semantic handoff followed a higher producer ancestor link\n' >&2
  exit 1
fi
grep -Eqi 'symlink|non-directory' "${tmp_dir}/higher-link.err"
test ! -e "${tmp_dir}/higher-link-output"

mkdir -p "${tmp_dir}/parent-component/real" \
  "${tmp_dir}/parent-component/foreign"
cp -a "${matrix}" "${tmp_dir}/parent-component/real/candidate"
cp -a "${matrix}" "${tmp_dir}/parent-component/foreign/candidate"
ln -s "${tmp_dir}/parent-component/foreign/nested" \
  "${tmp_dir}/parent-component/real/link"
mkdir "${tmp_dir}/parent-component/foreign/nested"
if run_stage \
  "${tmp_dir}/parent-component/real/link/../candidate" \
  "${tmp_dir}/parent-component-output" \
  >"${tmp_dir}/parent-component.out" \
  2>"${tmp_dir}/parent-component.err"; then
  printf 'Semantic handoff accepted a producer path with a parent component\n' >&2
  exit 1
fi
grep -Fq "must not contain '..'" "${tmp_dir}/parent-component.err"
test ! -e "${tmp_dir}/parent-component-output"

collision="${tmp_dir}/collision"
mkdir "${collision}"
printf 'sentinel\n' >"${collision}/sentinel"
if run_stage "${matrix}" "${collision}" >/dev/null 2>&1; then
  printf 'Semantic handoff replaced an existing destination\n' >&2
  exit 1
fi
grep -Fx sentinel "${collision}/sentinel"

printf 'Semantic release handoff asset contracts passed\n'
