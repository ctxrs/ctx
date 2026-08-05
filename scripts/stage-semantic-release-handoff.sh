#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
  printf 'usage: %s ARTIFACT_DIR OUTPUT_DIR\n' "$0" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${script_dir}/.." && pwd)"
bundle_tool="${repo_root}/scripts/release/release_bundle.py"

artifact_dir="$1"
output_dir="$2"
if [[ "${artifact_dir}" != /* ]]; then
  artifact_dir="${repo_root}/${artifact_dir}"
fi
if [[ "${output_dir}" != /* ]]; then
  output_dir="${repo_root}/${output_dir}"
fi

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  else
    shasum -a 256 "$1" | awk '{ print $1 }'
  fi
}

require_plain_directory() {
  local path="${1%/}"
  local label="$2"
  local parent

  [[ -n "${path}" ]] || path="/"
  [[ -d "${path}" && ! -L "${path}" ]] || {
    printf '%s must be a non-symlink directory: %s\n' "${label}" "${path}" >&2
    exit 1
  }
  parent="$(dirname "${path}")"
  [[ -d "${parent}" && ! -L "${parent}" ]] || {
    printf '%s parent must be a non-symlink directory: %s\n' \
      "${label}" "${parent}" >&2
    exit 1
  }
}

require_regular_input() {
  local path="$1"
  local label="$2"

  [[ -f "${path}" && ! -L "${path}" ]] || {
    printf '%s must be a regular non-symlink file: %s\n' \
      "${label}" "${path}" >&2
    exit 1
  }
}

stage_semantic_assets() {
  local artifact_dir="$1"
  local output_dir="$2"
  local script_dir="$3"
  local temporary artifact source checksum record expected actual
  local source_checksum_digest source_record_digest
  local staged_actual staged_checksum_digest staged_record_digest staged_expected
  local artifacts

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

  : > "${temporary}/SHA256SUMS"
  for artifact in "${artifacts[@]}"; do
    source="${artifact_dir%/}/${artifact}"
    checksum="${source}.sha256"
    record="${source}.asset.json"
    require_regular_input "${source}" "Semantic producer archive"
    require_regular_input "${checksum}" "Semantic producer checksum"
    require_regular_input "${record}" "Semantic producer record"
    [[ -s "${checksum}" && -s "${record}" ]] || {
      printf 'incomplete Semantic producer output for %s\n' "${artifact}" >&2
      exit 1
    }
    expected="$(awk 'NR == 1 { print $1 }' "${checksum}")"
    actual="$(sha256_file "${source}")"
    [[ "${expected}" =~ ^[0-9a-f]{64}$ && "${expected}" == "${actual}" ]] || {
      printf 'Semantic producer checksum mismatch for %s\n' "${artifact}" >&2
      exit 1
    }
    source_checksum_digest="$(sha256_file "${checksum}")"
    source_record_digest="$(sha256_file "${record}")"
    install -m 0644 "${source}" "${temporary}/${artifact}"
    require_regular_input \
      "${temporary}/${artifact}" "staged Semantic archive"
    staged_actual="$(sha256_file "${temporary}/${artifact}")"
    [[ "${staged_actual}" == "${expected}" ]] || {
      printf 'staged Semantic archive checksum mismatch for %s\n' "${artifact}" >&2
      exit 1
    }
    install -m 0644 "${checksum}" "${temporary}/${artifact}.sha256"
    require_regular_input \
      "${temporary}/${artifact}.sha256" "staged Semantic checksum"
    staged_checksum_digest="$(sha256_file "${temporary}/${artifact}.sha256")"
    [[ "${staged_checksum_digest}" == "${source_checksum_digest}" ]] || {
      printf 'staged Semantic checksum changed while copied for %s\n' "${artifact}" >&2
      exit 1
    }
    staged_expected="$(awk 'NR == 1 { print $1 }' "${temporary}/${artifact}.sha256")"
    [[ "${staged_expected}" =~ ^[0-9a-f]{64}$ && "${staged_expected}" == "${staged_actual}" ]] || {
      printf 'staged Semantic checksum does not bind archive %s\n' "${artifact}" >&2
      exit 1
    }
    install -m 0644 "${record}" "${temporary}/${artifact}.asset.json"
    require_regular_input \
      "${temporary}/${artifact}.asset.json" "staged Semantic record"
    staged_record_digest="$(sha256_file "${temporary}/${artifact}.asset.json")"
    [[ "${staged_record_digest}" == "${source_record_digest}" ]] || {
      printf 'staged Semantic record changed while copied for %s\n' "${artifact}" >&2
      exit 1
    }
    printf '%s  %s\n' "${staged_actual}" "${artifact}" >> "${temporary}/SHA256SUMS"
  done

  bash "${script_dir}/construct-semantic-release-catalog.sh" \
    "${temporary}" "${temporary}/semantic-release.env"

  python3 -I "${bundle_tool}" commit-directory \
    --stage-dir "${temporary}" \
    --output-dir "${output_dir}"
  trap - EXIT
  printf 'staged unsigned Semantic release handoff %s\n' "${output_dir}"
}

require_plain_directory "${artifact_dir}" "Semantic artifact root"
stage_semantic_assets "${artifact_dir}" "${output_dir}" "${script_dir}"
