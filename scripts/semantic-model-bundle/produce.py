#!/usr/bin/env python3
"""Create a deterministic, self-describing Core ML model bundle and archive."""

from __future__ import annotations

import argparse
import importlib.metadata
import json
import os
import platform
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

from bundle_format import (
    BUNDLE_ID,
    EMBEDDING_SPACE_ID,
    MANIFEST_NAME,
    MODEL_ID,
    MODEL_REVISION,
    SCHEMA_VERSION,
    BundleError,
    canonical_json,
    file_records,
    verify_bundle,
)

SCRIPT_ROOT = Path(__file__).resolve().parent
TOOLCHAIN_LOCK = SCRIPT_ROOT / "toolchain.lock.json"
NOTICES = SCRIPT_ROOT / "THIRD_PARTY_NOTICES.md"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tokenizer", type=Path, required=True)
    parser.add_argument("--document-model", type=Path, required=True)
    parser.add_argument("--query-model", type=Path)
    parser.add_argument("--model-license", type=Path, required=True)
    parser.add_argument("--bundle-version", required=True)
    parser.add_argument("--document-batch-size", type=int, required=True)
    parser.add_argument("--query-batch-size", type=int)
    parser.add_argument("--sequence-length", type=int, default=512)
    parser.add_argument("--source-revision", default=MODEL_REVISION)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--archive", type=Path, required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    if args.source_revision != MODEL_REVISION:
        raise SystemExit(f"source revision must equal pinned revision {MODEL_REVISION}")
    _validate_role_batches(args)
    if args.output_dir.exists() or args.output_dir.is_symlink():
        raise SystemExit(f"output directory already exists: {args.output_dir}")
    _validate_input_file(args.tokenizer, "tokenizer")
    _validate_input_file(args.model_license, "model license")
    _validate_mlpackage(args.document_model, "document model")
    if args.query_model is not None:
        _validate_mlpackage(args.query_model, "query model")
    lock = _load_and_check_toolchain()
    _validate_tokenizer(args.tokenizer)
    _validate_coreml_contract(
        args.document_model,
        args.document_batch_size,
        args.sequence_length,
        "document model",
    )
    if args.query_model is not None:
        _validate_coreml_contract(
            args.query_model,
            args.query_batch_size,
            args.sequence_length,
            "query model",
        )

    args.output_dir.parent.mkdir(parents=True, exist_ok=True)
    staging = Path(
        tempfile.mkdtemp(prefix=f".{args.output_dir.name}.bundle.", dir=args.output_dir.parent)
    )
    try:
        shutil.copyfile(args.tokenizer, staging / "tokenizer.json", follow_symlinks=False)
        _copy_tree(args.document_model, staging / "document.mlpackage")
        artifacts = {
            "document_model": "document.mlpackage",
            "tokenizer": "tokenizer.json",
        }
        if args.query_model is not None:
            _copy_tree(args.query_model, staging / "query.mlpackage")
            artifacts["query_model"] = "query.mlpackage"
        licenses = staging / "LICENSES"
        licenses.mkdir(mode=0o755)
        shutil.copyfile(
            args.model_license, licenses / "MODEL_LICENSE.txt", follow_symlinks=False
        )
        shutil.copyfile(NOTICES, staging / "THIRD_PARTY_NOTICES.md", follow_symlinks=False)
        provenance = {
            "bundle_id": BUNDLE_ID,
            "bundle_version": args.bundle_version,
            "conversion": {
                "compute_precision": "fp16",
                "document_batch_size": args.document_batch_size,
                "minimum_deployment_target": "macOS13",
                "sequence_length": args.sequence_length,
            },
            "model": {"id": MODEL_ID, "source_revision": MODEL_REVISION},
            "reproducibility": {
                "network_required_during_packaging": False,
                "private_evaluation_data_used": False,
                "source_date_epoch": 0,
                "toolchain_lock": lock,
            },
        }
        if args.query_batch_size is not None:
            provenance["conversion"]["query_batch_size"] = args.query_batch_size
        (staging / "PROVENANCE.json").write_bytes(canonical_json(provenance))
        manifest = _manifest(args, artifacts, staging)
        (staging / MANIFEST_NAME).write_bytes(canonical_json(manifest))
        verify_bundle(staging)
        os.rename(staging, args.output_dir)
    except BaseException:
        shutil.rmtree(staging, ignore_errors=True)
        raise

    try:
        _write_archive(args.output_dir, args.archive, args.bundle_version)
    except BaseException:
        shutil.rmtree(args.output_dir, ignore_errors=True)
        raise
    print(f"bundle={args.output_dir}")
    print(f"archive={args.archive}")


def _validate_role_batches(args: argparse.Namespace) -> None:
    if args.document_batch_size != 16 or args.sequence_length != 512:
        raise SystemExit("document batch size must be 16 and sequence length must be 512")
    if args.query_model is None and args.query_batch_size is not None:
        raise SystemExit("--query-batch-size requires --query-model")
    if args.query_model is not None and args.query_batch_size is None:
        raise SystemExit("--query-model requires --query-batch-size")
    if args.query_batch_size is not None and args.query_batch_size != 1:
        raise SystemExit("query batch size must be 1")


def _manifest(args: argparse.Namespace, artifacts: dict[str, str], root: Path) -> dict:
    sequence = args.sequence_length
    inputs = [
        {
            "dtype": "int32",
            "name": name,
            "shape": [args.document_batch_size, sequence],
        }
        for name in ("input_ids", "attention_mask", "token_type_ids")
    ]
    tensor_contract = {
        "document_batch_size": args.document_batch_size,
        "document_prefix": "passage: ",
        "embedding_dimensions": 384,
        "inputs": inputs,
        "max_sequence_length": sequence,
        "normalization": "l2",
        "output": {
            "dtype": "float32",
            "name": "sentence_embeddings",
            "shape": [args.document_batch_size, 384],
        },
        "pooling": "attention_mask_mean",
        "query_prefix": "query: ",
    }
    if args.query_batch_size is not None:
        tensor_contract["query_batch_size"] = args.query_batch_size
    return {
        "artifacts": artifacts,
        "bundle_id": BUNDLE_ID,
        "bundle_version": args.bundle_version,
        "files": file_records(root),
        "model": {
            "embedding_space_id": EMBEDDING_SPACE_ID,
            "id": MODEL_ID,
            "precision": "fp16",
            "source_revision": MODEL_REVISION,
        },
        "schema_version": SCHEMA_VERSION,
        "tensor_contract": tensor_contract,
    }


def _load_and_check_toolchain() -> dict:
    lock = json.loads(TOOLCHAIN_LOCK.read_text(encoding="utf-8"))
    if platform.system() != "Darwin" or platform.mac_ver()[0] != lock["macos"]:
        raise BundleError("host macOS version does not match toolchain lock")
    xcode = subprocess.run(
        ["xcodebuild", "-version"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    if not xcode or xcode[0] != f"Xcode {lock['xcode']}":
        raise BundleError("Xcode version does not match toolchain lock")
    expected_python = lock["python"]
    actual_python = ".".join(map(str, sys.version_info[:3]))
    if actual_python != expected_python:
        raise BundleError(f"Python {actual_python} does not match lock {expected_python}")
    for distribution, expected in lock["python_distributions"].items():
        actual = importlib.metadata.version(distribution)
        if actual != expected:
            raise BundleError(f"{distribution} {actual} does not match lock {expected}")
    return lock


def _validate_tokenizer(path: Path) -> None:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise BundleError("tokenizer.json must contain a JSON object")


def _validate_coreml_contract(path: Path, batch: int, sequence: int, name: str) -> None:
    import coremltools as ct
    from coremltools.proto.FeatureTypes_pb2 import ArrayFeatureType

    spec = ct.models.MLModel(str(path), skip_model_load=True).get_spec()
    expected_inputs = [
        ("input_ids", ArrayFeatureType.INT32, [batch, sequence]),
        ("attention_mask", ArrayFeatureType.INT32, [batch, sequence]),
        ("token_type_ids", ArrayFeatureType.INT32, [batch, sequence]),
    ]
    actual_inputs = list(spec.description.input)
    if len(actual_inputs) != len(expected_inputs):
        raise BundleError(f"{name} has an incompatible input count")
    for feature, (expected_name, expected_type, expected_shape) in zip(
        actual_inputs, expected_inputs, strict=True
    ):
        if feature.type.WhichOneof("Type") != "multiArrayType":
            raise BundleError(f"{name} input {expected_name} is not a multi-array")
        array = feature.type.multiArrayType
        if (
            feature.name != expected_name
            or array.dataType != expected_type
            or list(array.shape) != expected_shape
        ):
            raise BundleError(f"{name} input {expected_name} has an incompatible contract")
    outputs = list(spec.description.output)
    if len(outputs) != 1 or outputs[0].type.WhichOneof("Type") != "multiArrayType":
        raise BundleError(f"{name} must have one multi-array output")
    output = outputs[0]
    array = output.type.multiArrayType
    if (
        output.name != "sentence_embeddings"
        or array.dataType != ArrayFeatureType.FLOAT32
        or list(array.shape) != [batch, 384]
    ):
        raise BundleError(f"{name} output has an incompatible contract")


def _validate_input_file(path: Path, name: str) -> None:
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise BundleError(f"{name} must be a regular non-symlink file: {path}")


def _validate_mlpackage(path: Path, name: str) -> None:
    metadata = path.lstat()
    if (
        stat.S_ISLNK(metadata.st_mode)
        or not stat.S_ISDIR(metadata.st_mode)
        or path.suffix != ".mlpackage"
    ):
        raise BundleError(f"{name} must be a real .mlpackage directory: {path}")
    found_file = False
    for directory, names, filenames in os.walk(
        path, onerror=_raise_walk_error, followlinks=False
    ):
        names.sort()
        filenames.sort()
        children = [*(Path(directory) / value for value in names)]
        children.extend(Path(directory) / value for value in filenames)
        for child in children:
            metadata = child.lstat()
            if stat.S_ISLNK(metadata.st_mode) or not (
                stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode)
            ):
                raise BundleError(f"unsupported mlpackage entry: {child}")
            found_file |= stat.S_ISREG(metadata.st_mode)
    if not found_file:
        raise BundleError(f"{name} contains no files")


def _copy_tree(source: Path, destination: Path) -> None:
    shutil.copytree(
        source,
        destination,
        symlinks=False,
        copy_function=lambda src, dst: shutil.copyfile(src, dst, follow_symlinks=False),
    )


def _raise_walk_error(error: OSError) -> None:
    raise error


def _write_archive(bundle: Path, archive: Path, version: str) -> None:
    archive.parent.mkdir(parents=True, exist_ok=True)
    temporary = archive.with_name(f".{archive.name}.{os.getpid()}.tmp")
    if temporary.exists() or temporary.is_symlink():
        raise BundleError(f"archive temporary path already exists: {temporary}")
    root_name = f"ctx-multilingual-e5-small-coreml-fp16-{version}"
    try:
        with tarfile.open(temporary, "w:xz", format=tarfile.PAX_FORMAT, preset=9) as output:
            _add_tar_entry(output, bundle, root_name)
            paths = sorted(
                bundle.rglob("*"), key=lambda item: item.relative_to(bundle).as_posix()
            )
            for path in paths:
                archive_name = f"{root_name}/{path.relative_to(bundle).as_posix()}"
                _add_tar_entry(output, path, archive_name)
        with temporary.open("rb") as stream:
            os.fsync(stream.fileno())
        os.replace(temporary, archive)
    finally:
        temporary.unlink(missing_ok=True)


def _add_tar_entry(output: tarfile.TarFile, source: Path, archive_name: str) -> None:
    metadata = source.lstat()
    if stat.S_ISLNK(metadata.st_mode):
        raise BundleError(f"refusing to archive symlink: {source}")
    info = tarfile.TarInfo(archive_name)
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = 0
    if source.is_dir():
        info.type = tarfile.DIRTYPE
        info.mode = 0o755
        info.size = 0
        output.addfile(info)
    elif source.is_file():
        info.type = tarfile.REGTYPE
        info.mode = 0o644
        info.size = metadata.st_size
        with source.open("rb") as stream:
            output.addfile(info, stream)
    else:
        raise BundleError(f"refusing to archive unsupported entry: {source}")


if __name__ == "__main__":
    main()
