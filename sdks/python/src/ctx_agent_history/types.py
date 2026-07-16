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
    "locateEvent",
    "locateSession",
    "error",
]
BackendKind = Literal["local", "hosted"]
SearchBackendMode = Literal["hybrid", "semantic", "lexical"]
SearchSemanticReadiness = Literal["ready", "not_ready", "unsupported", "unavailable"]
SearchEffectiveBackend = Literal["none", "lexical", "semantic", "hybrid"]
SearchCompleteness = Literal["complete", "partial"]


class SearchAllClause(TypedDict):
    all: str


class SearchPhraseClause(TypedDict):
    phrase: str


class SearchLiteralClause(TypedDict):
    literal: str


class SearchSemanticClause(TypedDict):
    semantic: str


SearchLexicalClause = Union[SearchAllClause, SearchPhraseClause, SearchLiteralClause]
SearchClause = Union[SearchLexicalClause, SearchSemanticClause]


class _SearchQueryRequired(TypedDict):
    version: Literal["ctx-search-v1"]


class SearchQueryV1(_SearchQueryRequired, total=False):
    any: list[SearchClause]
    must: list[SearchLexicalClause]
    must_not: list[SearchLexicalClause]


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
    indexedItems: int
    indexedSources: int
    catalogedSessions: int
    indexedCatalogSessions: int
    pendingCatalogSessions: int
    failedCatalogSessions: int
    staleCatalogSessions: int
    freshness: Freshness
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
    sourcePath: Optional[str]
    sourceExists: Optional[bool]
    cursor: Optional[str]


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
    resultType: Optional[str]
    provider: Optional[str]
    timestamp: Optional[str]
    cwd: Optional[str]
    sourcePath: Optional[str]
    sourceExists: Optional[bool]
    cursor: Optional[str]
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


class SearchExecutionLimits(TypedDict):
    query_bytes: int
    clauses: int
    analyzed_tokens_per_clause: int
    candidates_per_positive_seed: int
    candidate_rows: int
    retained_candidate_ids: int
    residual_rows: int
    verification_bytes: int
    verification_lookup_bytes: int
    hydrated_rows: int
    hydration_input_bytes: int
    hydration_input_bytes_per_event: int
    snippet_input_bytes: int
    returned_text_bytes: int
    serialized_response_bytes: int
    results: int
    elapsed_ms: int


class SearchExecutionConsumption(TypedDict):
    query_bytes: int
    clauses: int
    analyzed_tokens: int
    largest_analyzed_tokens_per_clause: int
    largest_positive_seed_candidates: int
    candidate_rows: int
    retained_candidate_ids: int
    residual_rows: int
    verification_bytes: int
    largest_verification_lookup_bytes: int
    hydrated_rows: int
    legacy_fallback_rows: int
    hydration_input_bytes: int
    largest_hydration_input_bytes: int
    snippet_input_bytes: int
    returned_results: int
    returned_text_bytes: int
    serialized_response_bytes: int
    elapsed_ms: int


class SearchSemanticCoverage(TypedDict):
    indexed_documents: int
    searchable_documents: int


class _SearchSemanticExecutionRequired(TypedDict):
    attempted: bool
    required: bool
    readiness: SearchSemanticReadiness
    effective_backend: SearchEffectiveBackend
    candidates_supplied: int
    candidates_consumed: int
    candidates_used: int
    positive_text_rule_version: str


class SearchSemanticExecution(_SearchSemanticExecutionRequired, total=False):
    backend: Optional[str]
    requested_candidates: int
    eligible_candidates: int
    coverage: SearchSemanticCoverage
    completeness: SearchCompleteness
    incompleteness_reasons: list[str]
    skip_reason: Optional[str]


class SearchQueryExecution(TypedDict):
    query_version: Literal["ctx-search-v1"]
    candidate_strategy: str
    resolved: SearchExecutionLimits
    consumed: SearchExecutionConsumption
    semantic: SearchSemanticExecution
    requested_result_limit: int
    result_limit: int
    max_result_limit: int
    truncated: bool
    truncation_reasons: list[str]


class _SearchResultRequired(TypedDict):
    schema_version: Literal[2]
    query: Optional[SearchQueryV1]
    query_execution: SearchQueryExecution
    results: list[SearchHit]


class SearchResult(_SearchResultRequired, total=False):
    filters: JsonObject
    freshness: Freshness
    retrieval: SearchRetrieval
    generatedAt: Optional[str]
    pagination: JsonObject
    truncation: JsonObject


class SourceLocation(TypedDict, total=False):
    path: Optional[str]
    cursor: Optional[str]
    exists: Optional[bool]
    sourceId: Optional[str]
    sourceFormat: Optional[str]


class Event(TypedDict, total=False):
    ctxEventId: Optional[str]
    ctxSessionId: Optional[str]
    sequence: Optional[int]
    eventType: Optional[str]
    role: Optional[str]
    occurredAt: Optional[str]
    source: Optional[JsonValue]
    cursor: Optional[str]
    text: Optional[str]
    preview: Optional[str]
    citations: list[Citation]


class _EventResultRequired(TypedDict):
    events: list[Event]


class EventResult(_EventResultRequired, total=False):
    event: Event
    source: SourceLocation


class SessionSummary(TypedDict, total=False):
    ctxSessionId: Optional[str]
    provider: Optional[str]
    providerSessionId: Optional[str]
    title: Optional[str]


class SessionResult(TypedDict, total=False):
    session: SessionSummary
    events: list[Event]
    source: SourceLocation
    mode: Optional[str]
    format: Optional[str]


class _LocationResultRequired(TypedDict):
    ctxSessionId: str
    provider: str
    source: SourceLocation


class LocationResult(_LocationResultRequired, total=False):
    ctxEventId: Optional[str]
    providerSessionId: Optional[str]
    resume: JsonObject


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
LocateEventResponse = TypedDict(
    "LocateEventResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["locateEvent"],
        "backend": Backend,
        "location": LocationResult,
    },
)
LocateSessionResponse = TypedDict(
    "LocateSessionResponse",
    {
        "contractVersion": Literal["agent-history-v1"],
        "schemaVersion": Literal[1],
        "operation": Literal["locateSession"],
        "backend": Backend,
        "location": LocationResult,
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
    LocateEventResponse,
    LocateSessionResponse,
    ErrorResponse,
]
