#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-stage-assets-test.XXXXXX")"
trap 'rm -rf "${tmp_dir}"' EXIT
unset CTX_RELEASE_PINNED_CONSUMER CTX_PUBLIC_RELEASE_SOURCE_COMMIT
real_python3="$(command -v python3)"
real_install="$(command -v install)"
export CTX_REAL_INSTALL="${real_install}"

repo_root="${tmp_dir}/repo"
mkdir -p "${repo_root}/contracts" "${repo_root}/scripts/release"
cp "${source_root}/scripts/stage-github-release-assets.sh" \
  "${repo_root}/scripts/stage-github-release-assets.sh"
cp "${source_root}/scripts/release/release_bundle.py" \
  "${repo_root}/scripts/release/release_bundle.py"
ln -s "${source_root}/contracts/release-targets-v1.json" \
  "${repo_root}/contracts/release-targets-v1.json"
for dependency in \
  apple-developer-id-g2-ca.pem \
  build-onnxruntime-sidecar.sh \
  check-macos-release-signing.sh \
  check-public-cli-build-info.py \
  macos-release-signing-evidence.py \
  macos-release-publisher-policy.sh \
  release-sbom.py \
  verify-macos-release-attestation.sh; do
  ln -s "${source_root}/scripts/${dependency}" \
    "${repo_root}/scripts/${dependency}"
done
ln -s "${source_root}/scripts/release_sbom" \
  "${repo_root}/scripts/release_sbom"
git -C "${repo_root}" init -q
git -C "${repo_root}" add .
git -C "${repo_root}" \
  -c user.name='ctx release test' \
  -c user.email='ctx-release-test@example.invalid' \
  commit -qm 'create release staging fixture'
source_commit="$(git -C "${repo_root}" rev-parse --verify HEAD^{commit})"
stage="${repo_root}/scripts/stage-github-release-assets.sh"
bundle_tool="${repo_root}/scripts/release/release_bundle.py"

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
  *release_bundle.py*)
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
cat > "${fake_bin}/install" <<'SH'
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
chmod +x "${fake_bin}/bash" "${fake_bin}/python3" "${fake_bin}/install"

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
for asset in "${semantic_runtimes[@]}" "${extra_semantic_assets[@]}"; do
  printf '{}\n' >"${matrix}/${asset}.asset.json"
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
for platform in macos-arm64 macos-x64; do
  printf '{}\n' > "${matrix}/ctx-${platform}.signing.json"
  printf '{}\n' > "${matrix}/ctx-onnxruntime-${platform}.signing.json"
  printf '{}\n' > "${matrix}/ctx-${platform}.attestation.json"
  printf 'cms\n' > "${matrix}/ctx-${platform}.attestation.cms"
  printf '{}\n' > "${matrix}/ctx-onnxruntime-${platform}.attestation.json"
  printf 'cms\n' > "${matrix}/ctx-onnxruntime-${platform}.attestation.cms"
  printf '{}\n' \
    > "${matrix}/ctx-onnxruntime-${platform}.release-attestation.json"
  printf 'cms\n' \
    > "${matrix}/ctx-onnxruntime-${platform}.release-attestation.cms"
done

seal_linux_fixture() {
  local platform="$1"
  local binary="$2"
  local source_dir="$3"
  local commit="$4"
  local tag="$5"
  local runtime="ctx-onnxruntime-${platform}"
  local candidate="${tmp_dir}/candidate-${platform}-${tag}"
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
    cp "${source_dir}/${leaf}" "${candidate}/${leaf}"
  done
  "${real_python3}" -I "${bundle_tool}" seal \
    --candidate-dir "${candidate}" \
    --platform "${platform}" \
    --source-commit "${commit}" >/dev/null
  cp "${candidate}/ctx-${platform}.release-complete.json" "${source_dir}/"
}
seal_linux_fixture linux-x64 ctx "${matrix}" "${source_commit}" head
seal_linux_fixture \
  linux-aarch64 ctx-linux-aarch64 "${matrix}" "${source_commit}" head

forged_commit="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
forged_matrix="${tmp_dir}/forged-source"
cp -a "${matrix}" "${forged_matrix}"
rm "${forged_matrix}/ctx-linux-x64.release-complete.json" \
  "${forged_matrix}/ctx-linux-aarch64.release-complete.json"
seal_linux_fixture linux-x64 ctx \
  "${forged_matrix}" "${forged_commit}" forged
seal_linux_fixture linux-aarch64 ctx-linux-aarch64 \
  "${forged_matrix}" "${forged_commit}" forged

completed_fixture="${tmp_dir}/candidate-linux-x64-head"
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
grep -Fq 'sealed release bundle cannot be modified' \
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
  CTX_PUBLIC_RELEASE_SOURCE_COMMIT="${source_commit}" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${default_output}"
assert_exact_assets "${default_output}" 24 "${default_assets[@]}"
test "$(wc -l < "${default_sbom_log}")" -eq 6
test "$(wc -l < "${default_build_info_log}")" -eq 6
test "$(grep -Fc -- "--source-commit ${source_commit}" "${default_build_info_log}")" -eq 6

semantic_output="${tmp_dir}/semantic"
CTX_FAKE_SBOM_LOG="${tmp_dir}/semantic-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/semantic-build-info.log" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  --with-semantic "${matrix}" "${semantic_output}"
assert_exact_assets "${semantic_output}" 34 "${semantic_assets[@]}"

late_copy="${tmp_dir}/late-copy"
cp -a "${matrix}" "${late_copy}"
late_copy_leaf="${late_copy}/ctx-freebsd-x64"
printf 'late foreign CLI bytes\n' >"${tmp_dir}/late-copy-foreign"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/late-copy-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/late-copy-build-info.log" \
  CTX_FAKE_INSTALL_SUBSTITUTE_LEAF="${late_copy_leaf}" \
  CTX_FAKE_INSTALL_SUBSTITUTE_FOREIGN="${tmp_dir}/late-copy-foreign" \
  CTX_FAKE_INSTALL_SUBSTITUTION_FLAG="${tmp_dir}/late-copy.flag" \
  CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${late_copy}" "${tmp_dir}/late-copy-output" \
  >"${tmp_dir}/late-copy.out" 2>"${tmp_dir}/late-copy.err"; then
  printf 'GitHub stager accepted bytes substituted after source hashing\n' >&2
  exit 1
fi
grep -Fq 'staged artifact checksum mismatch' "${tmp_dir}/late-copy.err"
test ! -e "${tmp_dir}/late-copy-output"

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
grep -Fq 'release leaf does not match completion marker' \
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
grep -Eq 'checksum mismatch|does not match completion marker' \
  "${tmp_dir}/runtime-race.err"
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
grep -Fq 'release completion marker is invalid' \
  "${tmp_dir}/missing-marker.err"

if CTX_PUBLIC_RELEASE_SOURCE_COMMIT="${forged_commit}" \
  CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${forged_matrix}" \
  "${tmp_dir}/forged-source-output" \
  >"${tmp_dir}/forged-source.out" 2>"${tmp_dir}/forged-source.err"; then
  printf 'ambient source commit admitted a non-HEAD GitHub candidate\n' >&2
  exit 1
fi
grep -Fq 'ambient public source commit conflicts with checkout HEAD' \
  "${tmp_dir}/forged-source.err"
test ! -e "${tmp_dir}/forged-source-output"

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

cp -a "${matrix}" "${tmp_dir}/linked-record"
rm "${tmp_dir}/linked-record/ctx-macos-arm64.build-info.json"
ln -s "${tmp_dir}/leaf-sentinel" \
  "${tmp_dir}/linked-record/ctx-macos-arm64.build-info.json"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/linked-record" \
  "${tmp_dir}/linked-record-output" >/dev/null 2>&1; then
  printf 'GitHub stager followed a producer record link\n' >&2
  exit 1
fi
test ! -e "${tmp_dir}/linked-record-output"

mkdir "${tmp_dir}/linked-parent"
ln -s "${matrix}" "${tmp_dir}/linked-parent/candidate"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/linked-parent/candidate" \
  "${tmp_dir}/linked-parent-output" >/dev/null 2>&1; then
  printf 'GitHub stager followed a candidate ancestor link\n' >&2
  exit 1
fi

# Restore the runtime mutation so a deterministic source-root substitution
# reaches the validators after initial candidate verification.
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
grep -Eq 'regular non-symlink|missing public CLI artifact|checksum mismatch' \
  "${tmp_dir}/substitution.err" || {
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
