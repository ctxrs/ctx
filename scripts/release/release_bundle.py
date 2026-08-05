#!/usr/bin/env python3
"""Seal, verify, and atomically commit a native Linux release bundle."""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import json
import os
from pathlib import Path
import secrets
import shlex
import shutil
import stat
import sys
from typing import Callable


COMPLETION_KIND = "ctx-public-linux-release-completion"
COMPLETION_SCHEMA_VERSION = 1
MAX_COMPLETION_BYTES = 1024 * 1024
RENAME_NOREPLACE = 1


class BundleError(ValueError):
    pass


def completion_leaf(platform: str) -> str:
    if platform not in {"linux-x64", "linux-aarch64"}:
        raise BundleError(f"unsupported Linux release platform: {platform}")
    return f"ctx-{platform}.release-complete.json"


def expected_release_leaves(platform: str) -> list[str]:
    binaries = {"linux-x64": "ctx", "linux-aarch64": "ctx-linux-aarch64"}
    try:
        binary = binaries[platform]
    except KeyError as error:
        raise BundleError(f"unsupported Linux release platform: {platform}") from error
    runtime = f"ctx-onnxruntime-{platform}"
    return sorted(
        [
            binary,
            f"{binary}.build-info.json",
            f"{binary}.candidate.json",
            f"{binary}.cdx.json",
            f"{binary}.cdx.json.sha256",
            f"{binary}.dependency-advisory.json",
            f"{binary}.sha256",
            f"{binary}.size.json",
            f"{binary}.third-party-notices.txt",
            f"{binary}.third-party-notices.txt.sha256",
            f"{binary}.version",
            f"{runtime}.tar.gz",
            f"{runtime}.tar.gz.sha256",
            f"{runtime}.tar.zst",
            f"{runtime}.tar.zst.asset.json",
            f"{runtime}.tar.zst.sha256",
        ]
    )


def _valid_commit(value: str) -> bool:
    return (
        len(value) == 40
        and value != "0" * 40
        and all(character in "0123456789abcdef" for character in value)
    )


def _binding(path: Path) -> tuple[int, int, int, int, int, int]:
    value = path.lstat()
    return (
        value.st_dev,
        value.st_ino,
        value.st_mode,
        value.st_size,
        value.st_mtime_ns,
        value.st_ctime_ns,
    )


def _require_directory(path: Path, label: str) -> tuple[int, int, int, int, int, int]:
    try:
        binding = _binding(path)
    except FileNotFoundError as error:
        raise BundleError(f"{label} does not exist: {path}") from error
    if not stat.S_ISDIR(binding[2]):
        raise BundleError(f"{label} is not a directory: {path}")
    return binding


def _names(path: Path) -> list[str]:
    return sorted(entry.name for entry in path.iterdir())


def _fsync_directory(path: Path) -> None:
    descriptor = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def _file_record(
    path: Path, name: str, *, durable: bool = False
) -> dict[str, object]:
    before = _binding(path)
    if not stat.S_ISREG(before[2]):
        raise BundleError(f"release leaf is not a regular file: {name}")
    digest = hashlib.sha256()
    size = 0
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
            size += len(chunk)
        if durable:
            os.fsync(source.fileno())
    if _binding(path) != before:
        raise BundleError(f"release leaf changed while verified: {name}")
    return {
        "mode": f"{stat.S_IMODE(before[2]):04o}",
        "name": name,
        "sha256": digest.hexdigest(),
        "size": size,
    }


def seal_bundle(candidate: Path, platform: str, source_commit: str) -> str:
    if not _valid_commit(source_commit):
        raise BundleError("release source commit is invalid")
    root_binding = _require_directory(candidate, "release stage")
    expected = expected_release_leaves(platform)
    actual = _names(candidate)
    if actual != expected:
        raise BundleError(
            f"Linux release stage is incomplete; expected {expected}, got {actual}"
        )
    records = [
        _file_record(candidate / name, name, durable=True) for name in expected
    ]
    if _binding(candidate) != root_binding or _names(candidate) != expected:
        raise BundleError("release stage changed while sealed")
    payload = {
        "files": records,
        "kind": COMPLETION_KIND,
        "platform": platform,
        "schema_version": COMPLETION_SCHEMA_VERSION,
        "source_commit": source_commit,
    }
    marker = candidate / completion_leaf(platform)
    encoded = (json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n").encode()
    descriptor = os.open(
        marker,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW,
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb", closefd=False) as destination:
            destination.write(encoded)
            destination.flush()
            os.fsync(destination.fileno())
    finally:
        os.close(descriptor)
    _fsync_directory(candidate)
    return hashlib.sha256(encoded).hexdigest()


def verify_bundle(
    candidate: Path,
    platform: str,
    source_commit: str,
    *,
    allow_extra: bool = False,
    seal_sha256: str | None = None,
) -> dict[str, object]:
    if not _valid_commit(source_commit):
        raise BundleError("release source commit is invalid")
    root_binding = _require_directory(candidate, "release bundle")
    marker_name = completion_leaf(platform)
    marker_path = candidate / marker_name
    try:
        marker_binding = _binding(marker_path)
        if not stat.S_ISREG(marker_binding[2]) or marker_binding[3] > MAX_COMPLETION_BYTES:
            raise BundleError(f"release completion marker is invalid: {marker_name}")
        marker_bytes = marker_path.read_bytes()
        payload = json.loads(marker_bytes)
    except (FileNotFoundError, json.JSONDecodeError, UnicodeDecodeError) as error:
        raise BundleError(f"release completion marker is invalid: {marker_name}") from error
    if _binding(marker_path) != marker_binding:
        raise BundleError(f"release completion marker changed: {marker_name}")
    if seal_sha256 is not None and hashlib.sha256(marker_bytes).hexdigest() != seal_sha256:
        raise BundleError("release seal does not match the smoked candidate")
    identity = {
        "kind": COMPLETION_KIND,
        "platform": platform,
        "schema_version": COMPLETION_SCHEMA_VERSION,
        "source_commit": source_commit,
    }
    if (
        not isinstance(payload, dict)
        or set(payload) != {*identity, "files"}
        or {key: payload.get(key) for key in identity} != identity
    ):
        raise BundleError(f"release completion identity is invalid: {marker_name}")
    expected_names = expected_release_leaves(platform)
    records = payload.get("files")
    if not isinstance(records, list) or len(records) != len(expected_names):
        raise BundleError(f"release completion file manifest is invalid: {marker_name}")
    expected_records: dict[str, dict[str, object]] = {}
    for name, record in zip(expected_names, records, strict=True):
        if (
            not isinstance(record, dict)
            or set(record) != {"mode", "name", "sha256", "size"}
            or record.get("name") != name
        ):
            raise BundleError(f"release completion file manifest is invalid: {marker_name}")
        expected_records[name] = record

    actual_names = _names(candidate)
    required_names = sorted([marker_name, *expected_names])
    missing = sorted(set(required_names) - set(actual_names))
    if missing:
        raise BundleError(f"release bundle is missing declared leaves: {missing}")
    unexpected_markers = [
        name
        for name in actual_names
        if name.endswith(".release-complete.json") and name != marker_name
    ]
    if unexpected_markers and not allow_extra:
        raise BundleError(f"release bundle has unexpected completion markers: {unexpected_markers}")
    if not allow_extra and actual_names != required_names:
        raise BundleError(
            f"release bundle has unexpected leaves; expected {required_names}, got {actual_names}"
        )
    for name in actual_names:
        mode = _binding(candidate / name)[2]
        if not stat.S_ISREG(mode):
            raise BundleError(f"release bundle contains a non-regular leaf: {name}")
    for name in expected_names:
        if _file_record(candidate / name, name) != expected_records[name]:
            raise BundleError(f"release leaf does not match completion marker: {name}")
    if (
        _binding(candidate) != root_binding
        or _binding(marker_path) != marker_binding
        or _names(candidate) != actual_names
    ):
        raise BundleError("release bundle changed while verified")
    return payload


def require_unsealed(candidate: Path) -> None:
    _require_directory(candidate, "release stage")
    markers = [name for name in _names(candidate) if name.endswith(".release-complete.json")]
    if markers:
        raise BundleError(f"sealed release bundle cannot be modified: {markers}")


def _tree_records(root: Path) -> dict[str, tuple[str, int, str, int]]:
    root_binding = _require_directory(root, "private symbol bundle")
    records: dict[str, tuple[str, int, str, int]] = {}
    for current, directories, files in os.walk(root, followlinks=False):
        current_path = Path(current)
        for name in sorted(directories):
            path = current_path / name
            binding = _binding(path)
            if not stat.S_ISDIR(binding[2]):
                raise BundleError(f"private symbol tree contains a link: {path}")
            relative = path.relative_to(root).as_posix()
            records[relative] = ("directory", stat.S_IMODE(binding[2]), "", 0)
        for name in sorted(files):
            path = current_path / name
            relative = path.relative_to(root).as_posix()
            record = _file_record(path, relative)
            records[relative] = (
                "file",
                int(str(record["mode"]), 8),
                str(record["sha256"]),
                int(record["size"]),
            )
    if _binding(root) != root_binding:
        raise BundleError("private symbol bundle changed while verified")
    return records


def _rename_noreplace(source: Path, destination: Path) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise BundleError("Linux renameat2 is required for release publication")
    renameat2.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    renameat2.restype = ctypes.c_int
    result = renameat2(
        -100,
        os.fsencode(source),
        -100,
        os.fsencode(destination),
        RENAME_NOREPLACE,
    )
    if result == 0:
        return
    number = ctypes.get_errno()
    if number == errno.EEXIST:
        raise BundleError(f"release destination already exists: {destination}")
    raise OSError(number, f"could not commit release destination: {destination}")


def _absolute(path: Path) -> Path:
    return Path(os.path.abspath(path))


def resolve_destinations(
    repo_root: Path, output_argument: str, private_symbols_argument: str | None
) -> tuple[Path, Path]:
    output = Path(output_argument)
    if not output.is_absolute():
        output = repo_root / output
    output = _absolute(output)
    symbols = (
        Path(f"{output}.private-debug-symbols")
        if private_symbols_argument is None
        else Path(private_symbols_argument)
    )
    if not symbols.is_absolute():
        raise BundleError("--private-symbols-dir must be absolute")
    symbols = _absolute(symbols)
    if output == Path("/") or symbols == Path("/") or output == symbols:
        raise BundleError("release destinations are invalid")
    if output in symbols.parents or symbols in output.parents:
        raise BundleError("public and private release destinations must not be nested")
    return output, symbols


def _ensure_parent(path: Path) -> None:
    current = Path("/")
    for component in path.parts[1:]:
        current /= component
        try:
            mode = current.lstat().st_mode
        except FileNotFoundError:
            current.mkdir(mode=0o700)
            continue
        if not stat.S_ISDIR(mode):
            raise BundleError(f"release destination parent contains a link or file: {current}")


def preflight_destinations(
    repo_root: Path, output_argument: str, private_symbols_argument: str | None
) -> tuple[Path, Path]:
    output, symbols = resolve_destinations(
        repo_root, output_argument, private_symbols_argument
    )
    for destination in (output, symbols):
        if destination.exists() or destination.is_symlink():
            raise BundleError(f"release destination already exists: {destination}")
    _ensure_parent(output.parent)
    _ensure_parent(symbols.parent)
    return output, symbols


def commit_directory(stage: Path, output: Path) -> None:
    stage = _absolute(stage)
    output = _absolute(output)
    _require_directory(stage, "release stage")
    if stage.parent != output.parent or stage == output:
        raise BundleError("release stage must be a sibling of its final destination")
    _rename_noreplace(stage, output)
    _fsync_directory(output.parent)


def commit_bundle(
    stage: Path,
    output: Path,
    symbols_source: Path,
    symbols_output: Path,
    platform: str,
    source_commit: str,
    *,
    seal_sha256: str,
    phase_hook: Callable[[str], None] | None = None,
) -> None:
    stage = _absolute(stage)
    output = _absolute(output)
    symbols_source = _absolute(symbols_source)
    symbols_output = _absolute(symbols_output)
    if stage.parent != output.parent:
        raise BundleError("release stage must be a sibling of its final destination")
    if stage == output or output == symbols_output:
        raise BundleError("release commit paths are invalid")
    verify_bundle(stage, platform, source_commit, seal_sha256=seal_sha256)
    source_symbols = _tree_records(symbols_source)
    if phase_hook is not None:
        phase_hook("after-verification")
    verify_bundle(stage, platform, source_commit, seal_sha256=seal_sha256)
    if _tree_records(symbols_source) != source_symbols:
        raise BundleError("private symbol bundle changed after verification")
    if output.exists() or output.is_symlink():
        raise BundleError(f"release destination already exists: {output}")
    if symbols_output.exists() or symbols_output.is_symlink():
        raise BundleError(f"release destination already exists: {symbols_output}")
    _ensure_parent(symbols_output.parent)
    symbols_stage = symbols_output.parent / f".ctx-symbols.{secrets.token_hex(16)}"
    try:
        shutil.copytree(symbols_source, symbols_stage, symlinks=False)
        if (
            _tree_records(symbols_stage) != source_symbols
            or _tree_records(symbols_source) != source_symbols
        ):
            raise BundleError("private symbol bundle changed while staged")
        if phase_hook is not None:
            phase_hook("before-symbol-commit")
        _rename_noreplace(symbols_stage, symbols_output)
        _fsync_directory(symbols_output.parent)
        try:
            if phase_hook is not None:
                phase_hook("before-public-commit")
            _rename_noreplace(stage, output)
            _fsync_directory(output.parent)
        except BaseException:
            _rename_noreplace(symbols_output, symbols_stage)
            _fsync_directory(symbols_output.parent)
            raise
    finally:
        if symbols_stage.exists() and not symbols_stage.is_symlink():
            shutil.rmtree(symbols_stage)


def main() -> int:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    resolve = commands.add_parser("resolve")
    resolve.add_argument("--repo-root", type=Path, required=True)
    resolve.add_argument("--output-dir", required=True)
    resolve.add_argument("--private-symbols-dir")
    preflight = commands.add_parser("preflight")
    preflight.add_argument("--repo-root", type=Path, required=True)
    preflight.add_argument("--output-dir", required=True)
    preflight.add_argument("--private-symbols-dir")
    seal = commands.add_parser("seal")
    seal.add_argument("--candidate-dir", type=Path, required=True)
    seal.add_argument("--platform", required=True)
    seal.add_argument("--source-commit", required=True)
    verify = commands.add_parser("verify")
    verify.add_argument("--candidate-dir", type=Path, required=True)
    verify.add_argument("--platform", required=True)
    verify.add_argument("--source-commit", required=True)
    verify.add_argument("--allow-extra", action="store_true")
    verify.add_argument("--seal-sha256")
    unsealed = commands.add_parser("require-unsealed")
    unsealed.add_argument("--candidate-dir", type=Path, required=True)
    commit_dir = commands.add_parser("commit-directory")
    commit_dir.add_argument("--stage-dir", type=Path, required=True)
    commit_dir.add_argument("--output-dir", type=Path, required=True)
    commit = commands.add_parser("commit")
    commit.add_argument("--stage-dir", type=Path, required=True)
    commit.add_argument("--output-dir", type=Path, required=True)
    commit.add_argument("--private-symbols-source-dir", type=Path, required=True)
    commit.add_argument("--private-symbols-dir", type=Path, required=True)
    commit.add_argument("--platform", required=True)
    commit.add_argument("--source-commit", required=True)
    commit.add_argument("--seal-sha256", required=True)
    args = parser.parse_args()
    try:
        if args.command in {"resolve", "preflight"}:
            function = resolve_destinations if args.command == "resolve" else preflight_destinations
            output, symbols = function(args.repo_root, args.output_dir, args.private_symbols_dir)
            print(f"CTX_LINUX_RELEASE_OUTPUT_DIR={shlex.quote(str(output))}")
            print(
                "CTX_LINUX_RELEASE_PRIVATE_SYMBOLS_DIR="
                f"{shlex.quote(str(symbols))}"
            )
        elif args.command == "seal":
            print(seal_bundle(args.candidate_dir, args.platform, args.source_commit))
        elif args.command == "verify":
            verify_bundle(
                args.candidate_dir,
                args.platform,
                args.source_commit,
                allow_extra=args.allow_extra,
                seal_sha256=args.seal_sha256,
            )
        elif args.command == "require-unsealed":
            require_unsealed(args.candidate_dir)
        elif args.command == "commit-directory":
            commit_directory(args.stage_dir, args.output_dir)
        else:
            commit_bundle(
                args.stage_dir,
                args.output_dir,
                args.private_symbols_source_dir,
                args.private_symbols_dir,
                args.platform,
                args.source_commit,
                seal_sha256=args.seal_sha256,
            )
    except (BundleError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
