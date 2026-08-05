#!/usr/bin/env python3
"""Seal, publish, and consume complete Linux release candidates safely."""

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
import stat
import subprocess
import sys
from typing import Callable, NamedTuple, Sequence


DIRECTORY_FLAGS = os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
FILE_FLAGS = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
RENAME_NOREPLACE = 1
COMPLETION_KIND = "ctx-public-linux-release-completion"
COMPLETION_SCHEMA_VERSION = 1
MAX_COMPLETION_BYTES = 1024 * 1024


class PublicationError(ValueError):
    pass


def release_leaf(name: str) -> str:
    if not name or name in {".", ".."} or Path(name).name != name:
        raise PublicationError(f"invalid release output leaf: {name!r}")
    return name


def completion_leaf(platform: str) -> str:
    if platform not in {"linux-x64", "linux-aarch64"}:
        raise PublicationError(f"unsupported completed Linux platform: {platform}")
    return f"ctx-{platform}.release-complete.json"


def expected_release_leaves(platform: str) -> list[str]:
    binaries = {
        "linux-x64": "ctx",
        "linux-aarch64": "ctx-linux-aarch64",
    }
    try:
        binary = binaries[platform]
    except KeyError as error:
        raise PublicationError(
            f"unsupported completed Linux platform: {platform}"
        ) from error
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


def release_binary_leaf(platform: str) -> str:
    binaries = {
        "linux-x64": "ctx",
        "linux-aarch64": "ctx-linux-aarch64",
    }
    try:
        return binaries[platform]
    except KeyError as error:
        raise PublicationError(
            f"unsupported completed Linux platform: {platform}"
        ) from error


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
    if output == Path("/"):
        raise PublicationError("public release candidate cannot be the filesystem root")
    if private_symbols_argument is None:
        private_symbols = Path(f"{output}.private-debug-symbols")
    else:
        private_symbols = Path(private_symbols_argument)
        if not private_symbols.is_absolute():
            raise PublicationError("--private-symbols-dir must be absolute")
        private_symbols = Path(os.path.abspath(private_symbols))
    if private_symbols == Path("/"):
        raise PublicationError("private symbol destination cannot be the filesystem root")
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
                        "release destination gained a symlink or "
                        f"non-directory component: {path}"
                    ) from error
            except OSError as error:
                if error.errno in {errno.ELOOP, errno.ENOTDIR}:
                    raise PublicationError(
                        "release destination contains a symlink or "
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
        mode = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False).st_mode
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


def _require_named_directory_identity(
    parent_descriptor: int,
    name: str,
    descriptor: int,
    label: str,
) -> None:
    try:
        named = os.stat(name, dir_fd=parent_descriptor, follow_symlinks=False)
    except FileNotFoundError as error:
        raise PublicationError(f"{label} changed during publication: {name}") from error
    opened = os.fstat(descriptor)
    if not stat.S_ISDIR(named.st_mode) or (named.st_dev, named.st_ino) != (
        opened.st_dev,
        opened.st_ino,
    ):
        raise PublicationError(f"{label} changed during publication: {name}")


def _mount_id(descriptor: int) -> int:
    """Return the authoritative Linux mount ID for an open descriptor."""
    try:
        with open(f"/proc/self/fdinfo/{descriptor}", encoding="ascii") as source:
            for line in source:
                if line.startswith("mnt_id:"):
                    value = line.partition(":")[2].strip()
                    if value.isdecimal():
                        return int(value)
    except OSError as error:
        raise PublicationError(
            "Linux /proc fdinfo mount IDs are required for release cleanup"
        ) from error
    raise PublicationError(
        "Linux /proc fdinfo did not report a mount ID for release cleanup"
    )


def _descriptor_identity(descriptor: int) -> tuple[int, int, int]:
    value = os.fstat(descriptor)
    return value.st_dev, value.st_ino, _mount_id(descriptor)


def _verify_directory_binding(path: Path, descriptor: int, label: str) -> None:
    try:
        current = _open_directory(path, create=False)
    except (OSError, PublicationError) as error:
        raise PublicationError(f"{label} pathname was substituted: {path}") from error
    try:
        if _descriptor_identity(current) != _descriptor_identity(descriptor):
            raise PublicationError(f"{label} pathname was substituted: {path}")
    finally:
        os.close(current)


def preflight_destinations(
    repo_root: Path,
    output_argument: str,
    private_symbols_argument: str | None,
) -> tuple[Path, Path]:
    output, private_symbols = resolve_destinations(
        repo_root, output_argument, private_symbols_argument
    )

    # Inspect both requested paths before creating either parent. This catches
    # an ignored output ancestor or external symbol ancestor without first
    # writing to the other destination.
    output_probe = _probe_directory(output.parent)
    symbols_probe = _probe_directory(private_symbols.parent)
    try:
        if output_probe is not None:
            _require_absent(
                output_probe, output.name, "public release candidate"
            )
        if symbols_probe is not None:
            _require_absent(
                symbols_probe,
                private_symbols.name,
                "private symbol destination",
            )
    finally:
        if symbols_probe is not None:
            os.close(symbols_probe)
        if output_probe is not None:
            os.close(output_probe)

    # The final public candidate and private symbol bundle are directory commit
    # units. Their parents may now be created, but both final leaves stay absent.
    output_parent = _open_directory(output.parent, create=True)
    symbols_parent = -1
    try:
        symbols_parent = _open_directory(private_symbols.parent, create=True)
        _require_absent(output_parent, output.name, "public release candidate")
        _require_absent(
            symbols_parent, private_symbols.name, "private symbol destination"
        )
        _verify_directory_binding(output.parent, output_parent, "public output parent")
        _verify_directory_binding(
            private_symbols.parent, symbols_parent, "private symbol parent"
        )
    finally:
        if symbols_parent >= 0:
            os.close(symbols_parent)
        os.close(output_parent)
    return output, private_symbols


def _write_all(descriptor: int, value: bytes) -> None:
    offset = 0
    while offset < len(value):
        written = os.write(descriptor, value[offset:])
        if written == 0:
            raise OSError(errno.EIO, "short release publication write")
        offset += written


def _sha256_descriptor(
    descriptor: int,
    chunk_hook: Callable[[int], None] | None = None,
) -> tuple[str, int]:
    digest = hashlib.sha256()
    size = 0
    os.lseek(descriptor, 0, os.SEEK_SET)
    while True:
        chunk = os.read(descriptor, 1024 * 1024)
        if not chunk:
            break
        digest.update(chunk)
        size += len(chunk)
        if chunk_hook is not None:
            chunk_hook(size)
    os.lseek(descriptor, 0, os.SEEK_SET)
    return digest.hexdigest(), size


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


def _copy_flat_directory(source_descriptor: int, destination_descriptor: int) -> None:
    names = _directory_names(source_descriptor)
    for name in names:
        release_leaf(name)
        entry = os.stat(name, dir_fd=source_descriptor, follow_symlinks=False)
        if not stat.S_ISREG(entry.st_mode):
            raise PublicationError(
                f"public release source contains a link or non-file: {name}"
            )
        _copy_regular_file(source_descriptor, destination_descriptor, name)
    if _directory_names(source_descriptor) != names:
        raise PublicationError("public release source changed while copied")


def _copy_tree(source_descriptor: int, destination_descriptor: int) -> None:
    names = _directory_names(source_descriptor)
    for name in names:
        release_leaf(name)
        source_entry = os.stat(name, dir_fd=source_descriptor, follow_symlinks=False)
        if stat.S_ISREG(source_entry.st_mode):
            _copy_regular_file(source_descriptor, destination_descriptor, name)
            continue
        if not stat.S_ISDIR(source_entry.st_mode):
            raise PublicationError(
                f"private symbol source contains a link or special file: {name}"
            )
        source_child = os.open(name, DIRECTORY_FLAGS, dir_fd=source_descriptor)
        os.mkdir(name, 0o700, dir_fd=destination_descriptor)
        destination_child = os.open(name, DIRECTORY_FLAGS, dir_fd=destination_descriptor)
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


def _validate_tree_mounts(descriptor: int, expected_mount_id: int) -> None:
    if _mount_id(descriptor) != expected_mount_id:
        raise PublicationError("release cleanup refused to cross a mount boundary")
    for name in _directory_names(descriptor):
        entry = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        if not stat.S_ISDIR(entry.st_mode):
            continue
        child = os.open(name, DIRECTORY_FLAGS, dir_fd=descriptor)
        try:
            _validate_tree_mounts(child, expected_mount_id)
        finally:
            os.close(child)


def _remove_tree(descriptor: int, expected_mount_id: int) -> None:
    if _mount_id(descriptor) != expected_mount_id:
        raise PublicationError("release cleanup refused to cross a mount boundary")
    os.fchmod(descriptor, 0o700)
    for name in _directory_names(descriptor):
        entry = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        if stat.S_ISDIR(entry.st_mode):
            child = os.open(name, DIRECTORY_FLAGS, dir_fd=descriptor)
            try:
                _remove_tree(child, expected_mount_id)
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
    if not name:
        return
    expected_mount_id = _mount_id(descriptor)
    _validate_tree_mounts(descriptor, expected_mount_id)
    _remove_tree(descriptor, expected_mount_id)
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
        task_descriptor = os.open(task_root.name, DIRECTORY_FLAGS, dir_fd=work_descriptor)
        opened = os.fstat(task_descriptor)
        if (opened.st_dev, opened.st_ino) != (expected_device, expected_inode):
            raise PublicationError("release task root identity changed before cleanup")
        expected_mount_id = _mount_id(task_descriptor)
        # Validate the complete tree before deleting any leaf. A suspicious
        # task root is intentionally leaked rather than traversing a mount.
        _validate_tree_mounts(task_descriptor, expected_mount_id)
        if before_remove is not None:
            before_remove()
        _verify_directory_binding(work_root, work_descriptor, "release work root")
        _validate_tree_mounts(task_descriptor, expected_mount_id)
        _remove_tree(task_descriptor, expected_mount_id)
        named = os.stat(task_root.name, dir_fd=work_descriptor, follow_symlinks=False)
        if not stat.S_ISDIR(named.st_mode) or (named.st_dev, named.st_ino) != (
            expected_device,
            expected_inode,
        ):
            raise PublicationError("release task root changed during cleanup")
        os.rmdir(task_root.name, dir_fd=work_descriptor)
        _verify_directory_binding(work_root, work_descriptor, "release work root")
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
        error_number, f"could not publish release destination: {destination_name}"
    )


def _file_record(parent_descriptor: int, name: str) -> dict[str, object]:
    descriptor = os.open(name, FILE_FLAGS, dir_fd=parent_descriptor)
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise PublicationError(f"completed release leaf is not regular: {name}")
        digest, size = _sha256_descriptor(descriptor)
        return {
            "mode": f"{stat.S_IMODE(metadata.st_mode):04o}",
            "name": name,
            "sha256": digest,
            "size": size,
        }
    finally:
        os.close(descriptor)


def seal_candidate(candidate: Path, platform: str, source_commit: str) -> Path:
    if not _valid_commit(source_commit):
        raise PublicationError("completed release source commit is invalid")
    expected = expected_release_leaves(platform)
    marker = completion_leaf(platform)
    descriptor = _open_directory(candidate, create=False)
    marker_descriptor = -1
    try:
        actual = _directory_names(descriptor)
        if actual != expected:
            raise PublicationError(
                "Linux release bundle is incomplete before sealing; "
                f"expected {expected}, got {actual}"
            )
        records = [_file_record(descriptor, name) for name in expected]
        payload = {
            "files": records,
            "kind": COMPLETION_KIND,
            "platform": platform,
            "schema_version": COMPLETION_SCHEMA_VERSION,
            "source_commit": source_commit,
        }
        encoded = (
            json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n"
        ).encode("utf-8")
        marker_descriptor = os.open(
            marker,
            os.O_WRONLY
            | os.O_CLOEXEC
            | os.O_CREAT
            | os.O_EXCL
            | os.O_NOFOLLOW,
            0o600,
            dir_fd=descriptor,
        )
        _write_all(marker_descriptor, encoded)
        os.fsync(marker_descriptor)
        os.fsync(descriptor)
        _verify_directory_binding(candidate, descriptor, "release bundle")
    finally:
        if marker_descriptor >= 0:
            os.close(marker_descriptor)
        os.close(descriptor)
    return candidate / marker


def require_unsealed(candidate: Path) -> None:
    descriptor = _open_directory(candidate, create=False)
    try:
        markers = [
            name
            for name in _directory_names(descriptor)
            if name.endswith(".release-complete.json")
        ]
        if markers:
            raise PublicationError(
                "completed public candidate cannot be modified: "
                + ", ".join(markers)
            )
        _verify_directory_binding(candidate, descriptor, "release staging directory")
    finally:
        os.close(descriptor)


def _valid_commit(value: str) -> bool:
    return (
        len(value) == 40
        and value != "0" * 40
        and all(character in "0123456789abcdef" for character in value)
    )


class DescriptorBinding(NamedTuple):
    device: int
    inode: int
    mount_id: int
    mode: int
    size: int
    mtime_ns: int
    ctime_ns: int


def _descriptor_binding(descriptor: int) -> DescriptorBinding:
    metadata = os.fstat(descriptor)
    return DescriptorBinding(
        device=metadata.st_dev,
        inode=metadata.st_ino,
        mount_id=_mount_id(descriptor),
        mode=metadata.st_mode,
        size=metadata.st_size,
        mtime_ns=metadata.st_mtime_ns,
        ctime_ns=metadata.st_ctime_ns,
    )


def _read_descriptor(descriptor: int, name: str, maximum: int) -> bytes:
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
        raise PublicationError(f"release completion file is invalid: {name}")
    os.lseek(descriptor, 0, os.SEEK_SET)
    chunks: list[bytes] = []
    remaining = maximum + 1
    while remaining:
        chunk = os.read(descriptor, min(remaining, 64 * 1024))
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    value = b"".join(chunks)
    os.lseek(descriptor, 0, os.SEEK_SET)
    if len(value) > maximum:
        raise PublicationError(f"release completion file is too large: {name}")
    return value


class CompletedCandidateSnapshot:
    """Pinned source FDs and identities for one completed candidate root."""

    def __init__(
        self,
        descriptor: int,
        candidate: Path | None,
        platforms: Sequence[str],
        source_commit: str,
        *,
        allow_extra: bool,
        owns_descriptor: bool,
        hash_hook: Callable[[str, int], None] | None = None,
    ) -> None:
        if (
            not platforms
            or len(set(platforms)) != len(platforms)
            or not _valid_commit(source_commit)
        ):
            raise PublicationError("completed release snapshot identity is invalid")
        self.descriptor = descriptor
        self.candidate = candidate
        self.platforms = tuple(platforms)
        self.source_commit = source_commit
        self.allow_extra = allow_extra
        self.owns_descriptor = owns_descriptor
        self.descriptors: dict[str, int] = {}
        self.bindings: dict[str, DescriptorBinding] = {}
        self.records: dict[str, dict[str, object]] = {}
        self.root_binding = _descriptor_binding(descriptor)
        self.names: tuple[str, ...] = ()
        try:
            self._open_and_hash(hash_hook)
        except BaseException:
            self.close()
            raise

    @classmethod
    def open(
        cls,
        candidate: Path,
        platforms: Sequence[str],
        source_commit: str,
        *,
        allow_extra: bool,
        hash_hook: Callable[[str, int], None] | None = None,
    ) -> CompletedCandidateSnapshot:
        descriptor = _open_directory(candidate, create=False)
        return cls(
            descriptor,
            candidate,
            platforms,
            source_commit,
            allow_extra=allow_extra,
            owns_descriptor=True,
            hash_hook=hash_hook,
        )

    def _open_name(self, name: str) -> int:
        existing = self.descriptors.get(name)
        if existing is not None:
            return existing
        release_leaf(name)
        descriptor = os.open(name, FILE_FLAGS, dir_fd=self.descriptor)
        binding = _descriptor_binding(descriptor)
        try:
            named = os.stat(name, dir_fd=self.descriptor, follow_symlinks=False)
        except BaseException:
            os.close(descriptor)
            raise
        if (
            not stat.S_ISREG(binding.mode)
            or not stat.S_ISREG(named.st_mode)
            or (named.st_dev, named.st_ino) != (binding.device, binding.inode)
        ):
            os.close(descriptor)
            raise PublicationError(
                f"completed release leaf is not a pinned regular file: {name}"
            )
        self.descriptors[name] = descriptor
        self.bindings[name] = binding
        return descriptor

    def _manifest_records(self, platform: str) -> dict[str, dict[str, object]]:
        marker = completion_leaf(platform)
        descriptor = self._open_name(marker)
        try:
            payload = json.loads(
                _read_descriptor(descriptor, marker, MAX_COMPLETION_BYTES)
            )
        except (json.JSONDecodeError, UnicodeDecodeError) as error:
            raise PublicationError(
                f"completed release marker is missing or invalid: {marker}"
            ) from error
        if not isinstance(payload, dict) or {
            "schema_version": payload.get("schema_version"),
            "kind": payload.get("kind"),
            "platform": payload.get("platform"),
            "source_commit": payload.get("source_commit"),
        } != {
            "schema_version": COMPLETION_SCHEMA_VERSION,
            "kind": COMPLETION_KIND,
            "platform": platform,
            "source_commit": self.source_commit,
        }:
            raise PublicationError(f"completed release identity is invalid: {marker}")
        expected_names = expected_release_leaves(platform)
        records = payload.get("files")
        if not isinstance(records, list) or len(records) != len(expected_names):
            raise PublicationError(
                f"completed release file manifest is invalid: {marker}"
            )
        result: dict[str, dict[str, object]] = {}
        for index, name in enumerate(expected_names):
            record = records[index]
            if (
                not isinstance(record, dict)
                or set(record) != {"mode", "name", "sha256", "size"}
                or record.get("name") != name
            ):
                raise PublicationError(
                    f"completed release file manifest is invalid: {marker}"
                )
            result[name] = record
        return result

    def _open_and_hash(
        self,
        hash_hook: Callable[[str, int], None] | None,
    ) -> None:
        initial_names = _directory_names(self.descriptor)
        marker_names = sorted(completion_leaf(platform) for platform in self.platforms)
        unexpected_markers = sorted(
            name
            for name in initial_names
            if name.endswith(".release-complete.json") and name not in marker_names
        )
        if unexpected_markers:
            raise PublicationError(
                "completed release directory has unexpected completion markers: "
                + ", ".join(unexpected_markers)
            )
        expected_records: dict[str, dict[str, object]] = {}
        for platform in self.platforms:
            try:
                records = self._manifest_records(platform)
            except FileNotFoundError as error:
                raise PublicationError(
                    "completed release marker is missing or invalid: "
                    f"{completion_leaf(platform)}"
                ) from error
            for name, record in records.items():
                previous = expected_records.get(name)
                if previous is not None and previous != record:
                    raise PublicationError(
                        f"completed release manifests disagree about leaf: {name}"
                    )
                expected_records[name] = record

        expected_names = sorted([*marker_names, *expected_records])
        missing_names = sorted(set(expected_names) - set(initial_names))
        if missing_names:
            raise PublicationError(
                "completed release directory is missing declared leaves: "
                + ", ".join(missing_names)
            )
        if not self.allow_extra and initial_names != expected_names:
            raise PublicationError(
                "completed release directory has unexpected leaves; "
                f"expected {expected_names}, got {initial_names}"
            )
        names = initial_names if self.allow_extra else expected_names
        for name in names:
            self._open_name(name)
        self.names = tuple(names)

        for name in self.names:
            descriptor = self.descriptors[name]
            hook = None
            if hash_hook is not None:
                hook = lambda size, leaf=name: hash_hook(leaf, size)
            digest, size = _sha256_descriptor(descriptor, hook)
            binding = self.bindings[name]
            record = {
                "mode": f"{stat.S_IMODE(binding.mode):04o}",
                "name": name,
                "sha256": digest,
                "size": size,
            }
            self.records[name] = record
            expected = expected_records.get(name)
            if expected is not None and record != expected:
                raise PublicationError(
                    f"completed release leaf does not match marker: {name}"
                )
        self.revalidate()

    def revalidate(self) -> None:
        if _directory_names(self.descriptor) != list(self.names):
            raise PublicationError("completed release candidate names changed")
        for name in self.names:
            descriptor = self.descriptors[name]
            if _descriptor_binding(descriptor) != self.bindings[name]:
                raise PublicationError(
                    f"completed release leaf changed while pinned: {name}"
                )
            try:
                named = os.stat(name, dir_fd=self.descriptor, follow_symlinks=False)
                current = os.open(name, FILE_FLAGS, dir_fd=self.descriptor)
            except (OSError, PublicationError) as error:
                raise PublicationError(
                    f"completed release leaf name changed while pinned: {name}"
                ) from error
            try:
                binding = self.bindings[name]
                if (
                    not stat.S_ISREG(named.st_mode)
                    or (named.st_dev, named.st_ino)
                    != (binding.device, binding.inode)
                    or _descriptor_binding(current) != binding
                ):
                    raise PublicationError(
                        f"completed release leaf name changed while pinned: {name}"
                    )
            finally:
                os.close(current)
        if _descriptor_binding(self.descriptor) != self.root_binding:
            raise PublicationError("completed release candidate changed while pinned")
        if self.candidate is not None:
            _verify_directory_binding(
                self.candidate, self.descriptor, "completed release candidate"
            )

    def materialize(self, snapshot_root: Path) -> MaterializedCandidateSnapshot:
        return MaterializedCandidateSnapshot.create(self, snapshot_root)

    def close(self) -> None:
        for descriptor in self.descriptors.values():
            try:
                os.close(descriptor)
            except OSError:
                pass
        self.descriptors.clear()
        if self.owns_descriptor and self.descriptor >= 0:
            try:
                os.close(self.descriptor)
            except OSError:
                pass
            self.descriptor = -1


class MaterializedCandidateSnapshot:
    """A private read-only copy made only from a completed snapshot's FDs."""

    def __init__(
        self,
        snapshot_root: Path,
        parent_descriptor: int,
        name: str,
        descriptor: int,
        descriptors: dict[str, int],
        bindings: dict[str, DescriptorBinding],
        root_binding: DescriptorBinding,
    ) -> None:
        self.snapshot_root = snapshot_root
        self.parent_descriptor = parent_descriptor
        self.name = name
        self.descriptor = descriptor
        self.descriptors = descriptors
        self.bindings = bindings
        self.root_binding = root_binding

    @classmethod
    def create(
        cls,
        source: CompletedCandidateSnapshot,
        snapshot_root: Path,
    ) -> MaterializedCandidateSnapshot:
        parent_descriptor = _open_directory(snapshot_root, create=False)
        name = ""
        descriptor = -1
        descriptors: dict[str, int] = {}
        try:
            name, descriptor = _new_stage_container(
                parent_descriptor, "completed-candidate"
            )
            for leaf in source.names:
                source_descriptor = source.descriptors[leaf]
                source_binding = source.bindings[leaf]
                destination = os.open(
                    leaf,
                    os.O_WRONLY
                    | os.O_CLOEXEC
                    | os.O_CREAT
                    | os.O_EXCL
                    | os.O_NOFOLLOW,
                    0o600,
                    dir_fd=descriptor,
                )
                try:
                    os.lseek(source_descriptor, 0, os.SEEK_SET)
                    digest = hashlib.sha256()
                    size = 0
                    while True:
                        chunk = os.read(source_descriptor, 1024 * 1024)
                        if not chunk:
                            break
                        _write_all(destination, chunk)
                        digest.update(chunk)
                        size += len(chunk)
                    os.lseek(source_descriptor, 0, os.SEEK_SET)
                    if _descriptor_binding(source_descriptor) != source_binding:
                        raise PublicationError(
                            f"completed release leaf changed while copied: {leaf}"
                        )
                    expected = source.records[leaf]
                    if (
                        digest.hexdigest() != expected["sha256"]
                        or size != expected["size"]
                    ):
                        raise PublicationError(
                            f"completed release snapshot copy changed: {leaf}"
                        )
                    read_only_mode = stat.S_IMODE(source_binding.mode) & ~0o222
                    os.fchmod(destination, read_only_mode)
                    os.fsync(destination)
                finally:
                    os.close(destination)
                copied = os.open(leaf, FILE_FLAGS, dir_fd=descriptor)
                copied_digest, copied_size = _sha256_descriptor(copied)
                if (
                    copied_digest != source.records[leaf]["sha256"]
                    or copied_size != source.records[leaf]["size"]
                ):
                    os.close(copied)
                    raise PublicationError(
                        f"completed release snapshot digest mismatch: {leaf}"
                    )
                descriptors[leaf] = copied
            os.fchmod(descriptor, 0o500)
            os.fsync(descriptor)
            bindings = {
                leaf: _descriptor_binding(leaf_descriptor)
                for leaf, leaf_descriptor in descriptors.items()
            }
            result = cls(
                snapshot_root,
                parent_descriptor,
                name,
                descriptor,
                descriptors,
                bindings,
                _descriptor_binding(descriptor),
            )
            result.revalidate()
            return result
        except BaseException:
            for copied in descriptors.values():
                os.close(copied)
            if descriptor >= 0:
                try:
                    _remove_stage_container(parent_descriptor, name, descriptor)
                finally:
                    os.close(descriptor)
            os.close(parent_descriptor)
            raise

    def candidate_argument(self) -> str:
        return f"/proc/self/fd/{self.descriptor}"

    def leaf_argument(self, name: str) -> str:
        try:
            descriptor = self.descriptors[name]
        except KeyError as error:
            raise PublicationError(
                f"completed release command requested an unpinned leaf: {name}"
            ) from error
        return f"/proc/self/fd/{descriptor}"

    def pass_fds(self) -> tuple[int, ...]:
        return (self.descriptor, *self.descriptors.values())

    def revalidate(self) -> None:
        if _directory_names(self.descriptor) != sorted(self.descriptors):
            raise PublicationError("materialized completed candidate names changed")
        for name, descriptor in self.descriptors.items():
            binding = self.bindings[name]
            if _descriptor_binding(descriptor) != binding:
                raise PublicationError(
                    f"materialized completed candidate leaf changed: {name}"
                )
            named = os.stat(name, dir_fd=self.descriptor, follow_symlinks=False)
            if (
                not stat.S_ISREG(named.st_mode)
                or (named.st_dev, named.st_ino) != (binding.device, binding.inode)
            ):
                raise PublicationError(
                    f"materialized completed candidate leaf changed: {name}"
                )
        if _descriptor_binding(self.descriptor) != self.root_binding:
            raise PublicationError("materialized completed candidate changed")
        _verify_directory_binding(
            self.snapshot_root / self.name,
            self.descriptor,
            "materialized completed candidate",
        )

    def close(self) -> None:
        for descriptor in self.descriptors.values():
            try:
                os.close(descriptor)
            except OSError:
                pass
        self.descriptors.clear()
        try:
            _remove_stage_container(
                self.parent_descriptor, self.name, self.descriptor
            )
        finally:
            try:
                os.close(self.descriptor)
            finally:
                os.close(self.parent_descriptor)


def _verify_complete_descriptor(
    descriptor: int,
    platform: str,
    source_commit: str,
    *,
    allow_extra: bool,
) -> dict[str, object]:
    snapshot = CompletedCandidateSnapshot(
        descriptor,
        None,
        [platform],
        source_commit,
        allow_extra=allow_extra,
        owns_descriptor=False,
    )
    try:
        snapshot.revalidate()
        marker = completion_leaf(platform)
        return json.loads(
            _read_descriptor(snapshot.descriptors[marker], marker, MAX_COMPLETION_BYTES)
        )
    finally:
        snapshot.close()


def verify_candidate(
    candidate: Path,
    platform: str,
    source_commit: str,
    *,
    allow_extra: bool = False,
    hash_hook: Callable[[str, int], None] | None = None,
) -> tuple[int, int, int]:
    snapshot = CompletedCandidateSnapshot.open(
        candidate,
        [platform],
        source_commit,
        allow_extra=allow_extra,
        hash_hook=hash_hook,
    )
    try:
        snapshot.revalidate()
        return _descriptor_identity(snapshot.descriptor)
    finally:
        snapshot.close()


def publish(
    artifact_source: Path,
    output: Path,
    private_symbols_source: Path,
    private_symbols: Path,
    platform: str,
    source_commit: str,
    phase_hook: Callable[[str], None] | None = None,
) -> None:
    artifact_source_descriptor = _open_directory(artifact_source, create=False)
    symbols_source_descriptor = _open_directory(private_symbols_source, create=False)
    output_parent_descriptor = _open_directory(output.parent, create=False)
    symbols_parent_descriptor = _open_directory(private_symbols.parent, create=False)
    artifact_stage_name = ""
    artifact_stage_descriptor = -1
    symbols_stage_name = ""
    symbols_stage_descriptor = -1
    try:
        _verify_complete_descriptor(
            artifact_source_descriptor,
            platform,
            source_commit,
            allow_extra=False,
        )
        _require_absent(
            output_parent_descriptor, output.name, "public release candidate"
        )
        _require_absent(
            symbols_parent_descriptor,
            private_symbols.name,
            "private symbol destination",
        )
        artifact_stage_name, artifact_stage_descriptor = _new_stage_container(
            output_parent_descriptor, "release-publish"
        )
        _copy_flat_directory(
            artifact_source_descriptor, artifact_stage_descriptor
        )
        _verify_complete_descriptor(
            artifact_stage_descriptor,
            platform,
            source_commit,
            allow_extra=False,
        )
        os.fsync(artifact_stage_descriptor)

        symbols_stage_name, symbols_stage_descriptor = _new_stage_container(
            symbols_parent_descriptor, "symbol-publish"
        )
        _copy_tree(symbols_source_descriptor, symbols_stage_descriptor)
        os.fsync(symbols_stage_descriptor)

        if phase_hook is not None:
            phase_hook("before-symbol-commit")
        _verify_directory_binding(
            private_symbols.parent,
            symbols_parent_descriptor,
            "private symbol parent",
        )
        _verify_directory_binding(
            output.parent, output_parent_descriptor, "public output parent"
        )
        _require_absent(
            symbols_parent_descriptor,
            private_symbols.name,
            "private symbol destination",
        )
        _rename_noreplace(
            symbols_parent_descriptor,
            symbols_stage_name,
            symbols_parent_descriptor,
            private_symbols.name,
        )
        symbols_stage_name = ""
        os.fsync(symbols_parent_descriptor)
        _verify_directory_binding(
            private_symbols.parent,
            symbols_parent_descriptor,
            "private symbol parent",
        )
        _verify_directory_binding(
            private_symbols, symbols_stage_descriptor, "private symbol bundle"
        )

        if phase_hook is not None:
            phase_hook("after-symbol-commit")
        _verify_directory_binding(
            private_symbols.parent,
            symbols_parent_descriptor,
            "private symbol parent",
        )
        _verify_directory_binding(
            output.parent, output_parent_descriptor, "public output parent"
        )
        _require_absent(
            output_parent_descriptor, output.name, "public release candidate"
        )
        if phase_hook is not None:
            # This hook is intentionally after the final absence check. Tests
            # inject the decisive no-replace collision at the rename boundary.
            phase_hook("before-public-rename")
        _rename_noreplace(
            output_parent_descriptor,
            artifact_stage_name,
            output_parent_descriptor,
            output.name,
        )
        os.fsync(output_parent_descriptor)

        try:
            if phase_hook is not None:
                phase_hook("after-public-commit")
            _verify_directory_binding(
                output.parent, output_parent_descriptor, "public output parent"
            )
            _verify_directory_binding(
                output, artifact_stage_descriptor, "public release candidate"
            )
            _verify_directory_binding(
                private_symbols.parent,
                symbols_parent_descriptor,
                "private symbol parent",
            )
            _verify_directory_binding(
                private_symbols, symbols_stage_descriptor, "private symbol bundle"
            )
            _verify_complete_descriptor(
                artifact_stage_descriptor,
                platform,
                source_commit,
                allow_extra=False,
            )
        except BaseException:
            # A commit-boundary substitution must not leave a completed public
            # directory under the old parent. Roll the directory back to its
            # hidden stage atomically. If that no-replace rollback is itself
            # obstructed, invalidate the candidate through its retained
            # descriptor before surfacing the failure.
            try:
                _require_named_directory_identity(
                    output_parent_descriptor,
                    output.name,
                    artifact_stage_descriptor,
                    "public release candidate",
                )
                _rename_noreplace(
                    output_parent_descriptor,
                    output.name,
                    output_parent_descriptor,
                    artifact_stage_name,
                )
                os.fsync(output_parent_descriptor)
            except BaseException as rollback_error:
                try:
                    os.unlink(
                        completion_leaf(platform),
                        dir_fd=artifact_stage_descriptor,
                    )
                    os.fsync(artifact_stage_descriptor)
                except BaseException as invalidation_error:
                    raise PublicationError(
                        "failed publication could not be invalidated"
                    ) from invalidation_error
                raise PublicationError(
                    "failed publication was invalidated after rollback obstruction"
                ) from rollback_error
            raise
        artifact_stage_name = ""
    finally:
        if artifact_stage_descriptor >= 0:
            try:
                _remove_stage_container(
                    output_parent_descriptor,
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
        os.close(output_parent_descriptor)
        os.close(symbols_source_descriptor)
        os.close(artifact_source_descriptor)


def _expanded_snapshot_command(
    snapshot: MaterializedCandidateSnapshot,
    platforms: Sequence[str],
    command: Sequence[str],
) -> list[str]:
    if not command:
        raise PublicationError("completed release command is empty")
    replacements = {
        "{candidate}": snapshot.candidate_argument(),
    }
    if len(platforms) == 1:
        replacements["{ctx}"] = snapshot.leaf_argument(
            release_binary_leaf(platforms[0])
        )
    for name in snapshot.descriptors:
        replacements[f"{{leaf:{name}}}"] = snapshot.leaf_argument(name)
    result: list[str] = []
    for value in command:
        expanded = value
        for placeholder, replacement in replacements.items():
            expanded = expanded.replace(placeholder, replacement)
        if (
            "{candidate}" in expanded
            or "{ctx}" in expanded
            or "{leaf:" in expanded
        ):
            raise PublicationError(
                f"completed release command contains an unbound placeholder: {value}"
            )
        result.append(expanded)
    return result


def consume_complete_candidate(
    candidate: Path,
    snapshot_root: Path,
    platforms: Sequence[str],
    source_commit: str,
    command: Sequence[str],
    *,
    allow_extra: bool,
    before_consume: Callable[[], None] | None = None,
) -> int:
    source = CompletedCandidateSnapshot.open(
        candidate,
        platforms,
        source_commit,
        allow_extra=allow_extra,
    )
    snapshot: MaterializedCandidateSnapshot | None = None
    try:
        snapshot = source.materialize(snapshot_root)
        if before_consume is not None:
            before_consume()
        argv = _expanded_snapshot_command(snapshot, platforms, command)
        result = subprocess.run(
            argv,
            check=False,
            pass_fds=snapshot.pass_fds(),
        )
        snapshot.revalidate()
        source.revalidate()
        return result.returncode
    finally:
        if snapshot is not None:
            snapshot.close()
        source.close()


def run_complete_candidate(
    candidate: Path,
    platform: str,
    source_commit: str,
    command: Sequence[str],
    before_consume: Callable[[], None] | None = None,
) -> int:
    return consume_complete_candidate(
        candidate,
        Path(os.environ.get("TMPDIR", "/tmp")),
        [platform],
        source_commit,
        command,
        allow_extra=False,
        before_consume=before_consume,
    )


def _print_destinations(output: Path, private_symbols: Path) -> None:
    print(f"CTX_LINUX_RELEASE_OUTPUT_DIR={shlex.quote(str(output))}")
    print(
        "CTX_LINUX_RELEASE_PRIVATE_SYMBOLS_DIR="
        f"{shlex.quote(str(private_symbols))}"
    )


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
    seal_parser = commands.add_parser("seal")
    seal_parser.add_argument("--candidate-dir", type=Path, required=True)
    seal_parser.add_argument("--platform", required=True)
    seal_parser.add_argument("--source-commit", required=True)
    unsealed_parser = commands.add_parser("require-unsealed")
    unsealed_parser.add_argument("--candidate-dir", type=Path, required=True)
    publish_parser = commands.add_parser("publish")
    publish_parser.add_argument("--artifact-source-dir", type=Path, required=True)
    publish_parser.add_argument("--output-dir", type=Path, required=True)
    publish_parser.add_argument(
        "--private-symbols-source-dir", type=Path, required=True
    )
    publish_parser.add_argument("--private-symbols-dir", type=Path, required=True)
    publish_parser.add_argument("--platform", required=True)
    publish_parser.add_argument("--source-commit", required=True)
    verify_parser = commands.add_parser("verify-complete")
    verify_parser.add_argument("--candidate-dir", type=Path, required=True)
    verify_parser.add_argument("--platform", required=True)
    verify_parser.add_argument("--source-commit", required=True)
    verify_parser.add_argument("--allow-extra", action="store_true")
    consume_parser = commands.add_parser("consume-complete")
    consume_parser.add_argument("--candidate-dir", type=Path, required=True)
    consume_parser.add_argument(
        "--snapshot-root",
        type=Path,
        default=Path(os.environ.get("TMPDIR", "/tmp")),
    )
    consume_parser.add_argument("--platform", action="append", default=[])
    consume_parser.add_argument("--source-commit", required=True)
    consume_parser.add_argument("--allow-extra", action="store_true")
    consume_parser.add_argument("remainder", nargs=argparse.REMAINDER)
    run_parser = commands.add_parser("run-complete")
    run_parser.add_argument("--candidate-dir", type=Path, required=True)
    run_parser.add_argument("--platform", required=True)
    run_parser.add_argument("--source-commit", required=True)
    run_parser.add_argument("remainder", nargs=argparse.REMAINDER)
    cleanup_parser = commands.add_parser("cleanup-task-root")
    cleanup_parser.add_argument("--work-root", type=Path, required=True)
    cleanup_parser.add_argument("--task-root", type=Path, required=True)
    cleanup_parser.add_argument("--expected-device", type=int, required=True)
    cleanup_parser.add_argument("--expected-inode", type=int, required=True)
    args = parser.parse_args()
    try:
        if args.command == "resolve":
            output, private_symbols = resolve_destinations(
                args.repo_root, args.output_dir, args.private_symbols_dir
            )
            _print_destinations(output, private_symbols)
        elif args.command == "preflight":
            output, private_symbols = preflight_destinations(
                args.repo_root, args.output_dir, args.private_symbols_dir
            )
            _print_destinations(output, private_symbols)
        elif args.command == "seal":
            marker = seal_candidate(
                args.candidate_dir, args.platform, args.source_commit
            )
            print(marker)
        elif args.command == "require-unsealed":
            require_unsealed(args.candidate_dir)
        elif args.command == "publish":
            publish(
                args.artifact_source_dir,
                args.output_dir,
                args.private_symbols_source_dir,
                args.private_symbols_dir,
                args.platform,
                args.source_commit,
            )
        elif args.command == "verify-complete":
            verify_candidate(
                args.candidate_dir,
                args.platform,
                args.source_commit,
                allow_extra=args.allow_extra,
            )
        elif args.command == "consume-complete":
            remainder = args.remainder
            if remainder[:1] == ["--"]:
                remainder = remainder[1:]
            return consume_complete_candidate(
                args.candidate_dir,
                args.snapshot_root,
                args.platform,
                args.source_commit,
                remainder,
                allow_extra=args.allow_extra,
            )
        elif args.command == "run-complete":
            remainder = args.remainder
            if remainder[:1] == ["--"]:
                remainder = remainder[1:]
            return run_complete_candidate(
                args.candidate_dir,
                args.platform,
                args.source_commit,
                remainder,
            )
        else:
            cleanup_task_root(
                args.work_root,
                args.task_root,
                args.expected_device,
                args.expected_inode,
            )
    except (PublicationError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
