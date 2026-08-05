#!/usr/bin/env python3
"""Publish Linux Bazel release outputs without following destination links."""

from __future__ import annotations

import argparse
import ctypes
import errno
import os
from pathlib import Path
import secrets
import shlex
import stat
import sys
from typing import Callable


DIRECTORY_FLAGS = (
    os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
)
FILE_FLAGS = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
RENAME_NOREPLACE = 1


class PublicationError(ValueError):
    pass


def release_leaf(name: str) -> str:
    if not name or name in {".", ".."} or Path(name).name != name:
        raise PublicationError(f"invalid release output leaf: {name!r}")
    return name


def resolve_destinations(
    repo_root: Path,
    output_argument: str,
    private_symbols_argument: str | None,
) -> tuple[Path, Path]:
    if not repo_root.is_absolute():
        raise PublicationError("release repository root must be absolute")
    output = Path(output_argument)
    if not output.is_absolute():
        output = repo_root / output
    output = Path(os.path.abspath(output))
    if private_symbols_argument is None:
        private_symbols = Path(f"{output}.private-debug-symbols")
    else:
        private_symbols = Path(private_symbols_argument)
        if not private_symbols.is_absolute():
            raise PublicationError("--private-symbols-dir must be absolute")
        private_symbols = Path(os.path.abspath(private_symbols))
    if private_symbols == output or output in private_symbols.parents:
        raise PublicationError(
            "private symbol output must be outside the public candidate directory"
        )
    return output, private_symbols


def _path_components(path: Path) -> tuple[str, ...]:
    if not path.is_absolute():
        raise PublicationError(f"release destination must be absolute: {path}")
    components = path.parts[1:]
    if any(component in {"", ".", ".."} for component in components):
        raise PublicationError(f"release destination is not normalized: {path}")
    return components


def _open_directory(path: Path, *, create: bool) -> int:
    descriptor = os.open("/", DIRECTORY_FLAGS)
    try:
        for component in _path_components(path):
            try:
                child = os.open(component, DIRECTORY_FLAGS, dir_fd=descriptor)
            except FileNotFoundError:
                if not create:
                    raise
                try:
                    os.mkdir(component, 0o700, dir_fd=descriptor)
                except FileExistsError:
                    pass
                try:
                    child = os.open(component, DIRECTORY_FLAGS, dir_fd=descriptor)
                except OSError as error:
                    raise PublicationError(
                        f"release destination gained a symlink or "
                        f"non-directory component: {path}"
                    ) from error
            except OSError as error:
                if error.errno in {errno.ELOOP, errno.ENOTDIR}:
                    raise PublicationError(
                        f"release destination contains a symlink or "
                        f"non-directory component: {path}"
                    ) from error
                raise
            os.close(descriptor)
            descriptor = child
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def _probe_directory(path: Path) -> int | None:
    try:
        return _open_directory(path, create=False)
    except FileNotFoundError:
        return None


def _existing_kind(parent_descriptor: int, name: str) -> str | None:
    try:
        mode = os.stat(
            name,
            dir_fd=parent_descriptor,
            follow_symlinks=False,
        ).st_mode
    except FileNotFoundError:
        return None
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "regular file"
    if stat.S_ISDIR(mode):
        return "directory"
    return "nonregular file"


def _require_absent(parent_descriptor: int, name: str, label: str) -> None:
    kind = _existing_kind(parent_descriptor, name)
    if kind is not None:
        raise PublicationError(f"{label} already exists ({kind}): {name}")


def _artifact_names(names: list[str]) -> list[str]:
    validated = [release_leaf(name) for name in names]
    if not validated or len(validated) != len(set(validated)):
        raise PublicationError("release artifact leaf list is empty or duplicated")
    return validated


def preflight_destinations(
    repo_root: Path,
    output_argument: str,
    private_symbols_argument: str | None,
    artifact_leaves: list[str],
) -> tuple[Path, Path]:
    names = _artifact_names(artifact_leaves)
    output, private_symbols = resolve_destinations(
        repo_root,
        output_argument,
        private_symbols_argument,
    )

    # Inspect every existing component of both paths before creating either
    # destination. This catches an ignored `target` symlink without first
    # writing to the other destination.
    output_probe = _probe_directory(output)
    symbols_parent_probe = _probe_directory(private_symbols.parent)
    try:
        if output_probe is not None:
            for name in names:
                _require_absent(output_probe, name, "release artifact destination")
        if symbols_parent_probe is not None:
            _require_absent(
                symbols_parent_probe,
                private_symbols.name,
                "private symbol destination",
            )
    finally:
        if output_probe is not None:
            os.close(output_probe)
        if symbols_parent_probe is not None:
            os.close(symbols_parent_probe)

    output_descriptor = _open_directory(output, create=True)
    symbols_parent_descriptor = _open_directory(
        private_symbols.parent,
        create=True,
    )
    try:
        for name in names:
            _require_absent(
                output_descriptor,
                name,
                "release artifact destination",
            )
        _require_absent(
            symbols_parent_descriptor,
            private_symbols.name,
            "private symbol destination",
        )
    finally:
        os.close(output_descriptor)
        os.close(symbols_parent_descriptor)
    return output, private_symbols


def _write_all(descriptor: int, value: bytes) -> None:
    offset = 0
    while offset < len(value):
        written = os.write(descriptor, value[offset:])
        if written == 0:
            raise OSError(errno.EIO, "short release publication write")
        offset += written


def _copy_regular_file(
    source_parent: int,
    destination_parent: int,
    name: str,
) -> None:
    source_descriptor = os.open(name, FILE_FLAGS, dir_fd=source_parent)
    destination_descriptor = -1
    try:
        before = os.fstat(source_descriptor)
        if not stat.S_ISREG(before.st_mode):
            raise PublicationError(f"release source is not a regular file: {name}")
        destination_descriptor = os.open(
            name,
            os.O_WRONLY
            | os.O_CLOEXEC
            | os.O_CREAT
            | os.O_EXCL
            | os.O_NOFOLLOW,
            stat.S_IMODE(before.st_mode),
            dir_fd=destination_parent,
        )
        while True:
            chunk = os.read(source_descriptor, 1024 * 1024)
            if not chunk:
                break
            _write_all(destination_descriptor, chunk)
        after = os.fstat(source_descriptor)
        observed_before = (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
            before.st_ctime_ns,
        )
        observed_after = (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        )
        if observed_after != observed_before:
            raise PublicationError(f"release source changed while copied: {name}")
        os.fchmod(destination_descriptor, stat.S_IMODE(before.st_mode))
        os.fsync(destination_descriptor)
    finally:
        if destination_descriptor >= 0:
            os.close(destination_descriptor)
        os.close(source_descriptor)


def _directory_names(descriptor: int) -> list[str]:
    with os.scandir(descriptor) as entries:
        return sorted(entry.name for entry in entries)


def _copy_tree(source_descriptor: int, destination_descriptor: int) -> None:
    names = _directory_names(source_descriptor)
    for name in names:
        release_leaf(name)
        source_entry = os.stat(
            name,
            dir_fd=source_descriptor,
            follow_symlinks=False,
        )
        if stat.S_ISREG(source_entry.st_mode):
            _copy_regular_file(source_descriptor, destination_descriptor, name)
            continue
        if not stat.S_ISDIR(source_entry.st_mode):
            raise PublicationError(
                f"private symbol source contains a link or special file: {name}"
            )
        source_child = os.open(name, DIRECTORY_FLAGS, dir_fd=source_descriptor)
        os.mkdir(name, 0o700, dir_fd=destination_descriptor)
        destination_child = os.open(
            name,
            DIRECTORY_FLAGS,
            dir_fd=destination_descriptor,
        )
        try:
            _copy_tree(source_child, destination_child)
            os.fsync(destination_child)
        finally:
            os.close(destination_child)
            os.close(source_child)
    if _directory_names(source_descriptor) != names:
        raise PublicationError("private symbol source changed while copied")


def _new_stage_container(parent_descriptor: int, label: str) -> tuple[str, int]:
    for _ in range(128):
        name = f".ctx-{label}.{secrets.token_hex(16)}"
        try:
            os.mkdir(name, 0o700, dir_fd=parent_descriptor)
        except FileExistsError:
            continue
        descriptor = os.open(name, DIRECTORY_FLAGS, dir_fd=parent_descriptor)
        opened = os.fstat(descriptor)
        named = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino):
            os.close(descriptor)
            raise PublicationError("release publication stage was substituted")
        return name, descriptor
    raise PublicationError("could not allocate a release publication stage")


def _remove_tree(descriptor: int) -> None:
    os.fchmod(descriptor, 0o700)
    for name in _directory_names(descriptor):
        entry = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        if stat.S_ISDIR(entry.st_mode):
            child = os.open(name, DIRECTORY_FLAGS, dir_fd=descriptor)
            try:
                _remove_tree(child)
            finally:
                os.close(child)
            os.rmdir(name, dir_fd=descriptor)
        else:
            os.unlink(name, dir_fd=descriptor)


def _remove_stage_container(
    parent_descriptor: int,
    name: str,
    descriptor: int,
) -> None:
    _remove_tree(descriptor)
    opened = os.fstat(descriptor)
    try:
        named = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    except FileNotFoundError:
        return
    if stat.S_ISDIR(named.st_mode) and (opened.st_dev, opened.st_ino) == (
        named.st_dev,
        named.st_ino,
    ):
        os.rmdir(name, dir_fd=parent_descriptor)


def cleanup_task_root(
    work_root: Path,
    task_root: Path,
    expected_device: int,
    expected_inode: int,
    before_remove: Callable[[], None] | None = None,
) -> None:
    if task_root.parent != work_root or not task_root.name:
        raise PublicationError("release task root is outside its declared work root")
    work_descriptor = _open_directory(work_root, create=False)
    task_descriptor = -1
    try:
        task_descriptor = os.open(
            task_root.name,
            DIRECTORY_FLAGS,
            dir_fd=work_descriptor,
        )
        opened = os.fstat(task_descriptor)
        if (opened.st_dev, opened.st_ino) != (expected_device, expected_inode):
            raise PublicationError("release task root identity changed before cleanup")
        if before_remove is not None:
            before_remove()
        os.fchmod(task_descriptor, 0o700)
        _remove_tree(task_descriptor)
        named = os.stat(
            task_root.name,
            dir_fd=work_descriptor,
            follow_symlinks=False,
        )
        if not stat.S_ISDIR(named.st_mode) or (named.st_dev, named.st_ino) != (
            expected_device,
            expected_inode,
        ):
            raise PublicationError("release task root changed during cleanup")
        os.rmdir(task_root.name, dir_fd=work_descriptor)
    finally:
        if task_descriptor >= 0:
            os.close(task_descriptor)
        os.close(work_descriptor)


def _rename_noreplace(
    source_descriptor: int,
    source_name: str,
    destination_descriptor: int,
    destination_name: str,
) -> None:
    libc = ctypes.CDLL(None, use_errno=True)
    renameat2 = getattr(libc, "renameat2", None)
    if renameat2 is None:
        raise PublicationError("Linux renameat2 is required for release publication")
    renameat2.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    renameat2.restype = ctypes.c_int
    result = renameat2(
        source_descriptor,
        os.fsencode(source_name),
        destination_descriptor,
        os.fsencode(destination_name),
        RENAME_NOREPLACE,
    )
    if result == 0:
        return
    error_number = ctypes.get_errno()
    if error_number == errno.EEXIST:
        raise PublicationError(
            f"release destination appeared during publication: {destination_name}"
        )
    raise OSError(
        error_number,
        f"could not publish release destination: {destination_name}",
    )


def publish(
    artifact_source: Path,
    output: Path,
    private_symbols_source: Path,
    private_symbols: Path,
    artifact_leaves: list[str],
    before_commit: Callable[[], None] | None = None,
) -> None:
    names = _artifact_names(artifact_leaves)
    artifact_source_descriptor = _open_directory(artifact_source, create=False)
    symbols_source_descriptor = _open_directory(
        private_symbols_source,
        create=False,
    )
    output_descriptor = _open_directory(output, create=False)
    symbols_parent_descriptor = _open_directory(
        private_symbols.parent,
        create=False,
    )
    artifact_stage_name = ""
    artifact_stage_descriptor = -1
    symbols_stage_name = ""
    symbols_stage_descriptor = -1
    try:
        for name in names:
            _require_absent(
                output_descriptor,
                name,
                "release artifact destination",
            )
        _require_absent(
            symbols_parent_descriptor,
            private_symbols.name,
            "private symbol destination",
        )
        source_names = _directory_names(artifact_source_descriptor)
        if source_names != sorted(names):
            raise PublicationError(
                "packaged release output does not match the declared artifact leaves"
            )

        artifact_stage_name, artifact_stage_descriptor = _new_stage_container(
            output_descriptor,
            "release-publish",
        )
        for name in names:
            _copy_regular_file(
                artifact_source_descriptor,
                artifact_stage_descriptor,
                name,
            )
        os.fsync(artifact_stage_descriptor)

        symbols_stage_name, symbols_stage_descriptor = _new_stage_container(
            symbols_parent_descriptor,
            "symbol-publish",
        )
        os.mkdir("bundle", 0o700, dir_fd=symbols_stage_descriptor)
        bundle_descriptor = os.open(
            "bundle",
            DIRECTORY_FLAGS,
            dir_fd=symbols_stage_descriptor,
        )
        try:
            _copy_tree(symbols_source_descriptor, bundle_descriptor)
            os.fsync(bundle_descriptor)
        finally:
            os.close(bundle_descriptor)
        os.fsync(symbols_stage_descriptor)

        if before_commit is not None:
            before_commit()

        # Recheck after all copying and any concurrent path mutation. The
        # checks and commits use the same retained directory descriptors.
        for name in names:
            _require_absent(
                output_descriptor,
                name,
                "release artifact destination",
            )
        _require_absent(
            symbols_parent_descriptor,
            private_symbols.name,
            "private symbol destination",
        )
        for name in names:
            _rename_noreplace(
                artifact_stage_descriptor,
                name,
                output_descriptor,
                name,
            )
        _rename_noreplace(
            symbols_stage_descriptor,
            "bundle",
            symbols_parent_descriptor,
            private_symbols.name,
        )
        os.fsync(output_descriptor)
        os.fsync(symbols_parent_descriptor)
    finally:
        if artifact_stage_descriptor >= 0:
            try:
                _remove_stage_container(
                    output_descriptor,
                    artifact_stage_name,
                    artifact_stage_descriptor,
                )
            finally:
                os.close(artifact_stage_descriptor)
        if symbols_stage_descriptor >= 0:
            try:
                _remove_stage_container(
                    symbols_parent_descriptor,
                    symbols_stage_name,
                    symbols_stage_descriptor,
                )
            finally:
                os.close(symbols_stage_descriptor)
        os.close(symbols_parent_descriptor)
        os.close(output_descriptor)
        os.close(symbols_source_descriptor)
        os.close(artifact_source_descriptor)


def main() -> int:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    resolve_parser = commands.add_parser("resolve")
    resolve_parser.add_argument("--repo-root", type=Path, required=True)
    resolve_parser.add_argument("--output-dir", required=True)
    resolve_parser.add_argument("--private-symbols-dir")
    preflight_parser = commands.add_parser("preflight")
    preflight_parser.add_argument("--repo-root", type=Path, required=True)
    preflight_parser.add_argument("--output-dir", required=True)
    preflight_parser.add_argument("--private-symbols-dir")
    preflight_parser.add_argument(
        "--artifact-leaf",
        action="append",
        dest="artifact_leaves",
        default=[],
    )
    publish_parser = commands.add_parser("publish")
    publish_parser.add_argument("--artifact-source-dir", type=Path, required=True)
    publish_parser.add_argument("--output-dir", type=Path, required=True)
    publish_parser.add_argument(
        "--private-symbols-source-dir",
        type=Path,
        required=True,
    )
    publish_parser.add_argument("--private-symbols-dir", type=Path, required=True)
    publish_parser.add_argument(
        "--artifact-leaf",
        action="append",
        dest="artifact_leaves",
        default=[],
    )
    cleanup_parser = commands.add_parser("cleanup-task-root")
    cleanup_parser.add_argument("--work-root", type=Path, required=True)
    cleanup_parser.add_argument("--task-root", type=Path, required=True)
    cleanup_parser.add_argument("--expected-device", type=int, required=True)
    cleanup_parser.add_argument("--expected-inode", type=int, required=True)
    args = parser.parse_args()
    try:
        if args.command == "resolve":
            output, private_symbols = resolve_destinations(
                args.repo_root,
                args.output_dir,
                args.private_symbols_dir,
            )
        elif args.command == "preflight":
            output, private_symbols = preflight_destinations(
                args.repo_root,
                args.output_dir,
                args.private_symbols_dir,
                args.artifact_leaves,
            )
        elif args.command == "publish":
            publish(
                args.artifact_source_dir,
                args.output_dir,
                args.private_symbols_source_dir,
                args.private_symbols_dir,
                args.artifact_leaves,
            )
            return 0
        else:
            cleanup_task_root(
                args.work_root,
                args.task_root,
                args.expected_device,
                args.expected_inode,
            )
            return 0
        print(f"CTX_LINUX_RELEASE_OUTPUT_DIR={shlex.quote(str(output))}")
        print(
            "CTX_LINUX_RELEASE_PRIVATE_SYMBOLS_DIR="
            f"{shlex.quote(str(private_symbols))}"
        )
    except (PublicationError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
