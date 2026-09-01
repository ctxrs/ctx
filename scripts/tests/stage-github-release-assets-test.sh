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
cp "${source_root}/scripts/release/seal-linux-factory-candidate.py" \
  "${repo_root}/scripts/release/seal-linux-factory-candidate.py"
ln -s "${source_root}/contracts/release-targets-v1.json" \
  "${repo_root}/contracts/release-targets-v1.json"
for dependency in \
  apple-developer-id-g2-ca.pem \
  build-onnxruntime-sidecar.sh \
  check-macos-release-signing.sh \
  check-public-cli-build-info.py \
  macos-release-signing-evidence.py \
  native-execution-proof.py \
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

fake_bin="${tmp_dir}/bin"
matrix="${tmp_dir}/matrix"
mkdir -p "${fake_bin}" "${matrix}"

cat > "${fake_bin}/bash" <<'SH'
#!/bin/sh
exit 0
SH
cat > "${fake_bin}/python3" <<'SH'
#!/bin/sh
if [ "${1:-}" = - ] && [ -d "${2:-}" ] && [ "${#3}" -eq 40 ]; then
  exec "${CTX_REAL_PYTHON3:?}" "$@"
fi
case "$*" in
  *release_bundle.py*)
    exec "${CTX_REAL_PYTHON3:?}" "$@"
    ;;
  *seal-linux-factory-candidate.py*)
    exec "${CTX_REAL_PYTHON3:?}" "$@"
    ;;
  *native-execution-proof.py*)
    exec "${CTX_REAL_PYTHON3:?}" "$@"
    ;;
  *ctx-release-factory.json*)
    exec "${CTX_REAL_PYTHON3:?}" "$@"
    ;;
esac
case "$*" in
  *macos-release-signing-evidence.py\ verify-artifact*)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = --artifact ]; then
        artifact="$2"
        break
      fi
      shift
    done
    case "${artifact:?}" in
      */.github-release-assets.*/*) ;;
      *)
        printf 'fake signing did not inspect the staged artifact\n' >&2
        exit 1
        ;;
    esac
    if grep -Fq 'unsigned replacement' "${artifact:?}"; then
      printf 'fake signing rejected unsigned staged artifact\n' >&2
      exit 1
    fi
    ;;
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
if [ -n "${CTX_FAKE_COHERENT_TRIGGER:-}" ] \
  && [ "${3:-}" = "${CTX_FAKE_COHERENT_TRIGGER}" ] \
  && [ ! -e "${CTX_FAKE_COHERENT_FLAG:?}" ]; then
  cp "${CTX_FAKE_COHERENT_REPLACEMENT:?}" "${CTX_FAKE_COHERENT_TARGET:?}"
  sha256sum "${CTX_FAKE_COHERENT_TARGET}" \
    | awk '{print $1}' > "${CTX_FAKE_COHERENT_TARGET}.sha256"
  : >"${CTX_FAKE_COHERENT_FLAG}"
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
)
for asset in "${cli_sources[@]}"; do
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
for binary in "${cli_sources[@]}"; do
  printf 'ctx 1.0.0\n' > "${matrix}/${binary}.version"
  printf '{"status":"clean"}\n' \
    > "${matrix}/${binary}.dependency-advisory.json"
done
for platform in macos-arm64 macos-x64; do
  printf '{"origin":"factory-pending"}\n' \
    > "${matrix}/ctx-${platform}.signing.json"
  printf '{}\n' > "${matrix}/ctx-${platform}.attestation.json"
  printf 'cms\n' > "${matrix}/ctx-${platform}.attestation.cms"
  printf '{}\n' > "${matrix}/ctx-${platform}.notary-submit.json"
done
proof_root="${tmp_dir}/native-proofs"
for platform in linux-x64 linux-aarch64 macos-arm64 macos-x64 windows-x64; do
  binary="ctx-${platform}"
  [[ "${platform}" == "linux-x64" ]] && binary="ctx"
  [[ "${platform}" == "windows-x64" ]] && binary="ctx.exe"
  mkdir -p "${proof_root}/${platform}"
  printf '%s\n' '{"kind":"ctx-native-candidate-smoke","schema_version":1,"status":"passed"}' \
    > "${proof_root}/${platform}/candidate-smoke.json"
  CTX_NATIVE_PROOF_ARTIFACT="${matrix}/${binary}" \
    "${real_python3}" -I "${source_root}/scripts/native-execution-proof.py" create \
      --platform "${platform}" \
      --artifact "${matrix}/${binary}" \
      --smoke-result "${proof_root}/${platform}/candidate-smoke.json" \
      --output "${proof_root}/${platform}/ctx-${platform}.native-execution.json" >/dev/null
  if [[ "${platform}" == macos-* ]]; then
    printf '{"origin":"native-passed"}\n' \
      > "${proof_root}/${platform}/ctx-${platform}.signing.json"
  fi
done
export CTX_PUBLIC_NATIVE_PROOF_DIR="${proof_root}"

seal_core_fixture() {
  local candidate="$1"
  local commit="$2"
  "${real_python3}" - "${candidate}" "${commit}" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import sys

root = Path(sys.argv[1])
source_commit = sys.argv[2]
targets = (
    ("linux-arm64", "ctx-linux-aarch64"),
    ("linux-x64", "ctx"),
    ("macos-arm64", "ctx-macos-arm64"),
    ("macos-x64", "ctx-macos-x64"),
    ("windows-x64", "ctx.exe"),
)
leaves = ["ctx-release-factory.json"]
for _, binary in targets:
    leaves.extend(
        (
            binary,
            f"{binary}.build-info.json",
            f"{binary}.candidate.json",
            f"{binary}.cdx.json",
            f"{binary}.cdx.json.sha256",
            f"{binary}.dependency-advisory.json",
            f"{binary}.sha256",
            f"{binary}.size.json",
            f"{binary}.third-party-notices.txt",
            f"{binary}.third-party-notices.txt.sha256",
            f"{binary}.version",
        )
    )
records = []
for name in sorted(leaves):
    path = root / name
    value = path.lstat()
    records.append(
        {
            "name": name,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "size": value.st_size,
        }
    )
payload = {
    "files": records,
    "kind": "ctx-public-core-release-completion",
    "schema_version": 1,
    "source_commit": source_commit,
    "targets": [target_id for target_id, _ in targets],
}
marker = root / "ctx-core.release-complete.json"
marker.write_text(
    json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n",
    encoding="utf-8",
)
os.chmod(marker, 0o600)
PY
}

"${real_python3}" - "${matrix}/ctx-release-factory.json" \
  "${source_commit}" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

path, source_commit = sys.argv[1:]
root = Path(path).parent
files = []
for leaf in sorted(root.iterdir(), key=lambda item: item.name):
    if leaf.is_file() and not leaf.name.startswith(".") and leaf.name != Path(path).name:
        raw = leaf.read_bytes()
        files.append(
            {"file": leaf.name, "sha256": hashlib.sha256(raw).hexdigest(), "size_bytes": len(raw)}
        )
value = {
    "files": files,
    "kind": "ctx-linux-release-factory",
    "releasable": True,
    "runtime_sidecars_included": False,
    "schema_version": 1,
    "selected_targets": [
        "linux-arm64",
        "linux-x64",
        "macos-arm64",
        "macos-x64",
        "windows-x64",
    ],
    "source_commit": source_commit,
    "version": "1.0.0",
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
chmod 0755 \
  "${matrix}/ctx" \
  "${matrix}/ctx-linux-aarch64" \
  "${matrix}/ctx-macos-arm64" \
  "${matrix}/ctx-macos-x64"
seal_core_fixture "${matrix}" "${source_commit}"
# Buildkite artifact downloads preserve bytes, not Unix executable mode.
chmod 0644 \
  "${matrix}/ctx" \
  "${matrix}/ctx-linux-aarch64" \
  "${matrix}/ctx-macos-arm64" \
  "${matrix}/ctx-macos-x64"

forged_commit="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
forged_matrix="${tmp_dir}/forged-source"
cp -a "${matrix}" "${forged_matrix}"

completed_fixture="${tmp_dir}/candidate-linux-x64-head"
mkdir "${completed_fixture}"
for archive in tar.gz tar.zst; do
  printf 'completed runtime %s\n' "${archive}" \
    >"${completed_fixture}/ctx-onnxruntime-linux-x64.${archive}"
done
printf '{}\n' >"${completed_fixture}/ctx-core.release-complete.json"
completed_before="$(
  sha256sum \
    "${completed_fixture}/ctx-onnxruntime-linux-x64.tar.gz" \
    "${completed_fixture}/ctx-onnxruntime-linux-x64.tar.zst" \
    "${completed_fixture}/ctx-core.release-complete.json"
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
    "${completed_fixture}/ctx-core.release-complete.json"
)"

default_assets=(
  ctx-linux-aarch64
  ctx-linux-x64
  ctx-macos-arm64
  ctx-macos-x64
  ctx-windows-x64.exe
)
cli_evidence_assets=(
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
macos_cli_verifier_assets=()
for platform in macos-arm64 macos-x64; do
  macos_cli_verifier_assets+=(
    "ctx-${platform}.sha256"
    "ctx-${platform}.build-info.json"
    "ctx-${platform}.signing.json"
    "ctx-${platform}.attestation.json"
    "ctx-${platform}.attestation.cms"
    "ctx-${platform}.notary-submit.json"
  )
done

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
  printf '%s\n' "$@" "${macos_cli_verifier_assets[@]}" | sort > "${expected}"
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
assert_exact_assets "${default_output}" 15 "${default_assets[@]}"
for platform in macos-arm64 macos-x64; do
  grep -Fq '"origin":"native-passed"' \
    "${default_output}/ctx-${platform}.signing.json"
  grep -Fq '"origin":"factory-pending"' \
    "${matrix}/ctx-${platform}.signing.json"
done
default_authority="${default_output}.authority"
test "$(find "${default_authority}" -maxdepth 1 -type f | wc -l)" -eq 20
for candidate in \
  ctx.candidate.json \
  ctx-linux-aarch64.candidate.json \
  ctx-macos-arm64.candidate.json \
  ctx-macos-x64.candidate.json \
  ctx.exe.candidate.json; do
  test -s "${default_authority}/${candidate}"
  test -s "${default_authority}/${candidate}.sha256"
  test "$(sha256sum "${default_authority}/${candidate}" | awk '{print $1}')" = \
    "$(cat "${default_authority}/${candidate}.sha256")"
done
! grep -Fq '"release_sums"' "${default_authority}/ctx.exe.candidate.json"
! grep -Fq '"runtime"' "${default_authority}/ctx.exe.candidate.json"
for handoff_input in \
  ctx.exe \
  ctx.exe.build-info.json \
  ctx.exe.cdx.json \
  ctx.exe.size.json \
  ctx.exe.third-party-notices.txt \
  ctx-core.release-complete.json \
  ctx-core-github-handoff.json \
  ctx-core-github-handoff.json.sha256 \
  ctx-release-factory.json \
  SHA256SUMS; do
  test -s "${default_authority}/${handoff_input}"
done
test "$(sha256sum "${default_authority}/ctx-core-github-handoff.json" \
  | awk '{print $1}')" = \
  "$(cat "${default_authority}/ctx-core-github-handoff.json.sha256")"
"${real_python3}" - "${default_authority}/ctx-core-github-handoff.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source:
    value = json.load(source)
assert value["kind"] == "ctx-public-core-github-handoff"
assert len(value["candidate_manifests"]) == 5
assert value["release_sums"]["file"] == "SHA256SUMS"
assert value["factory_completion"]["file"] == "ctx-core.release-complete.json"
PY
test ! -e "${default_authority}/ctx-windows-x64.exe"
cmp "${default_authority}/ctx.exe" "${default_output}/ctx-windows-x64.exe"
cmp "${default_authority}/SHA256SUMS" "${default_output}/SHA256SUMS"
test -z "$(find "${default_output}" "${default_authority}" -maxdepth 1 \
  -type f -name '*runtime*' -print -quit)"
test "$(wc -l < "${default_sbom_log}")" -eq 5
grep -Fq -- "--candidate-artifact-name ctx " "${default_sbom_log}"
test "$(wc -l < "${default_build_info_log}")" -eq 5
test "$(grep -Fc -- "--source-commit ${source_commit}" "${default_build_info_log}")" -eq 5
grep -Fq -- "--artifact ${tmp_dir}/.github-release-assets." \
  "${default_build_info_log}"
grep -Fq -- "--artifact ${tmp_dir}/.github-release-assets." \
  "${default_sbom_log}"
grep -Fq -- "--artifact ${tmp_dir}/.github-release-authority." \
  "${default_sbom_log}"
grep -Fq -- "/ctx.exe --candidate-artifact-name ctx.exe --build-info ${tmp_dir}/.github-release-authority." \
  "${default_sbom_log}"
grep -Fq -- "--sbom ${tmp_dir}/.github-release-assets." \
  "${default_sbom_log}"
! grep -Fq -- "--artifact ${matrix}/" "${default_build_info_log}"
! grep -Fq -- "--artifact ${matrix}/" "${default_sbom_log}"

named_default_output="${repo_root}/target/github-core-release-assets"
CTX_FAKE_SBOM_LOG="${tmp_dir}/named-default-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/named-default-build-info.log" \
  CTX_PUBLIC_RELEASE_SOURCE_COMMIT="${source_commit}" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}"
assert_exact_assets "${named_default_output}" 15 "${default_assets[@]}"
test -d "${named_default_output}.authority"
test ! -e "${repo_root}/target/github-release-assets"

runtime_claim_candidate="${tmp_dir}/runtime-claim-candidate"
cp -a "${matrix}" "${runtime_claim_candidate}"
rm "${runtime_claim_candidate}/ctx-core.release-complete.json"
"${real_python3}" - "${runtime_claim_candidate}/ctx-release-factory.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["runtime_sidecars_included"] = True
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
seal_core_fixture "${runtime_claim_candidate}" "${source_commit}"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${runtime_claim_candidate}" \
  "${tmp_dir}/runtime-claim-output" \
  >"${tmp_dir}/runtime-claim.out" 2>"${tmp_dir}/runtime-claim.err"; then
  printf 'GitHub stager accepted a Core factory claiming Semantic runtimes\n' >&2
  exit 1
fi
grep -Fq 'not the exact releasable five-target source' \
  "${tmp_dir}/runtime-claim.err"

nonreleasable_candidate="${tmp_dir}/nonreleasable-candidate"
cp -a "${matrix}" "${nonreleasable_candidate}"
rm "${nonreleasable_candidate}/ctx-core.release-complete.json"
"${real_python3}" - "${nonreleasable_candidate}/ctx-release-factory.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    value = json.load(source)
value["releasable"] = False
with open(path, "w", encoding="utf-8") as output:
    json.dump(value, output, sort_keys=True, separators=(",", ":"))
    output.write("\n")
PY
seal_core_fixture "${nonreleasable_candidate}" "${source_commit}"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${nonreleasable_candidate}" \
  "${tmp_dir}/nonreleasable-output" \
  >"${tmp_dir}/nonreleasable.out" 2>"${tmp_dir}/nonreleasable.err"; then
  printf 'GitHub stager accepted a nonreleasable Core manifest\n' >&2
  exit 1
fi
grep -Fq 'not the exact releasable five-target source' \
  "${tmp_dir}/nonreleasable.err"

if PATH="${fake_bin}:${PATH}" /bin/bash "${stage}" --with-semantic \
  "${matrix}" "${tmp_dir}/semantic" \
  >"${tmp_dir}/semantic.out" 2>"${tmp_dir}/semantic.err"; then
  printf 'Core GitHub stager unexpectedly accepted semantic handoff mode\n' >&2
  exit 1
fi
grep -Fq 'unknown staging mode: --with-semantic' "${tmp_dir}/semantic.err"

stale_authority="${tmp_dir}/stale-authority"
mkdir "${stale_authority}"
printf 'stale\n' >"${stale_authority}/unexpected"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${tmp_dir}/stale-output" \
  "${stale_authority}" \
  >"${tmp_dir}/stale.out" 2>"${tmp_dir}/stale.err"; then
  printf 'GitHub stager reused a stale candidate authority directory\n' >&2
  exit 1
fi
grep -Fq 'release publication destination already exists' \
  "${tmp_dir}/stale.err"
test ! -e "${tmp_dir}/stale-output"
grep -Fqx stale "${stale_authority}/unexpected"

if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${tmp_dir}/aliased-output" \
  "${tmp_dir}/aliased-output" \
  >"${tmp_dir}/aliased.out" 2>"${tmp_dir}/aliased.err"; then
  printf 'GitHub stager aliased release assets and candidate authority\n' >&2
  exit 1
fi
grep -Fq 'release publication directories are invalid' \
  "${tmp_dir}/aliased.err"
test ! -e "${tmp_dir}/aliased-output"

late_copy="${tmp_dir}/late-copy"
cp -a "${matrix}" "${late_copy}"
late_copy_leaf="${late_copy}/ctx-macos-arm64"
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

unsigned_candidate="${tmp_dir}/unsigned-candidate"
cp -a "${matrix}" "${unsigned_candidate}"
printf 'unsigned replacement CLI bytes\n' >"${tmp_dir}/unsigned-replacement"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/unsigned-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/unsigned-build-info.log" \
  CTX_FAKE_COHERENT_TRIGGER="${unsigned_candidate}/ctx-linux-aarch64" \
  CTX_FAKE_COHERENT_TARGET="${unsigned_candidate}/ctx-macos-arm64" \
  CTX_FAKE_COHERENT_REPLACEMENT="${tmp_dir}/unsigned-replacement" \
  CTX_FAKE_COHERENT_FLAG="${tmp_dir}/unsigned.flag" \
  CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${unsigned_candidate}" \
  "${tmp_dir}/unsigned-output" \
  >"${tmp_dir}/unsigned.out" 2>"${tmp_dir}/unsigned.err"; then
  printf 'GitHub stager committed coherently substituted unsigned macOS bytes\n' >&2
  exit 1
fi
test -e "${tmp_dir}/unsigned.flag"
grep -Fq 'native execution proof is for different artifact' \
  "${tmp_dir}/unsigned.err"
test ! -e "${tmp_dir}/unsigned-output"

printf 'retired proof payload\n' > "${matrix}/ctx-linux-x64.native-runtime-proof.txt"
ignored_proof_output="${tmp_dir}/ignored-proof"
if CTX_FAKE_SBOM_LOG="${tmp_dir}/ignored-proof-sbom.log" \
  CTX_FAKE_BUILD_INFO_LOG="${tmp_dir}/ignored-proof-build-info.log" \
  CTX_REAL_PYTHON3="${real_python3}" \
  PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${matrix}" "${ignored_proof_output}" \
  >"${tmp_dir}/ignored-proof.out" 2>"${tmp_dir}/ignored-proof.err"; then
  printf 'GitHub stager ignored an unsealed extra factory leaf\n' >&2
  exit 1
fi
grep -Fq 'Core factory file inventory is not exact' \
  "${tmp_dir}/ignored-proof.err"
test ! -e "${ignored_proof_output}"
rm "${matrix}/ctx-linux-x64.native-runtime-proof.txt"

cp -a "${matrix}" "${tmp_dir}/missing-marker"
rm "${tmp_dir}/missing-marker/ctx-core.release-complete.json"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/missing-marker" \
  "${tmp_dir}/missing-marker-output" \
  >"${tmp_dir}/missing-marker.out" 2>"${tmp_dir}/missing-marker.err"; then
  printf 'GitHub stager accepted a candidate without completion identity\n' >&2
  exit 1
fi
grep -Fq 'Core release completion marker is invalid' \
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

mkdir -p "${tmp_dir}/higher-real/nested"
cp -a "${matrix}" "${tmp_dir}/higher-real/nested/candidate"
ln -s "${tmp_dir}/higher-real" "${tmp_dir}/higher-link"
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" "${tmp_dir}/higher-link/nested/candidate" \
  "${tmp_dir}/higher-link-output" \
  >"${tmp_dir}/higher-link.out" 2>"${tmp_dir}/higher-link.err"; then
  printf 'GitHub stager followed a higher candidate ancestor link\n' >&2
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
if CTX_REAL_PYTHON3="${real_python3}" PATH="${fake_bin}:${PATH}" \
  /bin/bash "${stage}" \
  "${tmp_dir}/parent-component/real/link/../candidate" \
  "${tmp_dir}/parent-component-output" \
  >"${tmp_dir}/parent-component.out" \
  2>"${tmp_dir}/parent-component.err"; then
  printf 'GitHub stager accepted a producer path with a parent component\n' >&2
  exit 1
fi
grep -Fq "must not contain '..'" "${tmp_dir}/parent-component.err"
test ! -e "${tmp_dir}/parent-component-output"

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
