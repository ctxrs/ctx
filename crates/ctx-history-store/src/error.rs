use std::path::PathBuf;

use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("uuid parse error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("record not found: {0}")]
    NotFound(Uuid),
    #[error("unsupported history store schema version: {0}")]
    UnsupportedSchemaVersion(i64),
    #[error("history store schema 47 is not the released final shape (identity: {0})")]
    UnsupportedSchemaIdentity(String),
    #[error("local Pro projection journal is not active")]
    ProjectionJournalInactive,
    #[error("local Pro projection journal contract fingerprint is invalid")]
    InvalidProjectionContractFingerprint,
    #[error(
        "local Pro projection journal position {generation}/{sequence} is stale; active generation is {active_generation}"
    )]
    StaleProjectionJournalPosition {
        generation: u64,
        sequence: u64,
        active_generation: u64,
    },
    #[error(
        "local Pro projection journal payload for {entity_kind}/{entity_id} is {encoded_bytes} bytes; maximum is {max_bytes}"
    )]
    ProjectionJournalPayloadTooLarge {
        entity_kind: &'static str,
        entity_id: Uuid,
        encoded_bytes: usize,
        max_bytes: usize,
    },
    #[error("local Pro projection journal contains invalid persisted data: {0}")]
    InvalidProjectionJournalData(String),
    #[error("unsupported session history archive version: {0}")]
    UnsupportedArchiveVersion(u32),
    #[error(
        "ctx index is busy: WAL checkpoint could not complete ({log_frames} log frames, {checkpointed_frames} checkpointed)"
    )]
    WalCheckpointBusy {
        log_frames: i64,
        checkpointed_frames: i64,
    },
    #[error("ctx index is busy: another bulk search import is active")]
    BulkSearchImportBusy,
    #[error(
        "ctx event-search FTS5 segment guard stopped bulk import: {table} has {segments} segments (safe guard {guard}, SQLite hard limit {hard_limit}); bounded maintenance could not create headroom; this is an FTS segment-limit condition, not evidence that the disk is full"
    )]
    EventSearchSegmentLimit {
        table: &'static str,
        segments: i64,
        guard: i64,
        hard_limit: i64,
    },
    #[error("ctx index is busy: another source inventory is active")]
    SourceInventoryBusy,
    #[error("catalog source contains at least {observed} sessions; maximum is {maximum}")]
    CatalogSessionLimitExceeded { observed: usize, maximum: usize },
    #[error(
        "source import observation changed before it could be marked {operation}; retry the import: {provider}/{source_path}"
    )]
    SourceImportObservationConflict {
        operation: &'static str,
        provider: String,
        source_path: String,
    },
    #[error(
        "provider history source relocation is ambiguous for {provider}/{source_format}; the prior and incoming exact locators cannot be proven to be one source"
    )]
    ProviderSourceRelocationAmbiguous {
        provider: String,
        source_format: String,
    },
    #[error("capture source {capture_source_id} has a conflicting local provider route binding")]
    CaptureSourceProviderRouteConflict { capture_source_id: Uuid },
    #[error(
        "provider history source route retirement conflicts with current authority for {provider}/{source_format}"
    )]
    ProviderSourceRouteRetirementConflict {
        provider: String,
        source_format: String,
    },
    #[error("no authorized current provider source route is available for event {event_id}")]
    AuthorizedSourceRouteUnavailable { event_id: Uuid },
    #[error("multiple authorized current provider source routes match event {event_id}")]
    AuthorizedSourceRouteAmbiguous { event_id: Uuid },
    #[error(
        "multiple canonical capture sources match {provider}/{source_format}/{external_session_id}"
    )]
    AmbiguousCaptureSourceIdentity {
        provider: String,
        source_format: String,
        external_session_id: String,
    },
    #[error("bulk search guard belongs to a different ctx index")]
    InvalidBulkSearchGuard,
    #[error("bulk search group admission is invalid or belongs to a different ctx index")]
    InvalidBulkSearchGroupAdmission,
    #[error("a bulk search group admission is already outstanding for this Store")]
    BulkSearchGroupAdmissionOutstanding,
    #[error("a NativePath publication group is already active on this Store")]
    NativePathGroupAlreadyActive,
    #[error("Store connection is quarantined after transaction rollback failed; reopen the Store")]
    StoreConnectionQuarantined,
    #[error("a NativePath publication group requires an outermost autocommit boundary")]
    NativePathGroupRequiresAutocommit,
    #[error("NativePath publication group {limit} accounting is {actual}; maximum is {maximum}")]
    NativePathGroupLimitExceeded {
        limit: &'static str,
        actual: usize,
        maximum: usize,
    },
    #[error("NativePath publication group was poisoned by a refused mutation")]
    NativePathGroupPoisoned,
    #[error("NativePath publication group transaction control is Store-owned")]
    NativePathTransactionControlDenied,
    #[error("projection journal lifecycle changes are not allowed inside a NativePath group")]
    NativePathJournalLifecycleDuringGroup,
    #[error("NativePath publication group journal has already been sealed")]
    NativePathJournalSealed,
    #[error("NativePath cursor set does not belong to the active publication group")]
    InvalidNativePathCursorSet,
    #[error("NativePath cursor compare-and-set conflicts with freshly read Store state")]
    NativePathCursorConflict,
    #[error("NativePath source generation conflicts with current route or staged state")]
    NativePathSourceGenerationConflict,
    #[error("NativePath legacy provider-hash migration was not exactly authorized")]
    InvalidNativePathLegacyProviderHashMigration,
    #[error("NativePath event identity alias conflicts with canonical Store identity")]
    NativePathEventIdentityAliasConflict,
    #[error("cold Store construction is supported only on Linux")]
    ColdStoreUnsupportedPlatform,
    #[error("cold Store target must be absent: {0:?}")]
    ColdStoreTargetIneligible(PathBuf),
    #[error("cold Store target changed after admission: {0:?}")]
    ColdStoreTargetChanged(PathBuf),
    #[error("another cold Store builder owns the target: {0:?}")]
    ColdStoreBuildBusy(PathBuf),
    #[error("cold Store build is not in the required lifecycle phase")]
    ColdStoreInvalidState,
    #[error("cold Store validation failed: {0}")]
    ColdStoreValidation(String),
    #[error(
        "session relationship update would change the canonical actor projection; dependent fanout is intentionally not performed"
    )]
    ProjectionChangingSessionRelationship,
    #[error("archive conflicts with existing {kind}: {id}")]
    ImportConflict { kind: &'static str, id: Uuid },
    #[error("archive artifact {id} content does not match its blob hash")]
    ArchiveArtifactHashMismatch { id: Uuid },
    #[error("unsafe blob path in local store: {0}")]
    UnsafeBlobPath(String),
    #[error("archive artifact {id} content byte size does not match archive metadata")]
    ArchiveArtifactSizeMismatch { id: Uuid },
    #[error("archive artifact {id} blob path is not canonical for its content hash")]
    ArchiveArtifactPathMismatch { id: Uuid },
    #[error("archive artifact {id} blob file is not a regular file: {path:?}")]
    ArchiveArtifactNonRegularFile { id: Uuid, path: PathBuf },
    #[error("archive artifact {id} is missing matching blob content")]
    ArchiveArtifactMissingContent { id: Uuid },
    #[error("result event {id} cannot reference a durable payload blob")]
    ResultPayloadBlobUnsupported { id: Uuid },
    #[error("provider event conflict for {provider}/{external_session_id} at index {provider_index}: existing hash {existing_hash}, new hash {new_hash}")]
    ProviderEventConflict {
        provider: String,
        external_session_id: String,
        provider_index: u64,
        existing_hash: String,
        new_hash: String,
    },
    #[error("SQL query is empty")]
    RawSqlEmpty,
    #[error("SQL query contains an interior NUL byte")]
    RawSqlInteriorNul,
    #[error("SQL query must be read-only")]
    RawSqlNotReadOnly,
    #[error("SQL query parameters are not supported")]
    RawSqlHasParameters,
    #[error("SQL query must return at least one column")]
    RawSqlNoColumns,
    #[error("SQL query returned {columns} columns; maximum is {max_columns}")]
    RawSqlTooManyColumns { columns: usize, max_columns: usize },
    #[error("{field} must be between {min} and {max}, got {value}")]
    RawSqlLimitOutOfRange {
        field: &'static str,
        value: usize,
        min: usize,
        max: usize,
    },
    #[error("SQL result preview budget {estimated_bytes} bytes exceeds maximum {max_result_bytes}; lower max_rows, max_columns, or max_value_bytes")]
    RawSqlResultBudgetTooLarge {
        estimated_bytes: usize,
        max_result_bytes: usize,
    },
    #[error("SQL query timed out after {timeout_ms}ms")]
    RawSqlTimedOut { timeout_ms: u64 },
}

pub type Result<T> = std::result::Result<T, StoreError>;
