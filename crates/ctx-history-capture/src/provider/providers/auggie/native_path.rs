use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    ContentRef, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncCursor,
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
    common::io::{
        ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    },
    complete_content::{
        attach_verified_content_locator, structured::STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, AUGGIE_SESSION_JSON_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    auggie_entry_time, auggie_event, auggie_request_text, auggie_response_text, AuggieEvent,
    AuggieEventInput, AuggieSessionData,
};

mod lifecycle;
mod model;
mod output;
mod parse;
mod publication;
mod source;
pub(super) mod source_backed;

pub(crate) use lifecycle::import_auggie_sessions_nativepath;
#[cfg(test)]
pub(super) use lifecycle::record_auggie_source_parse_error;
use model::*;
use output::*;
use parse::*;
use publication::*;
use source::*;
