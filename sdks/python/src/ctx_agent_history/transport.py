"""Transport implementations for agent-history-v1."""

from __future__ import annotations

import json
import os
import subprocess
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
    SearchResponse,
    ShowEventResponse,
    ShowSessionResponse,
    SourcesResponse,
    StatusResponse,
    SyncResponse,
)
from .validation import validate_search_intent


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
        query: Optional[str] = None,
        *,
        provider: Optional[str] = None,
        workspace: Optional[str] = None,
        since: Optional[str] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        terms: Optional[Sequence[str]] = None,
        events: bool = False,
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
        query: Optional[str] = None,
        *,
        provider: Optional[str] = None,
        workspace: Optional[str] = None,
        since: Optional[str] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        terms: Optional[Sequence[str]] = None,
        events: bool = False,
        primary_only: bool = False,
        include_subagents: bool = False,
        limit: Optional[int] = None,
        refresh: Optional[str] = None,
        include_current_session: bool = False,
    ) -> SearchResponse:
        validate_search_intent(query=query, terms=terms, file=file)
        args = ["search", "--json"]
        if query is not None:
            args.append(query)
        _extend_option(args, "--provider", provider)
        _extend_option(args, "--workspace", workspace)
        _extend_option(args, "--since", since)
        _extend_option(args, "--event-type", event_type)
        _extend_option(args, "--file", file)
        _extend_option(args, "--session", session)
        for term in terms or []:
            args.extend(["--term", term])
        if events:
            args.append("--events")
        if primary_only:
            args.append("--primary-only")
        if include_subagents:
            args.append("--include-subagents")
        if limit is not None:
            args.extend(["--limit", str(limit)])
        _extend_option(args, "--refresh", refresh)
        if include_current_session:
            args.append("--include-current-session")
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
        try:
            completed = subprocess.run(
                command,
                cwd=str(self.config.cwd) if self.config.cwd is not None else None,
                env=env,
                capture_output=True,
                timeout=self.config.timeout,
                check=False,
            )
        except OSError as exc:
            raise CtxAgentHistoryCliError(
                "failed to execute ctx CLI",
                command=command,
                exit_code=-1,
                stderr=str(exc),
                cause=exc,
            ) from exc
        except subprocess.TimeoutExpired as exc:
            raise CtxAgentHistoryTimeoutError(
                "ctx CLI timed out",
                details={
                    "command": command,
                    "stderr": _decode_process_output(exc.stderr),
                    "stdout": _decode_process_output(exc.stdout),
                    "timeout": self.config.timeout,
                },
                cause=exc,
            ) from exc
        if completed.returncode != 0:
            raise CtxAgentHistoryCliError(
                "ctx CLI command failed",
                command=command,
                exit_code=completed.returncode,
                stderr=_decode_process_output(completed.stderr),
                stdout=_decode_process_output(completed.stdout),
            )
        try:
            stdout = _decode_process_output_strict(completed.stdout)
            stderr = _decode_process_output_strict(completed.stderr)
        except UnicodeDecodeError as exc:
            raise CtxAgentHistoryProtocolError(
                "ctx returned invalid UTF-8",
                details={
                    "command": command,
                },
                cause=exc,
            ) from exc
        return subprocess.CompletedProcess(
            command,
            completed.returncode,
            stdout=stdout,
            stderr=stderr,
        )

    def _command(self, args: Sequence[str]) -> list[str]:
        command = [self.config.ctx_binary]
        if self.config.data_root is not None:
            command.extend(["--data-root", str(self.config.data_root)])
        command.extend(args)
        return command


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

    def search(self, query: Optional[str] = None, **kwargs: Any) -> SearchResponse:
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


def _decode_process_output(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return str(value)


def _decode_process_output_strict(value: object) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="strict")
    return str(value)
