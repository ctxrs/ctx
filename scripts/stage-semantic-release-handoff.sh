#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  printf 'usage: %s ARTIFACT_DIR OUTPUT_DIR\n' "$0" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
publisher="${repo_root}/scripts/release/publish-linux-bazel-release.py"
artifact_dir="$1"
output_dir="$2"
source_commit="${CTX_PUBLIC_RELEASE_SOURCE_COMMIT:-}"
if [[ -z "${source_commit}" ]]; then
  source_commit="$(git -C "${repo_root}" rev-parse --verify HEAD^{commit})"
fi
if [[ ! "${source_commit}" =~ ^[0-9a-f]{40}$ || "${source_commit}" == "0000000000000000000000000000000000000000" ]]; then
  printf 'could not resolve the exact public source commit\n' >&2
  exit 1
fi

requested_artifact_dir="${artifact_dir}"
if [[ "${requested_artifact_dir}" != /* ]]; then
  requested_artifact_dir="${PWD}/${requested_artifact_dir}"
fi
if [[ "${CTX_RELEASE_PINNED_CONSUMER:-0}" != "1" ]]; then
  python3 -I "${publisher}" consume-complete \
    --candidate-dir "${requested_artifact_dir}" \
    --snapshot-root "${TMPDIR:-/tmp}" \
    --platform linux-x64 \
    --platform linux-aarch64 \
    --source-commit "${source_commit}" \
    --allow-extra -- \
    env CTX_RELEASE_PINNED_CONSUMER=1 \
    /bin/bash "${BASH_SOURCE[0]}" "{candidate}" "${output_dir}"
  exit $?
fi
artifact_dir="${requested_artifact_dir}"

[[ ! -e "${output_dir}" && ! -L "${output_dir}" ]] || {
  printf 'refusing to replace existing Semantic handoff: %s\n' "${output_dir}" >&2
  exit 1
}
mkdir -p "$(dirname "${output_dir}")"
temporary="$(mktemp -d "$(dirname "${output_dir}")/.semantic-release-handoff.XXXXXX")"
trap 'rm -rf "${temporary}"' EXIT

artifacts=(
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

bash "${script_dir}/construct-semantic-release-catalog.sh" \
  "${artifact_dir}" "${temporary}/semantic-release.env"

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

: > "${temporary}/SHA256SUMS"
for artifact in "${artifacts[@]}"; do
  source="${artifact_dir%/}/${artifact}"
  checksum="${source}.sha256"
  record="${source}.asset.json"
  [[ -f "${source}" && -s "${checksum}" && -s "${record}" ]] || {
    printf 'incomplete Semantic producer output for %s\n' "${artifact}" >&2
    exit 1
  }
  expected="$(awk 'NR == 1 { print $1 }' "${checksum}")"
  actual="$(sha256_file "${source}")"
  [[ "${expected}" =~ ^[0-9a-f]{64}$ && "${expected}" == "${actual}" ]] || {
    printf 'Semantic producer checksum mismatch for %s\n' "${artifact}" >&2
    exit 1
  }
  install -m 0644 "${source}" "${temporary}/${artifact}"
  install -m 0644 "${checksum}" "${temporary}/${artifact}.sha256"
  install -m 0644 "${record}" "${temporary}/${artifact}.asset.json"
  printf '%s  %s\n' "${actual}" "${artifact}" >> "${temporary}/SHA256SUMS"
done

mv "${temporary}" "${output_dir}"
trap - EXIT
printf 'staged unsigned Semantic release handoff %s\n' "${output_dir}"
