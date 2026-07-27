#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if [[ $# -ne 2 ]]; then
  printf 'usage: %s ARTIFACT_DIR OUTPUT_ENV\n' "$0" >&2
  exit 2
fi

artifact_dir="$1"
output_env="$2"
model_cpu="ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz"
model_accelerator="ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz"
coreml="ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz"

python3 "${script_dir}/semantic-release-assets.py" validate-model \
  --variant cpu-fp32 --archive "${artifact_dir%/}/${model_cpu}"
python3 "${script_dir}/semantic-release-assets.py" validate-model \
  --variant accelerator-o4-fp16 --archive "${artifact_dir%/}/${model_accelerator}"

for platform in \
  linux-x64 \
  linux-x64-cuda12 \
  linux-aarch64 \
  macos-arm64 \
  macos-x64 \
  windows-x64-windowsml \
  freebsd-x64
do
  case "${platform}" in
    linux-x64-cuda12)
      runtime="ctx-onnxruntime-linux-x64-cuda12.tar.zst"
      ;;
    windows-x64-windowsml)
      runtime="ctx-windowsml-windows-x64.zip"
      ;;
    *)
      runtime="ctx-onnxruntime-${platform}.tar.zst"
      ;;
  esac
  bash "${script_dir}/build-onnxruntime-sidecar.sh" --validate \
    "${platform}" "${artifact_dir%/}/${runtime}"
done

artifacts=(
  "${model_cpu}"
  "${model_accelerator}"
  "${coreml}"
  "ctx-onnxruntime-linux-x64.tar.zst"
  "ctx-onnxruntime-linux-x64-cuda12.tar.zst"
  "ctx-onnxruntime-linux-aarch64.tar.zst"
  "ctx-onnxruntime-macos-arm64.tar.zst"
  "ctx-onnxruntime-macos-x64.tar.zst"
  "ctx-windowsml-windows-x64.zip"
  "ctx-onnxruntime-freebsd-x64.tar.zst"
)
record_args=()
for artifact in "${artifacts[@]}"; do
  record_args+=(--asset-record "${artifact_dir%/}/${artifact}.asset.json")
done

python3 - "${artifact_dir}" "${artifacts[@]}" <<'PY'
import hashlib
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
for artifact_name in sys.argv[2:]:
    artifact = root / artifact_name
    record_path = root / f"{artifact_name}.asset.json"
    raw = record_path.read_bytes()
    value = json.loads(raw)
    canonical = json.dumps(
        value, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("ascii") + b"\n"
    if raw != canonical:
        raise SystemExit(f"non-canonical Semantic asset record: {record_path}")
    if value.get("asset", {}).get("artifact") != artifact_name:
        raise SystemExit(f"Semantic asset record names the wrong archive: {record_path}")
    digest = hashlib.sha256()
    with artifact.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    if value["asset"].get("archive_sha256") != digest.hexdigest():
        raise SystemExit(f"Semantic archive hash does not match record: {artifact}")
PY

python3 "${script_dir}/semantic-release-assets.py" catalog \
  "${record_args[@]}" \
  --output "${output_env}"
printf 'constructed signed Semantic metadata input %s\n' "${output_env}"
