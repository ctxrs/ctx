"""Transport implementations for agent-history-v1."""

from __future__ import annotations

import json
import os
import subprocess
from typing import Any, Mapping, Optional, Protocol, Sequence, cast

from ._subprocess import run_local_cli
from .config import HostedConfig, LocalConfig
from .errors import (
    CtxAgentHistoryError,
    CtxAgentHistoryProtocolError,
    CtxAgentHistoryValidationError,
    HostedTransportNotImplementedError,
)
from .agent_history_v1 import (
    envelope,
    hosted_backend,
    local_backend,
    normalize_event,
    normalize_import,
    normalize_search,
    normalize_session,
    normalize_sources,
    normalize_status,
)
from .types import (
    ImportResponse,
    InitResponse,
    JsonObject,
    SearchBackendMode,
    SearchContentScope,
    SearchResponse,
    ShowEventResponse,
    ShowSessionResponse,
    SourcesResponse,
    StatusResponse,
    SyncResponse,
)
from .validation import validate_search_intent


class _DuplicateJSONMemberError(ValueError):
    pass


class _NonFiniteJSONConstantError(ValueError):
    pass


def _validate_search_class_filters(
    *,
    content_scope: Optional[SearchContentScope],
    event_type: Optional[str],
) -> None:
    if content_scope is not None and content_scope not in (
        "all",
        "transcript",
        "calls",
        "outputs",
    ):
        raise CtxAgentHistoryValidationError(
            "search content_scope must be one of all, transcript, calls, outputs",
            details={"content_scope": content_scope},
        )
    if content_scope is not None and event_type is not None:
        raise CtxAgentHistoryValidationError(
            "search content_scope and event_type are mutually exclusive",
            details={"content_scope": content_scope, "event_type": event_type},
        )


def _reject_duplicate_object_pairs(pairs: Sequence[tuple[str, Any]]) -> JsonObject:
    result: JsonObject = {}
    for key, value in pairs:
        if key in result:
            raise _DuplicateJSONMemberError(f"duplicate JSON object member {key!r}")
        result[key] = value
    return result


def _reject_non_finite_json_constant(value: str) -> Any:
    raise _NonFiniteJSONConstantError(f"non-finite JSON constant {value!r}")


class AgentHistoryTransport(Protocol):
    name: str

    def status(self) -> StatusResponse:
        ...

    def init(self, *, progress: Optional[str] = None) -> InitResponse:
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
        content_scope: Optional[SearchContentScope] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        terms: Optional[Sequence[str]] = None,
        events: bool = False,
        backend: Optional[SearchBackendMode] = None,
        semantic_weight: Optional[float] = None,
        primary_only: bool = False,
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

    def ctx_version(self) -> Optional[str]:
        ...


class LocalCliAdapter:
    """agent-history-v1 transport backed by the local ctx CLI."""

    name = "local-cli"

    def __init__(self, config: Optional[LocalConfig] = None) -> None:
        self.config = config or LocalConfig()

    def status(self) -> StatusResponse:
        raw = self._json(["status", "--format=json"])
        return cast(
            StatusResponse,
            envelope(
                "status",
                local_backend(self.config, raw),
                status=normalize_status(raw),
            ),
        )

    def init(self, *, progress: Optional[str] = None) -> InitResponse:
        args = ["setup", "--format=json"]
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
        raw = self._json(["sources", "--format=json"])
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
        args = ["import", "--format=json"]
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
        content_scope: Optional[SearchContentScope] = None,
        event_type: Optional[str] = None,
        file: Optional[str] = None,
        session: Optional[str] = None,
        terms: Optional[Sequence[str]] = None,
        events: bool = False,
        backend: Optional[SearchBackendMode] = None,
        semantic_weight: Optional[float] = None,
        primary_only: bool = False,
        limit: Optional[int] = None,
        refresh: Optional[str] = None,
        include_current_session: bool = False,
    ) -> SearchResponse:
        _validate_search_class_filters(content_scope=content_scope, event_type=event_type)
        validate_search_intent(query=query, terms=terms, file=file)
        args = ["search", "--format=json"]
        if query is not None:
            args.append(query)
        _extend_option(args, "--provider", provider)
        _extend_option(args, "--workspace", workspace)
        _extend_option(args, "--since", since)
        _extend_option(args, "--content-scope", content_scope)
        _extend_option(args, "--event-type", event_type)
        _extend_option(args, "--file", file)
        _extend_option(args, "--session", session)
        for term in terms or []:
            args.extend(["--term", term])
        if events:
            args.append("--events")
        _extend_option(args, "--backend", backend)
        if semantic_weight is not None:
            args.extend(["--semantic-weight", str(semantic_weight)])
        if primary_only:
            args.append("--primary-only")
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
            parsed = json.loads(
                stdout,
                object_pairs_hook=_reject_duplicate_object_pairs,
                parse_constant=_reject_non_finite_json_constant,
            )
        except (
            json.JSONDecodeError,
            _DuplicateJSONMemberError,
            _NonFiniteJSONConstantError,
        ) as exc:
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
        env["CTX_ANALYTICS_ENABLED"] = "false"
        return run_local_cli(
            command,
            cwd=str(self.config.cwd) if self.config.cwd is not None else None,
            env=env,
            timeout=self.config.timeout,
        )

    def _command(self, args: Sequence[str]) -> list[str]:
        command = [self.config.ctx_binary]
        if self.config.data_root is not None:
            command.extend(["--data-root", str(self.config.data_root)])
        command.extend(args)
        return command


class HostedAdapter:
    """Deprecated hosted placeholder that performs no network I/O.

    Hosted SDK placeholders are deprecated and will be removed in the next
    breaking SDK revision; hosted operations remain unsupported.
    """

    name = "hosted"

    def __init__(self, config: HostedConfig) -> None:
        self.config = config
        self.backend = hosted_backend(config)

    def status(self) -> StatusResponse:
        raise HostedTransportNotImplementedError("status")

    def init(self, *, progress: Optional[str] = None) -> InitResponse:
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

    def ctx_version(self) -> Optional[str]:
        return None


def _extend_option(args: list[str], flag: str, value: Optional[str]) -> None:
    if value is not None:
        args.extend([flag, value])
