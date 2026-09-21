"""Safe, bounded inspection for semantic runtime archives."""

from __future__ import annotations

import contextlib
import hashlib
import os
import posixpath
import stat
import subprocess
import tarfile
import tempfile
import zipfile
from pathlib import Path
from typing import BinaryIO, Iterable

CHUNK_BYTES = 1024 * 1024


class MetadataError(ValueError):
    pass


def _regular_file_identity(metadata: os.stat_result) -> tuple[int, int, int, int, int]:
    return (
        metadata.st_dev,
        metadata.st_ino,
        metadata.st_size,
        metadata.st_mtime_ns,
        metadata.st_ctime_ns,
    )


def copy_regular_file(path: Path, output: BinaryIO | None = None) -> tuple[int, str]:
    try:
        path_before = path.lstat()
    except OSError as error:
        raise MetadataError(f"could not inspect artifact {path}: {error}") from error
    if stat.S_ISLNK(path_before.st_mode) or not stat.S_ISREG(path_before.st_mode):
        raise MetadataError(f"artifact is not a regular non-symlink file: {path}")
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        raise MetadataError(f"could not open artifact {path}: {error}") from error
    digest = hashlib.sha256()
    size = 0
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or _regular_file_identity(opened) != _regular_file_identity(path_before)
        ):
            raise MetadataError(f"artifact changed before reading: {path}")
        with os.fdopen(descriptor, "rb", closefd=False) as handle:
            while chunk := handle.read(CHUNK_BYTES):
                size += len(chunk)
                digest.update(chunk)
                if output is not None:
                    written = output.write(chunk)
                    if written is not None and written != len(chunk):
                        raise MetadataError(f"short write while copying artifact: {path}")
        descriptor_after = os.fstat(descriptor)
    except OSError as error:
        action = "copy" if output is not None else "hash"
        raise MetadataError(f"could not {action} artifact {path}: {error}") from error
    finally:
        os.close(descriptor)
    try:
        path_after = path.lstat()
    except OSError as error:
        raise MetadataError(f"artifact changed while reading: {path}: {error}") from error
    before_identity = _regular_file_identity(opened)
    if (
        size != opened.st_size
        or before_identity != _regular_file_identity(descriptor_after)
        or stat.S_ISLNK(path_after.st_mode)
        or before_identity != _regular_file_identity(path_after)
    ):
        raise MetadataError(f"artifact changed while reading: {path}")
    return size, digest.hexdigest()


def hash_regular_file(path: Path) -> tuple[int, str]:
    return copy_regular_file(path)


@contextlib.contextmanager
def snapshot_regular_file(path: Path):
    with tempfile.TemporaryDirectory(prefix="ctx-semantic-archive-snapshot-") as temporary:
        snapshot = Path(temporary) / "archive"
        with snapshot.open("xb") as output:
            size, digest = copy_regular_file(path, output)
            output.flush()
            os.fsync(output.fileno())
        yield snapshot, size, digest


def canonical_member_path(
    raw: str, is_directory: bool, *, require_directory_slash: bool
) -> str:
    candidate = raw[:-1] if is_directory and raw.endswith("/") else raw
    if (
        not candidate
        or "\\" in raw
        or raw.startswith("/")
        or posixpath.normpath(candidate) != candidate
        or any(part in {"", ".", ".."} for part in candidate.split("/"))
        or (raw.endswith("/") and not is_directory)
        or (is_directory and require_directory_slash and not raw.endswith("/"))
    ):
        raise MetadataError(f"unsafe or non-canonical archive path: {raw!r}")
    return candidate


def relative_asset_path(name: str, asset: dict) -> str:
    prefix = asset["path_prefix"]
    if not prefix:
        return name
    expected = f"{prefix}/"
    if not name.startswith(expected):
        raise MetadataError(
            f"{asset['artifact']} archive path is outside required root {prefix}: {name}"
        )
    relative = name[len(expected) :]
    if not relative:
        raise MetadataError(f"{asset['artifact']} contains an empty rooted path")
    return relative


def validate_file_allowlist(records: list[dict], asset: dict) -> None:
    paths = [record["path"] for record in records]
    exact_paths = set(asset["exact_paths"])
    tree_prefixes = tuple(asset["tree_prefixes"])
    for path in paths:
        relative = relative_asset_path(path, asset)
        if relative not in exact_paths and not relative.startswith(tree_prefixes):
            raise MetadataError(f"unexpected file in {asset['artifact']}: {path}")
    relative_paths = [relative_asset_path(path, asset) for path in paths]
    missing = [path for path in asset["exact_paths"] if path not in relative_paths]
    missing_prefixes = [
        prefix
        for prefix in asset["tree_prefixes"]
        if not any(path.startswith(prefix) for path in relative_paths)
    ]
    if missing or missing_prefixes:
        details = ", ".join(missing + missing_prefixes)
        raise MetadataError(f"{asset['artifact']} is missing required archive paths: {details}")


def validate_record_limits(records: list[dict], asset: dict) -> None:
    if not records or len(records) > asset["max_files"]:
        raise MetadataError(
            f"{asset['artifact']} must contain 1..{asset['max_files']} regular files"
        )
    total = 0
    for record in records:
        if record["size"] <= 0:
            raise MetadataError(f"archive member must have positive size: {record['path']}")
        total += record["size"]
        if total > asset["max_expanded_bytes"]:
            raise MetadataError(
                f"{asset['artifact']} exceeds expanded-size limit "
                f"of {asset['max_expanded_bytes']} bytes"
            )


def digest_stream(handle: BinaryIO, expected_size: int, name: str) -> str:
    digest = hashlib.sha256()
    remaining = expected_size
    while remaining:
        chunk = handle.read(min(CHUNK_BYTES, remaining))
        if not chunk:
            raise MetadataError(f"archive member ended early: {name}")
        digest.update(chunk)
        remaining -= len(chunk)
    if handle.read(1):
        raise MetadataError(f"archive member exceeded declared size: {name}")
    return digest.hexdigest()


def validate_directories(directories: set[str], file_names: Iterable[str], artifact: str) -> None:
    files = tuple(file_names)
    for directory in directories:
        prefix = f"{directory}/"
        if not any(name.startswith(prefix) for name in files):
            raise MetadataError(f"{artifact} contains empty directory: {directory}")


def inspect_tar_file(archive: Path, asset: dict, mode: str) -> list[dict]:
    try:
        bundle = tarfile.open(archive, mode)
    except (OSError, tarfile.TarError) as error:
        raise MetadataError(f"could not open runtime archive {archive}: {error}") from error
    seen: set[str] = set()
    files: dict[str, tarfile.TarInfo] = {}
    directories: set[str] = set()
    max_entries = asset["max_files"] * 8 + 64
    with bundle:
        for index, member in enumerate(bundle, start=1):
            if index > max_entries:
                raise MetadataError(
                    f"{asset['artifact']} exceeds archive-entry limit of {max_entries}"
                )
            is_directory = member.isdir()
            name = canonical_member_path(
                member.name, is_directory, require_directory_slash=False
            )
            folded = name.casefold()
            if folded in seen:
                raise MetadataError(f"duplicate or case-colliding archive entry: {name}")
            seen.add(folded)
            if member.mode & 0o7000:
                raise MetadataError(f"unsafe permission bits on archive entry: {name}")
            if is_directory:
                directories.add(name)
            elif member.isfile():
                files[name] = member
            else:
                raise MetadataError(f"archive entry is not a regular file or directory: {name}")
        records = [{"path": name, "size": files[name].size} for name in sorted(files)]
        validate_directories(directories, files, asset["artifact"])
        validate_record_limits(records, asset)
        validate_file_allowlist(records, asset)
        for record in records:
            extracted = bundle.extractfile(files[record["path"]])
            if extracted is None:
                raise MetadataError(f"could not read archive member: {record['path']}")
            with extracted:
                record["sha256"] = digest_stream(
                    extracted, record["size"], record["path"]
                )
    return records


def inspect_tar_zst(archive: Path, asset: dict) -> list[dict]:
    max_tar_bytes = (
        asset["max_expanded_bytes"] + asset["max_files"] * 8192 + 1024 * 1024
    )
    with tempfile.TemporaryDirectory(prefix="ctx-semantic-tar-zst-") as temporary:
        expanded = Path(temporary) / "archive.tar"
        try:
            process = subprocess.Popen(
                ["zstd", "-q", "-d", "-c", "--", str(archive)],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        except OSError as error:
            raise MetadataError(f"could not execute zstd for {archive}: {error}") from error
        assert process.stdout is not None
        assert process.stderr is not None
        total = 0
        try:
            with expanded.open("wb") as output:
                while chunk := process.stdout.read(CHUNK_BYTES):
                    total += len(chunk)
                    if total > max_tar_bytes:
                        process.kill()
                        raise MetadataError(
                            f"{asset['artifact']} exceeds bounded tar.zst expansion limit"
                        )
                    output.write(chunk)
            error_text = process.stderr.read().decode("utf-8", errors="replace").strip()
            return_code = process.wait()
        finally:
            process.stdout.close()
            process.stderr.close()
            if process.poll() is None:
                process.kill()
                process.wait()
        if return_code != 0:
            raise MetadataError(
                f"could not decompress runtime archive {archive}: "
                f"{error_text or f'zstd exited {return_code}'}"
            )
        return inspect_tar_file(expanded, asset, "r:")


def inspect_tar(archive: Path, asset: dict) -> list[dict]:
    if asset["format"] == "tar.zst":
        return inspect_tar_zst(archive, asset)
    mode = "r:gz" if asset["format"] == "tar.gz" else "r:xz"
    return inspect_tar_file(archive, asset, mode)


def inspect_zip(archive: Path, asset: dict) -> list[dict]:
    try:
        bundle = zipfile.ZipFile(archive, "r")
    except (OSError, zipfile.BadZipFile) as error:
        raise MetadataError(f"could not open runtime archive {archive}: {error}") from error
    seen: set[str] = set()
    files: dict[str, zipfile.ZipInfo] = {}
    directories: set[str] = set()
    with bundle:
        members = bundle.infolist()
        max_entries = asset["max_files"] * 8 + 64
        if len(members) > max_entries:
            raise MetadataError(
                f"{asset['artifact']} exceeds archive-entry limit of {max_entries}"
            )
        for member in members:
            is_directory = member.is_dir()
            name = canonical_member_path(
                member.filename, is_directory, require_directory_slash=True
            )
            folded = name.casefold()
            if folded in seen:
                raise MetadataError(f"duplicate or case-colliding archive entry: {name}")
            seen.add(folded)
            if member.flag_bits & 0x1:
                raise MetadataError(f"encrypted archive entry: {name}")
            unix_mode = member.external_attr >> 16
            file_type = stat.S_IFMT(unix_mode)
            if unix_mode & 0o7000:
                raise MetadataError(f"unsafe permission bits on archive entry: {name}")
            if is_directory:
                if file_type not in (0, stat.S_IFDIR):
                    raise MetadataError(f"archive directory has invalid type: {name}")
                directories.add(name)
            elif file_type in (0, stat.S_IFREG):
                files[name] = member
            else:
                raise MetadataError(f"archive entry is not a regular file or directory: {name}")
        records = [{"path": name, "size": files[name].file_size} for name in sorted(files)]
        validate_directories(directories, files, asset["artifact"])
        validate_record_limits(records, asset)
        validate_file_allowlist(records, asset)
        for record in records:
            try:
                extracted = bundle.open(files[record["path"]], "r")
            except (OSError, RuntimeError, zipfile.BadZipFile) as error:
                raise MetadataError(
                    f"could not read archive member {record['path']}: {error}"
                ) from error
            with extracted:
                record["sha256"] = digest_stream(
                    extracted, record["size"], record["path"]
                )
    return records


def validate_model_publication_pin(
    model_pin: dict, assets: dict[str, dict]
) -> None:
    model_asset_id = model_pin["asset_id"]
    if model_asset_id in assets:
        model_asset = assets[model_asset_id]
        records = {record["path"]: record for record in model_asset["files"]}
        expected_paths = set(model_pin["signed_metadata_paths"]) | set(
            model_pin["runtime_files"]
        )
        if len(records) != 7 or set(records) != expected_paths:
            raise MetadataError(
                "pinned model package must contain exactly seven signed file records"
            )
        for path, expected in model_pin["runtime_files"].items():
            actual = records[path]
            for field in ("size", "sha256"):
                if actual[field] != expected[field]:
                    raise MetadataError(
                        f"immutable {model_asset_id} publication pin mismatch "
                        f"for {path} {field}: "
                        f"got {actual[field]}, expected {expected[field]}"
                    )


def validate_publication_pins(layout: dict, assets: dict[str, dict]) -> None:
    pins = layout["publication_pins"]
    validate_model_publication_pin(pins["model"], assets)
    validate_model_publication_pin(pins["accelerator_model"], assets)

    coreml_pin = pins["coreml"]
    coreml_asset_id = coreml_pin["asset_id"]
    if coreml_asset_id in assets:
        coreml_asset = assets[coreml_asset_id]
        if coreml_asset["archive_sha256"] != coreml_pin["archive_sha256"]:
            raise MetadataError(
                "immutable CoreML archive publication pin mismatch: "
                f"got {coreml_asset['archive_sha256']}, "
                f"expected {coreml_pin['archive_sha256']}"
            )
        manifest = next(
            (
                record
                for record in coreml_asset["files"]
                if record["path"] == coreml_pin["manifest_path"]
            ),
            None,
        )
        if manifest is None or manifest["sha256"] != coreml_pin["manifest_sha256"]:
            actual = "<missing>" if manifest is None else manifest["sha256"]
            raise MetadataError(
                "immutable CoreML manifest publication pin mismatch: "
                f"got {actual}, expected {coreml_pin['manifest_sha256']}"
            )
