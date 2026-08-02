"""Typed agent-history-v1 envelope shapes exposed by the Python SDK."""

from __future__ import annotations

from typing import Any, List, Literal, Optional, TypedDict, Union

JsonObject = dict[str, Any]
Operation = Literal[
    "status",
    "init",
    "sources",
    "import",
    "sync",
    "search",
    "showEvent",
    "showSession",
    "error",
]
BackendKind = Literal["local", "hosted"]
SearchBackendMode = Literal["hybrid", "semantic", "lexical"]
CoreContentPolicyStatus = Literal["selected", "redacted", "omitted"]


class _BackendRequired(TypedDict):
    kind: BackendKind


class Backend(_BackendRequired, total=False):
    dataRoot: Optional[str]
    baseUrl: Optional[str]


class Totals(TypedDict, total=False):
    sourceFiles: int
    sourceBytes: int
    importedSources: int
    failedSources: int
    importedSessions: int
    importedEvents: int
    importedEdges: int
    skipped: int
    failed: int


class Freshness(TypedDict, total=False):
    mode: Optional[str]
    status: Optional[str]
    reason: Optional[str]
    budgetReasons: Optional[List[str]]
    sourceCount: Optional[int]
    daemonLastRunAtMs: Optional[int]
    totals: Totals
    error: Optional[str]


class _StatusRequired(TypedDict):
    initialized: bool
    localOnly: bool


class Status(_StatusRequired, total=False):
    dataRoot: Optional[str]
    readOnly: bool
    indexedItems: int
    indexedSessions: int
    indexedEvents: int
    indexedSources: int
    historyEpoch: JsonObject
    lexical: JsonObject
    refresh: JsonObject
    semantic: JsonObject
    daemon: JsonObject


class _ProviderSourceRequired(TypedDict):
    provider: str
    path: str
    status: str
    importable: bool


class ProviderSource(_ProviderSourceRequired, total=False):
    exists: bool
    sourceFormat: Optional[str]
    importSupport: Optional[str]
    nativeImport: bool
    unsupportedReason: Optional[str]


class _ImportResultRequired(TypedDict):
    resume: bool
    totals: Totals


class ImportResult(_ImportResultRequired, total=False):
    resumeMode: Optional[str]
    sources: list[JsonObject]


class Citation(TypedDict, total=False):
    itemId: Optional[str]
    targetType: Optional[str]
    ctxEventId: Optional[str]
    ctxSessionId: Optional[str]
    label: Optional[str]
    time: Optional[str]
    provider: Optional[str]
    sessionId: Optional[str]
    eventSeq: Optional[int]


class _SearchHitRequired(TypedDict):
    resultScope: str


class SearchHit(_SearchHitRequired, total=False):
    ctxEventId: Optional[str]
    ctxSessionId: Optional[str]
    providerSessionId: Optional[str]
    eventSeq: Optional[int]
    title: Optional[str]
    snippet: Optional[str]
    rank: Optional[float]
    retrievalScore: Optional[float]
    resultType: Optional[str]
    provider: Optional[str]
    sourceFormat: Optional[str]
    timestamp: Optional[str]
    cwd: Optional[str]
    whyMatched: list[str]
    citations: list[Citation]
    suggestedNextCommands: list[str]
    visibility: Optional[str]


class RetrievalCoverage(TypedDict, total=False):
    embeddedItems: int
    embeddedChunks: int
    searchableItems: int
    indexedNow: int
    dirtyItems: int


class SearchRetrieval(TypedDict, total=False):
    requestedMode: Optional[SearchBackendMode]
    effectiveMode: Optional[SearchBackendMode]
    semanticWeight: Optional[float]
    semanticStatus: Optional[str]
    semanticFallbackCode: Optional[str]
    semanticFallback: Optional[str]
    embeddingModel: Optional[str]
    coverage: RetrievalCoverage
    worker: JsonObject
    diagnostics: JsonObject


class SearchResultWindow(TypedDict):
    limit: int
    returned: int
    moreAvailable: bool


class _SearchResultRequired(TypedDict):
    query: Optional[str]
    results: list[SearchHit]


class SearchResult(_SearchResultRequired, total=False):
    filters: JsonObject
    freshness: Freshness
    retrieval: SearchRetrieval
    generatedAt: Optional[str]
    resultWindow: SearchResultWindow
    pagination: JsonObject
    truncation: JsonObject


class _CoreContentMetadataRequired(TypedDict):
    complete: bool
    policyStatus: CoreContentPolicyStatus


class CoreContentMetadata(_CoreContentMetadataRequired, total=False):
    policyReason: Optional[str]


class Event(TypedDict, total=False):
    ctxEventId: Optional[str]
    ctxSessionId: Optional[str]
    provider: Optional[str]
    providerSessionId: Optional[str]
    sourceFormat: Optional[str]
    sequence: Optional[int]
    eventType: Optional[str]
    role: Optional[str]
    occurredAt: Optional[str]
    text: Optional[str]
    content: CoreContentMetadata
    citations: list[Citation]


class _EventResultRequired(TypedDict):
    events: list[Event]


class EventResult(_EventResultRequired, total=False):
    event: Event


class SessionSummary(TypedDict, total=False):
    ctxSessionId: Optional[str]
    provider: Optional[str]
    providerSessionId: Optional[str]
    sourceFormat: Optional[str]
    title: Optional[str]


class SessionResult(TypedDict, total=False):
    session: SessionSummary
    events: list[Event]
    mode: Optional[str]
    format: Optional[str]


AgentHistoryErrorCode = Literal[
    "invalid_request",
    "not_found",
    "not_initialized",
    "backend_unavailable",
    "timeout",
    "cancelled",
    "not_supported",
    "adapter_error",
    "decode_error",
    "unknown",
]


class _AgentHistoryErrorRequired(TypedDict):
    code: AgentHistoryErrorCode
    message: str
    retryable: bool


class AgentHistoryErrorPayload(_AgentHistoryErrorRequired, total=False):
    details: JsonObject
    cause: Optional[str]


StatusResponse = TypedDict(
    "StatusResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["status"],
        "backend": Backend,
        "status": Status,
    },
)
InitResponse = TypedDict(
    "InitResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["init"],
        "backend": Backend,
        "status": Status,
    },
)
SourcesResponse = TypedDict(
    "SourcesResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["sources"],
        "backend": Backend,
        "sources": list[ProviderSource],
    },
)
ImportResponse = TypedDict(
    "ImportResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["import"],
        "backend": Backend,
        "import": ImportResult,
    },
)
SyncResponse = TypedDict(
    "SyncResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["sync"],
        "backend": Backend,
        "import": ImportResult,
    },
)
SearchResponse = TypedDict(
    "SearchResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["search"],
        "backend": Backend,
        "search": SearchResult,
    },
)
ShowEventResponse = TypedDict(
    "ShowEventResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["showEvent"],
        "backend": Backend,
        "event": EventResult,
    },
)
ShowSessionResponse = TypedDict(
    "ShowSessionResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["showSession"],
        "backend": Backend,
        "session": SessionResult,
    },
)
ErrorResponse = TypedDict(
    "ErrorResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["error"],
        "backend": Backend,
        "error": AgentHistoryErrorPayload,
    },
)

AgentHistoryResponse = Union[
    StatusResponse,
    InitResponse,
    SourcesResponse,
    ImportResponse,
    SyncResponse,
    SearchResponse,
    ShowEventResponse,
    ShowSessionResponse,
    ErrorResponse,
]
