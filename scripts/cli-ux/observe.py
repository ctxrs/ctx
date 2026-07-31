#!/usr/bin/env python3
"""Observe one command through an isolated, fixed-geometry Linux PTY.

This module records process facts. It deliberately does not load a scenario
catalog or decide whether an observation is authoritative for one.
"""

from __future__ import annotations

import argparse
import base64
import codecs
import ctypes
import errno
import fcntl
import hashlib
import json
import os
import platform
import pty
import re
import select
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import termios
import time
import uuid
from pathlib import Path
from typing import Any


SCHEMA_VERSION = 1
DEFAULT_TIMEOUT_MS = 10_000
TERM_GRACE_SECONDS = 0.15
KILL_GRACE_SECONDS = 1.0
READ_SIZE = 65_536

PROFILE_NO_SOCKET = "no-socket"
PROFILE_LOCAL_UNIX = "local-unix"
ISOLATION_PROFILES = (PROFILE_NO_SOCKET, PROFILE_LOCAL_UNIX)

_CAPTURE_ID_ENV = "CTX_CLI_UX_CAPTURE_ID"
_CAPTURE_ROOT_TOKEN = "${CAPTURE_ROOT}"
_WORKSPACE_TOKEN = "${WORKSPACE}"
_CAPTURE_ID_TOKEN = "${CAPTURE_ID}"
_OVERRIDE_NAME = re.compile(
    r"^(?:CTX_[A-Z0-9_]+|RUST_LOG|RUST_BACKTRACE|NO_COLOR|FORCE_COLOR)$"
)

# Linux seccomp constants. The classic-BPF filter checks the socket domain
# argument: local-unix admits AF_UNIX only, while no-socket admits no sockets.
_PR_SET_PDEATHSIG = 1
_PR_SET_NO_NEW_PRIVS = 38
_PR_SET_SECCOMP = 22
_PR_SET_CHILD_SUBREAPER = 36
_SECCOMP_MODE_FILTER = 2
_SECCOMP_RET_KILL_PROCESS = 0x80000000
_SECCOMP_RET_ERRNO = 0x00050000
_SECCOMP_RET_ALLOW = 0x7FFF0000
_BPF_LD_W_ABS = 0x20
_BPF_JMP_JEQ_K = 0x15
_BPF_RET_K = 0x06
_SECCOMP_DATA_NR_OFFSET = 0
_SECCOMP_DATA_ARCH_OFFSET = 4
_SECCOMP_DATA_ARGS_OFFSET = 16
_EPERM = 1
_AF_UNIX = 1
_LINUX_SECCOMP_ARCHES = {
    "aarch64": (0xC00000B7, 198, 199, 425),
    "arm64": (0xC00000B7, 198, 199, 425),
    "x86_64": (0xC000003E, 41, 53, 425),
    "amd64": (0xC000003E, 41, 53, 425),
}


class ObservationError(RuntimeError):
    """The runner could not produce an admissible low-level observation."""


class _SockFilter(ctypes.Structure):
    _fields_ = [
        ("code", ctypes.c_ushort),
        ("jt", ctypes.c_ubyte),
        ("jf", ctypes.c_ubyte),
        ("k", ctypes.c_uint32),
    ]


class _SockFprog(ctypes.Structure):
    _fields_ = [
        ("length", ctypes.c_ushort),
        ("filter", ctypes.POINTER(_SockFilter)),
    ]


def isolation_supported() -> bool:
    """Return whether this host can enforce the Linux observation contract."""
    return (
        sys.platform.startswith("linux")
        and platform.machine().lower() in _LINUX_SECCOMP_ARCHES
        and hasattr(termios, "TIOCSCTTY")
        and hasattr(termios, "TIOCSWINSZ")
        and hasattr(pty, "openpty")
    )


def _libc() -> ctypes.CDLL:
    return ctypes.CDLL(None, use_errno=True)


def _prctl(option: int, argument: int) -> None:
    if _libc().prctl(option, argument, 0, 0, 0) != 0:
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value))


def _install_socket_filter(profile: str) -> None:
    machine = platform.machine().lower()
    try:
        audit_arch, socket_nr, socketpair_nr, io_uring_setup_nr = (
            _LINUX_SECCOMP_ARCHES[machine]
        )
    except KeyError as error:
        raise OSError(
            errno.ENOTSUP, f"unsupported Linux architecture {machine}"
        ) from error

    instructions = [
        _SockFilter(_BPF_LD_W_ABS, 0, 0, _SECCOMP_DATA_ARCH_OFFSET),
        _SockFilter(_BPF_JMP_JEQ_K, 1, 0, audit_arch),
        _SockFilter(_BPF_RET_K, 0, 0, _SECCOMP_RET_KILL_PROCESS),
        _SockFilter(_BPF_LD_W_ABS, 0, 0, _SECCOMP_DATA_NR_OFFSET),
    ]
    if profile == PROFILE_NO_SOCKET:
        for syscall_number in (socket_nr, socketpair_nr, io_uring_setup_nr):
            instructions.extend(
                [
                    _SockFilter(
                        _BPF_JMP_JEQ_K, 0, 1, syscall_number
                    ),
                    _SockFilter(
                        _BPF_RET_K, 0, 0, _SECCOMP_RET_ERRNO | _EPERM
                    ),
                ]
            )
    elif profile == PROFILE_LOCAL_UNIX:
        # If this is socket(2), inspect domain (args[0]); otherwise jump over
        # the domain block to the socketpair check.
        instructions.extend(
            [
                _SockFilter(_BPF_JMP_JEQ_K, 0, 4, socket_nr),
                _SockFilter(
                    _BPF_LD_W_ABS, 0, 0, _SECCOMP_DATA_ARGS_OFFSET
                ),
                _SockFilter(_BPF_JMP_JEQ_K, 0, 1, _AF_UNIX),
                _SockFilter(_BPF_RET_K, 0, 0, _SECCOMP_RET_ALLOW),
                _SockFilter(
                    _BPF_RET_K, 0, 0, _SECCOMP_RET_ERRNO | _EPERM
                ),
                _SockFilter(_BPF_JMP_JEQ_K, 0, 4, socketpair_nr),
                _SockFilter(
                    _BPF_LD_W_ABS, 0, 0, _SECCOMP_DATA_ARGS_OFFSET
                ),
                _SockFilter(_BPF_JMP_JEQ_K, 0, 1, _AF_UNIX),
                _SockFilter(_BPF_RET_K, 0, 0, _SECCOMP_RET_ALLOW),
                _SockFilter(
                    _BPF_RET_K, 0, 0, _SECCOMP_RET_ERRNO | _EPERM
                ),
                # A process without an io_uring instance cannot bypass the
                # socket-domain checks with IORING_OP_SOCKET.
                _SockFilter(
                    _BPF_JMP_JEQ_K, 0, 1, io_uring_setup_nr
                ),
                _SockFilter(
                    _BPF_RET_K, 0, 0, _SECCOMP_RET_ERRNO | _EPERM
                ),
            ]
        )
    else:
        raise OSError(errno.EINVAL, f"unknown isolation profile {profile}")
    instructions.append(
        _SockFilter(_BPF_RET_K, 0, 0, _SECCOMP_RET_ALLOW)
    )

    instruction_array = _SockFilter * len(instructions)
    instruction_buffer = instruction_array(*instructions)
    program = _SockFprog(len(instruction_buffer), instruction_buffer)
    _prctl(_PR_SET_NO_NEW_PRIVS, 1)
    libc = _libc()
    if (
        libc.prctl(
            _PR_SET_SECCOMP,
            _SECCOMP_MODE_FILTER,
            ctypes.byref(program),
            0,
            0,
        )
        != 0
    ):
        value = ctypes.get_errno()
        raise OSError(value, os.strerror(value))


def _child_setup(profile: str) -> None:
    _prctl(_PR_SET_PDEATHSIG, signal.SIGKILL)
    fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    _install_socket_filter(profile)


def _canonical_json_sha256(value: Any) -> str:
    encoded = json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _file_sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _hashed_bytes(
    data: bytes,
    *,
    include_text: bool = False,
    byte_count_name: str = "bytes",
) -> dict[str, Any]:
    result: dict[str, Any] = {
        byte_count_name: len(data),
        "sha256": hashlib.sha256(data).hexdigest(),
    }
    if include_text:
        result.update(
            {
                "encoding": "utf-8",
                "text": data.decode("utf-8"),
                "base64": base64.b64encode(data).decode("ascii"),
            }
        )
    return result


def _resolve_executable(argv: list[str]) -> tuple[list[str], Path]:
    if not argv or any(not isinstance(item, str) or not item for item in argv):
        raise ObservationError(
            "argv must contain at least one non-empty string"
        )
    if any("\x00" in item for item in argv):
        raise ObservationError("argv must not contain NUL")
    requested = Path(argv[0])
    if not requested.is_absolute():
        raise ObservationError("argv[0] must be an absolute executable path")
    try:
        executable = requested.resolve(strict=True)
    except OSError as error:
        raise ObservationError(
            f"cannot resolve executable {requested}: {error}"
        ) from error
    metadata = executable.stat()
    if not stat.S_ISREG(metadata.st_mode) or not os.access(executable, os.X_OK):
        raise ObservationError(
            f"executable is not an executable regular file: {executable}"
        )
    if metadata.st_uid not in {0, os.getuid()}:
        raise ObservationError(
            f"executable is not owned by root or the current user: {executable}"
        )
    if metadata.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise ObservationError(
            f"executable is group- or world-writable: {executable}"
        )
    executed = list(argv)
    executed[0] = str(executable)
    return executed, executable


def _validate_override_value(value: str) -> None:
    if "\x00" in value:
        raise ObservationError("environment values must not contain NUL")
    if value == "~" or "~/" in value:
        raise ObservationError("environment values must not use ambient ~")
    if re.search(r"(?:^|/)\.ctx(?:/|$)", value):
        raise ObservationError(
            "environment values must not reference an ambient .ctx root"
        )
    ambient_home = str(Path.home().resolve())
    if ambient_home in value:
        raise ObservationError(
            "environment values must not reference ambient HOME"
        )


def _environment(
    root: Path,
    capture_id: str,
    columns: int,
    rows: int,
    profile: str,
    overrides: dict[str, str],
) -> tuple[dict[str, str], dict[str, str]]:
    for name, value in overrides.items():
        if not _OVERRIDE_NAME.fullmatch(name):
            raise ObservationError(
                f"environment name is not explicitly allowlisted: {name}"
            )
        _validate_override_value(value)

    directory_tokens = {
        "HOME": "home",
        "XDG_CACHE_HOME": "cache",
        "XDG_CONFIG_HOME": "config",
        "XDG_DATA_HOME": "data",
        "XDG_RUNTIME_DIR": "runtime",
        "XDG_STATE_HOME": "state",
        "CTX_DATA_ROOT": "ctx-data",
        "TMPDIR": "tmp",
        "TMP": "tmp",
        "TEMP": "tmp",
    }
    for child in sorted(set(directory_tokens.values()) | {"workspace"}):
        (root / child).mkdir(mode=0o700)

    recorded = {
        name: f"{_CAPTURE_ROOT_TOKEN}/{child}"
        for name, child in directory_tokens.items()
    }
    recorded.update(
        {
            _CAPTURE_ID_ENV: _CAPTURE_ID_TOKEN,
            "COLORTERM": "truecolor",
            "COLUMNS": str(columns),
            "CTX_ANALYTICS_ENABLED": "false",
            "CTX_LOCAL_USAGE_ENABLED": "false",
            "CTX_UPGRADE_AUTO": "off",
            "LANG": "C.UTF-8",
            "LC_ALL": "C.UTF-8",
            "LINES": str(rows),
            "NO_PROXY": "*",
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "TERM": "xterm-256color",
            "TZ": "UTC",
            "http_proxy": "",
            "https_proxy": "",
            "no_proxy": "*",
        }
    )
    if profile == PROFILE_NO_SOCKET:
        recorded["CTX_DAEMON_AUTOSTART_OFF"] = "1"

    for name, value in overrides.items():
        if name in recorded:
            raise ObservationError(
                f"environment name is reserved by the observer: {name}"
            )
        recorded[name] = value

    replacements = {
        _CAPTURE_ROOT_TOKEN: str(root),
        _WORKSPACE_TOKEN: str(root / "workspace"),
        _CAPTURE_ID_TOKEN: capture_id,
    }
    actual: dict[str, str] = {}
    for name, value in recorded.items():
        expanded = value
        for token, replacement in replacements.items():
            expanded = expanded.replace(token, replacement)
        actual[name] = expanded
    return actual, dict(sorted(recorded.items()))


def plain_projection(text: str) -> str:
    """Strip terminal escape sequences and normalize PTY newlines.

    The projection is intentionally a stream projection, not a terminal
    emulator. Raw bytes remain authoritative for exact terminal playback.
    """
    output: list[str] = []
    index = 0
    length = len(text)
    while index < length:
        character = text[index]
        if character == "\x1b":
            if index + 1 >= length:
                raise ObservationError("PTY output ends with an incomplete ESC")
            introducer = text[index + 1]
            if introducer == "[":
                cursor = index + 2
                while cursor < length and not "@" <= text[cursor] <= "~":
                    cursor += 1
                if cursor >= length:
                    raise ObservationError(
                        "PTY output contains an incomplete CSI sequence"
                    )
                index = cursor + 1
                continue
            if introducer == "]":
                cursor = index + 2
                while cursor < length:
                    if text[cursor] == "\x07":
                        index = cursor + 1
                        break
                    if (
                        text[cursor] == "\x1b"
                        and cursor + 1 < length
                        and text[cursor + 1] == "\\"
                    ):
                        index = cursor + 2
                        break
                    cursor += 1
                else:
                    raise ObservationError(
                        "PTY output contains an incomplete OSC sequence"
                    )
                continue
            if introducer in "P^_X":
                cursor = index + 2
                while cursor + 1 < length:
                    if text[cursor] == "\x1b" and text[cursor + 1] == "\\":
                        index = cursor + 2
                        break
                    cursor += 1
                else:
                    raise ObservationError(
                        "PTY output contains an incomplete string control"
                    )
                continue
            if "@" <= introducer <= "_":
                index += 2
                continue
            raise ObservationError(
                f"PTY output contains unsupported ESC byte 0x{ord(introducer):02x}"
            )
        if character == "\r":
            if index + 1 < length and text[index + 1] == "\n":
                index += 2
            else:
                index += 1
            output.append("\n")
            continue
        if character in {"\n", "\t"}:
            output.append(character)
            index += 1
            continue
        codepoint = ord(character)
        if codepoint == 0x07:
            index += 1
            continue
        if codepoint < 0x20 or codepoint == 0x7F:
            raise ObservationError(
                f"PTY output contains unsupported control byte 0x{codepoint:02x}"
            )
        output.append(character)
        index += 1
    return "".join(output)


def _termination(
    returncode: int, timed_out: bool, timeout_ms: int
) -> dict[str, Any]:
    if timed_out:
        result: dict[str, Any] = {
            "kind": "timeout",
            "timeout_ms": timeout_ms,
        }
        if returncode < 0:
            number = -returncode
            result.update(
                {
                    "signal": number,
                    "signal_name": _signal_name(number),
                }
            )
        else:
            result["exit_code"] = returncode
        return result
    if returncode < 0:
        number = -returncode
        return {
            "kind": "signal",
            "signal": number,
            "signal_name": _signal_name(number),
        }
    return {"kind": "exit", "exit_code": returncode}


def _signal_name(number: int) -> str:
    try:
        return signal.Signals(number).name
    except ValueError:
        return f"SIG{number}"


def _signal_process_group(process: subprocess.Popen[bytes], number: int) -> None:
    try:
        os.killpg(process.pid, number)
    except ProcessLookupError:
        pass


def _process_identity(pid: int) -> tuple[int, int] | None:
    try:
        stat_line = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
    except (FileNotFoundError, PermissionError, ProcessLookupError):
        return None
    close = stat_line.rfind(")")
    if close < 0:
        return None
    fields = stat_line[close + 2 :].split()
    if len(fields) < 4:
        return None
    return int(fields[1]), int(fields[3])


def _process_diagnostic(pid: int) -> str:
    try:
        stat_line = Path(f"/proc/{pid}/stat").read_text(encoding="ascii")
        close = stat_line.rfind(")")
        fields = stat_line[close + 2 :].split() if close >= 0 else []
        state = fields[0] if fields else "?"
        parent = fields[1] if len(fields) > 1 else "?"
        session = fields[3] if len(fields) > 3 else "?"
        command = (
            Path(f"/proc/{pid}/cmdline")
            .read_bytes()
            .replace(b"\0", b" ")
            .decode("utf-8", errors="replace")
            .strip()
        )
        cwd = os.readlink(f"/proc/{pid}/cwd")
        return (
            f"{pid} state={state} ppid={parent} session={session} "
            f"cwd={cwd} argv={command[:512]}"
        )
    except (FileNotFoundError, PermissionError, ProcessLookupError, OSError):
        return str(pid)


def _direct_child_pids() -> set[int]:
    parent = os.getpid()
    children_path = Path(f"/proc/{parent}/task/{parent}/children")
    try:
        return {
            int(value)
            for value in children_path.read_text(encoding="ascii").split()
        }
    except (FileNotFoundError, PermissionError, ProcessLookupError, ValueError):
        pass
    children: set[int] = set()
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        identity = _process_identity(int(entry.name))
        if identity is not None and identity[0] == parent:
            children.add(int(entry.name))
    return children


def _pids_in_capture_scope(
    session_id: int | None, baseline_children: set[int]
) -> list[int]:
    parent = os.getpid()
    new_children = _direct_child_pids() - baseline_children
    session_group_exists = False
    if session_id is not None:
        try:
            os.killpg(session_id, 0)
            session_group_exists = True
        except ProcessLookupError:
            pass
        except PermissionError:
            session_group_exists = True
    if not new_children and not session_group_exists:
        return []

    matches: set[int] = set(new_children)
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        pid = int(entry.name)
        if pid == parent:
            continue
        identity = _process_identity(pid)
        if identity is None:
            continue
        process_parent, process_session = identity
        if session_id is not None and process_session == session_id:
            matches.add(pid)
        if process_parent == parent and pid not in baseline_children:
            matches.add(pid)
    return sorted(matches)


def _reap_pid(pid: int) -> None:
    try:
        while True:
            waited, _ = os.waitpid(pid, os.WNOHANG)
            if waited == 0:
                return
            if waited == pid:
                return
    except ChildProcessError:
        return


def _reap_terminated_children(baseline_children: set[int]) -> None:
    for pid in _direct_child_pids() - baseline_children:
        _reap_pid(pid)


def _settle_capture_scope(
    session_id: int | None, baseline_children: set[int]
) -> list[int]:
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    while True:
        _reap_terminated_children(baseline_children)
        scoped_pids = _pids_in_capture_scope(session_id, baseline_children)
        if not scoped_pids or time.monotonic() >= deadline:
            return scoped_pids
        time.sleep(0.01)


def _terminate_pids(pids: list[int]) -> list[int]:
    for pid in pids:
        try:
            os.kill(pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + TERM_GRACE_SECONDS
    remaining = list(pids)
    while remaining and time.monotonic() < deadline:
        time.sleep(0.01)
        for pid in remaining:
            _reap_pid(pid)
        remaining = [pid for pid in remaining if Path(f"/proc/{pid}").exists()]
    for pid in remaining:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
    deadline = time.monotonic() + KILL_GRACE_SECONDS
    while remaining and time.monotonic() < deadline:
        time.sleep(0.01)
        for pid in remaining:
            _reap_pid(pid)
        remaining = [pid for pid in remaining if Path(f"/proc/{pid}").exists()]
    return remaining


def _run_pty(
    argv: list[str],
    *,
    environment: dict[str, str],
    cwd: Path,
    columns: int,
    rows: int,
    timeout_ms: int,
    stdin_bytes: bytes,
    profile: str,
    identity: dict[str, int],
) -> tuple[bytes, int, bool]:
    master_fd, slave_fd = pty.openpty()
    process: subprocess.Popen[bytes] | None = None
    raw_chunks: list[bytes] = []
    decoder = codecs.getincrementaldecoder("utf-8")(errors="strict")
    timed_out = False
    eof = False
    term_sent_at: float | None = None
    kill_sent_at: float | None = None
    try:
        for descriptor in (master_fd, slave_fd):
            flags = fcntl.fcntl(descriptor, fcntl.F_GETFD)
            fcntl.fcntl(descriptor, fcntl.F_SETFD, flags | fcntl.FD_CLOEXEC)
        fcntl.ioctl(
            slave_fd,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", rows, columns, 0, 0),
        )
        attributes = termios.tcgetattr(slave_fd)
        attributes[3] &= ~(termios.ECHO | termios.ECHONL)
        termios.tcsetattr(slave_fd, termios.TCSANOW, attributes)
        eof_character = attributes[6][termios.VEOF]
        eof_bytes = (
            eof_character
            if isinstance(eof_character, bytes)
            else bytes([eof_character])
        )
        pending_stdin = memoryview(stdin_bytes + eof_bytes)
        os.set_blocking(master_fd, False)
        started_at = time.monotonic()
        deadline = started_at + timeout_ms / 1000
        try:
            process = subprocess.Popen(
                argv,
                cwd=cwd,
                env=environment,
                stdin=slave_fd,
                stdout=slave_fd,
                stderr=slave_fd,
                close_fds=True,
                start_new_session=True,
                preexec_fn=lambda: _child_setup(profile),
            )
        except (OSError, subprocess.SubprocessError) as error:
            raise ObservationError(
                f"could not start observation command: {error}"
            ) from error
        finally:
            os.close(slave_fd)
            slave_fd = -1
        identity["session_id"] = process.pid

        while not (eof and process.poll() is not None):
            now = time.monotonic()
            if now >= deadline and not timed_out:
                timed_out = True
                term_sent_at = now
                _signal_process_group(process, signal.SIGTERM)
            if (
                timed_out
                and term_sent_at is not None
                and now - term_sent_at >= TERM_GRACE_SECONDS
                and kill_sent_at is None
            ):
                kill_sent_at = now
                _signal_process_group(process, signal.SIGKILL)
            if (
                kill_sent_at is not None
                and now - kill_sent_at >= KILL_GRACE_SECONDS
                and process.poll() is None
            ):
                raise ObservationError(
                    "timed out process group survived SIGKILL"
                )

            wait_seconds = 0.02
            if not timed_out:
                wait_seconds = min(wait_seconds, max(0.0, deadline - now))
            readable_fds = [] if eof else [master_fd]
            writable_fds = (
                [master_fd] if pending_stdin and not eof else []
            )
            readable, writable, _ = select.select(
                readable_fds, writable_fds, [], wait_seconds
            )
            if writable:
                try:
                    written = os.write(master_fd, pending_stdin)
                    pending_stdin = pending_stdin[written:]
                except BlockingIOError:
                    pass
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    pending_stdin = memoryview(b"")
            if readable:
                try:
                    chunk = os.read(master_fd, READ_SIZE)
                except BlockingIOError:
                    chunk = None
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
                    chunk = b""
                if chunk == b"":
                    eof = True
                elif chunk:
                    raw_chunks.append(chunk)
                    try:
                        decoder.decode(chunk, final=False)
                    except UnicodeDecodeError as error:
                        raise ObservationError(
                            f"PTY output is not valid UTF-8: {error}"
                        ) from error

        try:
            decoder.decode(b"", final=True)
        except UnicodeDecodeError as error:
            raise ObservationError(
                f"PTY output is not valid UTF-8: {error}"
            ) from error
        returncode = process.poll()
        if returncode is None:
            raise ObservationError(
                "PTY reached EOF before the command terminated"
            )
        return b"".join(raw_chunks), returncode, timed_out
    finally:
        if slave_fd >= 0:
            os.close(slave_fd)
        os.close(master_fd)
        if process is not None and process.poll() is None:
            _signal_process_group(process, signal.SIGKILL)
            try:
                process.wait(timeout=KILL_GRACE_SECONDS)
            except subprocess.TimeoutExpired:
                pass


def observe(
    argv: list[str],
    *,
    columns: int,
    rows: int,
    timeout_ms: int = DEFAULT_TIMEOUT_MS,
    profile: str = PROFILE_NO_SOCKET,
    environment: dict[str, str] | None = None,
    stdin: bytes | str = b"",
    root_parent: Path | None = None,
) -> dict[str, Any]:
    """Return one exact EOF observation receipt for argv."""
    if not isolation_supported():
        raise ObservationError(
            "observation requires Linux x86_64/aarch64 seccomp and Unix PTYs"
        )
    if profile not in ISOLATION_PROFILES:
        raise ObservationError(f"unknown isolation profile: {profile}")
    if not 20 <= columns <= 500 or not 5 <= rows <= 200:
        raise ObservationError(
            "terminal dimensions must be columns 20..500 and rows 5..200"
        )
    if not 1 <= timeout_ms <= 300_000:
        raise ObservationError("timeout_ms must be in 1..300000")
    if isinstance(stdin, str):
        try:
            stdin_bytes = stdin.encode("utf-8")
        except UnicodeEncodeError as error:
            raise ObservationError(f"stdin is not valid Unicode: {error}") from error
    elif isinstance(stdin, bytes):
        stdin_bytes = stdin
    else:
        raise ObservationError("stdin must be bytes or text")

    requested_argv = list(argv)
    executed_argv, executable = _resolve_executable(requested_argv)
    executable_sha256 = _file_sha256(executable)
    runner_path = Path(__file__).resolve()
    runner_sha256 = _file_sha256(runner_path)

    if root_parent is None:
        bazel_tmp = os.environ.get("TEST_TMPDIR")
        root_parent = Path(bazel_tmp) if bazel_tmp else Path("/tmp")
    root_parent = root_parent.resolve(strict=True)
    if not root_parent.is_dir():
        raise ObservationError(f"root parent is not a directory: {root_parent}")

    _prctl(_PR_SET_CHILD_SUBREAPER, 1)
    baseline_children = _direct_child_pids()
    root = Path(
        tempfile.mkdtemp(prefix="ctx-cli-ux-observe.", dir=root_parent)
    )
    root.chmod(0o700)
    capture_id = uuid.uuid4().hex
    leaked_pids: list[int] = []
    residual_pids: list[int] = []
    process_identity: dict[str, int] = {}
    raw = b""
    returncode = 0
    timed_out = False
    recorded_environment: dict[str, str] = {}
    observation_error: BaseException | None = None
    try:
        actual_environment, recorded_environment = _environment(
            root,
            capture_id,
            columns,
            rows,
            profile,
            dict(environment or {}),
        )
        raw, returncode, timed_out = _run_pty(
            executed_argv,
            environment=actual_environment,
            cwd=root / "workspace",
            columns=columns,
            rows=rows,
            timeout_ms=timeout_ms,
            stdin_bytes=stdin_bytes,
            profile=profile,
            identity=process_identity,
        )
        leaked_pids = _settle_capture_scope(
            process_identity.get("session_id"), baseline_children
        )
        if leaked_pids:
            raise ObservationError(
                "observation command leaked descendant processes:\n  "
                + "\n  ".join(_process_diagnostic(pid) for pid in leaked_pids)
            )
        if _file_sha256(executable) != executable_sha256:
            raise ObservationError("executable changed during observation")
        if _file_sha256(runner_path) != runner_sha256:
            raise ObservationError("observer changed during observation")
    except BaseException as error:
        observation_error = error
    finally:
        cleanup_errors: list[str] = []
        cleanup_seen: set[int] = set()
        for _ in range(3):
            _reap_terminated_children(baseline_children)
            scoped_pids = _pids_in_capture_scope(
                process_identity.get("session_id"), baseline_children
            )
            if not scoped_pids:
                break
            cleanup_seen.update(scoped_pids)
            _terminate_pids(scoped_pids)
        residual_pids = _pids_in_capture_scope(
            process_identity.get("session_id"), baseline_children
        )
        leaked_pids = sorted(set(leaked_pids) | cleanup_seen)
        try:
            shutil.rmtree(root)
        except OSError as error:
            cleanup_errors.append(
                f"could not remove isolated observation root: {error}"
            )
        if root.exists():
            cleanup_errors.append(
                f"isolated observation root still exists: {root}"
            )
        if residual_pids:
            cleanup_errors.append(
                "observation descendants survived cleanup: "
                + ", ".join(str(pid) for pid in residual_pids)
            )
        if cleanup_errors:
            prior = (
                f"; prior observation error: {observation_error}"
                if observation_error is not None
                else ""
            )
            observation_error = ObservationError(
                "; ".join(cleanup_errors) + prior
            )
    if observation_error is not None:
        if isinstance(observation_error, ObservationError):
            raise observation_error
        raise observation_error

    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ObservationError(
            f"PTY output is not valid UTF-8: {error}"
        ) from error
    projection = plain_projection(text)
    projection_bytes = projection.encode("utf-8")
    termination = _termination(returncode, timed_out, timeout_ms)
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "ctx-cli-ux-observation",
        "runner": {
            "sha256": runner_sha256,
            "platform": {
                "system": platform.system().lower(),
                "machine": platform.machine().lower(),
            },
        },
        "inputs": {
            "requested_argv": requested_argv,
            "executed_argv": executed_argv,
            "argv_sha256": _canonical_json_sha256(executed_argv),
            "executable": {
                "path": str(executable),
                "sha256": executable_sha256,
            },
            "cwd": _WORKSPACE_TOKEN,
            "environment": {
                "inheritance": "clear",
                "allowlist": recorded_environment,
                "sha256": _canonical_json_sha256(recorded_environment),
            },
            "stdin": _hashed_bytes(stdin_bytes),
            "timeout_ms": timeout_ms,
            "isolation_profile": profile,
            "socket_policy": (
                "deny-all"
                if profile == PROFILE_NO_SOCKET
                else "allow-af-unix-only"
            ),
        },
        "terminal": {"columns": columns, "rows": rows},
        "observed": {
            "frames": [
                {
                    "boundary": "eof",
                    "delay_ms": 0,
                    "data": text,
                }
            ],
            "raw_stream": _hashed_bytes(
                raw,
                include_text=True,
                byte_count_name="utf8_bytes",
            ),
            "plain_projection": {
                **_hashed_bytes(
                    projection_bytes,
                    byte_count_name="utf8_bytes",
                ),
                "encoding": "utf-8",
                "normalization": "ansi-stripped-crlf-v1",
                "text": projection,
            },
            "exit_code": returncode if returncode >= 0 else None,
            "termination": termination,
        },
        "cleanup": {
            "descendant_processes_detected": len(leaked_pids),
            "descendant_processes_remaining": len(residual_pids),
            "root_removed": True,
        },
    }


def _parse_environment(values: list[str]) -> dict[str, str]:
    environment: dict[str, str] = {}
    for item in values:
        name, separator, value = item.partition("=")
        if not separator or not name:
            raise ObservationError(
                f"--env must be NAME=VALUE, received {item!r}"
            )
        if name in environment:
            raise ObservationError(f"duplicate --env name: {name}")
        environment[name] = value
    return environment


def _write_receipt(path: Path | None, receipt: dict[str, Any]) -> None:
    encoded = (
        json.dumps(receipt, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    )
    if path is None:
        sys.stdout.write(encoded)
        return
    with path.open("x", encoding="utf-8") as handle:
        handle.write(encoded)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Observe one command through an isolated fixed-geometry Linux PTY."
        )
    )
    parser.add_argument("--columns", type=int, required=True)
    parser.add_argument("--rows", type=int, required=True)
    parser.add_argument("--timeout-ms", type=int, default=DEFAULT_TIMEOUT_MS)
    parser.add_argument(
        "--profile", choices=ISOLATION_PROFILES, default=PROFILE_NO_SOCKET
    )
    parser.add_argument(
        "--env",
        action="append",
        default=[],
        help="explicit child environment entry; ambient environment is cleared",
    )
    stdin_group = parser.add_mutually_exclusive_group()
    stdin_group.add_argument("--stdin-text", default=None)
    stdin_group.add_argument("--stdin-file", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    command = list(args.command)
    if command and command[0] == "--":
        command.pop(0)
    try:
        environment = _parse_environment(args.env)
        if args.stdin_file is not None:
            stdin_bytes: bytes | str = args.stdin_file.read_bytes()
        else:
            stdin_bytes = args.stdin_text or ""
        receipt = observe(
            command,
            columns=args.columns,
            rows=args.rows,
            timeout_ms=args.timeout_ms,
            profile=args.profile,
            environment=environment,
            stdin=stdin_bytes,
        )
        _write_receipt(args.output, receipt)
    except (ObservationError, OSError) as error:
        print(f"observation failed: {error}", file=sys.stderr)
        return 1
    if receipt["observed"]["termination"]["kind"] == "timeout":
        return 124
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
