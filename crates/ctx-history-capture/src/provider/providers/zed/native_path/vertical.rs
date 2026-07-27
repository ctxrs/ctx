use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, FileTouched, Session, SessionEdge, SessionEdgeType, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_MUTATION_UNITS, NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::sqlite::attach_sqlite_complete_content_locator_with_ref,
    compute_payload_hash,
    native_source::NativeLocator,
    provider::importer::{
        provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
        provider_import_session_uuid, provider_path_identity, provider_scoped_source_identity_key,
        provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
        provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

use super::{
    dto::{
        ZedNativeGenerationAuthority, ZedNativeScanOutcome, ZedNativeSession,
        ZedNativeSourceSelection,
    },
    into_capture_error, revalidate_zed_snapshot_revision, scan_zed_nativepath,
    staging::{ZedNativeStaging, ZedStagedEvent, ZedStagedSession},
    ZedNativePathError,
};

mod publication;

use publication::*;

const ZED_NATIVE_CURSOR_VERSION: u32 = 1;
const ZED_NATIVE_CAPTURE_REVISION: u32 = 2;
const ZED_NATIVE_POLICY_REVISION: u32 = 2;
const ZED_PUBLICATION_DOMAIN: &[u8] = b"ctx-zed-nativepath-publication-v1\0";
const ZED_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-zed-nativepath-source-revision-v1\0";
const ZED_RELOCATION_FINGERPRINT_DOMAIN: &[u8] = b"ctx-zed-nativepath-relocation-fingerprint-v1\0";
const ZED_MESSAGE_LOCATOR_KIND: &str = "zed-thread-row-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ZedPublicationPhase {
    Sessions,
    Events,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ZedNativeCursor {
    version: u32,
    provider: String,
    source_format: String,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    raw_source_path: PathBuf,
    source_revision: String,
    #[serde(default)]
    relocation_fingerprint: Option<String>,
    snapshot_revision: String,
    capability_digest: String,
    source_integrity_digest: String,
    core_generation_digest: String,
    generation: u64,
    phase: ZedPublicationPhase,
    position: u64,
    session_count: u64,
    event_count: u64,
    rejection_count: u64,
    terminal: bool,
    retired: bool,
}

struct ZedPublicationContext<'a> {
    path: &'a Path,
    raw_source_path: String,
    source_root: String,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    source_revision: String,
    relocation_fingerprint: String,
    authority: &'a ZedNativeGenerationAuthority,
    adapter: &'a ProviderAdapterContext,
    options: &'a ProviderImportOptions,
}

struct CursorPlan {
    current: Option<SyncCursor>,
    cursor: ZedNativeCursor,
    publish_core: bool,
}

pub(in crate::provider::providers::zed) fn import_zed_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Zed SQLite source must be a regular non-symlink file",
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return retire_missing_zed_source(path, store, &context);
        }
        Err(error) => return Err(error.into()),
    }

    let canonical_path = std::fs::canonicalize(path)?;
    let selection = ZedNativeSourceSelection::exact(&canonical_path)
        .with_inventory_observation_token(options.inventory_observation_token.clone());
    let mut staging = ZedNativeStaging::new().map_err(map_native_error)?;
    let authority = match scan_zed_nativepath(&selection, &mut staging).map_err(map_native_error)? {
        ZedNativeScanOutcome::Complete(authority) => *authority,
        ZedNativeScanOutcome::Incomplete(_) => {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    };
    staging.validate_relationships().map_err(map_native_error)?;

    let raw_source_path = canonical_path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_revision = zed_source_revision(&authority, &options);
    let relocation_fingerprint = zed_relocation_fingerprint(&authority);
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Zed NativePath source has no canonical identity",
    ))?;
    let canonical_source_identity = predict_canonical_source_identity(
        store,
        &context.machine_id,
        &raw_source_path,
        &relocation_fingerprint,
        &proposed_source_identity,
    )?;
    let session_count = staging.session_count().map_err(map_native_error)?;
    let event_count = staging.event_count().map_err(map_native_error)?;
    let rejection_count = staging.rejection_count().map_err(map_native_error)?;
    let plan = cursor_plan(
        store,
        &context,
        &cursor_stream,
        &locator_identity,
        &canonical_source_identity,
        &canonical_path,
        &source_revision,
        &authority,
        session_count,
        event_count,
        rejection_count,
    )?;
    let publication = ZedPublicationContext {
        path: &canonical_path,
        raw_source_path,
        source_root,
        locator_identity,
        cursor_stream,
        canonical_source_identity,
        source_revision,
        relocation_fingerprint,
        authority: &authority,
        adapter: &context,
        options: &options,
    };
    let output_authority = super::output::ZedOutputReplayAuthority::new(
        &publication.canonical_source_identity,
        &publication.source_revision,
        publication.authority,
    );

    if options.import_profile.is_replay_only() {
        if !plan.cursor.terminal
            || plan.cursor.retired
            || plan.cursor.source_revision != publication.source_revision
        {
            if let Some(sink) = options.import_profile.sink() {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "zed_core_not_committed",
                    "Zed output replay requires an exact completed Core generation",
                ));
            }
            return Ok(ProviderImportSummary::default());
        }
        super::output::replay_zed_outputs_or_mark_behind(
            publication.path,
            &staging,
            &output_authority,
            options.import_profile.sink(),
        );
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = if plan.publish_core {
        publish_zed_core(store, &staging, &publication, plan)?
    } else {
        ProviderImportSummary::default()
    };
    let completed = load_native_cursor(store, &context.machine_id, &publication.cursor_stream)?
        .is_some_and(|cursor| {
            cursor.terminal
                && !cursor.retired
                && cursor.source_revision == publication.source_revision
        });
    if completed {
        super::output::replay_zed_outputs_or_mark_behind(
            publication.path,
            &staging,
            &output_authority,
            options.import_profile.sink(),
        );
    } else if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
        summary.work_remaining = true;
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn cursor_plan(
    store: &Store,
    context: &ProviderAdapterContext,
    cursor_stream: &str,
    locator_identity: &str,
    canonical_source_identity: &str,
    path: &Path,
    source_revision: &str,
    authority: &ZedNativeGenerationAuthority,
    session_count: u64,
    event_count: u64,
    rejection_count: u64,
) -> Result<CursorPlan> {
    let current = store.get_sync_cursor(None, &context.machine_id, cursor_stream)?;
    let fresh = || ZedNativeCursor {
        version: ZED_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::Zed.as_str().to_owned(),
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
        locator_identity: locator_identity.to_owned(),
        cursor_stream: cursor_stream.to_owned(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        raw_source_path: path.to_path_buf(),
        source_revision: source_revision.to_owned(),
        relocation_fingerprint: Some(zed_relocation_fingerprint(authority)),
        snapshot_revision: authority.snapshot_revision.clone(),
        capability_digest: authority.capability_digest.clone(),
        source_integrity_digest: authority.source_integrity_digest.clone(),
        core_generation_digest: authority.core_generation_digest.clone(),
        generation: 0,
        phase: if session_count == 0 {
            ZedPublicationPhase::Events
        } else {
            ZedPublicationPhase::Sessions
        },
        position: 0,
        session_count,
        event_count,
        rejection_count,
        terminal: false,
        retired: false,
    };
    let Some(stored) = current.as_ref() else {
        return Ok(CursorPlan {
            current,
            cursor: fresh(),
            publish_core: true,
        });
    };
    let prior = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => decode_cursor(committed.provider_cursor())?,
        Err(_) => {
            if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_some() {
                return Ok(CursorPlan {
                    current,
                    cursor: fresh(),
                    publish_core: true,
                });
            }
            return Err(CaptureError::InvalidPayload(
                "Zed NativePath cursor is neither a committed NativePath cursor nor a released legacy cursor"
                    .to_owned(),
            ));
        }
    };
    validate_cursor_authority(
        &prior,
        cursor_stream,
        locator_identity,
        canonical_source_identity,
        path,
    )?;
    if prior.source_revision == source_revision {
        if prior.session_count != session_count
            || prior.event_count != event_count
            || prior.rejection_count != rejection_count
            || prior.snapshot_revision != authority.snapshot_revision
            || prior.capability_digest != authority.capability_digest
            || prior.source_integrity_digest != authority.source_integrity_digest
            || prior.core_generation_digest != authority.core_generation_digest
            || prior.retired
        {
            return Err(CaptureError::InvalidPayload(
                "Zed NativePath cursor disagrees with exact source authority".to_owned(),
            ));
        }
        return Ok(CursorPlan {
            current,
            publish_core: !prior.terminal,
            cursor: prior,
        });
    }
    let mut cursor = fresh();
    cursor.generation = prior
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Zed NativePath generation overflowed",
        ))?;
    Ok(CursorPlan {
        current,
        cursor,
        publish_core: true,
    })
}

fn validate_cursor_authority(
    cursor: &ZedNativeCursor,
    cursor_stream: &str,
    locator_identity: &str,
    canonical_source_identity: &str,
    path: &Path,
) -> Result<()> {
    let phase_valid = match cursor.phase {
        ZedPublicationPhase::Sessions => {
            cursor.position <= cursor.session_count && !cursor.terminal
        }
        ZedPublicationPhase::Events => cursor.position <= cursor.event_count && !cursor.terminal,
        ZedPublicationPhase::Complete => cursor.terminal,
    };
    if cursor.version != ZED_NATIVE_CURSOR_VERSION
        || cursor.provider != CaptureProvider::Zed.as_str()
        || cursor.source_format != ZED_THREADS_SQLITE_SOURCE_FORMAT
        || cursor.cursor_stream != cursor_stream
        || cursor.locator_identity != locator_identity
        || cursor.canonical_source_identity != canonical_source_identity
        || cursor.raw_source_path != path
        || !phase_valid
    {
        return Err(CaptureError::InvalidPayload(
            "Zed NativePath cursor has inconsistent route or frontier authority".to_owned(),
        ));
    }
    Ok(())
}

fn load_native_cursor(
    store: &Store,
    machine_id: &str,
    cursor_stream: &str,
) -> Result<Option<ZedNativeCursor>> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, cursor_stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    decode_cursor(committed.provider_cursor()).map(Some)
}

fn encode_cursor(cursor: &ZedNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

fn decode_cursor(cursor: &str) -> Result<ZedNativeCursor> {
    serde_json::from_str(cursor).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Zed NativePath cursor: {error}"))
    })
}

fn zed_source_revision(
    authority: &ZedNativeGenerationAuthority,
    options: &ProviderImportOptions,
) -> String {
    let mut digest = Sha256::new();
    digest.update(ZED_SOURCE_REVISION_DOMAIN);
    digest.update(ZED_NATIVE_CAPTURE_REVISION.to_be_bytes());
    digest.update(ZED_NATIVE_POLICY_REVISION.to_be_bytes());
    hash_field(&mut digest, authority.snapshot_revision.as_bytes());
    hash_field(&mut digest, authority.capability_digest.as_bytes());
    hash_field(&mut digest, authority.source_integrity_digest.as_bytes());
    hash_field(&mut digest, authority.core_generation_digest.as_bytes());
    if let Some(token) = options.inventory_observation_token.as_deref() {
        hash_field(&mut digest, token.as_bytes());
    }
    format!("zed-nativepath-sha256-v1:{:x}", digest.finalize())
}

fn zed_relocation_fingerprint(authority: &ZedNativeGenerationAuthority) -> String {
    let mut digest = Sha256::new();
    digest.update(ZED_RELOCATION_FINGERPRINT_DOMAIN);
    digest.update(ZED_NATIVE_CAPTURE_REVISION.to_be_bytes());
    digest.update(ZED_NATIVE_POLICY_REVISION.to_be_bytes());
    hash_field(&mut digest, authority.capability_digest.as_bytes());
    hash_field(&mut digest, authority.source_integrity_digest.as_bytes());
    hash_field(&mut digest, authority.core_generation_digest.as_bytes());
    format!(
        "zed-nativepath-relocation-sha256-v1:{:x}",
        digest.finalize()
    )
}

fn predict_canonical_source_identity(
    store: &Store,
    machine_id: &str,
    raw_source_path: &str,
    relocation_fingerprint: &str,
    proposed: &str,
) -> Result<String> {
    let sources = store.list_capture_sources()?;
    let exact = sources
        .iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::Zed
                && source.descriptor.machine_id == machine_id
                && source.descriptor.source_format.as_deref()
                    == Some(ZED_THREADS_SQLITE_SOURCE_FORMAT)
                && source.descriptor.raw_source_path.as_deref() == Some(raw_source_path)
        })
        .filter_map(|source| source.descriptor.source_identity.clone())
        .collect::<BTreeSet<_>>();
    if exact.len() == 1 {
        return Ok(exact
            .into_iter()
            .next()
            .unwrap_or_else(|| proposed.to_owned()));
    }
    let relocation = sources
        .iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::Zed
                && source.descriptor.machine_id == machine_id
                && source.descriptor.source_format.as_deref()
                    == Some(ZED_THREADS_SQLITE_SOURCE_FORMAT)
                && source
                    .sync
                    .metadata
                    .get("relocation_fingerprint")
                    .and_then(Value::as_str)
                    == Some(relocation_fingerprint)
                && source
                    .descriptor
                    .raw_source_path
                    .as_deref()
                    .is_some_and(prior_source_path_is_missing)
        })
        .filter_map(|source| source.descriptor.source_identity.clone())
        .collect::<BTreeSet<_>>();
    Ok(if relocation.len() == 1 {
        relocation
            .into_iter()
            .next()
            .unwrap_or_else(|| proposed.to_owned())
    } else {
        proposed.to_owned()
    })
}

fn prior_source_path_is_missing(path: &str) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound
    )
}

fn retire_missing_zed_source(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let locator_identity = provider_path_identity(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(current) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Zed SQLite source does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&current.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "missing Zed source has no NativePath route authority to retire".to_owned(),
        )
    })?;
    let mut cursor = decode_cursor(committed.provider_cursor())?;
    validate_cursor_authority(
        &cursor,
        &cursor_stream,
        &locator_identity,
        &cursor.canonical_source_identity.clone(),
        path,
    )?;
    if cursor.retired {
        return Ok(ProviderImportSummary::default());
    }
    cursor.retired = true;
    cursor.terminal = true;
    cursor.phase = ZedPublicationPhase::Complete;
    let transition = NativePathCursorTransition::new(
        Some(current.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            cursor_stream.clone(),
            encode_cursor(&cursor)?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity,
        cursor_stream,
        expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
        expected_source_revision: cursor
            .relocation_fingerprint
            .clone()
            .unwrap_or_else(|| cursor.source_revision.clone()),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: if path
            .parent()
            .is_some_and(|parent| std::fs::symlink_metadata(parent).is_err())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let publication_id = retirement_publication_id(&retirement, transition.next().cursor.as_str());
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
        let changed =
            match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
                NativePathCursorSetClassification::AllExpected => {
                    let disposition = group.retire_provider_source_route(&retirement)?;
                    group.prepare_journal_checkpoint()?;
                    group.publish_cursor_set()?;
                    matches!(
                        disposition,
                        ProviderSourceRouteRetirementDisposition::Retired
                    )
                }
                NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
            };
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        if changed {
            summary.skipped_sessions = 1;
            summary.skipped = 1;
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Zed.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn publication_id(
    context: &ZedPublicationContext<'_>,
    transition: &NativePathCursorTransition,
    sessions: &[ZedStagedSession],
    events: &[ZedStagedEvent],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ZED_PUBLICATION_DOMAIN);
    hash_field(&mut digest, context.source_revision.as_bytes());
    hash_field(&mut digest, context.locator_identity.as_bytes());
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    for session in sessions {
        hash_field(&mut digest, session.session.thread_id.as_bytes());
    }
    for event in events {
        hash_field(&mut digest, event.event.content_hash.as_bytes());
    }
    format!("zed-nativepath-group-sha256-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    next_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-zed-nativepath-route-retirement-v1\0");
    hash_field(&mut digest, retirement.machine_id.as_bytes());
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    hash_field(&mut digest, next_cursor.as_bytes());
    format!(
        "zed-nativepath-retirement-sha256-v1:{:x}",
        digest.finalize()
    )
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn map_native_error(error: ZedNativePathError) -> CaptureError {
    into_capture_error(error)
}
