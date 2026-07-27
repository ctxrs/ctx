use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, Cursor},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    CtxHistoryJsonlEdgeRecord, CtxHistoryJsonlEventRecord, CtxHistoryJsonlFileTouchRecord,
    CtxHistoryJsonlRecord, CtxHistoryJsonlSessionRecord, CtxHistoryJsonlSourceRecord, Event,
    EventType, Fidelity, FileTouched, ProviderSourceTrust, Run, RunStatus, RunType, Session,
    SessionEdge, SessionEdgeType, SyncCursor, CTX_HISTORY_JSONL_V1_SCHEMA_VERSION,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    NativePathRetainedSourceEntities, NativePathSourceEntityFrontier, NativePathSourceEntityKind,
    NativePathSourceGenerationKey, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    common::io::{
        ensure_regular_provider_transcript_file, read_provider_jsonl_record_or_skip_oversized,
    },
    complete_content::{VerifiedContentLocatorsV1, VERIFIED_CONTENT_LOCATORS_METADATA_KEY},
    compute_payload_hash,
    provider::{
        importer::{
            compact_provider_result_payload, provider_edge_uuid,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_session_uuid, provider_source_identity, provider_source_root,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, CustomHistoryJsonlV1ImportOptions,
    OutputAssociations, OutputCommandContext, OutputNativeCoordinate, OutputObservationKind,
    OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity, OutputSourceLocator,
    ProOutputObservation, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportSummary,
    ProviderImportWorkResult, Result,
};

use super::{
    custom_history_effective_raw_source_path, custom_history_internal_session_id,
    custom_history_jsonl_v1_cursor_stream, custom_history_key, custom_history_metadata,
    push_provider_import_failure, reject_invalid_custom_history_references,
    retain_custom_history_content_sessions, validate_custom_history_identifier,
    validate_custom_source_record,
};

mod lifecycle;
mod model;
mod output;
mod projection;
mod publication;
mod reader;

use self::{lifecycle::*, model::*, output::*, projection::*, publication::*};
pub(super) use publication::decode_released_or_native_upstream_cursor;
pub(crate) use reader::{
    import_custom_history_nativepath, import_custom_history_nativepath_reader,
    validate_custom_history_nativepath, validate_custom_history_nativepath_reader,
};
