#!/usr/bin/env python3
"""Fail-closed atomic directory helpers for release publication."""

from __future__ import annotations

import argparse
import ctypes
import errno
import hashlib
import os
from pathlib import Path
import stat
import sys


RENAME_NOREPLACE = 1


class BundleError(ValueError):
    pass


class DestinationExists(BundleError):
    pass


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


def _require_bound_directory_identity(
    descriptor: int, path: Path, label: str
) -> tuple[int, int]:
    opened = os.fstat(descriptor)
    path_binding = _require_directory(path, label)
    if (opened.st_dev, opened.st_ino) != path_binding[:2]:
        raise BundleError(f"{label} no longer identifies the bound directory: {path}")
    return path_binding[:2]


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
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    try:
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != before[:2] or not stat.S_ISREG(
            opened.st_mode
        ):
            raise BundleError(f"release leaf changed while opened: {name}")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            while chunk := source.read(1024 * 1024):
                digest.update(chunk)
                size += len(chunk)
            if durable:
                os.fsync(source.fileno())
    finally:
        os.close(descriptor)
    if _binding(path) != before:
        raise BundleError(f"release leaf changed while verified: {name}")
    return {
        "name": name,
        "sha256": digest.hexdigest(),
        "size": size,
    }


def require_unsealed(candidate: Path) -> None:
    _require_directory(candidate, "release stage")
    markers = [name for name in _names(candidate) if name.endswith(".release-complete.json")]
    if markers:
        raise BundleError(f"sealed release bundle cannot be modified: {markers}")


def _durable_tree(root: Path, label: str) -> tuple[int, int]:
    root_binding = _require_directory(root, label)
    directories: list[Path] = []
    for current, child_directories, files in os.walk(root, followlinks=False):
        current_path = Path(current)
        current_binding = _require_directory(current_path, label)
        directories.append(current_path)
        for name in sorted(child_directories):
            child = current_path / name
            if not stat.S_ISDIR(_binding(child)[2]):
                raise BundleError(f"{label} contains a non-directory entry: {child}")
        for name in sorted(files):
            path = current_path / name
            before = _binding(path)
            if not stat.S_ISREG(before[2]):
                raise BundleError(f"{label} contains a non-regular file: {path}")
            descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            try:
                opened = os.fstat(descriptor)
                if (opened.st_dev, opened.st_ino) != before[:2] or not stat.S_ISREG(
                    opened.st_mode
                ):
                    raise BundleError(f"{label} changed while opened: {path}")
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
            if _binding(path) != before:
                raise BundleError(f"{label} changed while made durable: {path}")
        if _binding(current_path) != current_binding:
            raise BundleError(f"{label} changed while inspected: {current_path}")
    for directory in reversed(directories):
        _fsync_directory(directory)
    if _binding(root) != root_binding:
        raise BundleError(f"{label} changed while made durable")
    return root_binding[:2]


def _valid_leaf_name(name: str, label: str) -> str:
    if (
        name in {"", ".", ".."}
        or os.sep in name
        or (os.altsep is not None and os.altsep in name)
    ):
        raise BundleError(f"{label} has an invalid leaf name: {name!r}")
    return name


def _open_bound_directory(path: Path, label: str, *, create: bool = False) -> int:
    path = _absolute(path)
    descriptor = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        for component in path.parts[1:]:
            component = _valid_leaf_name(component, label)
            try:
                child = os.open(
                    component,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                    dir_fd=descriptor,
                )
            except FileNotFoundError:
                if not create:
                    raise BundleError(f"{label} does not exist: {path}")
                try:
                    os.mkdir(component, mode=0o700, dir_fd=descriptor)
                except FileExistsError:
                    pass
                child = os.open(
                    component,
                    os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW,
                    dir_fd=descriptor,
                )
            except OSError as error:
                if error.errno in {errno.ELOOP, errno.ENOTDIR}:
                    raise BundleError(
                        f"{label} contains a symlink or non-directory component: {path}"
                    ) from error
                raise
            os.close(descriptor)
            descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _entry_binding(parent_descriptor: int, leaf: str) -> os.stat_result | None:
    try:
        return os.stat(leaf, dir_fd=parent_descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return None


def _require_bound_child(
    parent_descriptor: int,
    parent_path: Path,
    path: Path,
    expected: tuple[int, int],
    label: str,
) -> tuple[int, int]:
    if path.parent != parent_path:
        raise BundleError(f"{label} is not inside its bound parent: {path}")
    _require_bound_directory_identity(parent_descriptor, parent_path, f"{label} parent")
    bound = _entry_binding(
        parent_descriptor, _valid_leaf_name(path.name, label)
    )
    path_binding = _require_directory(path, label)
    if (
        bound is None
        or not stat.S_ISDIR(bound.st_mode)
        or (bound.st_dev, bound.st_ino) != expected
        or path_binding[:2] != expected
    ):
        raise BundleError(f"{label} no longer identifies the expected directory: {path}")
    return expected


def _rename_noreplace_at(
    source_parent: int,
    source_leaf: str,
    destination_parent: int,
    destination_leaf: str,
    destination: Path,
) -> None:
    source_leaf = _valid_leaf_name(source_leaf, "release stage")
    destination_leaf = _valid_leaf_name(destination_leaf, "release destination")
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
        source_parent,
        os.fsencode(source_leaf),
        destination_parent,
        os.fsencode(destination_leaf),
        RENAME_NOREPLACE,
    )
    if result == 0:
        return
    number = ctypes.get_errno()
    if number == errno.EEXIST:
        raise DestinationExists(f"release destination already exists: {destination}")
    raise OSError(number, f"could not commit release destination: {destination}")


def _rename_bound_directory_noreplace(
    parent_descriptor: int,
    parent_path: Path,
    source: Path,
    destination: Path,
    expected: tuple[int, int],
    label: str,
) -> tuple[int, int]:
    _require_bound_child(
        parent_descriptor, parent_path, source, expected, f"{label} stage"
    )
    _rename_noreplace_at(
        parent_descriptor,
        source.name,
        parent_descriptor,
        destination.name,
        destination,
    )
    _require_bound_child(
        parent_descriptor, parent_path, destination, expected, label
    )
    os.fsync(parent_descriptor)
    return _require_bound_child(
        parent_descriptor, parent_path, destination, expected, label
    )


def _absolute(path: Path) -> Path:
    return Path(os.path.abspath(path))


def _ensure_parent(path: Path) -> None:
    descriptor = _open_bound_directory(
        path, "release destination parent", create=True
    )
    try:
        _require_bound_directory_identity(
            descriptor, path, "release destination parent"
        )
    finally:
        os.close(descriptor)


def preflight_publication_directories(
    input_directory: Path, output_directories: list[Path]
) -> None:
    source = _absolute(input_directory)
    source_descriptor = _open_bound_directory(
        source, "release publication input"
    )
    try:
        _require_bound_directory_identity(
            source_descriptor, source, "release publication input"
        )
    finally:
        os.close(source_descriptor)
    outputs = [_absolute(path) for path in output_directories]
    paths = [source, *outputs]
    if len(outputs) < 1 or len(set(paths)) != len(paths) or Path("/") in paths:
        raise BundleError("release publication directories are invalid")
    for index, path in enumerate(paths):
        for other in paths[index + 1 :]:
            if path in other.parents or other in path.parents:
                raise BundleError(
                    "release publication directories must not be nested"
                )
    for output in outputs:
        _ensure_parent(output.parent)
        parent = _open_bound_directory(
            output.parent, "release publication destination parent"
        )
        try:
            if _entry_binding(
                parent, _valid_leaf_name(output.name, "release publication output")
            ) is not None:
                raise BundleError(
                    f"release publication destination already exists: {output}"
                )
        finally:
            os.close(parent)


def commit_directory(stage: Path, output: Path) -> None:
    stage = _absolute(stage)
    output = _absolute(output)
    _require_directory(stage, "release stage")
    if stage.parent != output.parent or stage == output:
        raise BundleError("release stage must be a sibling of its final destination")
    parent = _open_bound_directory(output.parent, "release destination parent")
    try:
        stage_identity = _durable_tree(stage, "release staged tree")
        _rename_bound_directory_noreplace(
            parent,
            output.parent,
            stage,
            output,
            stage_identity,
            "release output",
        )
    finally:
        os.close(parent)


def main() -> int:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    publication = commands.add_parser("preflight-publication")
    publication.add_argument("--input-dir", type=Path, required=True)
    publication.add_argument(
        "--output-dir", type=Path, action="append", required=True
    )
    unsealed = commands.add_parser("require-unsealed")
    unsealed.add_argument("--candidate-dir", type=Path, required=True)
    require_directory = commands.add_parser("require-directory")
    require_directory.add_argument("--directory", type=Path, required=True)
    commit_dir = commands.add_parser("commit-directory")
    commit_dir.add_argument("--stage-dir", type=Path, required=True)
    commit_dir.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    try:
        if args.command == "preflight-publication":
            preflight_publication_directories(args.input_dir, args.output_dir)
        elif args.command == "require-unsealed":
            require_unsealed(args.candidate_dir)
        elif args.command == "require-directory":
            if ".." in args.directory.parts:
                raise BundleError("required directory must not contain '..'")
            directory = _absolute(args.directory)
            descriptor = _open_bound_directory(
                directory, "required directory"
            )
            try:
                _require_bound_directory_identity(
                    descriptor, directory, "required directory"
                )
            finally:
                os.close(descriptor)
        else:
            commit_directory(args.stage_dir, args.output_dir)
    except (BundleError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
