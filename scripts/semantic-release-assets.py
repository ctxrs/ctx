#!/usr/bin/env python3
"""Build, validate, and describe the signed public Semantic release assets."""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import stat
import tarfile
import tempfile
from pathlib import Path
from typing import BinaryIO


SCHEMA_VERSION = 1
MODEL_VERSION = "1.0.0"
MODEL_ID = "intfloat/multilingual-e5-small"
MODEL_REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"
MODEL_MAX_EXPANDED_BYTES = 768 * 1024 * 1024
MODEL_PATHS = (
    "LICENSE",
    "config.json",
    "manifest.json",
    "onnx/model.onnx",
    "special_tokens_map.json",
    "tokenizer.json",
    "tokenizer_config.json",
)
COMMON_MODEL_FILES = {
    "config.json": (
        655,
        "69137736cab8b8903a07fe8afaafdda25aac55415a12a55d1bffa9f581abf959",
    ),
    "special_tokens_map.json": (
        167,
        "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7",
    ),
    "tokenizer.json": (
        17_082_730,
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
    "tokenizer_config.json": (
        443,
        "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b",
    ),
}
MODEL_VARIANTS = {
    "cpu-fp32": {
        "asset_id": "onnx_model",
        "artifact": "ctx-multilingual-e5-small-onnx-fp32-1.0.0.tar.xz",
        "onnx_size": 470_268_510,
        "onnx_sha256": "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    },
    "accelerator-o4-fp16": {
        "asset_id": "onnx_model_o4_fp16",
        "artifact": "ctx-multilingual-e5-small-onnx-o4-fp16-1.0.0.tar.xz",
        "onnx_size": 235_052_531,
        "onnx_sha256": "4654c156f3e4171abc9c716cdb771bf9116455d15ac1aab364aeeede0e3205b0",
    },
}
EXPECTED_ASSET_IDS = {
    "apple_coreml",
    "freebsd_x64_cpu",
    "linux_aarch64_cpu",
    "linux_x64_cpu",
    "linux_cuda12",
    "macos_arm64_cpu",
    "macos_x64_cpu",
    "onnx_model",
    "onnx_model_o4_fp16",
    "windows_ml",
}
CPU_RUNTIME_FILES = (
    "GIT_COMMIT_ID",
    "LICENSE",
    "ThirdPartyNotices.txt",
    "VERSION_NUMBER",
)
CUDA_FILES = (
    *CPU_RUNTIME_FILES,
    "NVIDIA-CUDA-LICENSE.txt",
    "NVIDIA-CUDNN-LICENSE.txt",
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
)
WINDOWS_ML_FILES = (
    "LICENSE",
    "ThirdPartyNotices.txt",
    "lib/DirectML.dll",
    "lib/Microsoft.Windows.AI.MachineLearning.dll",
    "lib/onnxruntime.dll",
)
COREML_ARCHIVE_SHA256 = (
    "94c6fac5c4250079401d383adf1b10270fe5d370f2091dbad17bf4823222321e"
)
COREML_MANIFEST_SHA256 = (
    "576c68756563333fdf442e6859f2392ca0065b09a2cb5d73983e30de75df1ad6"
)
EXPECTED_ASSETS = {
    "onnx_model": {
        "role": "model",
        "backend": "onnx",
        "version": MODEL_VERSION,
        "platform": "any",
        "artifact": MODEL_VARIANTS["cpu-fp32"]["artifact"],
        "archive_format": "tar.xz",
        "archive_path_prefix": MODEL_VARIANTS["cpu-fp32"]["artifact"].removesuffix(
            ".tar.xz"
        ),
        "max_expanded_bytes": 603_979_776,
        "max_files": 16,
        "files": MODEL_PATHS,
    },
    "onnx_model_o4_fp16": {
        "role": "model",
        "backend": "onnx",
        "version": MODEL_VERSION,
        "platform": "any",
        "artifact": MODEL_VARIANTS["accelerator-o4-fp16"]["artifact"],
        "archive_format": "tar.xz",
        "archive_path_prefix": MODEL_VARIANTS["accelerator-o4-fp16"][
            "artifact"
        ].removesuffix(".tar.xz"),
        "max_expanded_bytes": 335_544_320,
        "max_files": 16,
        "files": MODEL_PATHS,
    },
    "apple_coreml": {
        "role": "accelerator",
        "backend": "coreml",
        "version": MODEL_VERSION,
        "platform": "macos-arm64",
        "artifact": "ctx-multilingual-e5-small-coreml-fp16-1.0.0.tar.xz",
        "archive_format": "tar.xz",
        "archive_path_prefix": "ctx-multilingual-e5-small-coreml-fp16-1.0.0",
        "max_expanded_bytes": 2_147_483_648,
        "max_files": 4096,
        "files": None,
    },
    "linux_x64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "linux-x64",
        "artifact": "ctx-onnxruntime-linux-x64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 67_108_864,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.so"),
    },
    "linux_aarch64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "linux-aarch64",
        "artifact": "ctx-onnxruntime-linux-aarch64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 67_108_864,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.so"),
    },
    "macos_arm64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "macos-arm64",
        "artifact": "ctx-onnxruntime-macos-arm64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 100_663_296,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.dylib"),
    },
    "macos_x64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "macos-x64",
        "artifact": "ctx-onnxruntime-macos-x64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 100_663_296,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.dylib"),
    },
    "windows_ml": {
        "role": "cpu-runtime",
        "backend": "windows-ml",
        "version": "2.1.74",
        "platform": "windows-x64",
        "artifact": "ctx-windowsml-windows-x64.zip",
        "archive_format": "zip",
        "archive_path_prefix": "",
        "max_expanded_bytes": 50_331_648,
        "max_files": 5,
        "files": WINDOWS_ML_FILES,
    },
    "freebsd_x64_cpu": {
        "role": "cpu-runtime",
        "backend": "ort-cpu",
        "version": "1.27.0",
        "platform": "freebsd-x64",
        "artifact": "ctx-onnxruntime-freebsd-x64.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 67_108_864,
        "max_files": 8,
        "files": (*CPU_RUNTIME_FILES, "lib/libonnxruntime.so"),
    },
    "linux_cuda12": {
        "role": "accelerator",
        "backend": "ort-cuda",
        "version": "1.27.0",
        "platform": "linux-x64-cuda12",
        "artifact": "ctx-onnxruntime-linux-x64-cuda12.tar.zst",
        "archive_format": "tar.zst",
        "archive_path_prefix": "",
        "max_expanded_bytes": 2_147_483_648,
        "max_files": 18,
        "files": tuple(sorted(CUDA_FILES)),
    },
}


class AssetError(ValueError):
    pass


def canonical_json(value: object) -> bytes:
    return json.dumps(
        value, ensure_ascii=True, separators=(",", ":"), sort_keys=True
    ).encode("ascii")


def sha256_stream(stream: BinaryIO, maximum: int) -> tuple[int, str]:
    size = 0
    digest = hashlib.sha256()
    while block := stream.read(1024 * 1024):
        size += len(block)
        if size > maximum:
            raise AssetError(f"file exceeds {maximum} byte safety limit")
        digest.update(block)
    return size, digest.hexdigest()


def sha256_file(path: Path, maximum: int = 2 * 1024 * 1024 * 1024) -> tuple[int, str]:
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise AssetError(f"not a regular file: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            size, digest = sha256_stream(stream, maximum)
        after = path.lstat()
        if (
            stat.S_ISLNK(after.st_mode)
            or (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
        ):
            raise AssetError(f"file changed while hashing: {path}")
        return size, digest
    finally:
        os.close(descriptor)


def validate_relative_path(value: str) -> None:
    if (
        not value
        or not value.isascii()
        or any(not 0x20 <= byte <= 0x7E for byte in value.encode("ascii"))
        or "\\" in value
        or ":" in value
        or value.startswith("/")
        or value.endswith("/")
        or "//" in value
        or len(value.encode("ascii")) > 512
    ):
        raise AssetError(f"unsafe asset path: {value!r}")
    if any(
        part in ("", ".", "..")
        or part.endswith(".")
        or part.endswith(" ")
        or windows_reserved_component(part)
        for part in value.split("/")
    ):
        raise AssetError(f"unsafe asset path: {value!r}")


def windows_reserved_component(component: str) -> bool:
    stem = component.split(".", 1)[0].upper()
    return stem in {"CON", "PRN", "AUX", "NUL"} or (
        len(stem) == 4
        and stem[:3] in {"COM", "LPT"}
        and stem[3] in "123456789"
    )


def validate_lowercase_sha256(value: object) -> None:
    if (
        not isinstance(value, str)
        or len(value) != 64
        or any(byte not in b"0123456789abcdef" for byte in value.encode("ascii", "ignore"))
        or not value.isascii()
        or value == "0" * 64
    ):
        raise AssetError("Semantic checksum must use lowercase SHA-256 hex")


def validate_artifact_name(value: object) -> None:
    if not isinstance(value, str):
        raise AssetError("Semantic artifact name must be a string")
    validate_relative_path(value)
    if "/" in value or value in (".", ".."):
        raise AssetError(f"unsafe Semantic artifact name: {value!r}")


def model_required_files(variant: str) -> dict[str, tuple[int, str]]:
    selected = MODEL_VARIANTS[variant]
    return {
        **COMMON_MODEL_FILES,
        "onnx/model.onnx": (
            selected["onnx_size"],
            selected["onnx_sha256"],
        ),
    }


def model_manifest(variant: str) -> bytes:
    return canonical_json(
        {
            "model_contract": {
                "dimensions": 384,
                "model_id": MODEL_ID,
                "normalization": "l2",
                "passage_prefix": "passage: ",
                "pooling": "attention_mask_mean",
                "query_prefix": "query: ",
                "revision": MODEL_REVISION,
            },
            "schema_version": SCHEMA_VERSION,
            "variant": variant,
            "version": MODEL_VERSION,
        }
    )


def verify_model_source(source: Path, variant: str) -> None:
    metadata = source.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise AssetError(f"model source is not a real directory: {source}")
    license_path = source / "LICENSE"
    license_size, _ = sha256_file(license_path, 4 * 1024 * 1024)
    if license_size == 0:
        raise AssetError("model LICENSE must not be empty")
    for relative, expected in model_required_files(variant).items():
        size, digest = sha256_file(source.joinpath(*relative.split("/")))
        if (size, digest) != expected:
            raise AssetError(
                f"pinned model file mismatch for {relative}: "
                f"expected {expected[0]}/{expected[1]}, got {size}/{digest}"
            )


def add_tar_directory(bundle: tarfile.TarFile, name: str) -> None:
    entry = tarfile.TarInfo(name)
    entry.type = tarfile.DIRTYPE
    entry.mode = 0o755
    entry.uid = entry.gid = 0
    entry.uname = entry.gname = ""
    entry.mtime = 0
    bundle.addfile(entry)


def add_tar_file(bundle: tarfile.TarFile, source: Path, name: str) -> None:
    size = source.stat().st_size
    entry = tarfile.TarInfo(name)
    entry.type = tarfile.REGTYPE
    entry.mode = 0o644
    entry.uid = entry.gid = 0
    entry.uname = entry.gname = ""
    entry.mtime = 0
    entry.size = size
    with source.open("rb") as stream:
        bundle.addfile(entry, stream)


def build_model(args: argparse.Namespace) -> None:
    selected = MODEL_VARIANTS[args.variant]
    verify_model_source(args.source, args.variant)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    artifact = args.output_dir / selected["artifact"]
    metadata_path = artifact.with_suffix(artifact.suffix + ".asset.json")
    checksum_path = artifact.with_suffix(artifact.suffix + ".sha256")
    for output in (artifact, metadata_path, checksum_path):
        if output.exists() or output.is_symlink():
            raise AssetError(f"refusing to replace existing output: {output}")

    prefix = artifact.name.removesuffix(".tar.xz")
    with tempfile.TemporaryDirectory(
        prefix=f".{prefix}.", dir=args.output_dir
    ) as temporary:
        staging = Path(temporary)
        for relative in MODEL_PATHS:
            destination = staging.joinpath(*relative.split("/"))
            destination.parent.mkdir(parents=True, exist_ok=True)
            if relative == "manifest.json":
                destination.write_bytes(model_manifest(args.variant))
            else:
                source = args.source.joinpath(*relative.split("/"))
                with source.open("rb") as input_stream, destination.open(
                    "xb"
                ) as output_stream:
                    while block := input_stream.read(1024 * 1024):
                        output_stream.write(block)
        temporary_archive = staging / f".{artifact.name}.tmp"
        with tarfile.open(
            temporary_archive, "w:xz", format=tarfile.USTAR_FORMAT, preset=9
        ) as bundle:
            add_tar_directory(bundle, prefix)
            add_tar_file(bundle, staging / "LICENSE", f"{prefix}/LICENSE")
            add_tar_file(bundle, staging / "config.json", f"{prefix}/config.json")
            add_tar_file(bundle, staging / "manifest.json", f"{prefix}/manifest.json")
            add_tar_directory(bundle, f"{prefix}/onnx")
            add_tar_file(
                bundle, staging / "onnx" / "model.onnx", f"{prefix}/onnx/model.onnx"
            )
            for relative in (
                "special_tokens_map.json",
                "tokenizer.json",
                "tokenizer_config.json",
            ):
                add_tar_file(bundle, staging / relative, f"{prefix}/{relative}")
        os.replace(temporary_archive, artifact)

    records = validate_model_archive(artifact, args.variant)
    write_asset_record(
        metadata_path,
        selected["asset_id"],
        "model",
        "onnx",
        MODEL_VERSION,
        "any",
        "tar.xz",
        prefix,
        artifact,
        records,
    )
    _, archive_sha256 = sha256_file(artifact)
    checksum_path.write_text(f"{archive_sha256}  {artifact.name}\n", encoding="ascii")
    print(f"artifact={artifact}")
    print(f"metadata={metadata_path}")


def canonical_tar_name(raw: str) -> str:
    if not raw or "\\" in raw or raw.startswith("/"):
        raise AssetError(f"unsafe model archive path: {raw!r}")
    directory = raw.endswith("/")
    name = raw[:-1] if directory else raw
    validate_relative_path(name)
    return name


def validate_model_archive(archive: Path, variant: str) -> list[dict[str, object]]:
    selected = MODEL_VARIANTS[variant]
    if archive.name != selected["artifact"]:
        raise AssetError(
            f"model archive must be named {selected['artifact']}, got {archive.name}"
        )
    prefix = archive.name.removesuffix(".tar.xz")
    expected_files = {f"{prefix}/{path}": path for path in MODEL_PATHS}
    expected_directories = {prefix, f"{prefix}/onnx"}
    seen: set[str] = set()
    records = []
    total = 0
    with tarfile.open(archive, "r:xz") as bundle:
        for member in bundle:
            name = canonical_tar_name(member.name)
            folded = name.casefold()
            if folded in seen:
                raise AssetError(f"duplicate or case-colliding archive path: {name}")
            seen.add(folded)
            if member.mode & 0o7000:
                raise AssetError(f"unsafe mode on model archive path: {name}")
            if member.isdir():
                if name not in expected_directories:
                    raise AssetError(f"unexpected model archive directory: {name}")
                continue
            relative = expected_files.get(name)
            if relative is None or not member.isfile():
                raise AssetError(f"unexpected model archive entry: {name}")
            total += member.size
            if member.size <= 0 or total > MODEL_MAX_EXPANDED_BYTES:
                raise AssetError("model archive exceeds its expanded-size limit")
            source = bundle.extractfile(member)
            if source is None:
                raise AssetError(f"could not read model archive entry: {name}")
            with source:
                size, digest = sha256_stream(source, member.size)
            if size != member.size:
                raise AssetError(f"truncated model archive entry: {name}")
            records.append({"path": relative, "sha256": digest, "size": size})
    if seen != {
        *(name.casefold() for name in expected_files),
        *(name.casefold() for name in expected_directories),
    }:
        raise AssetError("model archive does not contain the exact required path set")
    records.sort(key=lambda value: str(value["path"]))
    record_map = {
        str(record["path"]): (record["size"], record["sha256"]) for record in records
    }
    for relative, expected in model_required_files(variant).items():
        if record_map.get(relative) != expected:
            raise AssetError(f"pinned model identity mismatch in archive: {relative}")
    expected_manifest = model_manifest(variant)
    manifest_record = record_map["manifest.json"]
    if manifest_record != (
        len(expected_manifest),
        hashlib.sha256(expected_manifest).hexdigest(),
    ):
        raise AssetError("model archive manifest is not canonical")
    return records


def validate_model(args: argparse.Namespace) -> None:
    records = validate_model_archive(args.archive, args.variant)
    _, digest = sha256_file(args.archive)
    print(f"archive_sha256={digest}")
    print(f"files={len(records)}")


def collect_records(root: Path, paths: list[str]) -> list[dict[str, object]]:
    if paths != sorted(set(paths)):
        raise AssetError("--file values must be unique and sorted")
    records = []
    folded: set[str] = set()
    for relative in paths:
        validate_relative_path(relative)
        if relative.casefold() in folded:
            raise AssetError(f"case-colliding asset path: {relative}")
        folded.add(relative.casefold())
        size, digest = sha256_file(root.joinpath(*relative.split("/")))
        if size == 0:
            raise AssetError(f"asset file must not be empty: {relative}")
        records.append({"path": relative, "sha256": digest, "size": size})
    actual = []
    for directory, names, files in os.walk(root, followlinks=False):
        names.sort()
        files.sort()
        current = Path(directory)
        for name in names:
            metadata = (current / name).lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise AssetError(f"unsupported asset directory: {current / name}")
        for name in files:
            path = current / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                raise AssetError(f"unsupported asset file: {path}")
            actual.append(path.relative_to(root).as_posix())
    if sorted(actual) != paths:
        raise AssetError("staged asset files do not exactly match --file values")
    return records


ASSET_FIELDS = {
    "archive_format",
    "archive_path_prefix",
    "archive_sha256",
    "artifact",
    "backend",
    "files",
    "max_expanded_bytes",
    "max_files",
    "platform",
    "role",
    "version",
}
FILE_FIELDS = {"path", "sha256", "size"}
COREML_EXACT_PATHS = {
    "PROVENANCE.json",
    "THIRD_PARTY_NOTICES.md",
    "manifest.json",
    "tokenizer.json",
}
COREML_TREE_PREFIXES = ("LICENSES/", "document.mlpackage/", "query.mlpackage/")


def validate_asset_record(asset_id: str, asset: object) -> None:
    if asset_id not in EXPECTED_ASSETS:
        raise AssetError(f"unsupported Semantic asset ID: {asset_id!r}")
    if not isinstance(asset, dict) or set(asset) != ASSET_FIELDS:
        raise AssetError(f"invalid Semantic asset fields for {asset_id}")
    expected = EXPECTED_ASSETS[asset_id]
    for field in (
        "role",
        "backend",
        "version",
        "platform",
        "artifact",
        "archive_format",
        "archive_path_prefix",
        "max_expanded_bytes",
        "max_files",
    ):
        if asset[field] != expected[field] or type(asset[field]) is not type(expected[field]):
            raise AssetError(f"noncanonical {field} for Semantic asset {asset_id}")

    validate_artifact_name(asset["artifact"])
    validate_lowercase_sha256(asset["archive_sha256"])
    prefix = asset["archive_path_prefix"]
    if prefix:
        validate_relative_path(prefix)
    files = asset["files"]
    if not isinstance(files, list) or not files or len(files) > expected["max_files"]:
        raise AssetError(f"unsafe file count for Semantic asset {asset_id}")

    paths = []
    folded: set[str] = set()
    total = 0
    records: dict[str, tuple[int, str]] = {}
    previous = None
    for record in files:
        if not isinstance(record, dict) or set(record) != FILE_FIELDS:
            raise AssetError(f"invalid file record for Semantic asset {asset_id}")
        path = record["path"]
        size = record["size"]
        digest = record["sha256"]
        if not isinstance(path, str):
            raise AssetError(f"invalid file path for Semantic asset {asset_id}")
        validate_relative_path(path)
        if previous is not None and previous >= path:
            raise AssetError(f"file records are not strictly sorted for {asset_id}")
        previous = path
        if path.casefold() in folded:
            raise AssetError(f"duplicate or case-colliding file path for {asset_id}")
        folded.add(path.casefold())
        if (
            (asset["backend"].startswith("ort-") or asset["backend"] == "windows-ml")
            and path == "ctx-runtime-install.json"
        ):
            raise AssetError(f"{asset_id} claims the reserved install manifest path")
        if type(size) is not int or size <= 0:
            raise AssetError(f"invalid file size for Semantic asset {asset_id}")
        validate_lowercase_sha256(digest)
        total += size
        if total > expected["max_expanded_bytes"]:
            raise AssetError(f"expanded size exceeds signed limit for {asset_id}")
        paths.append(path)
        records[path] = (size, digest)

    expected_paths = expected["files"]
    if expected_paths is not None:
        if paths != sorted(expected_paths):
            raise AssetError(f"wrong file inventory for Semantic asset {asset_id}")
    else:
        path_set = set(paths)
        missing = COREML_EXACT_PATHS - path_set
        missing_prefixes = [
            prefix for prefix in COREML_TREE_PREFIXES if not any(path.startswith(prefix) for path in paths)
        ]
        if missing or missing_prefixes:
            raise AssetError(
                f"Core ML asset is missing required paths: "
                f"{sorted(missing) + missing_prefixes}"
            )
        if any(
            path not in COREML_EXACT_PATHS
            and not path.startswith(COREML_TREE_PREFIXES)
            for path in paths
        ):
            raise AssetError("Core ML asset contains an unexpected path")

    if asset_id in ("onnx_model", "onnx_model_o4_fp16"):
        variant = (
            "cpu-fp32" if asset_id == "onnx_model" else "accelerator-o4-fp16"
        )
        for path, pinned in model_required_files(variant).items():
            if records.get(path) != pinned:
                raise AssetError(f"pinned model identity mismatch for {asset_id}: {path}")
        if records["LICENSE"][0] <= 0:
            raise AssetError(f"model LICENSE must not be empty for {asset_id}")
        manifest = model_manifest(variant)
        if records["manifest.json"] != (
            len(manifest),
            hashlib.sha256(manifest).hexdigest(),
        ):
            raise AssetError(f"model manifest is not canonical for {asset_id}")
    elif asset_id == "apple_coreml":
        if asset["archive_sha256"] != COREML_ARCHIVE_SHA256:
            raise AssetError("Core ML archive does not match its publication pin")
        if records["manifest.json"][1] != COREML_MANIFEST_SHA256:
            raise AssetError("Core ML manifest does not match its publication pin")


def asset_record(
    asset_id: str,
    role: str,
    backend: str,
    version: str,
    platform: str,
    archive_format: str,
    prefix: str,
    artifact: Path,
    records: list[dict[str, object]],
) -> dict[str, object]:
    expected = EXPECTED_ASSETS.get(asset_id)
    if expected is None:
        raise AssetError(f"unsupported Semantic asset ID: {asset_id!r}")
    supplied = {
        "role": role,
        "backend": backend,
        "version": version,
        "platform": platform,
        "archive_format": archive_format,
        "archive_path_prefix": prefix,
        "artifact": artifact.name,
    }
    for field, value in supplied.items():
        if value != expected[field]:
            raise AssetError(f"noncanonical {field} for Semantic asset {asset_id}")
    _, archive_sha256 = sha256_file(artifact)
    value = {
        "id": asset_id,
        "asset": {
            "archive_format": archive_format,
            "archive_path_prefix": prefix,
            "archive_sha256": archive_sha256,
            "artifact": artifact.name,
            "backend": backend,
            "files": records,
            "max_expanded_bytes": expected["max_expanded_bytes"],
            "max_files": expected["max_files"],
            "platform": platform,
            "role": role,
            "version": version,
        },
    }
    validate_asset_record(asset_id, value["asset"])
    return value


def write_asset_record(
    output: Path,
    asset_id: str,
    role: str,
    backend: str,
    version: str,
    platform: str,
    archive_format: str,
    prefix: str,
    artifact: Path,
    records: list[dict[str, object]],
) -> None:
    output.write_bytes(
        canonical_json(
            asset_record(
                asset_id,
                role,
                backend,
                version,
                platform,
                archive_format,
                prefix,
                artifact,
                records,
            )
        )
        + b"\n"
    )


def record_asset(args: argparse.Namespace) -> None:
    records = collect_records(args.root, args.file)
    write_asset_record(
        args.output,
        args.asset_id,
        args.role,
        args.backend,
        args.version,
        args.platform,
        args.archive_format,
        args.archive_path_prefix,
        args.archive,
        records,
    )
    print(f"metadata={args.output}")


def authority(target: str, backend: str, asset_ids: list[str]) -> dict[str, object]:
    return {
        "asset_ids": asset_ids,
        "backend": backend,
        "model_contract": {
            "dimensions": 384,
            "model_id": MODEL_ID,
            "normalization": "l2",
            "passage_prefix": "passage: ",
            "pooling": "attention_mask_mean",
            "query_prefix": "query: ",
            "revision": MODEL_REVISION,
        },
        "runtime_install_manifest_schema_version": 1,
        "schema_version": 1,
        "target": target,
    }


def encode_record(value: object) -> str:
    return base64.b64encode(canonical_json(value)).decode("ascii")


def reject_duplicate_json_keys(pairs: list[tuple[str, object]]) -> dict[str, object]:
    value: dict[str, object] = {}
    for key, item in pairs:
        if key in value:
            raise AssetError(f"duplicate JSON key in Semantic asset record: {key}")
        value[key] = item
    return value


def build_catalog(args: argparse.Namespace) -> None:
    records: dict[str, dict[str, object]] = {}
    for path in args.asset_record:
        raw = path.read_bytes()
        value = json.loads(raw, object_pairs_hook=reject_duplicate_json_keys)
        if set(value) != {"asset", "id"} or not isinstance(value["id"], str):
            raise AssetError(f"invalid Semantic asset record: {path}")
        asset_id = value["id"]
        if asset_id in records:
            raise AssetError(f"duplicate Semantic asset record: {asset_id}")
        if canonical_json(value) + b"\n" != raw:
            raise AssetError(f"Semantic asset record is not canonical JSON: {path}")
        validate_asset_record(asset_id, value["asset"])
        records[asset_id] = value["asset"]
    if set(records) != EXPECTED_ASSET_IDS:
        missing = sorted(EXPECTED_ASSET_IDS - set(records))
        extra = sorted(set(records) - EXPECTED_ASSET_IDS)
        raise AssetError(f"wrong Semantic asset set; missing={missing}, extra={extra}")

    values = {
        "CTX_RELEASE_SEMANTIC_SCHEMA_VERSION": "1",
        "CTX_RELEASE_SEMANTIC_ASSETS": encode_record(
            {"assets": records, "schema_version": 1}
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_apple_silicon_coreml": encode_record(
            authority(
                "apple-silicon",
                "coreml",
                ["onnx_model", "macos_arm64_cpu", "apple_coreml"],
            )
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_windows_windows_ml": encode_record(
            authority(
                "windows",
                "windows-ml",
                ["onnx_model_o4_fp16", "windows_ml"],
            )
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_linux_nvidia_ort_cuda": encode_record(
            authority(
                "linux-nvidia",
                "ort-cuda",
                ["onnx_model_o4_fp16", "linux_cuda12"],
            )
        ),
        "CTX_RELEASE_SEMANTIC_AUTHORITY_universal_ort_cpu": encode_record(
            authority(
                "universal",
                "ort-cpu",
                [
                    "onnx_model",
                    "linux_x64_cpu",
                    "linux_aarch64_cpu",
                    "macos_arm64_cpu",
                    "macos_x64_cpu",
                    "windows_ml",
                    "freebsd_x64_cpu",
                ],
            )
        ),
    }
    args.output.write_text(
        "".join(f"{key}={value}\n" for key, value in values.items()),
        encoding="ascii",
    )
    print(f"metadata={args.output}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)

    build = commands.add_parser("build-model")
    build.add_argument("--variant", choices=tuple(MODEL_VARIANTS), required=True)
    build.add_argument("--source", type=Path, required=True)
    build.add_argument("--output-dir", type=Path, required=True)
    build.set_defaults(run=build_model)

    validate = commands.add_parser("validate-model")
    validate.add_argument("--variant", choices=tuple(MODEL_VARIANTS), required=True)
    validate.add_argument("--archive", type=Path, required=True)
    validate.set_defaults(run=validate_model)

    record = commands.add_parser("record")
    record.add_argument("--asset-id", required=True)
    record.add_argument("--role", required=True)
    record.add_argument("--backend", required=True)
    record.add_argument("--version", required=True)
    record.add_argument("--platform", required=True)
    record.add_argument("--archive-format", choices=("tar.xz", "tar.zst", "zip"), required=True)
    record.add_argument("--archive-path-prefix", default="")
    record.add_argument("--archive", type=Path, required=True)
    record.add_argument("--root", type=Path, required=True)
    record.add_argument("--file", action="append", default=[], required=True)
    record.add_argument("--output", type=Path, required=True)
    record.set_defaults(run=record_asset)

    catalog = commands.add_parser("catalog")
    catalog.add_argument("--asset-record", action="append", type=Path, required=True)
    catalog.add_argument("--output", type=Path, required=True)
    catalog.set_defaults(run=build_catalog)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    try:
        args.run(args)
    except (AssetError, OSError, tarfile.TarError, json.JSONDecodeError) as error:
        raise SystemExit(f"error: {error}") from error


if __name__ == "__main__":
    main()
