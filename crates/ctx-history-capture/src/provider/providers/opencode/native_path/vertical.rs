//! Provider-owned NativePath Store publication for the OpenCode SQLite family.
//!
//! The reader owns discovery, snapshotting, parsing, source mutation evidence,
//! and independent output replay. This module is the narrow typed Store leaf;
//! it deliberately does not expose a generic provider record envelope.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence, Event,
    EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, NativePathRetainedSourceEntities,
    NativePathSourceEntityFrontier, NativePathSourceEntityKind, NativePathSourceGenerationKey,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    compute_payload_hash,
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_edge_uuid,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputNativeCursor, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    classify_opencode_native_lifecycle, OpenCodeNativeEvent, OpenCodeNativeEventKind,
    OpenCodeNativeFrontier, OpenCodeNativeGenerationChange, OpenCodeNativePage,
    OpenCodeNativePageLimits, OpenCodeNativePathReader, OpenCodeNativePersistedState,
    OpenCodeNativePhysicalSourceIdentity, OpenCodeNativePriorGeneration, OpenCodeNativeProFrontier,
    OpenCodeNativeProfile, OpenCodeNativePublicationMode, OpenCodeNativeScanPhase,
    OpenCodeNativeScanSummary, OpenCodeNativeSession, OpenCodeNativeSourceSelection,
};
use crate::provider::providers::opencode::OpenCodeSqliteDialect;

mod core;
mod entities;
mod output;
mod routes;
#[cfg(test)]
mod tests;

use self::core::*;
use self::entities::*;
use self::output::*;
use self::routes::*;

const OPENCODE_NATIVE_STORE_CURSOR_VERSION: u32 = 2;
const OPENCODE_NATIVE_PRIOR_STORE_CURSOR_VERSION: u32 = 1;
const OPENCODE_NATIVE_OUTPUT_CURSOR_VERSION: u32 = 1;
const OPENCODE_NATIVE_OUTPUT_PARSER_REVISION: &str = "opencode-family-nativepath-output-v1";
const OPENCODE_NATIVE_SOURCE_REVISION_DOMAIN: &[u8] =
    b"ctx-opencode-family-nativepath-source-revision-v1\0";
const OPENCODE_NATIVE_PUBLICATION_DOMAIN: &[u8] =
    b"ctx-opencode-family-nativepath-publication-v1\0";
const OPENCODE_NATIVE_GENERATION_DOMAIN: &[u8] = b"ctx-opencode-family-nativepath-generation-v1\0";
const OPENCODE_NATIVE_SOURCE_STAGE_DOMAIN: &[u8] =
    b"ctx-opencode-family-nativepath-source-stage-v1\0";
const OPENCODE_NATIVE_RETIREMENT_DOMAIN: &[u8] = b"ctx-opencode-family-nativepath-retirement-v1\0";
const OPENCODE_NATIVE_SOURCE_STAGE_IDS: usize = 64;
const OPENCODE_NATIVE_RETIREMENT_ENTITIES: usize = 64;
const OPENCODE_NATIVE_STORE_PAGE_ROWS: usize = 32;
const OPENCODE_NATIVE_LIFECYCLE_PAGE_BYTES: usize = 16 * 1024;
const OPENCODE_NATIVE_MAX_REJECTION_TEXT_BYTES: usize = 1024;
const OPENCODE_NATIVE_MAX_REJECTION_IDENTITY_BYTES: usize = 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCodeStoredRejection {
    native_identity: String,
    line: usize,
    error: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCodeRetirementFrontier {
    kind: String,
    id: Uuid,
}

impl OpenCodeRetirementFrontier {
    fn from_store(value: NativePathSourceEntityFrontier) -> Self {
        Self {
            kind: value.kind.as_str().to_owned(),
            id: value.id,
        }
    }

    fn to_store(&self) -> Result<NativePathSourceEntityFrontier> {
        let kind = match self.kind.as_str() {
            "session" => NativePathSourceEntityKind::Session,
            "session_edge" => NativePathSourceEntityKind::SessionEdge,
            "run" => NativePathSourceEntityKind::Run,
            "event" => NativePathSourceEntityKind::Event,
            "file_touch" => NativePathSourceEntityKind::FileTouch,
            _ => {
                return Err(CaptureError::InvalidPayload(
                    "OpenCode NativePath retirement frontier is invalid".to_owned(),
                ));
            }
        };
        Ok(NativePathSourceEntityFrontier { kind, id: self.id })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum OpenCodeGenerationPhase {
    #[default]
    Scan,
    StageSources {
        after: Option<Uuid>,
    },
    Retire {
        after: Option<OpenCodeRetirementFrontier>,
    },
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenCodeNativeStoreCursor {
    version: u32,
    provider: String,
    source_format: String,
    selected_path: PathBuf,
    cursor_path_identity: String,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    generation: u64,
    rejected_records: u64,
    #[serde(default)]
    rejections: Vec<OpenCodeStoredRejection>,
    frontier: OpenCodeNativeFrontier,
    #[serde(default)]
    generation_phase: OpenCodeGenerationPhase,
    route_retired: bool,
    completed_state: Option<OpenCodeNativePersistedState>,
    pending_state: OpenCodeNativePersistedState,
}

// The decoded native cursor stays inline so cursor-state handling remains allocation-free.
#[allow(clippy::large_enum_variant)]
enum StoredCursor {
    None,
    Native {
        stored: SyncCursor,
        cursor: OpenCodeNativeStoreCursor,
    },
    Released {
        stored: SyncCursor,
    },
}

struct OpenCodePublicationContext<'a> {
    dialect: &'a OpenCodeSqliteDialect,
    adapter: &'a ProviderAdapterContext,
    options: &'a ProviderImportOptions,
    selected_path: &'a Path,
    raw_source_path: String,
    source_root: String,
    cursor_path_identity: String,
    locator_identity: String,
    cursor_stream: String,
    source_revision: String,
    canonical_source_identity: String,
    generation: u64,
    replacement: bool,
    current_state: OpenCodeNativePersistedState,
}

pub(in crate::provider::providers::opencode) fn import_opencode_nativepath(
    path: &Path,
    store: &mut Store,
    mut adapter: ProviderAdapterContext,
    options: ProviderImportOptions,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderImportSummary> {
    adapter.source_path = Some(path.to_path_buf());
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.file_type().is_file() => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason:
                    "OpenCode-family SQLite source component must be a regular non-symlink file",
            });
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return import_missing_source(path, store, &adapter, &options, dialect);
        }
        Err(error) => return Err(error.into()),
    }

    let selected_path = fs::canonicalize(path)?;
    let cursor_path_identity = provider_path_identity(&selected_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.source_format,
        &cursor_path_identity,
    );
    let stored = load_store_cursor(store, &adapter.machine_id, &cursor_stream, dialect)?;
    let prior_completed = match &stored {
        StoredCursor::Native { cursor, .. } if !cursor.route_retired => {
            Some(cursor_lifecycle_state(cursor))
        }
        StoredCursor::None | StoredCursor::Released { .. } | StoredCursor::Native { .. } => None,
    };

    let selection = OpenCodeNativeSourceSelection::exact(&selected_path)
        .with_inventory_observation_token(options.inventory_observation_token.clone());
    let reader = OpenCodeNativePathReader::acquire_for_dialect(selection, dialect)?;
    let current_summary = scan_current_summary(&reader, prior_completed.as_ref())?;
    let current_state = current_summary.persisted_state();
    let source_revision = source_revision(dialect, &current_summary);
    let physical_locator_identity = sqlite_locator_identity(
        &cursor_path_identity,
        &current_summary.physical_source_identity,
    )?;
    let previous = prior_completed
        .clone()
        .map(OpenCodeNativePriorGeneration::from_persisted)
        .into_iter()
        .collect::<Vec<_>>();
    let plan = classify_opencode_native_lifecycle(&previous, &current_summary)?;

    let raw_source_path = selected_path.display().to_string();
    let source_root = adapter
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let default_source_identity = provider_source_identity(
        dialect.provider,
        dialect.source_format,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "OpenCode NativePath source has no canonical identity",
    ))?;
    let replacement = matches!(
        plan.change,
        OpenCodeNativeGenerationChange::Rewrite
            | OpenCodeNativeGenerationChange::Rewind
            | OpenCodeNativeGenerationChange::RewriteAndRewind
            | OpenCodeNativeGenerationChange::Replacement
    );
    let locator_identity = if replacement {
        sqlite_generation_locator_identity(&physical_locator_identity, &source_revision)
    } else if let StoredCursor::Native { cursor, .. } = &stored {
        if !cursor.route_retired
            && cursor.pending_state.physical_source_identity
                == current_summary.physical_source_identity
        {
            cursor.locator_identity.clone()
        } else {
            physical_locator_identity
        }
    } else {
        physical_locator_identity
    };
    let relocated_source_identity = prior_relocation_identity(
        store,
        dialect,
        &adapter.machine_id,
        &source_revision,
        &raw_source_path,
    )?;
    let canonical_source_identity = match &stored {
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.provider == dialect.provider.as_str()
                && cursor.source_format == dialect.source_format =>
        {
            cursor.canonical_source_identity.clone()
        }
        _ if relocated_source_identity.is_some() => {
            relocated_source_identity.expect("guarded OpenCode relocation identity is present")
        }
        _ => default_source_identity,
    };
    let generation = next_generation(&stored, &current_state, replacement)?;
    let context = OpenCodePublicationContext {
        dialect,
        adapter: &adapter,
        options: &options,
        selected_path: &selected_path,
        raw_source_path,
        source_root,
        cursor_path_identity,
        locator_identity,
        cursor_stream,
        source_revision,
        canonical_source_identity,
        generation,
        replacement,
        current_state,
    };

    if options.import_profile.is_replay_only() {
        verify_committed_core(&stored, &context)?;
        replay_outputs_or_mark_behind(&context, options.import_profile.sink().map(AsRef::as_ref));
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = if plan.publication == OpenCodeNativePublicationMode::ObservationOnly
        && stored_is_terminal_for(&stored, &context.current_state)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        summary
    } else {
        publish_core(store, &reader, &stored, &context)?
    };
    replay_outputs_or_mark_behind(&context, options.import_profile.sink().map(AsRef::as_ref));
    if summary.work_result() == ProviderImportWorkResult::NoOp && summary.failed == 0 {
        summary.skipped_sessions =
            usize::try_from(current_summary.metrics.native_sessions).unwrap_or(usize::MAX);
        summary.skipped = summary.skipped.saturating_add(summary.skipped_sessions);
    }
    Ok(summary)
}

fn scan_current_summary(
    reader: &OpenCodeNativePathReader,
    prior: Option<&OpenCodeNativePersistedState>,
) -> Result<OpenCodeNativeScanSummary> {
    let mut scanner = match prior {
        Some(prior) if prior.is_supported() => reader.scanner_with_profile_and_prior(
            OpenCodeNativeProfile::CoreOnly,
            OpenCodeNativePageLimits::default(),
            prior,
        )?,
        _ => reader.scanner(OpenCodeNativePageLimits::default())?,
    };
    while scanner.next_page()?.is_some() {}
    scanner.finish()
}

fn load_store_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
    dialect: &OpenCodeSqliteDialect,
) -> Result<StoredCursor> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(StoredCursor::None);
    };
    if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
        let cursor: OpenCodeNativeStoreCursor = serde_json::from_str(committed.provider_cursor())
            .map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "{} NativePath cursor is malformed: {error}",
                dialect.display_name
            ))
        })?;
        validate_store_cursor(&cursor, dialect, machine_id, stream)?;
        return Ok(StoredCursor::Native { stored, cursor });
    }
    match CertifiedProviderCursor::decode_if_certified(&stored.cursor)? {
        Some(_) => Ok(StoredCursor::Released { stored }),
        None => Err(CaptureError::InvalidPayload(format!(
            "{} cursor is neither NativePath nor a released migration cursor",
            dialect.display_name
        ))),
    }
}

fn validate_store_cursor(
    cursor: &OpenCodeNativeStoreCursor,
    dialect: &OpenCodeSqliteDialect,
    machine_id: &str,
    stream: &str,
) -> Result<()> {
    let supported_version = matches!(
        cursor.version,
        OPENCODE_NATIVE_PRIOR_STORE_CURSOR_VERSION | OPENCODE_NATIVE_STORE_CURSOR_VERSION
    );
    let valid_rejection_ledger = cursor.rejections.len()
        <= crate::summaries::MAX_RETAINED_PROVIDER_FAILURES
        && u64::try_from(cursor.rejections.len()).unwrap_or(u64::MAX) <= cursor.rejected_records
        && cursor.rejections.iter().all(|rejection| {
            !rejection.native_identity.is_empty()
                && rejection.native_identity.len() <= OPENCODE_NATIVE_MAX_REJECTION_IDENTITY_BYTES
                && !rejection.error.is_empty()
                && rejection.error.len() <= OPENCODE_NATIVE_MAX_REJECTION_TEXT_BYTES
        });
    let valid_generation_phase = cursor.version == OPENCODE_NATIVE_PRIOR_STORE_CURSOR_VERSION
        || match &cursor.generation_phase {
            OpenCodeGenerationPhase::Scan => cursor.completed_state.is_none(),
            OpenCodeGenerationPhase::StageSources { .. }
            | OpenCodeGenerationPhase::Retire { .. } => {
                cursor.frontier.phase == OpenCodeNativeScanPhase::Complete
                    && cursor.rejected_records == 0
                    && cursor.completed_state.is_none()
            }
            OpenCodeGenerationPhase::Complete => {
                cursor.frontier.phase == OpenCodeNativeScanPhase::Complete
                    && cursor.rejected_records == 0
                    && cursor.completed_state.is_some()
            }
        };
    if !supported_version
        || !valid_rejection_ledger
        || !valid_generation_phase
        || cursor.provider != dialect.provider.as_str()
        || cursor.source_format != dialect.source_format
        || cursor.selected_path.as_os_str().is_empty()
        || cursor.cursor_path_identity.is_empty()
        || cursor.locator_identity.is_empty()
        || cursor.canonical_source_identity.is_empty()
        || cursor.source_revision.is_empty()
        || cursor.pending_state.selected_path != cursor.selected_path
        || !cursor.pending_state.is_supported_cursor_migration_source()
        || cursor
            .completed_state
            .as_ref()
            .is_some_and(|state| !state.is_supported_cursor_migration_source())
        || provider_source_cursor_stream_for_path(
            dialect.provider,
            dialect.source_format,
            &cursor.cursor_path_identity,
        ) != stream
        || machine_id.is_empty()
    {
        return Err(CaptureError::InvalidPayload(format!(
            "{} NativePath cursor is inconsistent",
            dialect.display_name
        )));
    }
    Ok(())
}

fn cursor_lifecycle_state(cursor: &OpenCodeNativeStoreCursor) -> OpenCodeNativePersistedState {
    if cursor.version == OPENCODE_NATIVE_STORE_CURSOR_VERSION
        && !matches!(&cursor.generation_phase, OpenCodeGenerationPhase::Complete)
    {
        return cursor.pending_state.clone();
    }
    cursor
        .completed_state
        .clone()
        .unwrap_or_else(|| cursor.pending_state.clone())
}

fn next_generation(
    stored: &StoredCursor,
    current: &OpenCodeNativePersistedState,
    replacement: bool,
) -> Result<u64> {
    let StoredCursor::Native { cursor, .. } = stored else {
        return Ok(0);
    };
    if !replacement && !cursor.route_retired && same_generation(&cursor.pending_state, current) {
        return Ok(cursor.generation);
    }
    cursor
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "OpenCode NativePath generation overflowed",
        ))
}

fn same_generation(
    left: &OpenCodeNativePersistedState,
    right: &OpenCodeNativePersistedState,
) -> bool {
    left.source_generation_digest == right.source_generation_digest
        && left.capability_digest == right.capability_digest
        && left.semantic_digest == right.semantic_digest
        && left.schema_family == right.schema_family
        && left.parser_revision == right.parser_revision
        && left.policy_revision == right.policy_revision
}

fn stored_is_terminal_for(stored: &StoredCursor, current: &OpenCodeNativePersistedState) -> bool {
    matches!(
        stored,
        StoredCursor::Native { cursor, .. }
            if !cursor.route_retired
                && cursor.rejected_records == 0
                && cursor.frontier.phase == OpenCodeNativeScanPhase::Complete
                && (cursor.version == OPENCODE_NATIVE_PRIOR_STORE_CURSOR_VERSION
                    || matches!(
                        &cursor.generation_phase,
                        OpenCodeGenerationPhase::Complete
                    ))
                && cursor.completed_state.as_ref().is_some_and(|state| same_generation(state, current))
    )
}

fn verify_committed_core(
    stored: &StoredCursor,
    context: &OpenCodePublicationContext<'_>,
) -> Result<()> {
    if !stored_is_terminal_for(stored, &context.current_state) {
        return Err(CaptureError::InvalidPayload(format!(
            "{} output replay requires exact committed NativePath Core",
            context.dialect.display_name
        )));
    }
    Ok(())
}
