#!/usr/bin/env bash
set -euo pipefail

if [[ -n "${TEST_SRCDIR:-}" && -n "${TEST_WORKSPACE:-}" ]]; then
  source_root="${TEST_SRCDIR}/${TEST_WORKSPACE}"
else
  source_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
assembler="${source_root}/scripts/assemble-github-release-assets.sh"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/ctx-assemble-github-assets.XXXXXX")"
trap 'rm -rf "${tmp}"' EXIT
core="${tmp}/core"
runtime="${tmp}/runtime"
receipts="${tmp}/macos-release-pair-qualification"
mkdir -p "${core}" "${runtime}" "${receipts}"

# Exercise assembly using authored upstream-verifier stubs in a disposable repo.
# These tests do not claim platform signature/native execution evidence.
fixture_repo="${tmp}/repo"
mkdir -p "${fixture_repo}/scripts/release" "${tmp}/bin" "${tmp}/authority"
cp "${assembler}" "${fixture_repo}/scripts/"
cp "${source_root}/scripts/release/release_bundle.py" "${fixture_repo}/scripts/release/"
cp "${source_root}/scripts/release/verify-runtime-signatures.py" "${fixture_repo}/scripts/release/"
# Keep the real authority/policy/extraction owner; stub external signature tools.
cat > "${fixture_repo}/scripts/macos-release-signing-evidence.py" <<'PY_STUB'
import os
raise SystemExit(int(os.environ.get("CTX_ASSEMBLY_REJECT_SIGNATURE", "0")))
PY_STUB
cat > "${fixture_repo}/scripts/verify-macos-release-attestation.sh" <<'SH_STUB'
#!/usr/bin/env bash
exit "${CTX_ASSEMBLY_REJECT_SIGNATURE:-0}"
SH_STUB
chmod +x "${fixture_repo}/scripts/verify-macos-release-attestation.sh"
assembler="${fixture_repo}/scripts/assemble-github-release-assets.sh"
export CTX_RELEASE_AUTHORITY_DIR="${tmp}/authority"
export CTX_RELEASE_HANDOFF_SHA256="$(printf '%064d' 1)"
export CTX_ASSEMBLY_REAL_PYTHON="$(command -v python3)"
cat > "${tmp}/bin/python3" <<'SH'
#!/usr/bin/env bash
case "$*" in
  *release-sbom.py*) exit "${CTX_ASSEMBLY_REJECT_HANDOFF:-0}" ;;
esac
exec "$CTX_ASSEMBLY_REAL_PYTHON" "$@"
SH
chmod +x "${tmp}/bin/python3"
export PATH="${tmp}/bin:$PATH"
printf '{"source_commit":"1111111111111111111111111111111111111111","validation_policy":"native-receipts-required-v1"}\n' > "${CTX_RELEASE_AUTHORITY_DIR}/release-validation.json"

core_assets=(
  ctx-linux-x64
  ctx-linux-x64.cdx.json
  ctx-linux-x64.third-party-notices.txt
  ctx-linux-aarch64
  ctx-linux-aarch64.cdx.json
  ctx-linux-aarch64.third-party-notices.txt
  ctx-macos-arm64
  ctx-macos-arm64.cdx.json
  ctx-macos-arm64.third-party-notices.txt
  ctx-macos-x64
  ctx-macos-x64.cdx.json
  ctx-macos-x64.third-party-notices.txt
  ctx-windows-x64.exe
  ctx-windows-x64.exe.cdx.json
  ctx-windows-x64.exe.third-party-notices.txt
)
runtime_assets=(
  ctx-onnxruntime-linux-x64.tar.gz
  ctx-onnxruntime-linux-aarch64.tar.gz
  ctx-onnxruntime-macos-arm64.tar.gz
  ctx-onnxruntime-macos-x64.tar.gz
  ctx-onnxruntime-windows-x64.zip
)

for asset in "${core_assets[@]}"; do
  printf 'qualified Core fixture: %s\n' "${asset}" > "${core}/${asset}"
  printf '%s  %s\n' "$(sha256sum "${core}/${asset}" | awk '{print $1}')" "${asset}" \
    >> "${core}/SHA256SUMS"
done
for asset in "${runtime_assets[@]}"; do
  printf 'qualified runtime fixture: %s\n' "${asset}" > "${runtime}/${asset}"
  sha256sum "${runtime}/${asset}" | awk '{print $1}' > "${runtime}/${asset}.sha256"
done
for platform in macos-arm64 macos-x64; do
  "${CTX_ASSEMBLY_REAL_PYTHON}" - "${runtime}/ctx-onnxruntime-${platform}.tar.gz" <<'ARCHIVE'
import io, sys, tarfile
payload = b"authored non-native dylib fixture\n"
with tarfile.open(sys.argv[1], "w:gz") as archive:
    member = tarfile.TarInfo("lib/libonnxruntime.dylib")
    member.size = len(payload)
    archive.addfile(member, io.BytesIO(payload))
ARCHIVE
  sha256sum "${runtime}/ctx-onnxruntime-${platform}.tar.gz" | awk '{print $1}' \
    > "${runtime}/ctx-onnxruntime-${platform}.tar.gz.sha256"
  cli="ctx-${platform}"
  runtime_archive="ctx-onnxruntime-${platform}.tar.gz"
  {
    sha256sum "${core}/${cli}"
    sha256sum "${runtime}/${runtime_archive}"
  } | sed "s#  ${core}/#  #; s#  ${runtime}/#  #" \
    > "${receipts}/${cli}.release-pair.sha256"
done

cp "${core}/SHA256SUMS" "${CTX_RELEASE_AUTHORITY_DIR}/SHA256SUMS"
for gate in HANDOFF SIGNATURE; do
  if env "CTX_ASSEMBLY_REJECT_${gate}=1" bash "${assembler}" "${core}" "${runtime}" "${tmp}/rejected-${gate}" "${receipts}"; then
    printf 'assembler ignored required %s verification\n' "$gate" >&2
    exit 1
  fi
  test ! -e "${tmp}/rejected-${gate}"
done

missing_receipts="${tmp}/missing-receipts"
mkdir "${missing_receipts}"
if bash "${assembler}" "${core}" "${runtime}" "${tmp}/missing-output" "${missing_receipts}" \
  > "${tmp}/missing.out" 2> "${tmp}/missing.err"; then
  printf 'assembler accepted missing macOS release-pair receipts\n' >&2
  exit 1
fi
grep -Fq 'macOS release-pair digest receipt must be a regular non-symlink file' \
  "${tmp}/missing.err"
test ! -e "${tmp}/missing-output"

mismatched_receipts="${tmp}/mismatched-receipts"
cp -a "${receipts}" "${mismatched_receipts}"
printf '%064d  ctx-macos-arm64\n' 0 \
  > "${mismatched_receipts}/ctx-macos-arm64.release-pair.sha256"
sha256sum "${runtime}/ctx-onnxruntime-macos-arm64.tar.gz" \
  | sed "s#  ${runtime}/#  #" \
  >> "${mismatched_receipts}/ctx-macos-arm64.release-pair.sha256"
if bash "${assembler}" "${core}" "${runtime}" "${tmp}/mismatched-output" "${mismatched_receipts}" \
  > "${tmp}/mismatched.out" 2> "${tmp}/mismatched.err"; then
  printf 'assembler accepted a mismatched macOS release-pair receipt\n' >&2
  exit 1
fi
grep -Fq 'macOS release-pair receipt digest mismatch for ctx-macos-arm64' \
  "${tmp}/mismatched.err"
test ! -e "${tmp}/mismatched-output"

swapped_receipts="${tmp}/swapped-receipts"
cp -a "${receipts}" "${swapped_receipts}"
{
  sed -n '2p' "${swapped_receipts}/ctx-macos-arm64.release-pair.sha256"
  sed -n '1p' "${swapped_receipts}/ctx-macos-arm64.release-pair.sha256"
} > "${tmp}/swapped-receipt"
mv "${tmp}/swapped-receipt" \
  "${swapped_receipts}/ctx-macos-arm64.release-pair.sha256"
if bash "${assembler}" "${core}" "${runtime}" "${tmp}/swapped-output" "${swapped_receipts}" \
  > "${tmp}/swapped.out" 2> "${tmp}/swapped.err"; then
  printf 'assembler accepted a swapped macOS release-pair receipt\n' >&2
  exit 1
fi
grep -Fq 'macOS release-pair digest receipt must list ctx-macos-arm64 first' \
  "${tmp}/swapped.err"
test ! -e "${tmp}/swapped-output"

unqualified_runtime="${tmp}/unqualified-runtime"
cp -a "${runtime}" "${unqualified_runtime}"
printf 'unqualified macOS runtime fixture\n' \
  > "${unqualified_runtime}/ctx-onnxruntime-macos-arm64.tar.gz"
sha256sum "${unqualified_runtime}/ctx-onnxruntime-macos-arm64.tar.gz" | awk '{print $1}' \
  > "${unqualified_runtime}/ctx-onnxruntime-macos-arm64.tar.gz.sha256"
if bash "${assembler}" "${core}" "${unqualified_runtime}" "${tmp}/unqualified-output" "${receipts}" \
  > "${tmp}/unqualified.out" 2> "${tmp}/unqualified.err"; then
  printf 'assembler accepted unqualified macOS runtime bytes\n' >&2
  exit 1
fi
grep -Fq 'macOS release-pair receipt digest mismatch for ctx-onnxruntime-macos-arm64.tar.gz' \
  "${tmp}/unqualified.err"
test ! -e "${tmp}/unqualified-output"

output="${tmp}/release"
bash "${assembler}" "${core}" "${runtime}" "${output}" "${receipts}"
test "$(find "${output}" -maxdepth 1 -type f | wc -l)" -eq 21
test "$(wc -l < "${output}/SHA256SUMS")" -eq 20
(
  cd "${output}"
  sha256sum -c SHA256SUMS >/dev/null
)
test -x "${output}/ctx-linux-x64"
test ! -x "${output}/ctx-onnxruntime-linux-x64.tar.gz"

if bash "${assembler}" "${core}" "${runtime}" "${output}" "${receipts}" \
  > "${tmp}/existing.out" 2> "${tmp}/existing.err"; then
  printf 'assembler replaced an existing release directory\n' >&2
  exit 1
fi
grep -Fq 'release publication destination already exists' "${tmp}/existing.err"

bad_runtime="${tmp}/bad-runtime"
cp -a "${runtime}" "${bad_runtime}"
printf 'corrupt\n' >> "${bad_runtime}/ctx-onnxruntime-linux-x64.tar.gz"
if bash "${assembler}" "${core}" "${bad_runtime}" "${tmp}/bad-runtime-output" "${receipts}" \
  > "${tmp}/bad-runtime.out" 2> "${tmp}/bad-runtime.err"; then
  printf 'assembler accepted a runtime checksum mismatch\n' >&2
  exit 1
fi
grep -Fq 'runtime checksum mismatch for ctx-onnxruntime-linux-x64.tar.gz' \
  "${tmp}/bad-runtime.err"
test ! -e "${tmp}/bad-runtime-output"

bad_core="${tmp}/bad-core"
cp -a "${core}" "${bad_core}"
printf '%064d  unexpected\n' 0 >> "${bad_core}/SHA256SUMS"
if bash "${assembler}" "${bad_core}" "${runtime}" "${tmp}/bad-core-output" "${receipts}" \
  > "${tmp}/bad-core.out" 2> "${tmp}/bad-core.err"; then
  printf 'assembler accepted an expanded Core inventory\n' >&2
  exit 1
fi
grep -Fq 'Core SHA256SUMS must contain exactly 15 assets' "${tmp}/bad-core.err"
test ! -e "${tmp}/bad-core-output"

symlink_runtime="${tmp}/symlink-runtime"
cp -a "${runtime}" "${symlink_runtime}"
rm "${symlink_runtime}/ctx-onnxruntime-windows-x64.zip"
ln -s "${runtime}/ctx-onnxruntime-windows-x64.zip" \
  "${symlink_runtime}/ctx-onnxruntime-windows-x64.zip"
if bash "${assembler}" "${core}" "${symlink_runtime}" "${tmp}/symlink-output" "${receipts}" \
  > "${tmp}/symlink.out" 2> "${tmp}/symlink.err"; then
  printf 'assembler accepted a symlink runtime\n' >&2
  exit 1
fi
grep -Fq 'runtime release asset must be a regular non-symlink file' \
  "${tmp}/symlink.err"
test ! -e "${tmp}/symlink-output"

# A policy switch cannot detach assembly from its admitted staged authority.
if CTX_RELEASE_VALIDATION_POLICY=factory-only-human-override-v1 \
  bash "${assembler}" "${core}" "${runtime}" "${tmp}/wrong-policy" "${tmp}/absent" \
  >"${tmp}/wrong-policy.out" 2>"${tmp}/wrong-policy.err"; then
  printf 'assembly accepted a different staged validation policy\n' >&2
  exit 1
fi
grep -Fq 'assembly and staged validation selections differ' "${tmp}/wrong-policy.err"
test ! -e "${tmp}/wrong-policy"
printf '{"source_commit":"1111111111111111111111111111111111111111","validation_policy":"factory-only-human-override-v1"}\n' \
  > "${CTX_RELEASE_AUTHORITY_DIR}/release-validation.json"
# The authorized route omits execution receipts but retains both verifier calls.
CTX_RELEASE_VALIDATION_POLICY=factory-only-human-override-v1 \
  bash "${assembler}" "${core}" "${runtime}" "${tmp}/no-native" "${tmp}/absent"
test -s "${tmp}/no-native.assembly.json"
"${CTX_ASSEMBLY_REAL_PYTHON}" - "${tmp}/no-native.assembly.json" <<'PY_CHECK'
import json, sys
value = json.load(open(sys.argv[1]))
assert value["macos_cli_runtime_native_execution"] == "not_run"
assert len(value["artifacts"]) == 21
PY_CHECK

printf 'GitHub release final assembly tests passed\n'
