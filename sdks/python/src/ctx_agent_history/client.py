"""Public agent-history-v1 client."""

from __future__ import annotations

from pathlib import Path
from typing import Mapping, Optional, Sequence, Union

from .config import HostedConfig, LocalConfig
from .transport import HostedAdapter, LocalCliAdapter, AgentHistoryTransport
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
from .version import API_VERSION, SDK_VERSION, VersionInfo

Pathish = Union[str, Path]


class AgentHistoryClient:
    """Client for the ctx agent-history-v1 API.

    The default transport is local and executes the ctx CLI. Data methods
    return agent-history-v1 envelope dictionaries unless noted otherwise.
    """

    def __init__(self, transport: AgentHistoryTransport):
        self._transport = transport

    @classmethod
    def local(
        cls,
        *,
        ctx_binary: str = "ctx",
        data_root: Optional[Pathish] = None,
        env: Optional[Mapping[str, str]] = None,
        cwd: Optional[Pathish] = None,
        timeout: Optional[float] = None,
    ) -> "AgentHistoryClient":
        return cls(
            LocalCliAdapter(
                LocalConfig(
                    ctx_binary=ctx_binary,
                    data_root=_path_or_none(data_root),
                    env=dict(env) if env is not None else None,
                    cwd=_path_or_none(cwd),
                    timeout=timeout,
                )
            )
        )

    @classmethod
    def hosted(cls, config: HostedConfig) -> "AgentHistoryClient":
        return cls(HostedAdapter(config))

    @classmethod
    def from_config(cls, config: Union[LocalConfig, HostedConfig]) -> "AgentHistoryClient":
        if isinstance(config, LocalConfig):
            return cls(LocalCliAdapter(config))
        if isinstance(config, HostedConfig):
            return cls(HostedAdapter(config))
        raise TypeError(f"unsupported config type: {type(config)!r}")

    @property
    def transport(self) -> AgentHistoryTransport:
        return self._transport

    def status(self) -> StatusResponse:
        return self._transport.status()

    def init(self, *, catalog_only: bool = False, progress: Optional[str] = None) -> InitResponse:
        return self._transport.init(catalog_only=catalog_only, progress=progress)

    def sources(self) -> SourcesResponse:
        return self._transport.sources()

    def import_(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[Pathish] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> ImportResponse:
        return self._transport.import_(
            all=all,
            provider=provider,
            path=str(path) if path is not None else None,
            resume=resume,
            progress=progress,
        )

    def sync(
        self,
        *,
        all: bool = False,
        provider: Optional[str] = None,
        path: Optional[Pathish] = None,
        resume: bool = False,
        progress: Optional[str] = None,
    ) -> SyncResponse:
        return self._transport.sync(
            all=all,
            provider=provider,
            path=str(path) if path is not None else None,
            resume=resume,
            progress=progress,
        )

    def search(
        self,
        query: Optional[str] = None,
        *,
        provider: Optional[str] = None,
        workspace: Optional[str] = None,
        since: Optional[str] = None,
        event_type: Optional[str] = None,
        file: Optional[Pathish] = None,
        session: Optional[str] = None,
        terms: Optional[Sequence[str]] = None,
        events: bool = False,
        primary_only: bool = False,
        include_subagents: bool = False,
        limit: Optional[int] = None,
        refresh: Optional[str] = None,
        include_current_session: bool = False,
    ) -> SearchResponse:
        file_value = str(file) if file is not None else None
        validate_search_intent(query=query, terms=terms, file=file_value)
        return self._transport.search(
            query=query,
            provider=provider,
            workspace=workspace,
            since=since,
            event_type=event_type,
            file=file_value,
            session=session,
            terms=list(terms) if terms is not None else None,
            events=events,
            primary_only=primary_only,
            include_subagents=include_subagents,
            limit=limit,
            refresh=refresh,
            include_current_session=include_current_session,
        )

    def show_event(
        self,
        event_id: str,
        *,
        window: Optional[int] = None,
        before: Optional[int] = None,
        after: Optional[int] = None,
    ) -> ShowEventResponse:
        return self._transport.show_event(
            event_id,
            window=window,
            before=before,
            after=after,
        )

    def showEvent(
        self,
        event_id: str,
        *,
        window: Optional[int] = None,
        before: Optional[int] = None,
        after: Optional[int] = None,
    ) -> ShowEventResponse:
        return self.show_event(event_id, window=window, before=before, after=after)

    def show_session(self, session_id: str, *, mode: Optional[str] = None) -> ShowSessionResponse:
        return self._transport.show_session(session_id, mode=mode)

    def showSession(self, session_id: str, *, mode: Optional[str] = None) -> ShowSessionResponse:
        return self.show_session(session_id, mode=mode)

    def locate_event(self, event_id: str) -> LocateEventResponse:
        return self._transport.locate_event(event_id)

    def locateEvent(self, event_id: str) -> LocateEventResponse:
        return self.locate_event(event_id)

    def locate_session(self, session_id: str) -> LocateSessionResponse:
        return self._transport.locate_session(session_id)

    def locateSession(self, session_id: str) -> LocateSessionResponse:
        return self.locate_session(session_id)

    def version(self) -> VersionInfo:
        return VersionInfo(
            sdk_version=SDK_VERSION,
            api_version=API_VERSION,
            transport=self._transport.name,
            ctx_version=self._transport.ctx_version(),
        )

    def versioning(self) -> JsonObject:
        return self.version().as_dict()


def _path_or_none(value: Optional[Pathish]) -> Optional[Path]:
    if value is None:
        return None
    return Path(value)
