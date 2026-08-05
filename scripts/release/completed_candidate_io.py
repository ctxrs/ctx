"""Descriptor primitives shared by completed release candidate operations."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import stat
from typing import Callable, Mapping, NamedTuple, Sequence


class CandidateIOError(ValueError):
    pass


class DescriptorBinding(NamedTuple):
    device: int
    inode: int
    mount_id: int
    mode: int
    size: int
    mtime_ns: int
    ctime_ns: int


DIRECTORY_FLAGS = os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
FILE_FLAGS = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW


def mount_id(descriptor: int) -> int:
    try:
        with open(f"/proc/self/fdinfo/{descriptor}", encoding="ascii") as source:
            for line in source:
                if line.startswith("mnt_id:"):
                    value = line.partition(":")[2].strip()
                    if value.isdecimal():
                        return int(value)
    except OSError as error:
        raise CandidateIOError(
            "Linux /proc fdinfo mount IDs are required for release descriptor safety"
        ) from error
    raise CandidateIOError(
        "Linux /proc fdinfo did not report a release descriptor mount ID"
    )


def descriptor_binding(descriptor: int) -> DescriptorBinding:
    metadata = os.fstat(descriptor)
    return DescriptorBinding(
        device=metadata.st_dev,
        inode=metadata.st_ino,
        mount_id=mount_id(descriptor),
        mode=metadata.st_mode,
        size=metadata.st_size,
        mtime_ns=metadata.st_mtime_ns,
        ctime_ns=metadata.st_ctime_ns,
    )


def _directory_names(descriptor: int) -> list[str]:
    with os.scandir(descriptor) as entries:
        return sorted(entry.name for entry in entries)


def _release_leaf(name: str) -> None:
    if not name or name in {".", ".."} or Path(name).name != name:
        raise CandidateIOError(f"invalid release output leaf: {name!r}")


def _sha256_descriptor(descriptor: int) -> tuple[str, int]:
    os.lseek(descriptor, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    size = 0
    while True:
        chunk = os.read(descriptor, 1024 * 1024)
        if not chunk:
            break
        digest.update(chunk)
        size += len(chunk)
    os.lseek(descriptor, 0, os.SEEK_SET)
    return digest.hexdigest(), size


def _write_all(descriptor: int, value: bytes) -> None:
    offset = 0
    while offset < len(value):
        written = os.write(descriptor, value[offset:])
        if written == 0:
            raise OSError("short completed candidate copy write")
        offset += written


def copy_regular_descriptor(
    source_descriptor: int,
    destination_parent: int,
    name: str,
    binding: DescriptorBinding,
    digest: str,
    size: int,
) -> None:
    destination_descriptor = -1
    try:
        if descriptor_binding(source_descriptor) != binding:
            raise CandidateIOError(f"release source changed before copy: {name}")
        if not stat.S_ISREG(binding.mode):
            raise CandidateIOError(f"release source is not a regular file: {name}")
        destination_descriptor = os.open(
            name,
            os.O_WRONLY
            | os.O_CLOEXEC
            | os.O_CREAT
            | os.O_EXCL
            | os.O_NOFOLLOW,
            stat.S_IMODE(binding.mode),
            dir_fd=destination_parent,
        )
        os.lseek(source_descriptor, 0, os.SEEK_SET)
        copied_digest = hashlib.sha256()
        copied_size = 0
        while True:
            chunk = os.read(source_descriptor, 1024 * 1024)
            if not chunk:
                break
            _write_all(destination_descriptor, chunk)
            copied_digest.update(chunk)
            copied_size += len(chunk)
        os.lseek(source_descriptor, 0, os.SEEK_SET)
        if descriptor_binding(source_descriptor) != binding:
            raise CandidateIOError(f"release source changed while copied: {name}")
        if copied_digest.hexdigest() != digest or copied_size != size:
            raise CandidateIOError(
                f"release source content changed while copied: {name}"
            )
        os.fchmod(destination_descriptor, stat.S_IMODE(binding.mode))
        os.fsync(destination_descriptor)
    finally:
        if destination_descriptor >= 0:
            os.close(destination_descriptor)


class PinnedTreeSnapshot:
    """A recursively pinned regular-file tree copied only from held FDs."""

    def __init__(
        self,
        descriptor: int,
        source: Path | None,
        *,
        owns_descriptor: bool,
        verify_root: Callable[[Path, int, str], None] | None = None,
    ) -> None:
        self.descriptor = descriptor
        self.source = source
        self.owns_descriptor = owns_descriptor
        self.verify_root = verify_root
        self.root_binding = descriptor_binding(descriptor)
        self.names = tuple(_directory_names(descriptor))
        self.files: dict[
            str, tuple[int, DescriptorBinding, str, int]
        ] = {}
        self.directories: dict[str, PinnedTreeSnapshot] = {}
        try:
            for name in self.names:
                _release_leaf(name)
                entry = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
                if stat.S_ISREG(entry.st_mode):
                    leaf = os.open(name, FILE_FLAGS, dir_fd=descriptor)
                    try:
                        binding = descriptor_binding(leaf)
                        digest, size = _sha256_descriptor(leaf)
                    except BaseException:
                        os.close(leaf)
                        raise
                    self.files[name] = (leaf, binding, digest, size)
                elif stat.S_ISDIR(entry.st_mode):
                    child = os.open(name, DIRECTORY_FLAGS, dir_fd=descriptor)
                    self.directories[name] = PinnedTreeSnapshot(
                        child, None, owns_descriptor=True
                    )
                else:
                    raise CandidateIOError(
                        f"private symbol source contains a link or special file: {name}"
                    )
            self.revalidate()
        except BaseException:
            self.close()
            raise

    def revalidate(self) -> None:
        if tuple(_directory_names(self.descriptor)) != self.names:
            raise CandidateIOError("private symbol source names changed")
        for name, (descriptor, binding, _, _) in self.files.items():
            named = os.stat(name, dir_fd=self.descriptor, follow_symlinks=False)
            if (
                descriptor_binding(descriptor) != binding
                or not stat.S_ISREG(named.st_mode)
                or (named.st_dev, named.st_ino) != (binding.device, binding.inode)
            ):
                raise CandidateIOError(
                    f"private symbol source leaf changed while pinned: {name}"
                )
        for name, child in self.directories.items():
            named = os.stat(name, dir_fd=self.descriptor, follow_symlinks=False)
            if (
                not stat.S_ISDIR(named.st_mode)
                or (named.st_dev, named.st_ino)
                != (child.root_binding.device, child.root_binding.inode)
            ):
                raise CandidateIOError(
                    f"private symbol source directory changed while pinned: {name}"
                )
            child.revalidate()
        if descriptor_binding(self.descriptor) != self.root_binding:
            raise CandidateIOError("private symbol source changed while pinned")
        if self.source is not None and self.verify_root is not None:
            self.verify_root(self.source, self.descriptor, "private symbol source")

    def copy_to(self, destination_descriptor: int) -> None:
        for name in self.names:
            if name in self.files:
                descriptor, binding, digest, size = self.files[name]
                copy_regular_descriptor(
                    descriptor, destination_descriptor, name, binding, digest, size
                )
                continue
            child = self.directories[name]
            os.mkdir(name, 0o700, dir_fd=destination_descriptor)
            destination_child = os.open(
                name, DIRECTORY_FLAGS, dir_fd=destination_descriptor
            )
            try:
                child.copy_to(destination_child)
                os.fsync(destination_child)
            finally:
                os.close(destination_child)
        self.revalidate()

    def close(self) -> None:
        for descriptor, _, _, _ in self.files.values():
            try:
                os.close(descriptor)
            except OSError:
                pass
        self.files.clear()
        for child in self.directories.values():
            child.close()
        self.directories.clear()
        if self.owns_descriptor and self.descriptor >= 0:
            try:
                os.close(self.descriptor)
            except OSError:
                pass
            self.descriptor = -1


def expand_command(
    candidate: str,
    ctx: str | None,
    leaves: Mapping[str, str],
    command: Sequence[str],
) -> list[str]:
    if not command:
        raise CandidateIOError("completed release command is empty")
    replacements = {"{candidate}": candidate}
    if ctx is not None:
        replacements["{ctx}"] = ctx
    replacements.update(
        (f"{{leaf:{name}}}", argument) for name, argument in leaves.items()
    )
    result: list[str] = []
    for value in command:
        expanded = value
        for placeholder, replacement in replacements.items():
            expanded = re.sub(
                rf"(?<!\$){re.escape(placeholder)}",
                lambda _: replacement,
                expanded,
            )
        if re.search(r"(?<!\$)\{(?:candidate\}|ctx\}|leaf:)", expanded):
            raise CandidateIOError(
                f"completed release command contains an unbound placeholder: {value}"
            )
        result.append(expanded)
    return result
