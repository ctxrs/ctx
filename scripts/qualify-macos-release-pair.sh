#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat >&2 <<'USAGE'
Usage: scripts/qualify-macos-release-pair.sh PLATFORM CLI RUNTIME_ARCHIVE

Verifies the complete staged macOS CLI verifier input set and the exact final
runtime archive attestation before running the authoritative semantic smoke.
USAGE
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_regular_nonempty() {
  local path="$1"
  local label="$2"
  [[ -f "${path}" && ! -L "${path}" && -s "${path}" ]] || \
    die "${label} must be a non-empty regular non-symlink file: ${path}"
}

[[ $# -eq 3 ]] || { usage; exit 2; }
platform="$1"
cli="$2"
runtime_archive="$3"
case "${platform}" in
  macos-arm64|macos-x64) ;;
  *) usage; exit 2 ;;
esac
[[ "${cli##*/}" == "ctx-${platform}" ]] || \
  die "macOS CLI must be named ctx-${platform}"
[[ "${runtime_archive##*/}" == "ctx-onnxruntime-${platform}.tar.gz" ]] || \
  die "macOS runtime archive has the wrong name for ${platform}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"
cli_dir="$(cd "$(dirname "${cli}")" && pwd)"
runtime_dir="$(cd "$(dirname "${runtime_archive}")" && pwd)"
cli="${cli_dir}/$(basename "${cli}")"
runtime_archive="${runtime_dir}/$(basename "${runtime_archive}")"
runtime_prefix="${runtime_archive%.tar.gz}"

for input in \
  "${cli}" \
  "${cli}.sha256" \
  "${cli}.build-info.json" \
  "${cli}.signing.json" \
  "${cli}.attestation.json" \
  "${cli}.attestation.cms" \
  "${cli}.notary-submit.json"; do
  require_regular_nonempty "${input}" "staged macOS CLI verifier input"
done
for input in \
  "${runtime_archive}" \
  "${runtime_archive}.sha256" \
  "${runtime_prefix}.signing.json" \
  "${runtime_prefix}.attestation.json" \
  "${runtime_prefix}.attestation.cms" \
  "${runtime_prefix}.release-attestation.json" \
  "${runtime_prefix}.release-attestation.cms" \
  "${runtime_prefix}.notary-submit.json"; do
  require_regular_nonempty "${input}" "runtime verifier input"
done

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/ctx-macos-release-pair.XXXXXX")"
trap 'rm -rf "${work_dir}"' EXIT
nested_runtime="${work_dir}/libonnxruntime.dylib"
python3 - "${runtime_archive}" "${nested_runtime}" <<'PY'
import shutil
import sys
import tarfile

archive, output = sys.argv[1:]
with tarfile.open(archive, "r:gz") as bundle:
    matches = [member for member in bundle.getmembers()
               if member.name == "lib/libonnxruntime.dylib"]
    if len(matches) != 1 or not matches[0].isfile():
        raise SystemExit("runtime archive must contain one regular lib/libonnxruntime.dylib")
    source = bundle.extractfile(matches[0])
    if source is None:
        raise SystemExit("could not read runtime libonnxruntime.dylib")
    with source, open(output, "wb") as destination:
        shutil.copyfileobj(source, destination)
PY

scripts/check-macos-release-signing.sh "${platform}" cli "${cli}"
scripts/check-macos-release-signing.sh "${platform}" runtime "${runtime_archive}"
scripts/verify-macos-release-attestation.sh --runtime-archive \
  "${platform}" "${runtime_archive}" "${nested_runtime}" \
  "${runtime_prefix}.release-attestation.json" \
  "${runtime_prefix}.release-attestation.cms"
chmod 755 "${cli}"
scripts/smoke-daemon-semantic-release.sh \
  --ctx "${cli}" \
  --runtime-archive "${runtime_archive}" \
  --runtime-platform "${platform}" \
  --require-authoritative
