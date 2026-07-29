use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched,
    Run, RunStatus, RunType, Session, SessionStatus, SyncCursor,
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
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps,
        },
        normalization::{
            provider_capped_json_value, provider_local_preview, provider_policy_body,
            provider_policy_event_text, provider_result_identifier_evidence,
            provider_result_outcome_evidence, provider_timestamp_millis,
        },
    },
    CaptureError, CaptureWorkLimit, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    JUNIE_SESSION_EVENTS_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    assistant::{
        junie_buffer_result_text, junie_merge_buffered_agent_event, junie_step_output_projection,
        JunieAssistantBuffer, JunieOutputOutcome, JunieStepAgg,
    },
    session_tree::{
        bounded_junie_index_meta, junie_provider_session_id, JunieIndexMeta, JunieSessionPath,
    },
    source::JunieSessionObservation,
    MAX_JUNIE_FAILURES, MAX_JUNIE_FAILURE_BYTES, MAX_JUNIE_TRANSIENT_TURN_BYTES,
};

const CURSOR_VERSION: u32 = 1;
const PUBLICATION_REVISION: &str = "junie-nativepath-v1";
const RECORD_SET_KIND: &str = "junie-jsonl-record-set-v1";
const MAX_RECORD_SET_ENTRIES: usize = 64;
const RECORD_SET_DIGEST_DOMAIN: &[u8] = b"ctx-junie-jsonl-record-set-v1\0";
const CORE_PAGE_MAX_ROWS: usize = 48;
const CORE_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 192 * 1024;
const GENERATION_EVENT_STRIDE: u64 = 1_000_000_000;

mod core;
mod cursor;
mod lifecycle;
mod projection;
mod publication;
mod source_backed;

use core::*;
use cursor::*;
use projection::*;
use publication::*;

#[cfg(test)]
use lifecycle::discover;
pub(crate) use lifecycle::import_junie_nativepath;
pub(crate) use source_backed::{
    JunieLocatorResolverV0, JunieSourceBackedEmissionV0, JunieSourceBackedErrorV0,
    JunieSourceBackedResultV0, JunieSourceBackedScannerV0,
};

#[cfg(test)]
mod tests;
