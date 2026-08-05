"""One-shot inherited capabilities for completed release candidate workers."""

from __future__ import annotations

import json
import os
import secrets
import stat
from typing import Mapping, NamedTuple, Sequence


ADMISSION_KIND = "ctx-completed-candidate-consumer-admission"
ADMISSION_SCHEMA_VERSION = 1
ADMISSION_CONSUMERS = {"github", "semantic"}
MAX_ADMISSION_BYTES = 4096


class AdmissionError(ValueError):
    pass


class DescriptorBinding(NamedTuple):
    device: int
    inode: int
    mount_id: int
    mode: int
    size: int
    mtime_ns: int
    ctime_ns: int


def _valid_commit(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 40
        and value != "0" * 40
        and all(character in "0123456789abcdef" for character in value)
    )


def mount_id(descriptor: int) -> int:
    try:
        with open(f"/proc/self/fdinfo/{descriptor}", encoding="ascii") as source:
            for line in source:
                if line.startswith("mnt_id:"):
                    value = line.partition(":")[2].strip()
                    if value.isdecimal():
                        return int(value)
    except OSError as error:
        raise AdmissionError(
            "Linux /proc fdinfo mount IDs are required for release descriptor safety"
        ) from error
    raise AdmissionError(
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


def _write_all(descriptor: int, value: bytes) -> None:
    offset = 0
    while offset < len(value):
        written = os.write(descriptor, value[offset:])
        if written == 0:
            raise OSError("short completed candidate admission write")
        offset += written


def issue_admission(
    candidate_descriptor: int,
    root_binding: Sequence[int],
    consumer: str,
    source_commit: str,
) -> int:
    if consumer not in ADMISSION_CONSUMERS or not _valid_commit(source_commit):
        raise AdmissionError("completed candidate admission identity is invalid")
    read_descriptor, write_descriptor = os.pipe2(os.O_CLOEXEC)
    try:
        payload = {
            "candidate_fd": candidate_descriptor,
            "consumer": consumer,
            "kind": ADMISSION_KIND,
            "nonce": secrets.token_hex(32),
            "root_binding": list(root_binding),
            "schema_version": ADMISSION_SCHEMA_VERSION,
            "source_commit": source_commit,
        }
        encoded = (
            json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n"
        ).encode("ascii")
        if len(encoded) > MAX_ADMISSION_BYTES:
            raise AdmissionError("completed candidate admission is too large")
        _write_all(write_descriptor, encoded)
    except BaseException:
        os.close(read_descriptor)
        raise
    finally:
        os.close(write_descriptor)
    return read_descriptor


def claim_admission(
    admission_descriptor: int,
    candidate: str,
    consumer: str,
) -> str:
    if admission_descriptor < 3 or consumer not in ADMISSION_CONSUMERS:
        raise AdmissionError("completed candidate admission is invalid")
    try:
        admission_metadata = os.fstat(admission_descriptor)
    except OSError as error:
        raise AdmissionError(
            "completed candidate admission descriptor was not inherited"
        ) from error
    if not stat.S_ISFIFO(admission_metadata.st_mode):
        raise AdmissionError(
            "completed candidate admission is not an inherited pipe capability"
        )
    os.set_blocking(admission_descriptor, False)
    chunks: list[bytes] = []
    remaining = MAX_ADMISSION_BYTES + 1
    try:
        while remaining:
            chunk = os.read(admission_descriptor, min(remaining, 1024))
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
    except BlockingIOError as error:
        raise AdmissionError(
            "completed candidate admission capability is incomplete"
        ) from error
    encoded = b"".join(chunks)
    if not encoded or len(encoded) > MAX_ADMISSION_BYTES:
        raise AdmissionError("completed candidate admission capability is invalid")
    try:
        payload = json.loads(encoded)
    except (json.JSONDecodeError, UnicodeDecodeError) as error:
        raise AdmissionError(
            "completed candidate admission capability is invalid"
        ) from error
    required = {
        "candidate_fd",
        "consumer",
        "kind",
        "nonce",
        "root_binding",
        "schema_version",
        "source_commit",
    }
    if not isinstance(payload, dict) or set(payload) != required:
        raise AdmissionError("completed candidate admission capability is invalid")
    candidate_descriptor = payload.get("candidate_fd")
    source_commit = payload.get("source_commit")
    nonce = payload.get("nonce")
    binding_values = payload.get("root_binding")
    if (
        payload.get("kind") != ADMISSION_KIND
        or payload.get("schema_version") != ADMISSION_SCHEMA_VERSION
        or payload.get("consumer") != consumer
        or not isinstance(candidate_descriptor, int)
        or candidate_descriptor < 3
        or candidate_descriptor == admission_descriptor
        or candidate != f"/proc/self/fd/{candidate_descriptor}"
        or not _valid_commit(source_commit)
        or not isinstance(nonce, str)
        or len(nonce) != 64
        or any(character not in "0123456789abcdef" for character in nonce)
        or not isinstance(binding_values, list)
        or len(binding_values) != 7
        or any(not isinstance(value, int) for value in binding_values)
    ):
        raise AdmissionError("completed candidate admission capability is invalid")
    try:
        binding = descriptor_binding(candidate_descriptor)
    except OSError as error:
        raise AdmissionError(
            "completed candidate root descriptor was not inherited"
        ) from error
    if not stat.S_ISDIR(binding.mode) or binding != DescriptorBinding(*binding_values):
        raise AdmissionError(
            "completed candidate admission does not match its inherited root"
        )
    assert isinstance(source_commit, str)
    return source_commit


def expand_command(
    candidate: str,
    ctx: str | None,
    leaves: Mapping[str, str],
    command: Sequence[str],
    admission_descriptor: int | None,
) -> list[str]:
    if not command:
        raise AdmissionError("completed release command is empty")
    replacements = {"{candidate}": candidate}
    if ctx is not None:
        replacements["{ctx}"] = ctx
    if admission_descriptor is not None:
        replacements["{admission-fd}"] = str(admission_descriptor)
    replacements.update(
        (f"{{leaf:{name}}}", argument) for name, argument in leaves.items()
    )
    result: list[str] = []
    for value in command:
        expanded = value
        for placeholder, replacement in replacements.items():
            expanded = expanded.replace(placeholder, replacement)
        if any(
            placeholder in expanded
            for placeholder in ("{candidate}", "{ctx}", "{leaf:", "{admission-fd}")
        ):
            raise AdmissionError(
                f"completed release command contains an unbound placeholder: {value}"
            )
        result.append(expanded)
    return result
