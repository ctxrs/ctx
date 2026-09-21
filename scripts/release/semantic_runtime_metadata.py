#!/usr/bin/env python3
"""Generate canonical signed metadata for the four semantic runtime paths."""

from __future__ import annotations

import argparse
import base64
import json
import os
import posixpath
import re
import sys
from pathlib import Path

for module_directory in (
    Path(__file__).resolve().parent,
    Path.cwd() / "scripts" / "release",
):
    if (module_directory / "semantic_runtime_archive.py").is_file():
        module_path = str(module_directory)
        if module_path not in sys.path:
            sys.path.insert(0, module_path)
        break

from semantic_runtime_archive import (
    CHUNK_BYTES,
    MetadataError,
    _regular_file_identity,
    copy_regular_file,
    hash_regular_file,
    snapshot_regular_file,
    canonical_member_path,
    relative_asset_path,
    validate_file_allowlist,
    validate_record_limits,
    digest_stream,
    validate_directories,
    inspect_tar_file,
    inspect_tar_zst,
    inspect_tar,
    inspect_zip,
    validate_model_publication_pin,
    validate_publication_pins,
)


LAYOUT_PATH = Path(__file__).with_name("semantic-runtime-layout-v1.json")
EXPECTED_PATHS = [
    ("apple-silicon", "coreml", "apple_silicon_coreml"),
    ("windows", "windows-ml", "windows_windows_ml"),
    ("linux-nvidia", "ort-cuda", "linux_nvidia_ort_cuda"),
    ("universal", "ort-cpu", "universal_ort_cpu"),
]
EXPECTED_CPU_PLATFORMS = [
    "linux-x64",
    "linux-aarch64",
    "macos-arm64",
    "macos-x64",
    "windows-x64",
]
EXPECTED_CPU_BACKENDS = [
    "ort-cpu",
    "ort-cpu",
    "ort-cpu",
    "ort-cpu",
    "windows-ml",
]
RUNTIME_TRANSPORT_ARTIFACTS = {
    "linux_x64": "ctx-onnxruntime-linux-x64.tar.gz",
    "linux_aarch64": "ctx-onnxruntime-linux-aarch64.tar.gz",
    "windows_x64": "ctx-onnxruntime-windows-x64.zip",
    "macos_x64": "ctx-onnxruntime-macos-x64.tar.gz",
    "macos_arm64": "ctx-onnxruntime-macos-arm64.tar.gz",
}
EXPECTED_ASSET_IDS = {
    "onnx_model",
    "onnx_model_o4_fp16",
    "apple_coreml",
    "linux_x64_cpu",
    "linux_aarch64_cpu",
    "macos_arm64_cpu",
    "macos_x64_cpu",
    "windows_ml",
    "linux_cuda12",
}
EXPECTED_APPLE_COREML_ASSET_IDS = [
    "onnx_model",
    "macos_arm64_cpu",
    "apple_coreml",
]
EXPECTED_TARGET_ASSET_IDS = [
    EXPECTED_APPLE_COREML_ASSET_IDS,
    ["onnx_model_o4_fp16", "windows_ml"],
    ["onnx_model_o4_fp16", "linux_cuda12"],
    [
        "onnx_model",
        "linux_x64_cpu",
        "linux_aarch64_cpu",
        "macos_arm64_cpu",
        "macos_x64_cpu",
        "windows_ml",
    ],
]
EXPECTED_MODEL_CONTRACT = {
    "model_id": "intfloat/multilingual-e5-small",
    "revision": "614241f622f53c4eeff9890bdc4f31cfecc418b3",
    "dimensions": 384,
    "pooling": "attention_mask_mean",
    "normalization": "l2",
    "query_prefix": "query: ",
    "passage_prefix": "passage: ",
}
EXPECTED_RUNTIME_INSTALL_MANIFEST_CONTRACT = {
    "schema_version": 1,
    "manager": "ctx-hosted-installer",
    "metadata_trust": "signed-release-metadata",
    "required_fields": [
        "schema_version",
        "manager",
        "metadata_trust",
        "runtime",
        "platform",
        "version",
        "sha256",
        "artifact_url",
        "installed_at",
        "files",
    ],
    "publication": "write_last_after_extraction_and_file_verification",
    "loader": "rehash_every_required_file_before_load",
}
EXPECTED_MODEL_ARTIFACTS = {
    "onnx_model": "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz",
    "onnx_model_o4_fp16": "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz",
}
EXPECTED_CUDA_PATHS = [
    "GIT_COMMIT_ID",
    "LICENSE",
    "NVIDIA-CUDA-LICENSE.txt",
    "NVIDIA-CUDNN-LICENSE.txt",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
    "lib/libcublas.so.12",
    "lib/libcublasLt.so.12",
    "lib/libcudart.so.12",
    "lib/libcudnn.so.9",
    "lib/libcudnn_graph.so.9",
    "lib/libcudnn_ops.so.9",
    "lib/libcufft.so.11",
    "lib/libcurand.so.10",
    "lib/libnvrtc.so.12",
    "lib/libonnxruntime.so",
    "lib/libonnxruntime_providers_cuda.so",
    "lib/libonnxruntime_providers_shared.so",
]
SEMANTIC_METADATA_PREFIX = "CTX_RELEASE_SEMANTIC_"
SEMANTIC_REQUIRED_RELEASE_VERSION = "0.26.0"
SEMANTIC_REQUIRED_RELEASE_COMPONENTS = (0, 26, 0)
SEMANTIC_METADATA_KEYS = {
    "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION",
    "CTX_RELEASE_SEMANTIC_ASSETS",
    *{
        f"CTX_RELEASE_SEMANTIC_AUTHORITY_{key}"
        for _, _, key in EXPECTED_PATHS
    },
}



def load_layout(layout_path: Path = LAYOUT_PATH) -> dict:
    with layout_path.open("r", encoding="utf-8") as handle:
        layout = json.load(handle, object_pairs_hook=reject_duplicate_keys)
    if set(layout) != {
        "schema_version",
        "model_contract",
        "runtime_install_manifest_contract",
        "publication_pins",
        "assets",
        "targets",
    }:
        raise MetadataError("semantic runtime layout has missing or unknown fields")
    if layout["schema_version"] != 1:
        raise MetadataError("semantic runtime layout has unsupported schema")
    if layout["model_contract"] != EXPECTED_MODEL_CONTRACT:
        raise MetadataError("semantic runtime layout does not use the pinned multilingual E5 contract")
    if (
        layout["runtime_install_manifest_contract"]
        != EXPECTED_RUNTIME_INSTALL_MANIFEST_CONTRACT
    ):
        raise MetadataError("semantic runtime layout has the wrong verified install-manifest contract")
    assets = layout["assets"]
    if not isinstance(assets, dict) or set(assets) != EXPECTED_ASSET_IDS:
        raise MetadataError(
            "semantic runtime layout must contain the exact nine-asset catalog"
        )
    for asset_id, asset in assets.items():
        if (
            not asset_id
            or any(character not in "abcdefghijklmnopqrstuvwxyz0123456789_" for character in asset_id)
        ):
            raise MetadataError(f"invalid semantic asset ID: {asset_id!r}")
        validate_asset_layout(asset)
    artifacts = [asset["artifact"] for asset in assets.values()]
    if len(artifacts) != len(set(artifacts)):
        raise MetadataError("semantic runtime asset catalog repeats an artifact")
    validate_publication_pin_layout(layout)
    targets = layout["targets"]
    if not isinstance(targets, list) or [
        (item.get("target"), item.get("backend"), item.get("key")) for item in targets
    ] != EXPECTED_PATHS:
        raise MetadataError("semantic runtime layout must contain the exact four target/backend paths")
    for target, expected_ids in zip(targets, EXPECTED_TARGET_ASSET_IDS, strict=True):
        if set(target) != {"target", "backend", "key", "asset_ids"}:
            raise MetadataError(f"invalid target fields for {target.get('key')}")
        if target["asset_ids"] != expected_ids:
            raise MetadataError(f"target {target['key']} has the wrong asset composition")
        if any(asset_id not in assets for asset_id in target["asset_ids"]):
            raise MetadataError(f"target {target['key']} references an unknown asset")
    coreml_assets = target_assets(layout, targets[0])
    if (
        [asset["role"] for asset in coreml_assets]
        != ["model", "cpu-runtime", "accelerator"]
        or coreml_assets[0]["backend"] != "onnx"
        or coreml_assets[0]["platform"] != "any"
        or coreml_assets[1]["backend"] != "ort-cpu"
        or coreml_assets[1]["platform"] != "macos-arm64"
        or coreml_assets[2]["backend"] != "coreml"
        or coreml_assets[2]["platform"] != "macos-arm64"
    ):
        raise MetadataError(
            "Core ML authority must bind the FP32 ONNX model, macOS arm64 "
            "CPU runtime, and CoreML model bundle"
        )
    windows_assets = target_assets(layout, targets[1])
    if (
        [asset["role"] for asset in windows_assets] != ["model", "cpu-runtime"]
        or windows_assets[0]["backend"] != "onnx"
        or windows_assets[0]["platform"] != "any"
        or windows_assets[1]["backend"] != "windows-ml"
        or windows_assets[1]["platform"] != "windows-x64"
    ):
        raise MetadataError(
            "Windows ML authority must bind the O4 FP16 model and one "
            "self-contained Windows ML runtime"
        )
    cuda_assets = target_assets(layout, targets[2])
    if (
        [asset["role"] for asset in cuda_assets] != ["model", "accelerator"]
        or cuda_assets[0]["backend"] != "onnx"
        or cuda_assets[0]["platform"] != "any"
        or cuda_assets[1]["backend"] != "ort-cuda"
        or cuda_assets[1]["platform"] != "linux-x64-cuda12"
    ):
        raise MetadataError(
            "CUDA authority must bind the O4 FP16 model and self-contained "
            "CUDA runtime"
        )
    universal_assets = target_assets(layout, targets[3])
    if (
        [asset["role"] for asset in universal_assets]
        != ["model", *(["cpu-runtime"] * len(EXPECTED_CPU_PLATFORMS))]
        or [asset["platform"] for asset in universal_assets[1:]]
        != EXPECTED_CPU_PLATFORMS
        or [asset["backend"] for asset in universal_assets[1:]]
        != EXPECTED_CPU_BACKENDS
    ):
        raise MetadataError(
            "universal authority must bind CPU assets for every public platform"
        )
    referenced = {
        asset_id for target in targets for asset_id in target["asset_ids"]
    }
    if referenced != set(assets):
        raise MetadataError(
            "semantic asset catalog and authority references must exactly match"
        )
    return layout


def reject_duplicate_keys(pairs: list[tuple[str, object]]) -> dict:
    value = {}
    for key, item in pairs:
        if key in value:
            raise MetadataError(f"duplicate JSON key: {key}")
        value[key] = item
    return value


def validate_sha256(value: object, field: str) -> None:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(character not in "0123456789abcdef" for character in value)
        or value == "0" * 64
    ):
        raise MetadataError(f"{field} must be a non-placeholder lowercase SHA-256 digest")


def validate_model_publication_pin_layout(
    layout: dict, model: object, expected_asset_id: str
) -> None:
    if not isinstance(model, dict) or set(model) != {
        "asset_id",
        "signed_metadata_paths",
        "runtime_files",
    }:
        raise MetadataError("semantic model publication pins have missing or unknown fields")
    if model["asset_id"] != expected_asset_id:
        raise MetadataError(
            f"semantic model publication pins must bind {expected_asset_id}"
        )
    metadata_paths = model["signed_metadata_paths"]
    if metadata_paths != ["LICENSE", "manifest.json"]:
        raise MetadataError(
            "semantic model publication pins must identify LICENSE and manifest.json metadata"
        )
    runtime_files = model["runtime_files"]
    if not isinstance(runtime_files, dict) or len(runtime_files) != 5:
        raise MetadataError(
            "semantic model publication pins must contain exactly five runtime files"
        )
    for path, pin in runtime_files.items():
        validate_relative_layout_path(path, allow_empty=False, allow_trailing=False)
        if (
            not isinstance(pin, dict)
            or set(pin) != {"size", "sha256"}
            or not isinstance(pin["size"], int)
            or isinstance(pin["size"], bool)
            or pin["size"] <= 0
        ):
            raise MetadataError(f"invalid semantic model publication pin for {path}")
        validate_sha256(pin["sha256"], f"semantic model publication pin for {path}")
    model_asset = layout["assets"].get(model["asset_id"])
    expected_model_paths = set(metadata_paths) | set(runtime_files)
    if (
        model_asset is None
        or model_asset["role"] != "model"
        or model_asset["backend"] != "onnx"
        or model_asset["artifact"] != EXPECTED_MODEL_ARTIFACTS[expected_asset_id]
        or model_asset["path_prefix"]
        != EXPECTED_MODEL_ARTIFACTS[expected_asset_id].removesuffix(".tar.xz")
        or model_asset["tree_prefixes"]
        or len(model_asset["exact_paths"]) != 7
        or set(model_asset["exact_paths"]) != expected_model_paths
    ):
        raise MetadataError(
            "semantic model package must contain exactly two signed metadata "
            "and five pinned runtime files"
        )


def validate_publication_pin_layout(layout: dict) -> None:
    pins = layout["publication_pins"]
    if not isinstance(pins, dict) or set(pins) != {
        "schema_version",
        "model",
        "accelerator_model",
        "coreml",
        "windows_ml",
    }:
        raise MetadataError("semantic publication pins have missing or unknown fields")
    if pins["schema_version"] != 1:
        raise MetadataError("semantic publication pins have unsupported schema")

    validate_model_publication_pin_layout(layout, pins["model"], "onnx_model")
    validate_model_publication_pin_layout(
        layout, pins["accelerator_model"], "onnx_model_o4_fp16"
    )

    coreml = pins["coreml"]
    if not isinstance(coreml, dict) or set(coreml) != {
        "asset_id",
        "archive_sha256",
        "manifest_path",
        "manifest_sha256",
    }:
        raise MetadataError("CoreML publication pins have missing or unknown fields")
    if coreml["asset_id"] != "apple_coreml":
        raise MetadataError("CoreML publication pins must bind apple_coreml")
    validate_relative_layout_path(
        coreml["manifest_path"], allow_empty=False, allow_trailing=False
    )
    validate_sha256(coreml["archive_sha256"], "CoreML archive publication pin")
    validate_sha256(coreml["manifest_sha256"], "CoreML manifest publication pin")
    coreml_asset = layout["assets"].get(coreml["asset_id"])
    if (
        coreml_asset is None
        or coreml_asset["role"] != "accelerator"
        or coreml_asset["backend"] != "coreml"
        or coreml["manifest_path"] not in coreml_asset["exact_paths"]
    ):
        raise MetadataError(
            "CoreML publication pins do not match the canonical CoreML asset"
        )

    windows_ml = pins["windows_ml"]
    if not isinstance(windows_ml, dict) or set(windows_ml) != {
        "asset_id",
        "source_package",
        "source_package_sha256",
        "ort_version",
        "ort_commit",
    }:
        raise MetadataError("Windows ML publication pins have missing or unknown fields")
    validate_sha256(
        windows_ml["source_package_sha256"],
        "Windows ML source-package publication pin",
    )
    if (
        windows_ml["asset_id"] != "windows_ml"
        or windows_ml["source_package"] != "Microsoft.Windows.AI.MachineLearning"
        or windows_ml["source_package_sha256"]
        != "691165fa3c07a04b752cbf4a07e93ed13a418e9dea1ee89eb163d2225e2ba3af"
        or windows_ml["ort_version"] != "1.24.6"
        or windows_ml["ort_commit"] != "800ac32bc82d562c611d641b1112a8aa9f90c4f9"
    ):
        raise MetadataError("Windows ML publication pins do not match the stable package")
    windows_ml_asset = layout["assets"].get(windows_ml["asset_id"])
    if (
        windows_ml_asset is None
        or windows_ml_asset["role"] != "cpu-runtime"
        or windows_ml_asset["backend"] != "windows-ml"
        or windows_ml_asset["version"] != "2.1.74"
        or windows_ml_asset["platform"] != "windows-x64"
    ):
        raise MetadataError(
            "Windows ML publication pins do not match the canonical Windows ML asset"
        )


def validate_asset_layout(asset: dict) -> None:
    expected = {
        "role",
        "backend",
        "version",
        "platform",
        "artifact",
        "format",
        "path_prefix",
        "max_expanded_bytes",
        "max_files",
        "exact_paths",
        "tree_prefixes",
    }
    if set(asset) != expected:
        raise MetadataError(f"invalid asset layout fields for {asset.get('artifact')}")
    if asset["role"] not in {"model", "cpu-runtime", "accelerator"}:
        raise MetadataError(f"invalid asset role for {asset['artifact']}")
    expected_backends = {
        "model": {"onnx"},
        "cpu-runtime": {"ort-cpu", "windows-ml"},
    }.get(asset["role"])
    if expected_backends is not None and asset["backend"] not in expected_backends:
        raise MetadataError(f"invalid asset backend for {asset['artifact']}")
    if asset["role"] == "accelerator" and asset["backend"] not in {
        "coreml",
        "ort-cuda",
    }:
        raise MetadataError(f"invalid accelerator backend for {asset['artifact']}")
    expected_version = {
        "onnx": "1.0.0",
        "coreml": "1.0.0",
        "windows-ml": "2.1.74",
    }.get(asset["backend"], "1.27.0")
    if asset["version"] != expected_version:
        raise MetadataError(f"invalid asset version for {asset['artifact']}")
    if asset["backend"] == "windows-ml":
        if asset["artifact"] != "ctx-windowsml-windows-x64.zip":
            raise MetadataError(f"invalid Windows ML artifact for {asset['artifact']}")
    if asset["backend"] == "ort-cuda":
        if (
            asset["artifact"] != "ctx-onnxruntime-linux-x64-cuda12.tar.zst"
            or asset["platform"] != "linux-x64-cuda12"
            or asset["format"] != "tar.zst"
            or asset["path_prefix"] != ""
            or asset["max_expanded_bytes"] != 2147483648
            or asset["max_files"] != len(EXPECTED_CUDA_PATHS)
            or asset["exact_paths"] != EXPECTED_CUDA_PATHS
            or asset["tree_prefixes"]
        ):
            raise MetadataError(
                "CUDA runtime layout must contain the exact self-contained "
                "ORT/CUDA12/cuDNN archive inventory"
            )
    if asset["format"] not in {"tar.gz", "tar.xz", "tar.zst", "zip"}:
        raise MetadataError(f"unsupported archive format for {asset['artifact']}")
    if (
        not isinstance(asset["max_expanded_bytes"], int)
        or isinstance(asset["max_expanded_bytes"], bool)
        or asset["max_expanded_bytes"] <= 0
        or not isinstance(asset["max_files"], int)
        or isinstance(asset["max_files"], bool)
        or asset["max_files"] <= 0
    ):
        raise MetadataError(f"invalid archive limits for {asset['artifact']}")
    if (
        not asset["artifact"]
        or "/" in asset["artifact"]
        or "\\" in asset["artifact"]
        or ".." in asset["artifact"]
    ):
        raise MetadataError(f"unsafe artifact name: {asset['artifact']!r}")
    validate_relative_layout_path(asset["path_prefix"], allow_empty=True, allow_trailing=False)
    for field in ("exact_paths", "tree_prefixes"):
        values = asset[field]
        if (
            not isinstance(values, list)
            or (field == "exact_paths" and not values)
            or len(values) != len(set(values))
        ):
            raise MetadataError(f"{asset['artifact']} has invalid {field}")
        for value in values:
            validate_relative_layout_path(
                value,
                allow_empty=False,
                allow_trailing=field == "tree_prefixes",
            )


def validate_relative_layout_path(
    value: object, *, allow_empty: bool, allow_trailing: bool
) -> None:
    if not isinstance(value, str):
        raise MetadataError("archive layout paths must be strings")
    if value == "" and allow_empty:
        return
    candidate = value[:-1] if allow_trailing and value.endswith("/") else value
    if (
        not candidate
        or "\\" in value
        or value.startswith("/")
        or (value.endswith("/") != allow_trailing)
        or posixpath.normpath(candidate) != candidate
        or any(part in {"", ".", ".."} for part in candidate.split("/"))
    ):
        raise MetadataError(f"unsafe archive layout path: {value!r}")


def asset_layouts(layout: dict) -> list[dict]:
    return list(layout["assets"].values())


def target_assets(layout: dict, target: dict) -> list[dict]:
    return [layout["assets"][asset_id] for asset_id in target["asset_ids"]]


def asset_layout(layout: dict, artifact: str) -> dict:
    matches = [entry for entry in asset_layouts(layout) if entry["artifact"] == artifact]
    if len(matches) != 1:
        raise MetadataError(f"unknown semantic runtime artifact: {artifact}")
    return matches[0]


def inspect_asset(archive: Path, asset: dict) -> dict:
    with snapshot_regular_file(archive) as (snapshot, _, archive_sha256):
        if asset["format"].startswith("tar."):
            records = inspect_tar(snapshot, asset)
        elif asset["format"] == "zip":
            records = inspect_zip(snapshot, asset)
        else:
            raise MetadataError(f"unsupported archive format: {asset['format']}")
    installed_records = [
        {**record, "path": relative_asset_path(record["path"], asset)}
        for record in records
    ]
    record = {
        "role": asset["role"],
        "backend": asset["backend"],
        "version": asset["version"],
        "platform": asset["platform"],
        "artifact": asset["artifact"],
        "archive_format": asset["format"],
        "archive_path_prefix": asset["path_prefix"],
        "archive_sha256": archive_sha256,
        "max_expanded_bytes": asset["max_expanded_bytes"],
        "max_files": asset["max_files"],
        "files": installed_records,
    }
    return record


def inspect_github_transcode(archive: Path, asset: dict) -> list[dict]:
    if asset["format"] != "tar.zst" or asset["backend"] != "ort-cpu":
        raise MetadataError(
            "GitHub transcode inspection is limited to ORT CPU tar.zst assets"
        )
    expected_name = asset["artifact"].removesuffix(".tar.zst") + ".tar.gz"
    if archive.name != expected_name:
        raise MetadataError(
            f"GitHub transcode name {archive.name} does not match {expected_name}"
        )
    with snapshot_regular_file(archive) as (snapshot, _, _):
        records = inspect_tar_file(snapshot, asset, "r:gz")
    return [
        {**record, "path": relative_asset_path(record["path"], asset)}
        for record in records
    ]




def canonical_json(value: object) -> str:
    return json.dumps(value, ensure_ascii=True, separators=(",", ":"), sort_keys=True)


def encode(value: object) -> str:
    return base64.b64encode(canonical_json(value).encode("utf-8")).decode("ascii")


def inspect_catalog(layout: dict, artifact_dir: Path) -> dict[str, dict]:
    if not artifact_dir.is_dir():
        raise MetadataError(f"artifact directory does not exist: {artifact_dir}")
    inspected = {}
    for asset_id, asset in layout["assets"].items():
        archive = artifact_dir / asset["artifact"]
        inspected[asset_id] = inspect_asset(archive, asset)
    validate_publication_pins(layout, inspected)
    return inspected


def generate_authorities(layout: dict) -> list[tuple[str, dict]]:
    result = []
    for authority in layout["targets"]:
        record = {
            "schema_version": 1,
            "target": authority["target"],
            "backend": authority["backend"],
            "model_contract": layout["model_contract"],
            "runtime_install_manifest_schema_version": layout[
                "runtime_install_manifest_contract"
            ]["schema_version"],
            "asset_ids": authority["asset_ids"],
        }
        result.append((authority["key"], record))
    return result


def generate_lines(layout: dict, artifact_dir: Path) -> list[str]:
    catalog = {
        "schema_version": 1,
        "assets": inspect_catalog(layout, artifact_dir),
    }
    lines = [
        "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION=1",
        f"CTX_RELEASE_SEMANTIC_ASSETS={encode(catalog)}",
    ]
    lines.extend(
        f"CTX_RELEASE_SEMANTIC_AUTHORITY_{key}={encode(record)}"
        for key, record in generate_authorities(layout)
    )
    return lines


def parse_release_metadata_bytes(body: bytes, label: str) -> dict[str, str]:
    try:
        text = body.decode("utf-8")
    except UnicodeDecodeError as error:
        raise MetadataError(f"could not read UTF-8 release metadata {label}: {error}") from error
    values = {}
    for number, raw in enumerate(text.splitlines(), start=1):
        line = raw.removesuffix("\r")
        if not line or line.lstrip().startswith("#"):
            continue
        if "=" not in line:
            raise MetadataError(f"release metadata line {number} is malformed")
        key, value = line.split("=", 1)
        if (
            not key
            or not all(
                character.isascii()
                and (character.isalnum() or character == "_")
                for character in key
            )
            or key in values
        ):
            raise MetadataError(
                f"release metadata line {number} has an invalid or duplicate key"
            )
        values[key] = value
    return values


def parse_release_metadata(path: Path) -> dict[str, str]:
    try:
        body = path.read_bytes()
    except OSError as error:
        raise MetadataError(f"could not read release metadata {path}: {error}") from error
    return parse_release_metadata_bytes(body, str(path))


def validate_signing_metadata_bytes(
    layout: dict,
    metadata: bytes,
    artifact_dir: Path | None,
    *,
    label: str = "<stdin>",
    allow_legacy_pre_v0260_nonsemantic: bool = False,
    allow_managed_pair_nonsemantic: bool = False,
) -> None:
    values = parse_release_metadata_bytes(metadata, label)
    components = values.get("CTX_RELEASE_VERSION", "").split(".")
    release_components = tuple(int(component) for component in components) if (
        len(components) == 3
        and all(component.isascii() and component.isdigit()
                and (len(component) == 1 or not component.startswith("0"))
                for component in components)
    ) else None
    semantic_keys = {key for key in values if key.startswith(SEMANTIC_METADATA_PREFIX)}
    if (values.get("CTX_RELEASE_CHANNEL") == "stable" and release_components
            and release_components[0] >= 1 and not allow_managed_pair_nonsemantic):
        raise MetadataError(
            "current stable signing requires --managed-pair-publication and --runtime-handoff"
        )
    if allow_legacy_pre_v0260_nonsemantic and (semantic_keys or allow_managed_pair_nonsemantic):
        raise MetadataError("legacy signing accepts only pre-0.26.0 non-semantic metadata")
    onnxruntime_values = {
        key: value
        for key, value in values.items()
        if key.startswith("CTX_RELEASE_ONNXRUNTIME_")
    }
    if onnxruntime_values:
        expected_onnxruntime_keys = {"CTX_RELEASE_ONNXRUNTIME_VERSION"}
        for target in RUNTIME_TRANSPORT_ARTIFACTS:
            expected_onnxruntime_keys.update({
                f"CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{target}",
                f"CTX_RELEASE_ONNXRUNTIME_SHA256_{target}",
            })
        runtime_version = onnxruntime_values.get("CTX_RELEASE_ONNXRUNTIME_VERSION", "")
        valid_artifacts = all(
            onnxruntime_values.get(f"CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{target}") == artifact
            for target, artifact in RUNTIME_TRANSPORT_ARTIFACTS.items()
        )
        valid_digests = all(
            len(
                digest := onnxruntime_values.get(
                    f"CTX_RELEASE_ONNXRUNTIME_SHA256_{target}", ""
                )
            )
            == 64
            and digest != "0" * 64
            and all(character in "0123456789abcdef" for character in digest)
            for target in RUNTIME_TRANSPORT_ARTIFACTS
        )
        if (
            not allow_managed_pair_nonsemantic
            or set(onnxruntime_values) != expected_onnxruntime_keys
            or re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", runtime_version) is None
            or not valid_artifacts
            or not valid_digests
        ):
            raise MetadataError(
                "release metadata contains invalid platform compatibility ONNX Runtime fields"
            )
    elif allow_managed_pair_nonsemantic:
        raise MetadataError(
            "managed-pair metadata requires the complete platform compatibility "
            "ONNX Runtime field set"
        )
    if not semantic_keys:
        managed_pair_keys = {
            key for key in values if key.startswith("CTX_RELEASE_MANAGED_PAIR_")
        }
        managed_pair_expected = {
            f"CTX_RELEASE_MANAGED_PAIR_{field}_{target}"
            for field in (
                "ENVELOPE",
                "CORE_OBJECT",
                "CORE_SHA256",
                "COMPANION_OBJECT",
                "COMPANION_SHA256",
            )
            for target in (
                "linux_x64",
                "linux_aarch64",
                "macos_arm64",
                "macos_x64",
                "windows_x64",
            )
        }
        if allow_managed_pair_nonsemantic:
            stable_v1 = release_components is not None and release_components[0] == 1
            if (
                not stable_v1
                or values.get("CTX_RELEASE_CHANNEL") != "stable"
                or managed_pair_keys != managed_pair_expected
                or artifact_dir is not None
            ):
                raise MetadataError(
                    "managed-pair non-semantic mode requires an exact stable 1.x "
                    "five-target pair field set and no semantic artifact directory"
                )
            return
        if managed_pair_keys:
            raise MetadataError(
                "managed-pair non-semantic metadata requires its exact publication mode"
            )
        if values.get("CTX_RELEASE_VERSION") == SEMANTIC_REQUIRED_RELEASE_VERSION:
            raise MetadataError(
                f"release {SEMANTIC_REQUIRED_RELEASE_VERSION} metadata requires "
                "the complete semantic field set and --semantic-artifact-dir"
            )
        if not allow_legacy_pre_v0260_nonsemantic:
            raise MetadataError(
                "non-semantic release metadata requires the explicit "
                "--allow-legacy-pre-v0260-nonsemantic mode"
            )
        if (
            release_components is None
            or release_components >= SEMANTIC_REQUIRED_RELEASE_COMPONENTS
        ):
            raise MetadataError(
                "--allow-legacy-pre-v0260-nonsemantic only accepts canonical "
                "release versions older than 0.26.0"
            )
        if artifact_dir is not None:
            raise MetadataError(
                "--semantic-artifact-dir was provided but metadata has no semantic fields"
            )
        return
    if semantic_keys != SEMANTIC_METADATA_KEYS:
        missing = sorted(SEMANTIC_METADATA_KEYS - semantic_keys)
        unexpected = sorted(semantic_keys - SEMANTIC_METADATA_KEYS)
        details = []
        if missing:
            details.append(f"missing {', '.join(missing)}")
        if unexpected:
            details.append(f"unexpected {', '.join(unexpected)}")
        raise MetadataError(
            "release metadata has the wrong semantic field set: "
            + "; ".join(details)
        )
    if artifact_dir is None:
        raise MetadataError(
            "semantic release metadata requires --semantic-artifact-dir "
            "for final-archive verification before signing"
        )
    generated = dict(line.split("=", 1) for line in generate_lines(layout, artifact_dir))
    for key in sorted(SEMANTIC_METADATA_KEYS):
        if values.get(key) != generated[key]:
            raise MetadataError(
                f"release metadata {key} does not match final semantic artifact bytes"
            )


def validate_signing_metadata(
    layout: dict,
    metadata: Path,
    artifact_dir: Path | None,
    *,
    allow_legacy_pre_v0260_nonsemantic: bool = False,
    allow_managed_pair_nonsemantic: bool = False,
) -> None:
    try:
        body = metadata.read_bytes()
    except OSError as error:
        raise MetadataError(f"could not read release metadata {metadata}: {error}") from error
    validate_signing_metadata_bytes(
        layout,
        body,
        artifact_dir,
        label=str(metadata),
        allow_legacy_pre_v0260_nonsemantic=allow_legacy_pre_v0260_nonsemantic,
        allow_managed_pair_nonsemantic=allow_managed_pair_nonsemantic,
    )


def write_output(lines: list[str], output: Path | None) -> None:
    text = "\n".join(lines) + "\n"
    if output is None:
        sys.stdout.write(text)
        return
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = output.with_name(f".{output.name}.{os.getpid()}.tmp")
    try:
        temporary.write_text(text, encoding="utf-8")
        os.replace(temporary, output)
    finally:
        temporary.unlink(missing_ok=True)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate canonical CTX_RELEASE_SEMANTIC_AUTHORITY_* metadata"
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    generate = subparsers.add_parser(
        "generate", help="generate all four target/backend authorities"
    )
    generate.add_argument("--artifact-dir", required=True, type=Path)
    generate.add_argument("--output", type=Path)
    inspect = subparsers.add_parser(
        "inspect", help="inspect one named final archive and print its canonical base64 record"
    )
    inspect.add_argument("--artifact", required=True)
    inspect.add_argument("--archive", required=True, type=Path)
    inspect_transcode = subparsers.add_parser(
        "inspect-github-transcode",
        help="inspect one GitHub tar.gz transcode against its signed tar.zst layout",
    )
    inspect_transcode.add_argument("--artifact", required=True)
    inspect_transcode.add_argument("--archive", required=True, type=Path)
    validate_signing = subparsers.add_parser(
        "validate-signing",
        help="fail closed unless semantic metadata exactly matches final archives",
    )
    metadata_source = validate_signing.add_mutually_exclusive_group(required=True)
    metadata_source.add_argument("--metadata", type=Path)
    metadata_source.add_argument("--metadata-stdin", action="store_true")
    validate_signing.add_argument("--metadata-label", default="<stdin>")
    validate_signing.add_argument("--semantic-artifact-dir", type=Path)
    for option in (
        "--allow-legacy-pre-v0260-nonsemantic",
        "--allow-managed-pair-nonsemantic",
    ):
        validate_signing.add_argument(option, action="store_true")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        layout = load_layout()
        if args.command == "generate":
            write_output(generate_lines(layout, args.artifact_dir), args.output)
        elif args.command in {"inspect", "inspect-github-transcode"}:
            asset = asset_layout(layout, args.artifact)
            if args.command == "inspect" and args.archive.name != asset["artifact"]:
                raise MetadataError(
                    f"archive name {args.archive.name} does not match {asset['artifact']}"
                )
            if args.command == "inspect":
                record = inspect_asset(args.archive, asset)
                asset_id = next(
                    asset_id
                    for asset_id, candidate in layout["assets"].items()
                    if candidate is asset
                )
                validate_publication_pins(layout, {asset_id: record})
                sys.stdout.write(encode(record) + "\n")
            else:
                sys.stdout.write(
                    encode(inspect_github_transcode(args.archive, asset)) + "\n"
                )
        else:
            validation_modes = {
                "allow_legacy_pre_v0260_nonsemantic": args.allow_legacy_pre_v0260_nonsemantic,
                "allow_managed_pair_nonsemantic": args.allow_managed_pair_nonsemantic,
            }
            if args.metadata_stdin:
                validate_signing_metadata_bytes(
                    layout,
                    sys.stdin.buffer.read(),
                    args.semantic_artifact_dir,
                    label=args.metadata_label,
                    **validation_modes,
                )
            else:
                validate_signing_metadata(
                    layout,
                    args.metadata,
                    args.semantic_artifact_dir,
                    **validation_modes,
                )
    except (MetadataError, KeyError, TypeError, ValueError) as error:
        print(f"semantic runtime metadata failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
