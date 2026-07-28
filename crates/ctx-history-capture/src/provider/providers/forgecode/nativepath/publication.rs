mod events;
use events::{generation_source_id, publish_events, publish_touches};

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Event,
    Fidelity, FileTouched, ProviderSourceTrust, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{VerifiedContentLocatorsV1, VERIFIED_CONTENT_LOCATORS_METADATA_KEY},
    compute_payload_hash,
    provider::importer::{
        compact_provider_result_payload, provider_event_import_identity_with_exact_legacy_source,
        provider_file_touch_import_id, provider_import_session_uuid, provider_path_identity,
        provider_scoped_source_identity_key, provider_scoped_source_uuid, provider_session_uuid,
        provider_source_cursor_stream_for_path, provider_source_identity, provider_source_root,
        provider_sync_metadata, timestamps, CertifiedProviderCursor, ProviderEventImportIdentity,
    },
    stable_capture_uuid, CaptureError, ProviderAdapterContext, ProviderImportOptions,
    ProviderImportSummary, ProviderImportWorkResult, Result, FORGECODE_SQLITE_SOURCE_FORMAT,
};

use super::source::{
    frontier_bytes, ForgeCodeConversationRow, ForgeCodeFrontier, ForgeCodeMissingSource,
    ForgeCodePage, ForgeCodeSourceObservation, FORGECODE_NATIVE_PARSER_REVISION,
    FORGECODE_NATIVE_POLICY_REVISION,
};

const FORGECODE_CURSOR_VERSION: u32 = 1;
const FORGECODE_RELEASED_CAPTURE_REVISION: u32 = 1;
const FORGECODE_RELEASED_POLICY_REVISION: u32 = 5;
const FORGECODE_RELEASED_POSITION_KIND: &str = "forgecode-conversation-rowid-v1";
const FORGECODE_PUBLICATION_DOMAIN: &[u8] = b"ctx-forgecode-nativepath-publication-v1\0";
const FORGECODE_RETIREMENT_DOMAIN: &[u8] = b"ctx-forgecode-nativepath-retirement-v1\0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeCodeCursorWire {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    source_revision: String,
    canonical_source_identity: String,
    raw_source_path: String,
    frontier: ForgeCodeFrontier,
    terminal: bool,
    generation: u64,
    rejected_records: u64,
}

pub(super) struct ForgeCodeCoreStart {
    pub(super) frontier: ForgeCodeFrontier,
    pub(super) terminal: bool,
    pub(super) generation: u64,
    pub(super) source_changed: bool,
}

enum StoredCursorKind {
    Native(ForgeCodeCursorWire),
    ReleasedLegacy {
        source_revision: Option<String>,
        rejected_records: u64,
    },
}

struct CursorPlan {
    expected_cursor: Option<String>,
    prior_rejected_records: u64,
}

pub(super) fn load_core_start(
    store: &Store,
    machine_id: &str,
    source: &ForgeCodeSourceObservation,
) -> Result<ForgeCodeCoreStart> {
    let stream = cursor_stream(&source.canonical_path)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(ForgeCodeCoreStart {
            frontier: ForgeCodeFrontier::initial(),
            terminal: false,
            generation: 0,
            source_changed: false,
        });
    };
    match decode_stored_cursor(&stored.cursor)? {
        StoredCursorKind::Native(cursor)
            if cursor.source_revision == source.source_revision
                && cursor.raw_source_path == source.canonical_path.display().to_string() =>
        {
            Ok(ForgeCodeCoreStart {
                frontier: cursor.frontier,
                terminal: cursor.terminal,
                generation: cursor.generation,
                source_changed: false,
            })
        }
        StoredCursorKind::ReleasedLegacy {
            source_revision, ..
        } if source_revision.as_deref() == Some(legacy_source_revision(source).as_str()) => {
            Ok(ForgeCodeCoreStart {
                // The released row frontier certifies the migration CAS, but
                // replay starts at zero so every legacy conversation source
                // receives a NativePath route binding.
                frontier: ForgeCodeFrontier::initial(),
                terminal: false,
                generation: 0,
                source_changed: false,
            })
        }
        StoredCursorKind::Native(_) | StoredCursorKind::ReleasedLegacy { .. } => {
            Ok(ForgeCodeCoreStart {
                frontier: ForgeCodeFrontier::initial(),
                terminal: false,
                generation: match decode_stored_cursor(&stored.cursor)? {
                    StoredCursorKind::Native(cursor) => cursor.generation,
                    StoredCursorKind::ReleasedLegacy { .. } => 0,
                },
                source_changed: true,
            })
        }
    }
}

pub(super) fn generation_for_current_source(
    committed_store: &Store,
    source: &ForgeCodeSourceObservation,
    context: &ProviderAdapterContext,
    start: &ForgeCodeCoreStart,
) -> Result<u64> {
    if !start.source_changed {
        return Ok(start.generation);
    }
    let raw_source_path = source.canonical_path.display().to_string();
    let source_root = context.source_root_display().or_else(|| {
        source
            .canonical_path
            .parent()
            .map(|path| path.display().to_string())
    });
    let canonical_source_identity = proposed_source_identity(
        source_root.as_deref(),
        &raw_source_path,
        &source.schema_fingerprint,
    )?;
    let mut scanner = super::source::ForgeCodeScanner::new(
        source.clone(),
        ForgeCodeFrontier::initial(),
        context.clone(),
        false,
    )?;
    while let Some(page) = scanner.next_page()? {
        let Some(row) = page.row.as_ref() else {
            continue;
        };
        let source_id = generation_source_id(
            committed_store,
            &context.machine_id,
            &raw_source_path,
            &canonical_source_identity,
            &row.conversation_id,
            start.generation,
        )?;
        let legacy_session =
            provider_session_uuid(CaptureProvider::ForgeCode, &row.conversation_id)
                == provider_import_session_uuid(
                    committed_store,
                    CaptureProvider::ForgeCode,
                    &row.conversation_id,
                    source_id,
                    Some(&canonical_source_identity),
                )?;
        for retained in &page.events {
            let event_hash = retained
                .event
                .provider_event_hash
                .clone()
                .unwrap_or(compute_payload_hash(&retained.event.payload)?);
            let identity = provider_event_import_identity_with_exact_legacy_source(
                committed_store,
                CaptureProvider::ForgeCode,
                &row.conversation_id,
                source_id,
                retained.provider_event_index,
                retained.provider_event_index,
                &event_hash,
                None,
                Some(retained.provider_event_index),
                legacy_session,
            )?;
            match committed_store.get_event(identity.id) {
                Ok(existing) if existing.dedupe_key.as_deref() != Some(&identity.dedupe_key) => {
                    return start.generation.checked_add(1).ok_or_else(|| {
                        CaptureError::InvalidPayload(
                            "ForgeCode NativePath generation overflowed".to_owned(),
                        )
                    });
                }
                Ok(_) | Err(StoreError::NotFound(_)) => {}
                Err(error) => return Err(CaptureError::Store(error)),
            }
        }
    }
    Ok(start.generation)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    source: &ForgeCodeSourceObservation,
    source_root: Option<&str>,
    page: &ForgeCodePage,
    generation: u64,
) -> Result<ProviderImportSummary> {
    source.database.revalidate()?;
    let raw_source_path = source.canonical_path.display().to_string();
    let proposed_source_identity =
        proposed_source_identity(source_root, &raw_source_path, &source.schema_fingerprint)?;
    let stream = cursor_stream(&source.canonical_path)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(stored.as_ref(), source, page, generation)?;
    let Some(plan) = plan else {
        let mut summary = page_summary(page);
        summary.skipped_events = summary.skipped_events.saturating_add(page.events.len());
        summary.skipped = summary.skipped.saturating_add(page.events.len());
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    };
    let rejected_records = plan
        .prior_rejected_records
        .saturating_add(u64::try_from(page.rejections.len()).unwrap_or(u64::MAX));
    let next_wire = ForgeCodeCursorWire {
        version: FORGECODE_CURSOR_VERSION,
        parser_revision: FORGECODE_NATIVE_PARSER_REVISION,
        policy_revision: FORGECODE_NATIVE_POLICY_REVISION,
        source_revision: source.source_revision.clone(),
        canonical_source_identity: proposed_source_identity.clone(),
        raw_source_path: raw_source_path.clone(),
        frontier: page.next_frontier.clone(),
        terminal: page.terminal,
        generation,
        rejected_records,
    };
    let next_cursor = provider_sync_cursor(
        &context.machine_id,
        stream.clone(),
        serde_json::to_string(&next_wire)?,
        context.imported_at,
    );
    let transition = NativePathCursorTransition::new(plan.expected_cursor, next_cursor);
    let publication_id = publication_id(source, page, &transition)?;
    let accounting = NativePathGroupAccounting::new(1, 1, page.retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = page_summary(page);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let locator_identity = provider_path_identity(&source.canonical_path)?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::ForgeCode,
            source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = page_summary(page);
    let (source_id, session) = resolve_source_and_session(
        committed_store,
        &mut group,
        context,
        import_options,
        source,
        source_root,
        &raw_source_path,
        &resolution.canonical_source_identity,
        page.row.as_ref(),
        generation,
        &mut summary,
    )?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    if let (Some(row), Some(session)) = (page.row.as_ref(), session.as_ref()) {
        let event_ids = publish_events(
            committed_store,
            &mut group,
            context,
            import_options,
            source_id,
            session,
            row,
            page,
            &mut summary,
        )?;
        publish_touches(
            committed_store,
            &mut group,
            import_options,
            source_id,
            session,
            page,
            &event_ids,
        )?;
    }

    source.database.revalidate()?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn classify_cursor(
    stored: Option<&SyncCursor>,
    source: &ForgeCodeSourceObservation,
    page: &ForgeCodePage,
    generation: u64,
) -> Result<Option<CursorPlan>> {
    let Some(stored) = stored else {
        if generation != 0 || page.expected_frontier != ForgeCodeFrontier::initial() {
            return Err(corrupt_cursor());
        }
        return Ok(Some(CursorPlan {
            expected_cursor: None,
            prior_rejected_records: 0,
        }));
    };
    match decode_stored_cursor(&stored.cursor)? {
        StoredCursorKind::ReleasedLegacy {
            source_revision: _,
            rejected_records,
        } => {
            if generation != 0 {
                return Err(corrupt_cursor());
            }
            if page.expected_frontier != ForgeCodeFrontier::initial() {
                return Err(corrupt_cursor());
            }
            Ok(Some(CursorPlan {
                expected_cursor: Some(stored.cursor.clone()),
                prior_rejected_records: rejected_records,
            }))
        }
        StoredCursorKind::Native(prior) => {
            if prior.source_revision == source.source_revision {
                if generation != prior.generation {
                    return Err(corrupt_cursor());
                }
                if prior.frontier > page.next_frontier
                    || (prior.frontier == page.next_frontier && (prior.terminal || !page.terminal))
                {
                    return Ok(None);
                }
                if prior.frontier != page.expected_frontier {
                    return Err(corrupt_cursor());
                }
                return Ok(Some(CursorPlan {
                    expected_cursor: Some(stored.cursor.clone()),
                    prior_rejected_records: prior.rejected_records,
                }));
            }
            if page.expected_frontier != ForgeCodeFrontier::initial() {
                return Err(corrupt_cursor());
            }
            let next_generation = prior.generation.checked_add(1).ok_or_else(|| {
                CaptureError::InvalidPayload(
                    "ForgeCode NativePath generation overflowed".to_owned(),
                )
            })?;
            if generation != prior.generation && generation != next_generation {
                return Err(corrupt_cursor());
            }
            Ok(Some(CursorPlan {
                expected_cursor: Some(stored.cursor.clone()),
                prior_rejected_records: 0,
            }))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_source_and_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    import_options: &ProviderImportOptions,
    source: &ForgeCodeSourceObservation,
    source_root: Option<&str>,
    raw_source_path: &str,
    canonical_source_identity: &str,
    row: Option<&ForgeCodeConversationRow>,
    generation: u64,
    summary: &mut ProviderImportSummary,
) -> Result<(Uuid, Option<Session>)> {
    let source_id = if let Some(row) = row {
        generation_source_id(
            committed_store,
            &context.machine_id,
            raw_source_path,
            canonical_source_identity,
            &row.conversation_id,
            generation,
        )?
    } else {
        stable_capture_uuid(
            &format!("forgecode-nativepath-source:{canonical_source_identity}:{raw_source_path}"),
            "source",
        )
    };
    let started_at = row
        .map(|row| {
            super::super::event::forgecode_timestamp(Some(&row.created_at), context.imported_at)
        })
        .unwrap_or(context.imported_at);
    let ended_at = row.and_then(|row| {
        row.updated_at
            .as_deref()
            .map(|updated| super::super::event::forgecode_timestamp(Some(updated), started_at))
    });
    group.upsert_capture_source(&CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::ForgeCode,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(FORGECODE_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: source_root.map(str::to_owned),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: row.map(|row| row.conversation_id.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.map(|row| &row.conversation_id),
                "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderNative,
                "source_identity": canonical_source_identity,
                "source_revision": source.source_revision,
                "source_identity_key": row.map(|row| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::ForgeCode,
                        &row.conversation_id,
                        FORGECODE_SQLITE_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
                "adapter": "forgecode-nativepath",
                "sqlite_user_version": source.user_version,
                "schema_fingerprint": source.schema_fingerprint,
                "upstream_tables": ["conversations"],
            }),
        ),
    })?;
    let Some(row) = row else {
        return Ok((source_id, None));
    };
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::ForgeCode,
        &row.conversation_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    let session = Session {
        id: session_id,
        history_record_id: import_options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::ForgeCode,
        external_session_id: Some(row.conversation_id.clone()),
        external_agent_id: row.initiator.clone(),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.conversation_id,
                "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderNative,
                "metadata": {
                    "conversation_id": row.conversation_id,
                    "title": row.title,
                    "workspace_id": row.workspace_id,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                    "context_message_count": row.context_message_count,
                    "initiator": row.initiator,
                    "context": row.context_metadata,
                    "context_messages_retention": "omitted_from_core_session_metadata",
                    "metrics": row.metrics_metadata,
                }
            }),
        ),
    };
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok((source_id, Some(session)))
}

fn page_summary(page: &ForgeCodePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    for rejection in &page.rejections {
        summary.record_failure(rejection.clone());
    }
    summary
}

pub(super) fn proposed_source_identity(
    source_root: Option<&str>,
    raw_source_path: &str,
    schema_fingerprint: &str,
) -> Result<String> {
    provider_source_identity(
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        source_root,
        Some(raw_source_path),
        None,
        &json!({ "schema_fingerprint": schema_fingerprint }),
    )
    .ok_or(CaptureError::SystemInvariant(
        "ForgeCode NativePath source has no canonical identity",
    ))
}

pub(super) fn verify_core_page_committed(
    store: &Store,
    machine_id: &str,
    source: &ForgeCodeSourceObservation,
    page: &ForgeCodePage,
) -> Result<()> {
    let stream = cursor_stream(&source.canonical_path)?;
    let stored = store
        .get_sync_cursor(None, machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "ForgeCode output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let StoredCursorKind::Native(prior) = decode_stored_cursor(&stored.cursor)? else {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode output replay cannot use a released legacy Core cursor".to_owned(),
        ));
    };
    if prior.source_revision != source.source_revision
        || prior.raw_source_path != source.canonical_path.display().to_string()
        || prior.frontier < page.next_frontier
        || (page.terminal && prior.frontier == page.next_frontier && !prior.terminal)
    {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn cursor_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        &identity,
    ))
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: chrono::DateTime<chrono::Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::ForgeCode.as_str(),
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

fn decode_stored_cursor(encoded: &str) -> Result<StoredCursorKind> {
    match decode_native_path_committed_cursor(encoded) {
        Ok(committed) => {
            let wire: ForgeCodeCursorWire =
                serde_json::from_str(committed.provider_cursor()).map_err(|_| corrupt_cursor())?;
            if wire.version != FORGECODE_CURSOR_VERSION
                || wire.parser_revision != FORGECODE_NATIVE_PARSER_REVISION
                || wire.policy_revision != FORGECODE_NATIVE_POLICY_REVISION
            {
                return Err(corrupt_cursor());
            }
            Ok(StoredCursorKind::Native(wire))
        }
        Err(_) => {
            let legacy = CertifiedProviderCursor::decode_if_certified(encoded)?;
            let Some(legacy) = legacy else {
                return Ok(StoredCursorKind::ReleasedLegacy {
                    source_revision: None,
                    rejected_records: 0,
                });
            };
            if legacy.parser_revision() == FORGECODE_RELEASED_CAPTURE_REVISION
                && legacy.policy_revision() == FORGECODE_RELEASED_POLICY_REVISION
            {
                decode_released_frontier(legacy.native_position())?;
            }
            Ok(StoredCursorKind::ReleasedLegacy {
                source_revision: Some(legacy.source_revision().to_owned()),
                rejected_records: legacy.rejected_records(),
            })
        }
    }
}

fn decode_released_frontier(
    position: &crate::native_source::NativePosition,
) -> Result<Option<ForgeCodeFrontier>> {
    if position.kind() != FORGECODE_RELEASED_POSITION_KIND {
        return Err(corrupt_cursor());
    }
    if position.value() == [0] {
        return Ok(Some(ForgeCodeFrontier::initial()));
    }
    let bytes: [u8; 17] = position.value().try_into().map_err(|_| corrupt_cursor())?;
    if bytes[0] != 1 {
        return Err(corrupt_cursor());
    }
    let ordered_rowid = u64::from_be_bytes(bytes[9..17].try_into().map_err(|_| corrupt_cursor())?);
    Ok(Some(ForgeCodeFrontier {
        rowid: Some((ordered_rowid ^ (1_u64 << 63)) as i64),
        next_message: 0,
        row_complete: true,
    }))
}

pub(in crate::provider::providers::forgecode) fn legacy_source_revision(
    source: &ForgeCodeSourceObservation,
) -> String {
    format!(
        "forgecode-sqlite-snapshot-v1:capture={FORGECODE_RELEASED_CAPTURE_REVISION};policy={FORGECODE_RELEASED_POLICY_REVISION};schema={};{}",
        source.schema_fingerprint,
        source.database.revision_component(),
    )
}

fn publication_id(
    source: &ForgeCodeSourceObservation,
    page: &ForgeCodePage,
    transition: &NativePathCursorTransition,
) -> Result<String> {
    let mut digest = Sha256::new();
    digest.update(FORGECODE_PUBLICATION_DOMAIN);
    digest.update(source.source_revision.as_bytes());
    digest.update(frontier_bytes(&page.expected_frontier)?);
    digest.update(frontier_bytes(&page.next_frontier)?);
    digest.update([u8::from(page.terminal)]);
    for event in &page.events {
        digest.update(event.provider_event_index.to_le_bytes());
        digest.update(compute_payload_hash(&event.event.payload)?.as_bytes());
    }
    for touch in &page.touches {
        digest.update(serde_json::to_vec(touch)?);
    }
    for rejection in &page.rejections {
        digest.update(rejection.line.to_le_bytes());
        digest.update(rejection.error.as_bytes());
    }
    digest.update(transition.next().stream.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    Ok(format!("forgecode-nativepath-v1:{:x}", digest.finalize()))
}

pub(super) fn retire_missing_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    missing: &ForgeCodeMissingSource,
) -> Result<ProviderImportSummary> {
    let (path, stream, stored) = resolve_missing_cursor(store, &context.machine_id, missing)?;
    let selected_path = path.display().to_string();
    let (
        provider_cursor,
        locator_identity,
        canonical_source_identity,
        source_revision,
        raw_source_path,
    ) = match decode_stored_cursor(&stored.cursor)? {
        StoredCursorKind::Native(cursor) => {
            if cursor_stream(Path::new(&cursor.raw_source_path))? != stream {
                return Err(corrupt_cursor());
            }
            let locator_identity = if released_source_revision(&cursor.source_revision) {
                released_locator_identity(Path::new(&cursor.raw_source_path))?
            } else {
                provider_path_identity(Path::new(&cursor.raw_source_path))?
            };
            (
                serde_json::to_string(&cursor)?,
                locator_identity,
                cursor.canonical_source_identity,
                cursor.source_revision,
                cursor.raw_source_path,
            )
        }
        StoredCursorKind::ReleasedLegacy {
            source_revision,
            rejected_records,
        } => {
            let authority = released_route_authority(store, &context.machine_id, &stream)?;
            if source_revision.as_deref() != Some(authority.source_revision.as_str()) {
                return Err(corrupt_cursor());
            }
            let locator_identity =
                released_locator_identity(Path::new(&authority.raw_source_path))?;
            let cursor = ForgeCodeCursorWire {
                version: FORGECODE_CURSOR_VERSION,
                parser_revision: FORGECODE_NATIVE_PARSER_REVISION,
                policy_revision: FORGECODE_NATIVE_POLICY_REVISION,
                source_revision: authority.source_revision.clone(),
                canonical_source_identity: authority.canonical_source_identity.clone(),
                raw_source_path: authority.raw_source_path.clone(),
                frontier: ForgeCodeFrontier::initial(),
                terminal: true,
                generation: 0,
                rejected_records,
            };
            (
                serde_json::to_string(&cursor)?,
                locator_identity,
                authority.canonical_source_identity,
                authority.source_revision,
                authority.raw_source_path,
            )
        }
    };
    if raw_source_path != selected_path {
        return Err(corrupt_cursor());
    }
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::ForgeCode,
        source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity,
        cursor_stream: stream.clone(),
        expected_canonical_source_identity: canonical_source_identity,
        expected_source_revision: source_revision,
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    let publication_id = retirement_publication_id(&retirement, &provider_cursor);
    if decode_native_path_committed_cursor(&stored.cursor)
        .ok()
        .is_some_and(|cursor| cursor.publication_id() == publication_id)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let transition = NativePathCursorTransition::new(
        Some(stored.cursor),
        provider_sync_cursor(
            &context.machine_id,
            stream,
            provider_cursor,
            context.imported_at,
        ),
    );
    let accounting = NativePathGroupAccounting::new(0, 1, 0)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => {
            summary.skipped = 1;
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => {
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    Ok(summary)
}

fn resolve_missing_cursor(
    store: &Store,
    machine_id: &str,
    missing: &ForgeCodeMissingSource,
) -> Result<(PathBuf, String, SyncCursor)> {
    let mut matches = Vec::new();
    for path in &missing.candidates {
        let stream = cursor_stream(path)?;
        if let Some(cursor) = store.get_sync_cursor(None, machine_id, &stream)? {
            matches.push((path.clone(), stream, cursor));
        }
    }
    match matches.len() {
        0 => Err(CaptureError::InvalidProviderTranscriptPath {
            path: missing.preferred_path.clone(),
            reason: "ForgeCode SQLite source is missing and has no prior route authority",
        }),
        1 => matches.pop().ok_or_else(|| {
            CaptureError::SystemInvariant(
                "ForgeCode missing-source cursor count changed unexpectedly",
            )
        }),
        _ => Err(CaptureError::InvalidProviderTranscriptPath {
            path: missing.preferred_path.clone(),
            reason: "ForgeCode missing path has ambiguous exact-file and default-child authority",
        }),
    }
}

struct ReleasedRouteAuthority {
    canonical_source_identity: String,
    raw_source_path: String,
    source_revision: String,
}

fn released_route_authority(
    store: &Store,
    machine_id: &str,
    cursor_stream: &str,
) -> Result<ReleasedRouteAuthority> {
    let conn = Connection::open_with_flags(
        store.path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mut statement = conn.prepare(
        "SELECT canonical_source_identity, raw_source_path, source_revision
         FROM provider_source_locators
         WHERE provider = ?1 AND source_format = ?2 AND machine_id = ?3
           AND cursor_stream = ?4 AND is_current = 1
           AND raw_source_path IS NOT NULL AND raw_source_path <> ''
         ORDER BY locator_identity
         LIMIT 2",
    )?;
    let rows = statement.query_map(
        params![
            CaptureProvider::ForgeCode.as_str(),
            FORGECODE_SQLITE_SOURCE_FORMAT,
            machine_id,
            cursor_stream,
        ],
        |row| {
            Ok(ReleasedRouteAuthority {
                canonical_source_identity: row.get(0)?,
                raw_source_path: row.get(1)?,
                source_revision: row.get(2)?,
            })
        },
    )?;
    let mut authorities = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    match authorities.len() {
        1 => authorities.pop().ok_or_else(|| {
            CaptureError::SystemInvariant("released ForgeCode route count changed unexpectedly")
        }),
        _ => Err(CaptureError::InvalidPayload(
            "released ForgeCode cursor has no unique current route authority".to_owned(),
        )),
    }
}

fn released_source_revision(source_revision: &str) -> bool {
    source_revision.starts_with("forgecode-sqlite-snapshot-v1:capture=1;policy=5;")
}

fn released_locator_identity(raw_source_path: &Path) -> Result<String> {
    Ok(format!(
        "forgecode-sqlite:{}",
        provider_path_identity(raw_source_path)?
    ))
}

fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    provider_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(FORGECODE_RETIREMENT_DOMAIN);
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(provider_cursor.as_bytes());
    format!("forgecode-nativepath-retired-v1:{:x}", digest.finalize())
}

fn corrupt_cursor() -> CaptureError {
    CaptureError::InvalidPayload(
        "ForgeCode NativePath cursor is malformed or inconsistent".to_owned(),
    )
}
