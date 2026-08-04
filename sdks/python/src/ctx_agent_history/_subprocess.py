"""Bounded, tree-owning local CLI subprocess execution."""

from __future__ import annotations

import ctypes
import os
import signal
import subprocess
import threading
import time
from typing import Any, Mapping, Optional, Sequence

from .errors import (
    CtxAgentHistoryCliError,
    CtxAgentHistoryProtocolError,
    CtxAgentHistoryTimeoutError,
)


# Match the CLI presentation ceiling so the adapter cannot reject a valid
# protocol response before the decoder sees it.
_STDOUT_CAP_BYTES = 64 * 1024 * 1024
_STDERR_CAP_BYTES = 256 * 1024
_READ_BUFFER_BYTES = 64 * 1024
_PROCESS_POLL_SECONDS = 0.01
_GRACEFUL_TERMINATION_SECONDS = 0.1
_REAP_SECONDS = 0.5
_READER_JOIN_SECONDS = 0.5

_CREATE_SUSPENDED = 0x00000004
_CREATE_NEW_PROCESS_GROUP = 0x00000200
_JOB_OBJECT_EXTENDED_LIMIT_INFORMATION = 9
_JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000


def run_local_cli(
    command: Sequence[str],
    *,
    cwd: Optional[str],
    env: Mapping[str, str],
    timeout: Optional[float],
) -> subprocess.CompletedProcess[str]:
    """Run one CLI command while owning and bounding its complete process scope."""

    argv = list(command)
    deadline = None if timeout is None else time.monotonic() + max(0.0, timeout)
    try:
        owned = _OwnedProcess.start(argv, cwd=cwd, env=env)
    except OSError as exc:
        raise CtxAgentHistoryCliError(
            "failed to execute ctx CLI",
            command=argv,
            exit_code=-1,
            stderr=str(exc),
            cause=exc,
        ) from exc

    stdout_capture = _BoundedCapture("stdout", _STDOUT_CAP_BYTES)
    stderr_capture = _BoundedCapture("stderr", _STDERR_CAP_BYTES)
    capture_io_failed = threading.Event()
    stop_reading = threading.Event()
    readers = [
        threading.Thread(
            name="ctx-python-cli-stdout",
            target=_drain_process_stream,
            args=(owned.process.stdout, stdout_capture, capture_io_failed, stop_reading),
            daemon=True,
        ),
        threading.Thread(
            name="ctx-python-cli-stderr",
            target=_drain_process_stream,
            args=(owned.process.stderr, stderr_capture, capture_io_failed, stop_reading),
            daemon=True,
        ),
    ]
    owned.register_capture(readers, stop_reading)

    failure: Optional[str] = None
    try:
        for reader in readers:
            reader.start()
        failure = _monitor_process(
            owned.process,
            readers,
            capture_io_failed,
            deadline,
        )
    except BaseException:
        owned.cleanup()
        raise

    owned.cleanup()
    overflow = stdout_capture.overflow or stderr_capture.overflow
    capture_error = stdout_capture.error or stderr_capture.error

    if overflow is not None:
        stream, cap = overflow
        raise CtxAgentHistoryProtocolError(
            "ctx CLI output exceeded its capture limit",
            details={"command": argv, "stream": stream, "cap_bytes": cap},
        )

    stdout_bytes = stdout_capture.value()
    stderr_bytes = stderr_capture.value()
    if failure == "timeout":
        cause = subprocess.TimeoutExpired(
            argv,
            timeout,
            output=stdout_bytes,
            stderr=stderr_bytes,
        )
        raise CtxAgentHistoryTimeoutError(
            "ctx CLI timed out",
            details={
                "command": argv,
                "stderr": _decode_process_output(stderr_bytes),
                "stdout": _decode_process_output(stdout_bytes),
                "timeout": timeout,
            },
            cause=cause,
        ) from cause

    live_reader = next((reader.name for reader in readers if reader.is_alive()), None)
    if failure == "capture" or capture_error is not None or live_reader is not None:
        stream, error = capture_error or (
            "pipe",
            RuntimeError(f"reader did not stop: {live_reader}"),
        )
        raise CtxAgentHistoryProtocolError(
            "ctx CLI output capture failed",
            details={"command": argv, "stream": stream},
            cause=error,
        )

    returncode = owned.process.returncode
    if returncode is None:
        returncode = -1
    if returncode != 0:
        raise CtxAgentHistoryCliError(
            "ctx CLI command failed",
            command=argv,
            exit_code=returncode,
            stderr=_decode_process_output(stderr_bytes),
            stdout=_decode_process_output(stdout_bytes),
        )

    try:
        stdout = _decode_process_output_strict(stdout_bytes)
        stderr = _decode_process_output_strict(stderr_bytes)
    except UnicodeDecodeError as exc:
        raise CtxAgentHistoryProtocolError(
            "ctx returned invalid UTF-8",
            details={"command": argv},
            cause=exc,
        ) from exc

    return subprocess.CompletedProcess(
        argv,
        returncode,
        stdout=stdout,
        stderr=stderr,
    )


def _monitor_process(
    process: subprocess.Popen[bytes],
    readers: Sequence[threading.Thread],
    capture_io_failed: threading.Event,
    deadline: Optional[float],
) -> Optional[str]:
    """Wait for process exit and both pipe EOFs under one operation deadline."""

    while True:
        if capture_io_failed.is_set():
            return "capture"
        process_exited = process.poll() is not None
        pipes_closed = all(not reader.is_alive() for reader in readers)
        if process_exited and pipes_closed:
            return None
        remaining = None if deadline is None else deadline - time.monotonic()
        if remaining is not None and remaining <= 0:
            return "timeout"
        capture_io_failed.wait(
            _PROCESS_POLL_SECONDS
            if remaining is None
            else min(_PROCESS_POLL_SECONDS, remaining)
        )


class _OwnedProcess:
    def __init__(
        self,
        process: subprocess.Popen[bytes],
        *,
        process_group: Optional[int],
        windows_job: Optional[_WindowsJob],
    ) -> None:
        self.process = process
        self.process_group = process_group
        self.windows_job = windows_job
        self.readers: Sequence[threading.Thread] = ()
        self.stop_reading: Optional[threading.Event] = None
        self._cleanup_lock = threading.Lock()
        self._cleaned = False

    @classmethod
    def start(
        cls,
        command: Sequence[str],
        *,
        cwd: Optional[str],
        env: Mapping[str, str],
    ) -> _OwnedProcess:
        windows_job: Optional[_WindowsJob] = None
        creationflags = 0
        if os.name == "nt":
            windows_job = _WindowsJob.create()
            creationflags = _CREATE_SUSPENDED | _CREATE_NEW_PROCESS_GROUP

        try:
            process = subprocess.Popen(
                list(command),
                cwd=cwd,
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                bufsize=0,
                start_new_session=os.name != "nt",
                creationflags=creationflags,
            )
        except BaseException:
            if windows_job is not None:
                windows_job.close()
            raise

        try:
            if windows_job is not None:
                windows_job.assign_and_resume(process)
        except BaseException:
            if windows_job is not None:
                windows_job.terminate(1)
                windows_job.close()
            _kill_and_reap_direct(process)
            raise

        return cls(
            process,
            process_group=process.pid if os.name != "nt" else None,
            windows_job=windows_job,
        )

    def register_capture(
        self,
        readers: Sequence[threading.Thread],
        stop_reading: threading.Event,
    ) -> None:
        self.readers = readers
        self.stop_reading = stop_reading

    def cleanup(self) -> None:
        with self._cleanup_lock:
            if self._cleaned:
                return
            self._cleaned = True

        try:
            if self.windows_job is not None:
                _terminate_windows_scope(self.process, self.windows_job)
            elif self.process_group is not None:
                _terminate_posix_scope(self.process_group)
            _reap_direct(self.process)
        finally:
            if self.windows_job is not None:
                self.windows_job.close()
            if self.stop_reading is not None:
                self.stop_reading.set()
            _join_readers(self.readers, _READER_JOIN_SECONDS)
            _close_process_pipes(self.process)
            _join_readers(self.readers, _READER_JOIN_SECONDS)


class _BoundedCapture:
    def __init__(self, stream: str, cap: int) -> None:
        self.stream = stream
        self.cap = cap
        self.data = bytearray()
        self.size = 0
        self.overflow: Optional[tuple[str, int]] = None
        self.error: Optional[tuple[str, BaseException]] = None

    def append(self, chunk: bytes) -> None:
        if self.overflow is not None:
            return
        remaining = self.cap - self.size
        retained = min(len(chunk), remaining)
        if retained:
            self.data.extend(memoryview(chunk)[:retained])
            self.size += retained
        if retained != len(chunk):
            self.overflow = (self.stream, self.cap)

    def value(self) -> bytes:
        return bytes(self.data)


def _drain_process_stream(
    stream: Optional[Any],
    capture: _BoundedCapture,
    capture_io_failed: threading.Event,
    stop_reading: threading.Event,
) -> None:
    if stream is None:
        capture.error = (capture.stream, RuntimeError("process pipe is unavailable"))
        capture_io_failed.set()
        return
    try:
        read = getattr(stream, "read1", stream.read)
        while not stop_reading.is_set():
            chunk = read(_READ_BUFFER_BYTES)
            if not chunk:
                return
            capture.append(chunk)
    except (OSError, ValueError) as exc:
        if not stop_reading.is_set():
            capture.error = (capture.stream, exc)
            capture_io_failed.set()


def _terminate_posix_scope(process_group: int) -> None:
    if not _process_group_exists(process_group):
        return
    try:
        os.killpg(process_group, signal.SIGTERM)
    except (OSError, ProcessLookupError):
        pass

    deadline = time.monotonic() + _GRACEFUL_TERMINATION_SECONDS
    while _process_group_exists(process_group) and time.monotonic() < deadline:
        time.sleep(_PROCESS_POLL_SECONDS)
    if _process_group_exists(process_group):
        try:
            os.killpg(process_group, signal.SIGKILL)
        except (OSError, ProcessLookupError):
            pass


def _terminate_windows_scope(
    process: subprocess.Popen[bytes], windows_job: _WindowsJob
) -> None:
    if process.poll() is None:
        control_break = getattr(signal, "CTRL_BREAK_EVENT", None)
        if control_break is not None:
            try:
                process.send_signal(control_break)
            except (OSError, ValueError):
                pass
        deadline = time.monotonic() + _GRACEFUL_TERMINATION_SECONDS
        while process.poll() is None and time.monotonic() < deadline:
            time.sleep(_PROCESS_POLL_SECONDS)
    windows_job.terminate(1)


def _process_group_exists(process_group: int) -> bool:
    try:
        os.killpg(process_group, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False


def _reap_direct(process: subprocess.Popen[bytes]) -> None:
    try:
        process.wait(timeout=_REAP_SECONDS)
        return
    except (OSError, subprocess.TimeoutExpired):
        pass
    _kill_and_reap_direct(process)


def _kill_and_reap_direct(process: subprocess.Popen[bytes]) -> None:
    try:
        process.kill()
    except OSError:
        pass
    try:
        process.wait(timeout=_REAP_SECONDS)
    except (OSError, subprocess.TimeoutExpired):
        pass


def _join_readers(readers: Sequence[threading.Thread], timeout: float) -> None:
    deadline = time.monotonic() + timeout
    for reader in readers:
        if reader.ident is None:
            continue
        try:
            reader.join(max(0.0, deadline - time.monotonic()))
        except RuntimeError:
            pass


def _close_process_pipes(process: subprocess.Popen[bytes]) -> None:
    for stream in (process.stdin, process.stdout, process.stderr):
        if stream is not None:
            try:
                stream.close()
            except (OSError, ValueError):
                pass


class _JobObjectBasicLimitInformation(ctypes.Structure):
    _fields_ = [
        ("PerProcessUserTimeLimit", ctypes.c_int64),
        ("PerJobUserTimeLimit", ctypes.c_int64),
        ("LimitFlags", ctypes.c_uint32),
        ("MinimumWorkingSetSize", ctypes.c_size_t),
        ("MaximumWorkingSetSize", ctypes.c_size_t),
        ("ActiveProcessLimit", ctypes.c_uint32),
        ("Affinity", ctypes.c_size_t),
        ("PriorityClass", ctypes.c_uint32),
        ("SchedulingClass", ctypes.c_uint32),
    ]


class _IoCounters(ctypes.Structure):
    _fields_ = [
        ("ReadOperationCount", ctypes.c_uint64),
        ("WriteOperationCount", ctypes.c_uint64),
        ("OtherOperationCount", ctypes.c_uint64),
        ("ReadTransferCount", ctypes.c_uint64),
        ("WriteTransferCount", ctypes.c_uint64),
        ("OtherTransferCount", ctypes.c_uint64),
    ]


class _JobObjectExtendedLimitInformation(ctypes.Structure):
    _fields_ = [
        ("BasicLimitInformation", _JobObjectBasicLimitInformation),
        ("IoInfo", _IoCounters),
        ("ProcessMemoryLimit", ctypes.c_size_t),
        ("JobMemoryLimit", ctypes.c_size_t),
        ("PeakProcessMemoryUsed", ctypes.c_size_t),
        ("PeakJobMemoryUsed", ctypes.c_size_t),
    ]


class _WindowsJob:
    """Race-free Windows process-tree owner backed by a Job Object."""

    def __init__(self, handle: int, kernel32: Any) -> None:
        self.handle = handle
        self.kernel32 = kernel32
        self.closed = False

    @classmethod
    def create(cls) -> _WindowsJob:
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)  # type: ignore[attr-defined]
        kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        kernel32.CreateJobObjectW.restype = ctypes.c_void_p
        kernel32.SetInformationJobObject.argtypes = [
            ctypes.c_void_p,
            ctypes.c_int,
            ctypes.c_void_p,
            ctypes.c_uint32,
        ]
        kernel32.SetInformationJobObject.restype = ctypes.c_int
        kernel32.AssignProcessToJobObject.argtypes = [ctypes.c_void_p, ctypes.c_void_p]
        kernel32.AssignProcessToJobObject.restype = ctypes.c_int
        kernel32.TerminateJobObject.argtypes = [ctypes.c_void_p, ctypes.c_uint32]
        kernel32.TerminateJobObject.restype = ctypes.c_int
        kernel32.CloseHandle.argtypes = [ctypes.c_void_p]
        kernel32.CloseHandle.restype = ctypes.c_int

        handle = kernel32.CreateJobObjectW(None, None)
        if not handle:
            raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined]

        limits = _JobObjectExtendedLimitInformation()
        limits.BasicLimitInformation.LimitFlags = _JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        configured = kernel32.SetInformationJobObject(
            handle,
            _JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            ctypes.byref(limits),
            ctypes.sizeof(limits),
        )
        if not configured:
            error = ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined]
            kernel32.CloseHandle(handle)
            raise error
        return cls(handle, kernel32)

    def assign_and_resume(self, process: subprocess.Popen[bytes]) -> None:
        process_handle = ctypes.c_void_p(int(getattr(process, "_handle")))
        if not self.kernel32.AssignProcessToJobObject(self.handle, process_handle):
            raise ctypes.WinError(ctypes.get_last_error())  # type: ignore[attr-defined]

        ntdll = ctypes.WinDLL("ntdll", use_last_error=True)  # type: ignore[attr-defined]
        ntdll.NtResumeProcess.argtypes = [ctypes.c_void_p]
        ntdll.NtResumeProcess.restype = ctypes.c_long
        status = ntdll.NtResumeProcess(process_handle)
        if status != 0:
            raise OSError(f"NtResumeProcess failed with NTSTATUS 0x{status & 0xFFFFFFFF:08x}")

    def terminate(self, exit_code: int) -> None:
        if not self.closed:
            self.kernel32.TerminateJobObject(self.handle, exit_code)

    def close(self) -> None:
        if not self.closed:
            self.closed = True
            self.kernel32.CloseHandle(self.handle)


def _decode_process_output(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).decode("utf-8", errors="replace")
    return str(value)


def _decode_process_output_strict(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        return bytes(value).decode("utf-8", errors="strict")
    return str(value)
