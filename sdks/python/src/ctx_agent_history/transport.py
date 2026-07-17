"""Transport implementations for agent-history-v1."""

from __future__ import annotations

import json
import os
from pathlib import Path
import signal
import subprocess
import threading
import time
from typing import Any, Mapping, Optional, Protocol, Sequence, cast

from .config import HostedConfig, LocalConfig
from .errors import (
    CtxAgentHistoryCliError,
    CtxAgentHistoryError,
    CtxAgentHistoryProtocolError,
    CtxAgentHistoryTimeoutError,
    HostedTransportNotImplementedError,
)
from .agent_history_v1 import (
    envelope,
    hosted_backend,
    local_backend,
    normalize_event,
    normalize_import,
    normalize_location,
    normalize_search,
    normalize_session,
    normalize_sources,
    normalize_status,
)
from .types import (
    ImportResponse,
    InitResponse,
    JsonObject,
    LocateEventResponse,
    LocateSessionResponse,
    SearchBackendMode,
    SearchQueryV1,
    SearchResponse,
    ShowEventResponse,
    ShowSessionResponse,
    SourcesResponse,
    StatusResponse,
    SyncResponse,
)
from .validation import serialize_search_query, validate_search_intent


class AgentHistoryTransport(Protocol):
    name: str

    def status(self) -> StatusResponse:
        ...

    def init(self, *, catalog_only: bool = False, progress: Optional[str] = None) -> InitResponse:
        ...

    def sources(self) -> SourcesResponse:
        ...

    def import_(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[str] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> ImportResponse:
        ...

    def sync(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[str] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> SyncResponse:
        ...

    def search(
        self,
        query: Optional[SearchQueryV1] = None,
        *,
        provider: Optional[str] = None,
        history_source: Optional[str] = None,
        provider_key: Optional[str] = None,
        source_id: Optional[str] = None,
        source_format: Optional[str] = None,
        workspace: Optional[str] = None,
        since: Optional[str] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        events: bool = False,
        backend: Optional[SearchBackendMode] = None,
        primary_only: bool = False,
        include_subagents: bool = False,
        limit: Optional[int] = None,
        refresh: Optional[str] = None,
        include_current_session: bool = False,
    ) -> SearchResponse:
        ...

    def show_event(
        self,
        event_id: str,
        *,
        window: Optional[int] = None,
        before: Optional[int] = None,
        after: Optional[int] = None,
    ) -> ShowEventResponse:
        ...

    def show_session(self, session_id: str, *, mode: Optional[str] = None) -> ShowSessionResponse:
        ...

    def locate_event(self, event_id: str) -> LocateEventResponse:
        ...

    def locate_session(self, session_id: str) -> LocateSessionResponse:
        ...

    def ctx_version(self) -> Optional[str]:
        ...


class LocalCliAdapter:
    """agent-history-v1 transport backed by the local ctx CLI."""

    name = "local-cli"

    def __init__(self, config: Optional[LocalConfig] = None) -> None:
        self.config = config or LocalConfig()

    def status(self) -> StatusResponse:
        raw = self._json(["status", "--json"])
        return cast(
            StatusResponse,
            envelope(
                "status",
                local_backend(self.config, raw),
                status=normalize_status(raw),
            ),
        )

    def init(self, *, catalog_only: bool = False, progress: Optional[str] = None) -> InitResponse:
        args = ["setup", "--json"]
        if catalog_only:
            args.append("--catalog-only")
        if progress is not None:
            args.extend(["--progress", progress])
        raw = self._json(args)
        return cast(
            InitResponse,
            envelope(
                "init",
                local_backend(self.config, raw),
                status=normalize_status(raw),
            ),
        )

    def sources(self) -> SourcesResponse:
        raw = self._json(["sources", "--json"])
        return cast(
            SourcesResponse,
            envelope(
                "sources",
                local_backend(self.config, raw),
                sources=normalize_sources(raw),
            ),
        )

    def import_(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[str] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> ImportResponse:
        args = ["import", "--json"]
        if all:
            args.append("--all")
        if provider is not None:
            args.extend(["--provider", provider])
        if path is not None:
            args.extend(["--path", path])
        if resume:
            args.append("--resume")
        if progress is not None:
            args.extend(["--progress", progress])
        raw = self._json(args)
        return cast(
            ImportResponse,
            envelope(
                "import",
                local_backend(self.config, raw),
                import_=normalize_import(raw),
            ),
        )

    def sync(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[str] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> SyncResponse:
        result = cast(
            JsonObject,
            self.import_(
                all=all,
                provider=provider,
                path=path,
                resume=resume,
                progress=progress,
            ),
        )
        result["operation"] = "sync"
        return cast(SyncResponse, result)

    def search(
        self,
        query: Optional[SearchQueryV1] = None,
        *,
        provider: Optional[str] = None,
        history_source: Optional[str] = None,
        provider_key: Optional[str] = None,
        source_id: Optional[str] = None,
        source_format: Optional[str] = None,
        workspace: Optional[str] = None,
        since: Optional[str] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        events: bool = False,
        backend: Optional[SearchBackendMode] = None,
        primary_only: bool = False,
        include_subagents: bool = False,
        limit: Optional[int] = None,
        refresh: Optional[str] = None,
        include_current_session: bool = False,
    ) -> SearchResponse:
        validate_search_intent(query=query, file=file, limit=limit)
        args = ["search"]
        if query is not None:
            args.extend(["--query-json", serialize_search_query(query)])
        _extend_option(args, "--provider", provider)
        _extend_option(args, "--history-source", history_source)
        _extend_option(args, "--provider-key", provider_key)
        _extend_option(args, "--source-id", source_id)
        _extend_option(args, "--source-format", source_format)
        _extend_option(args, "--workspace", workspace)
        _extend_option(args, "--since", since)
        _extend_option(args, "--event-type", event_type)
        _extend_option(args, "--file", file)
        _extend_option(args, "--session", session)
        if events:
            args.append("--events")
        _extend_option(args, "--backend", backend)
        if primary_only:
            args.append("--primary-only")
        if include_subagents:
            args.append("--include-subagents")
        if limit is not None:
            args.extend(["--limit", str(limit)])
        _extend_option(args, "--refresh", refresh)
        if include_current_session:
            args.append("--include-current-session")
        args.append("--json")
        raw = self._json(args)
        return cast(
            SearchResponse,
            envelope(
                "search",
                local_backend(self.config, raw),
                search=normalize_search(raw),
            ),
        )

    def show_event(
        self,
        event_id: str,
        *,
        window: Optional[int] = None,
        before: Optional[int] = None,
        after: Optional[int] = None,
    ) -> ShowEventResponse:
        args = ["show", "event", event_id, "--format", "json"]
        if window is not None:
            args.extend(["--window", str(window)])
        if before is not None:
            args.extend(["--before", str(before)])
        if after is not None:
            args.extend(["--after", str(after)])
        raw = self._json(args)
        return cast(
            ShowEventResponse,
            envelope(
                "showEvent",
                local_backend(self.config, raw),
                event=normalize_event(raw),
            ),
        )

    def show_session(self, session_id: str, *, mode: Optional[str] = None) -> ShowSessionResponse:
        args = ["show", "session", session_id, "--format", "json"]
        if mode is not None:
            args.extend(["--mode", mode])
        raw = self._json(args)
        return cast(
            ShowSessionResponse,
            envelope(
                "showSession",
                local_backend(self.config, raw),
                session=normalize_session(raw),
            ),
        )

    def locate_event(self, event_id: str) -> LocateEventResponse:
        raw = self._json(["locate", "event", event_id, "--format", "json"])
        return cast(
            LocateEventResponse,
            envelope(
                "locateEvent",
                local_backend(self.config, raw),
                location=normalize_location(raw),
            ),
        )

    def locate_session(self, session_id: str) -> LocateSessionResponse:
        raw = self._json(["locate", "session", session_id, "--format", "json"])
        return cast(
            LocateSessionResponse,
            envelope(
                "locateSession",
                local_backend(self.config, raw),
                location=normalize_location(raw),
            ),
        )

    def ctx_version(self) -> Optional[str]:
        try:
            completed = self._run(["--version"])
        except CtxAgentHistoryError:
            return None
        return completed.stdout.strip() or None

    def _json(self, args: Sequence[str]) -> JsonObject:
        completed = self._run(args)
        stdout = completed.stdout.strip()
        if not stdout:
            raise CtxAgentHistoryProtocolError(
                "ctx returned no JSON on stdout",
                details={"command": self._command(args), "stderr": completed.stderr},
            )
        try:
            parsed = json.loads(stdout)
        except json.JSONDecodeError as exc:
            raise CtxAgentHistoryProtocolError(
                "ctx returned invalid JSON",
                details={
                    "command": self._command(args),
                    "stdout": completed.stdout,
                    "stderr": completed.stderr,
                },
                cause=exc,
            ) from exc
        if not isinstance(parsed, dict):
            raise CtxAgentHistoryProtocolError(
                "ctx returned a non-object JSON value",
                details={"command": self._command(args), "stdout": completed.stdout},
            )
        return parsed

    def _run(self, args: Sequence[str]) -> subprocess.CompletedProcess[str]:
        command = self._command(args)
        env = os.environ.copy()
        if self.config.env:
            env.update(self.config.env)
        launch_command, uses_windows_launcher = _scoped_launch_command(command, env)
        creationflags = 0
        if os.name == "nt":
            creationflags = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
        try:
            process = subprocess.Popen(
                launch_command,
                cwd=str(self.config.cwd) if self.config.cwd is not None else None,
                env=env,
                stdin=subprocess.PIPE if uses_windows_launcher else subprocess.DEVNULL,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                start_new_session=os.name != "nt",
                creationflags=creationflags,
            )
        except OSError as exc:
            raise CtxAgentHistoryCliError(
                "failed to execute ctx CLI",
                command=command,
                exit_code=-1,
                stderr=str(exc),
                cause=exc,
            ) from exc

        cleanup = _ProcessScopeCleanup(process)
        try:
            return self._supervise_process(
                command,
                process,
                uses_windows_launcher,
                cleanup,
            )
        except BaseException:
            cleanup.run()
            raise

    def _supervise_process(
        self,
        command: list[str],
        process: subprocess.Popen[bytes],
        uses_windows_launcher: bool,
        cleanup: _ProcessScopeCleanup,
    ) -> subprocess.CompletedProcess[str]:
        stdout_capture = _BoundedCapture("stdout", _STDOUT_CAP_BYTES)
        stderr_capture = _BoundedCapture("stderr", _STDERR_CAP_BYTES)
        stop = threading.Event()
        readers = [
            threading.Thread(
                target=_drain_process_stream,
                args=(process.stdout, stdout_capture, stop),
                daemon=True,
            ),
            threading.Thread(
                target=_drain_process_stream,
                args=(process.stderr, stderr_capture, stop),
                daemon=True,
            ),
        ]
        cleanup.register_capture(stop, readers)
        try:
            for reader in readers:
                reader.start()
            return self._monitor_process(
                command,
                process,
                uses_windows_launcher,
                stdout_capture,
                stderr_capture,
                stop,
                readers,
                cleanup,
            )
        except BaseException:
            cleanup.run()
            raise

    def _monitor_process(
        self,
        command: list[str],
        process: subprocess.Popen[bytes],
        uses_windows_launcher: bool,
        stdout_capture: _BoundedCapture,
        stderr_capture: _BoundedCapture,
        stop: threading.Event,
        readers: list[threading.Thread],
        cleanup: _ProcessScopeCleanup,
    ) -> subprocess.CompletedProcess[str]:
        deadline = (
            None
            if self.config.timeout is None
            else time.monotonic() + max(0.0, self.config.timeout)
        )
        failure: Optional[str] = None
        launcher_acknowledged = False
        while process.poll() is None and not stop.wait(_PROCESS_POLL_SECONDS):
            if (
                uses_windows_launcher
                and not launcher_acknowledged
                and all(not reader.is_alive() for reader in readers)
            ):
                launcher_acknowledged = _acknowledge_windows_launcher(process)
            if deadline is not None and time.monotonic() >= deadline:
                failure = "timeout"
                stop.set()
                break
        if failure is None and stop.is_set():
            failure = "capture"

        if failure is None:
            clean_drain_deadline = time.monotonic() + _CLEAN_DRAIN_SECONDS
            for reader in readers:
                reader.join(max(0.0, clean_drain_deadline - time.monotonic()))
            if any(reader.is_alive() for reader in readers):
                failure = "capture"
                stop.set()

        if failure is not None:
            cleanup.run()
        else:
            teardown_deadline = time.monotonic() + _TEARDOWN_SECONDS
            for reader in readers:
                reader.join(max(0.0, teardown_deadline - time.monotonic()))
            _close_process_pipes(process)

        overflow = stdout_capture.overflow or stderr_capture.overflow
        capture_error = stdout_capture.error or stderr_capture.error
        if overflow is not None:
            stream, cap = overflow
            raise CtxAgentHistoryProtocolError(
                "ctx CLI output exceeded its capture limit",
                details={"command": command, "stream": stream, "cap_bytes": cap},
            )
        if failure == "timeout":
            raise CtxAgentHistoryTimeoutError(
                "ctx CLI timed out",
                details={
                    "command": command,
                    "timeout": self.config.timeout,
                },
            )
        if (
            failure == "capture"
            or capture_error is not None
            or any(reader.is_alive() for reader in readers)
        ):
            stream, error = capture_error or ("pipe", RuntimeError("reader did not stop"))
            raise CtxAgentHistoryProtocolError(
                "ctx CLI output capture failed",
                details={"command": command, "stream": stream},
                cause=error,
            )
        if uses_windows_launcher and process.returncode == _WINDOWS_DRAIN_FAILURE_EXIT:
            raise CtxAgentHistoryProtocolError(
                "ctx CLI output capture failed",
                details={"command": command, "stream": "pipe"},
            )

        try:
            stdout = _decode_process_output_strict(stdout_capture.value())
            stderr = _decode_process_output_strict(stderr_capture.value())
        except UnicodeDecodeError as exc:
            cleanup.run()
            raise CtxAgentHistoryProtocolError(
                "ctx returned invalid UTF-8",
                details={
                    "command": command,
                },
                cause=exc,
            ) from exc
        returncode = process.returncode if process.returncode is not None else -1
        if returncode != 0:
            cleanup.run()
            raise CtxAgentHistoryCliError(
                "ctx CLI command failed",
                command=command,
                exit_code=returncode,
                stderr=stderr,
                stdout=stdout,
            )
        cleanup.run()
        return subprocess.CompletedProcess(
            command,
            returncode,
            stdout=stdout,
            stderr=stderr,
        )

    def _command(self, args: Sequence[str]) -> list[str]:
        command = [self.config.ctx_binary]
        if self.config.data_root is not None:
            command.extend(["--data-root", str(self.config.data_root)])
        command.extend(args)
        return command


_STDOUT_CAP_BYTES = 2 * 1024 * 1024
_STDERR_CAP_BYTES = 256 * 1024
_READ_BUFFER_BYTES = 64 * 1024
_PROCESS_POLL_SECONDS = 0.01
_CLEAN_DRAIN_SECONDS = 0.1
_TEARDOWN_SECONDS = 1.0
_PROCESS_SCOPE_LAUNCHER_ARG = "__ctx_sdk_process_scope_v1"
_PROCESS_SCOPE_LAUNCHER_ENV = "CTX_SDK_PROCESS_SCOPE_LAUNCHER"
_WINDOWS_DRAIN_FAILURE_EXIT = 252
_WINDOWS_LAUNCHER_ACK = b"\x06"


class _ProcessScopeCleanup:
    def __init__(self, process: subprocess.Popen[bytes]) -> None:
        self.process = process
        self.stop: Optional[threading.Event] = None
        self.readers: list[threading.Thread] = []
        self.lock = threading.Lock()
        self.cleaned = False

    def register_capture(
        self,
        stop: threading.Event,
        readers: list[threading.Thread],
    ) -> None:
        self.stop = stop
        self.readers = readers

    def run(self) -> None:
        with self.lock:
            if self.cleaned:
                return
            self.cleaned = True
        if self.stop is not None:
            self.stop.set()
        _terminate_process_scope(self.process)
        _close_process_pipes(self.process)
        teardown_deadline = time.monotonic() + _TEARDOWN_SECONDS
        for reader in self.readers:
            if reader.ident is None:
                continue
            try:
                reader.join(max(0.0, teardown_deadline - time.monotonic()))
            except RuntimeError:
                pass


def _scoped_launch_command(
    command: list[str], env: dict[str, str]
) -> tuple[list[str], bool]:
    launcher = env.get(_PROCESS_SCOPE_LAUNCHER_ENV)
    executable_name = Path(command[0]).name.lower()
    if launcher is None and (
        executable_name in {"ctx", "ctx.exe"}
        or executable_name.startswith(("ctx-", "ctx_"))
    ):
        launcher = command[0]
    if launcher is None:
        if os.name == "nt":
            raise CtxAgentHistoryError(
                "local CLI process containment is unavailable",
                code="backend_unavailable",
                details={"backend": "process_scope", "platform": "windows"},
            )
        return command, False
    return [launcher, _PROCESS_SCOPE_LAUNCHER_ARG, "--", *command], os.name == "nt"


def _acknowledge_windows_launcher(process: subprocess.Popen[bytes]) -> bool:
    if process.stdin is None:
        return False
    try:
        process.stdin.write(_WINDOWS_LAUNCHER_ACK)
        process.stdin.flush()
        process.stdin.close()
        return True
    except (BrokenPipeError, OSError, ValueError):
        return False


class _BoundedCapture:
    def __init__(self, stream: str, cap: int) -> None:
        self.stream = stream
        self.cap = cap
        self.data = bytearray(cap)
        self.size = 0
        self.overflow: Optional[tuple[str, int]] = None
        self.error: Optional[tuple[str, BaseException]] = None

    def append(self, chunk: bytes) -> bool:
        remaining = self.cap - self.size
        if len(chunk) > remaining:
            if remaining > 0:
                self.data[self.size : self.size + remaining] = memoryview(chunk)[:remaining]
                self.size += remaining
            self.overflow = (self.stream, self.cap)
            return False
        self.data[self.size : self.size + len(chunk)] = chunk
        self.size += len(chunk)
        return True

    def value(self) -> memoryview:
        return memoryview(self.data)[: self.size]


def _drain_process_stream(
    stream: Optional[Any], capture: _BoundedCapture, stop: threading.Event
) -> None:
    if stream is None:
        capture.error = (capture.stream, RuntimeError("process pipe is unavailable"))
        stop.set()
        return
    try:
        read = getattr(stream, "read1", stream.read)
        while not stop.is_set():
            chunk = read(_READ_BUFFER_BYTES)
            if not chunk:
                return
            if not capture.append(chunk):
                stop.set()
                return
    except (OSError, ValueError) as exc:
        if not stop.is_set():
            capture.error = (capture.stream, exc)
            stop.set()


def _terminate_process_scope(process: subprocess.Popen[bytes]) -> None:
    if os.name == "nt":
        if process.poll() is None:
            try:
                subprocess.run(
                    ["taskkill", "/PID", str(process.pid), "/T", "/F"],
                    stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL,
                    timeout=_TEARDOWN_SECONDS / 2,
                    check=False,
                )
            except (OSError, subprocess.TimeoutExpired):
                try:
                    process.kill()
                except OSError:
                    pass
    elif _process_group_exists(process.pid):
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except (OSError, ProcessLookupError):
            pass
        deadline = time.monotonic() + 0.1
        while _process_group_exists(process.pid) and time.monotonic() < deadline:
            time.sleep(0.01)
        if _process_group_exists(process.pid):
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except (OSError, ProcessLookupError):
                pass
    try:
        process.wait(timeout=_TEARDOWN_SECONDS / 2)
    except subprocess.TimeoutExpired:
        try:
            process.kill()
        except OSError:
            pass
        try:
            process.wait(timeout=_TEARDOWN_SECONDS / 2)
        except (OSError, subprocess.TimeoutExpired):
            pass
    except OSError:
        pass


def _process_group_exists(pid: int) -> bool:
    try:
        os.killpg(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False


def _close_process_pipes(process: subprocess.Popen[bytes]) -> None:
    for stream in (process.stdin, process.stdout, process.stderr):
        if stream is not None:
            try:
                stream.close()
            except (OSError, ValueError):
                pass


class HostedAdapter:
    """Hosted agent-history-v1 placeholder that performs no network I/O."""

    name = "hosted"

    def __init__(self, config: HostedConfig) -> None:
        self.config = config
        self.backend = hosted_backend(config)

    def status(self) -> StatusResponse:
        raise HostedTransportNotImplementedError("status")

    def init(self, *, catalog_only: bool = False, progress: Optional[str] = None) -> InitResponse:
        raise HostedTransportNotImplementedError("init")

    def sources(self) -> SourcesResponse:
        raise HostedTransportNotImplementedError("sources")

    def import_(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[str] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> ImportResponse:
        raise HostedTransportNotImplementedError("import")

    def sync(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[str] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> SyncResponse:
        raise HostedTransportNotImplementedError("sync")

    def search(
        self,
        query: Optional[SearchQueryV1] = None,
        *,
        provider: Optional[str] = None,
        history_source: Optional[str] = None,
        provider_key: Optional[str] = None,
        source_id: Optional[str] = None,
        source_format: Optional[str] = None,
        workspace: Optional[str] = None,
        since: Optional[str] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        events: bool = False,
        backend: Optional[SearchBackendMode] = None,
        primary_only: bool = False,
        include_subagents: bool = False,
        limit: Optional[int] = None,
        refresh: Optional[str] = None,
        include_current_session: bool = False,
    ) -> SearchResponse:
        validate_search_intent(query=query, file=file, limit=limit)
        if query is not None:
            serialize_search_query(query)
        raise HostedTransportNotImplementedError("search")

    def show_event(self, event_id: str, **kwargs: Any) -> ShowEventResponse:
        raise HostedTransportNotImplementedError("showEvent")

    def show_session(self, session_id: str, **kwargs: Any) -> ShowSessionResponse:
        raise HostedTransportNotImplementedError("showSession")

    def locate_event(self, event_id: str) -> LocateEventResponse:
        raise HostedTransportNotImplementedError("locateEvent")

    def locate_session(self, session_id: str) -> LocateSessionResponse:
        raise HostedTransportNotImplementedError("locateSession")

    def ctx_version(self) -> Optional[str]:
        return None


def _extend_option(args: list[str], flag: str, value: Optional[str]) -> None:
    if value is not None:
        args.extend([flag, value])


def _decode_process_output_strict(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        return str(value, "utf-8", errors="strict")
    return str(value)
