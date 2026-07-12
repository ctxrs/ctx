#!/usr/bin/env python3
"""Strict, bounded bundle-format helpers shared by production and verification."""

from __future__ import annotations

import hashlib
import json
import os
import re
import stat
from pathlib import Path, PurePosixPath
from typing import Any, BinaryIO

SCHEMA_VERSION = 1
MODEL_ID = "intfloat/multilingual-e5-small"
MODEL_REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"
EMBEDDING_SPACE_ID = "e5-small-v1:mean-pool:l2:query-passage"
BUNDLE_ID = "ctx.multilingual-e5-small.coreml.fp16"
MANIFEST_NAME = "manifest.json"
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_FILES = 4096
MAX_DIRECTORIES = 1024
MAX_FILE_BYTES = 1024 * 1024 * 1024
MAX_BUNDLE_BYTES = 2 * 1024 * 1024 * 1024
MAX_TOKENIZER_BYTES = 64 * 1024 * 1024
MAX_METADATA_FILE_BYTES = 4 * 1024 * 1024
MAX_PATH_BYTES = 512
SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
REVISION_RE = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
SEMVER_RE = re.compile(
    r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
    r"(?:[-+][0-9A-Za-z.-]+)?\Z"
)


class BundleError(ValueError):
    pass


def canonical_json(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode()


def sha256_file(path: Path, expected_size: int | None = None) -> str:
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    digest = hashlib.sha256()
    count = 0
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise BundleError(f"not a regular file: {path}")
        if before.st_size > MAX_FILE_BYTES:
            raise BundleError(f"file exceeds size limit: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            count = _hash_stream(stream, digest)
        after = path.lstat()
        if stat.S_ISLNK(after.st_mode) or (before.st_dev, before.st_ino) != (
            after.st_dev,
            after.st_ino,
        ):
            raise BundleError(f"file changed during verification: {path}")
    finally:
        os.close(descriptor)
    if expected_size is not None and count != expected_size:
        raise BundleError(f"file size mismatch: {path}")
    return digest.hexdigest()


def read_bounded_regular_file(path: Path, maximum: int) -> bytes:
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags)
    try:
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode) or before.st_size > maximum:
            raise BundleError(f"file is not regular or exceeds size limit: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as stream:
            body = stream.read(maximum + 1)
        after = path.lstat()
        if (
            len(body) > maximum
            or stat.S_ISLNK(after.st_mode)
            or (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino)
        ):
            raise BundleError(f"file changed or exceeds size limit: {path}")
        return body
    finally:
        os.close(descriptor)


def _hash_stream(stream: BinaryIO, digest: Any) -> int:
    count = 0
    while block := stream.read(1024 * 1024):
        count += len(block)
        if count > MAX_FILE_BYTES:
            raise BundleError("file exceeds size limit while hashing")
        digest.update(block)
    return count


def validate_relative_path(value: str) -> None:
    if (
        not value
        or len(value.encode()) > MAX_PATH_BYTES
        or "\\" in value
        or ":" in value
        or value.startswith("/")
        or value.endswith("/")
    ):
        raise BundleError(f"invalid relative path: {value!r}")
    parts = PurePosixPath(value).parts
    if len(parts) > 64 or any(part in {"", ".", ".."} for part in value.split("/")):
        raise BundleError(f"invalid relative path: {value!r}")


def collect_payload_files(root: Path) -> dict[str, Path]:
    root_metadata = root.lstat()
    if stat.S_ISLNK(root_metadata.st_mode) or not stat.S_ISDIR(root_metadata.st_mode):
        raise BundleError(f"bundle root is not a real directory: {root}")
    files: dict[str, Path] = {}
    directories = 0
    for directory, names, filenames in os.walk(
        root, topdown=True, onerror=_raise_walk_error, followlinks=False
    ):
        directories += 1
        if directories > MAX_DIRECTORIES:
            raise BundleError("bundle contains too many directories")
        names.sort()
        filenames.sort()
        current = Path(directory)
        for name in names:
            path = current / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                raise BundleError(f"unsupported bundle directory: {path}")
        if current != root and not names and not filenames:
            raise BundleError(f"empty bundle directory: {current}")
        for name in filenames:
            path = current / name
            metadata = path.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
                raise BundleError(f"unsupported bundle file: {path}")
            relative = path.relative_to(root).as_posix()
            validate_relative_path(relative)
            if relative != MANIFEST_NAME:
                files[relative] = path
                if len(files) > MAX_FILES:
                    raise BundleError("bundle contains too many files")
    return files


def file_records(root: Path) -> list[dict[str, Any]]:
    total = 0
    records = []
    for relative, path in sorted(collect_payload_files(root).items()):
        size = path.stat().st_size
        total += size
        if size > MAX_FILE_BYTES or total > MAX_BUNDLE_BYTES:
            raise BundleError("bundle exceeds size limits")
        records.append(
            {"path": relative, "sha256": sha256_file(path, size), "size_bytes": size}
        )
    return records


def verify_bundle(root: Path) -> dict[str, Any]:
    manifest_path = root / MANIFEST_NAME
    raw = read_bounded_regular_file(manifest_path, MAX_MANIFEST_BYTES)
    manifest = json.loads(raw, object_pairs_hook=_reject_duplicate_keys)
    validate_manifest(manifest)
    actual = file_records(root)
    if actual != manifest["files"]:
        raise BundleError("payload file set, size, or SHA-256 does not match manifest")
    return manifest


def validate_manifest(manifest: Any) -> None:
    _keys(
        manifest,
        {
            "schema_version",
            "bundle_id",
            "bundle_version",
            "model",
            "tensor_contract",
            "artifacts",
            "files",
        },
        "manifest",
    )
    if (
        type(manifest["schema_version"]) is not int
        or manifest["schema_version"] != SCHEMA_VERSION
        or manifest["bundle_id"] != BUNDLE_ID
    ):
        raise BundleError("unsupported schema or bundle id")
    if not isinstance(manifest["bundle_version"], str) or not SEMVER_RE.fullmatch(
        manifest["bundle_version"]
    ):
        raise BundleError("bundle_version is not strict semantic version syntax")
    _validate_model(manifest["model"])
    artifacts = manifest["artifacts"]
    required_artifact_keys = {"tokenizer", "document_model"}
    allowed_artifact_keys = required_artifact_keys | {"query_model"}
    _keys(artifacts, allowed_artifact_keys, "artifacts", required_artifact_keys)
    if artifacts["tokenizer"] != "tokenizer.json" or artifacts["document_model"] != "document.mlpackage":
        raise BundleError("invalid required artifact paths")
    if "query_model" in artifacts and artifacts["query_model"] != "query.mlpackage":
        raise BundleError("invalid query artifact path")
    has_query = "query_model" in artifacts
    _validate_contract(manifest["tensor_contract"], has_query)
    _validate_records(manifest["files"], has_query)


def _validate_model(model: Any) -> None:
    _keys(model, {"id", "source_revision", "embedding_space_id", "precision"}, "model")
    if model["id"] != MODEL_ID or model["precision"] != "fp16":
        raise BundleError("unsupported model or precision")
    if not isinstance(model["source_revision"], str) or not REVISION_RE.fullmatch(
        model["source_revision"]
    ):
        raise BundleError("invalid source revision")
    if model["embedding_space_id"] != EMBEDDING_SPACE_ID:
        raise BundleError("unsupported embedding space")


def _validate_contract(contract: Any, has_query: bool) -> None:
    fields = {
        "inputs",
        "output",
        "document_batch_size",
        "max_sequence_length",
        "embedding_dimensions",
        "document_prefix",
        "query_prefix",
        "pooling",
        "normalization",
    }
    if has_query:
        fields.add("query_batch_size")
    _keys(
        contract,
        fields,
        "tensor_contract",
    )
    document_batch = contract["document_batch_size"]
    sequence = contract["max_sequence_length"]
    if (
        type(document_batch) is not int
        or type(sequence) is not int
        or document_batch != 16
        or sequence != 512
    ):
        raise BundleError("document contract must use fixed batch 16 and sequence length 512")
    if has_query and (
        type(contract["query_batch_size"]) is not int
        or contract["query_batch_size"] != 1
    ):
        raise BundleError("query contract must use fixed batch 1")
    expected_inputs = (
        ("input_ids", "int32"),
        ("attention_mask", "int32"),
        ("token_type_ids", "int32"),
    )
    inputs = contract["inputs"]
    if not isinstance(inputs, list) or len(inputs) != len(expected_inputs):
        raise BundleError("invalid input tensor count")
    for value, (name, dtype) in zip(inputs, expected_inputs, strict=True):
        _validate_tensor(value, name, dtype, document_batch, sequence)
    _validate_tensor(
        contract["output"], "sentence_embeddings", "float32", document_batch, 384
    )
    expected = {
        "embedding_dimensions": 384,
        "document_prefix": "passage: ",
        "query_prefix": "query: ",
        "pooling": "attention_mask_mean",
        "normalization": "l2",
    }
    if any(contract[key] != value for key, value in expected.items()):
        raise BundleError("incompatible embedding contract")


def _validate_tensor(value: Any, name: str, dtype: str, batch: int, width: int) -> None:
    _keys(value, {"name", "dtype", "shape"}, f"tensor {name}")
    if value != {"name": name, "dtype": dtype, "shape": [batch, width]}:
        raise BundleError(f"incompatible tensor {name}")


def _validate_records(records: Any, has_query: bool) -> None:
    if not isinstance(records, list) or not 1 <= len(records) <= MAX_FILES:
        raise BundleError("invalid file record count")
    paths: set[str] = set()
    total = 0
    for record in records:
        _keys(record, {"path", "size_bytes", "sha256"}, "file record")
        path = record["path"]
        if not isinstance(path, str):
            raise BundleError("file path must be a string")
        validate_relative_path(path)
        allowed = path in {"tokenizer.json", "PROVENANCE.json", "THIRD_PARTY_NOTICES.md"}
        allowed |= path.startswith("LICENSES/") or path.startswith("document.mlpackage/")
        allowed |= has_query and path.startswith("query.mlpackage/")
        if not allowed or path in paths:
            raise BundleError(f"duplicate or unsupported file path: {path}")
        paths.add(path)
        size = record["size_bytes"]
        if not isinstance(size, int) or isinstance(size, bool) or not 0 <= size <= MAX_FILE_BYTES:
            raise BundleError(f"invalid file size: {path}")
        total += size
        if total > MAX_BUNDLE_BYTES:
            raise BundleError("bundle exceeds total size limit")
        if size > _payload_size_limit(path):
            raise BundleError(f"file exceeds role-specific size limit: {path}")
        if not isinstance(record["sha256"], str) or not SHA256_RE.fullmatch(record["sha256"]):
            raise BundleError(f"invalid SHA-256: {path}")
    required = {"tokenizer.json", "PROVENANCE.json", "THIRD_PARTY_NOTICES.md"}
    if not required <= paths or not any(path.startswith("LICENSES/") for path in paths):
        raise BundleError("bundle is missing metadata or license payloads")
    if not any(path.startswith("document.mlpackage/") for path in paths):
        raise BundleError("bundle is missing document model files")
    if has_query != any(path.startswith("query.mlpackage/") for path in paths):
        raise BundleError("query model declaration and files disagree")


def _payload_size_limit(path: str) -> int:
    if path == "tokenizer.json":
        return MAX_TOKENIZER_BYTES
    if path in {"PROVENANCE.json", "THIRD_PARTY_NOTICES.md"} or path.startswith(
        "LICENSES/"
    ):
        return MAX_METADATA_FILE_BYTES
    return MAX_FILE_BYTES


def _keys(
    value: Any,
    allowed: set[str],
    name: str,
    required: set[str] | None = None,
) -> None:
    if not isinstance(value, dict):
        raise BundleError(f"{name} must be an object")
    required = allowed if required is None else required
    if set(value) - allowed or not required <= set(value):
        raise BundleError(f"{name} has missing or unknown fields")


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise BundleError(f"duplicate JSON field: {key}")
        value[key] = item
    return value


def _raise_walk_error(error: OSError) -> None:
    raise error
