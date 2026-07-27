use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event, EventRole,
    EventType, Fidelity, FileTouched, Run, RunStatus, RunType, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps,
        },
        native_ingestion::{NativeIngestionPage, NativePublicationPage, NativeSourceIdentity},
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ClineTaskJsonImportOptions, ImportProfile,
    OutputOutcome, ProOutputSinkError, ProviderImportSummary, ProviderImportWorkResult, Result,
    RooTaskJsonImportOptions,
};

use super::store_adapter::{
    ClineNativePageAdapter, ClineNativePageAdapterError, ClineNativeStoreCursor,
};
use super::{
    ClineArrayCheckpoint, ClineCatalogCompletion, ClineCatalogIndex, ClineCertifiedPage,
    ClineComponent, ClineComponentObservation, ClineDiscovery, ClineEventComponent, ClineEventKind,
    ClineEventRole, ClineEventRow, ClineFileSourceIdentity, ClineLiveTaskObservation,
    ClineMetadataCheckpoint, ClineNativeItemKey, ClineNativePathError, ClineNativeProfile,
    ClineNativeReader, ClineObservedFileState, ClineSessionRow, ClineTaskCheckpoint,
    ClineTaskIdentity, ClineTaskIdentityOrigin, TaskJsonNativeDialect,
};

const CLINE_TASK_CURSOR_VERSION: u32 = 1;
const TASK_JSON_PACKED_EVENT_INDEX_LIMIT: u64 = 1 << 47;

#[derive(Debug, Error)]
pub(crate) enum ClineNativeVerticalError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Store(#[from] ctx_history_store::StoreError),
    #[error(transparent)]
    Adapter(#[from] ClineNativePageAdapterError),
    #[error(transparent)]
    Source(#[from] ClineNativePathError),
    #[error("Cline NativePath page source identity does not match its certified payload")]
    SourceIdentityMismatch,
    #[error("Cline NativePath component cursor is malformed or inconsistent")]
    CorruptCursor,
    #[error("Cline NativePath component generation is exhausted")]
    GenerationExhausted,
    #[error("Cline NativePath page has retained events but no certified task session")]
    MissingSession,
    #[error("Cline NativePath event index exceeds the canonical packed-index range")]
    EventIndexOverflow,
    #[error("Cline NativePath source changed before Core publication")]
    SourceChanged,
}

struct ClineFreshPublicationContext<'a> {
    options: &'a TaskJsonNativeImportOptions,
    configured_source_root: &'a Path,
    dialect: TaskJsonNativeDialect,
}

struct TaskJsonNativeImportOptions {
    machine_id: String,
    source_path: Option<PathBuf>,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    capture_work_limit: CaptureWorkLimit,
    import_profile: ImportProfile,
}

impl From<ClineTaskJsonImportOptions> for TaskJsonNativeImportOptions {
    fn from(options: ClineTaskJsonImportOptions) -> Self {
        Self {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            import_profile: options.import_profile,
        }
    }
}

impl From<RooTaskJsonImportOptions> for TaskJsonNativeImportOptions {
    fn from(options: RooTaskJsonImportOptions) -> Self {
        Self {
            machine_id: options.machine_id,
            source_path: options.source_path,
            imported_at: options.imported_at,
            history_record_id: options.history_record_id,
            capture_work_limit: options.capture_work_limit,
            import_profile: options.import_profile,
        }
    }
}

struct ResolvedClineSource {
    source_id: Uuid,
    session: Session,
    relocated: bool,
}

struct CorePublicationOutcome {
    summary: ProviderImportSummary,
    relocated_task_identities: Box<[String]>,
}

// Publication classification carries the full atomic cursor transition without heap indirection.
#[allow(clippy::large_enum_variant)]
enum ComponentCursorPlan {
    AlreadyCommitted,
    Publish {
        transition: NativePathCursorTransition,
        generation: u64,
        rejected_records: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClinePersistedObservation {
    component: u8,
    path: PathBuf,
    stamp_token: Option<String>,
    missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineArrayCheckpointWire {
    component: u8,
    observation: ClinePersistedObservation,
    certified_revision_sha256: [u8; 32],
    complete_bytes: u64,
    observed_items: u64,
    retained_rows: u64,
    final_frontier: super::ClinePageFrontier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineSessionRowWire {
    identity: String,
    identity_origin: u8,
    #[serde(default)]
    identity_aliases: Vec<String>,
    title: Option<String>,
    workspace_directory: Option<String>,
    created_at: Option<String>,
    last_modified: Option<String>,
    model_id: Option<String>,
    model_provider: Option<String>,
    tokens_input: Option<u64>,
    tokens_output: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineMetadataCheckpointWire {
    observation: ClinePersistedObservation,
    content_sha256: Option<[u8; 32]>,
    session: ClineSessionRowWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineTaskCheckpointWire {
    version: u32,
    canonical_task_path: PathBuf,
    api_history: Option<ClineArrayCheckpointWire>,
    ui_messages: Option<ClineArrayCheckpointWire>,
    fallback_history: Option<ClineArrayCheckpointWire>,
    task_metadata: ClineMetadataCheckpointWire,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClineRootManifestWire {
    version: u32,
    tasks_root: PathBuf,
    source_root: String,
    task_names: Vec<String>,
}

pub(crate) fn import_cline_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: ClineTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    import_task_json_nativepath_history(path, store, options.into(), TaskJsonNativeDialect::CLINE)
}

pub(crate) fn import_roo_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: RooTaskJsonImportOptions,
) -> Result<ProviderImportSummary> {
    import_task_json_nativepath_history(path, store, options.into(), TaskJsonNativeDialect::ROO)
}

mod checkpoint;
mod coordinator;
mod events;
mod identity;
mod publication;

use checkpoint::*;
use coordinator::*;
use events::*;
pub(super) use identity::*;
use publication::*;
