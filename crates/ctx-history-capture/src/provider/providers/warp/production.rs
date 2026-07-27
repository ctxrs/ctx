use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventType, Fidelity, Session, SessionEdge, SessionEdgeType, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    CanonicalActor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::nativepath::{
    prepare_warp_nativepath_lifecycle, scan_prepared_warp_nativepath, WarpNativeEvent,
    WarpNativeFrontier, WarpNativeHierarchyEdge, WarpNativeMessageIdentity, WarpNativePage,
    WarpNativePersistedState, WarpNativePreparationInputs, WarpNativePreparationOutcome,
    WarpNativeProOutputPage, WarpNativeProOutputPageReceipt, WarpNativeProfile,
    WarpNativeRejection, WarpNativeScanOutcome, WarpNativeSession, WarpNativeSink,
    WarpNativeSourceAuthority, WarpNativeSourceFailure, WarpNativeSourceFailureKind,
    WarpNativeSourceIdentity, WARP_NATIVE_PARSER_REVISION, WARP_NATIVE_POLICY_REVISION,
};
use crate::complete_content::{
    attach_verified_content_locator, verified_content_profile, CompleteContentSourceFamily,
    VerifiedContentLocatorV1, VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use crate::provider::importer::{
    provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
    provider_path_identity, provider_source_cursor_stream_for_path, provider_sync_metadata,
    timestamps, CertifiedProviderCursor,
};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputNativeCursor, OutputSourceIdentity,
    ProOutputMaterializationPage, ProOutputProgress, ProOutputSink, ProOutputSinkError,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    WARP_SQLITE_SOURCE_FORMAT,
};

const WARP_CURSOR_VERSION: u32 = 1;
const WARP_CURSOR_KIND: &str = "warp-nativepath";
const WARP_PUBLICATION_DOMAIN: &[u8] = b"ctx-warp-nativepath-publication-v1\0";
const WARP_RETIREMENT_DOMAIN: &[u8] = b"ctx-warp-nativepath-retirement-v1\0";
const WARP_OUTPUT_FRONTIER_VERSION: u32 = 1;
const WARP_OUTPUT_PARSER_REVISION: &str = "warp-nativepath-output-v1";
const WARP_CONTENT_LOCATOR_KIND: &str = "warp-task-message-v1";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WarpCursorWire {
    version: u32,
    kind: String,
    state: WarpNativePersistedState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replacement_prior_source_identity: Option<String>,
}

struct DecodedWarpCursor {
    state: WarpNativePersistedState,
    replacement_prior_source_identity: Option<String>,
}

#[derive(Clone)]
struct WarpPublicationContext {
    machine_id: String,
    raw_source_path: String,
    source_root: String,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
    locator_identity: String,
    cursor_stream: String,
    proposed_source_identity: String,
    source_revision: String,
    replacement_prior_source_id: Option<Uuid>,
    replacement_prior_source_identity: Option<String>,
}

#[derive(Clone)]
struct KnownWarpRoute {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    cursor: SyncCursor,
}

pub(super) fn import_warp_nativepath(
    path: &Path,
    store: &mut Store,
    mut adapter: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if adapter.source_path.is_none() {
        adapter.source_path = Some(path.to_path_buf());
    }
    let configured_root = adapter
        .source_root
        .clone()
        .or_else(|| adapter.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let sink = options.import_profile.sink().cloned();

    let path_identity = provider_path_identity_for_missing(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        &path_identity,
    );
    let current_cursor = store.get_sync_cursor(None, &adapter.machine_id, &cursor_stream)?;
    let decoded_cursor = current_cursor
        .as_ref()
        .map(|cursor| decode_warp_cursor(&cursor.cursor))
        .transpose()?
        .flatten();
    let previous = decoded_cursor.as_ref().map(|cursor| cursor.state.clone());
    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(path, &adapter, sink.as_deref(), previous.as_ref());
        return Ok(ProviderImportSummary::default());
    }
    let known_route = known_warp_route(
        store,
        &adapter.machine_id,
        path,
        &cursor_stream,
        current_cursor.as_ref(),
        previous.as_ref(),
    )?;

    let prepared = match prepare_warp_nativepath_lifecycle(path, previous.as_slice()) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp {
            persisted_state, ..
        } => {
            replay_outputs_or_mark_behind(path, &adapter, sink.as_deref(), Some(&persisted_state));
            return Ok(noop_summary());
        }
        WarpNativePreparationOutcome::Incomplete(failure) => {
            return Err(preparation_error(failure));
        }
        WarpNativePreparationOutcome::Failed(failure)
            if failure.kind == WarpNativeSourceFailureKind::NotFound =>
        {
            if let Some(route) = known_route {
                let reason = if adapter
                    .source_root
                    .as_deref()
                    .is_some_and(|root| !root.exists())
                {
                    ProviderSourceRouteRetirementReason::RootMissing
                } else {
                    ProviderSourceRouteRetirementReason::SourceMissing
                };
                return retire_known_route(
                    store,
                    &adapter.machine_id,
                    adapter.imported_at,
                    &route,
                    reason,
                );
            }
            return Err(preparation_error(failure));
        }
        WarpNativePreparationOutcome::Failed(failure) => {
            return Err(preparation_error(failure));
        }
    };

    let mut replacement_prior_source_identity = decoded_cursor
        .as_ref()
        .and_then(|cursor| cursor.replacement_prior_source_identity.clone());
    if let (Some(previous), Some(route)) = (previous.as_ref(), known_route.as_ref()) {
        if previous.source_identity != prepared.inputs.source_identity {
            retire_known_route(
                store,
                &adapter.machine_id,
                adapter.imported_at,
                route,
                ProviderSourceRouteRetirementReason::Replaced,
            )?;
            replacement_prior_source_identity = Some(route.canonical_source_identity.clone());
        }
    }
    let context = publication_context(
        &prepared.inputs,
        &adapter,
        &configured_root,
        cursor_stream,
        options.history_record_id,
        options.inventory_observation_token.as_deref(),
        replacement_prior_source_identity,
    )?;
    let preparation_inputs = prepared.inputs.clone();
    let committed = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = {
        let mut sink = WarpCoreStoreSink {
            store,
            committed: &committed,
            bulk_guard: &bulk_guard,
            context: &context,
            inputs: &preparation_inputs,
            work_limit: options.capture_work_limit,
            pages_committed: 0,
            stopped: false,
            summary: ProviderImportSummary::default(),
        };
        let outcome =
            scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreOnly, &mut sink);
        match outcome {
            Ok(WarpNativeScanOutcome::Complete(authority)) if !sink.stopped => {
                sink.publish_terminal(&authority)?;
                Ok((std::mem::take(&mut sink.summary), Some(authority)))
            }
            Ok(WarpNativeScanOutcome::Complete(_)) => {
                sink.summary.work_remaining = true;
                Ok((std::mem::take(&mut sink.summary), None))
            }
            Ok(WarpNativeScanOutcome::Incomplete(_)) => {
                Err(CaptureError::SourceChangedDuringCapture)
            }
            Err(error) => Err(error),
        }
    };
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let (mut summary, authority) = match (operation, finish) {
        (Ok(result), Ok(())) => result,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    if let (Some(authority), Some(output_sink)) = (authority.as_ref(), sink.as_deref()) {
        replay_outputs_or_mark_behind(
            path,
            &adapter,
            Some(output_sink),
            Some(&authority.persisted_state),
        );
    }
    if summary.work_result() == ProviderImportWorkResult::NoOp && summary.skipped == 0 {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

fn provider_path_identity_for_missing(path: &Path) -> Result<String> {
    match provider_path_identity(path) {
        Ok(identity) => Ok(identity),
        Err(_) => {
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir()?.join(path)
            };
            Ok(absolute.display().to_string())
        }
    }
}

fn preparation_error(failure: WarpNativeSourceFailure) -> CaptureError {
    match failure.kind {
        WarpNativeSourceFailureKind::SourceChanged | WarpNativeSourceFailureKind::Locked => {
            CaptureError::SourceChangedDuringCapture
        }
        _ => CaptureError::InvalidPayload(format!(
            "Warp source {} is not ingestible: {}",
            failure.canonical_route.display(),
            failure.detail
        )),
    }
}

fn publication_context(
    inputs: &WarpNativePreparationInputs,
    adapter: &ProviderAdapterContext,
    configured_root: &Path,
    cursor_stream: String,
    history_record_id: Option<Uuid>,
    inventory_observation_token: Option<&str>,
    replacement_prior_source_identity: Option<String>,
) -> Result<WarpPublicationContext> {
    let proposed_source_identity =
        warp_canonical_source_identity(&adapter.machine_id, &inputs.source_identity)?;
    let locator_identity = format!("{cursor_stream}#{proposed_source_identity}");
    let mut source_revision = format!(
        "warp-nativepath-v1:parser={};policy={};capability={};snapshot={}",
        inputs.parser_revision,
        inputs.policy_revision,
        inputs.capability_digest,
        inputs.snapshot_revision,
    );
    if let Some(token) = inventory_observation_token {
        let mut digest = Sha256::new();
        digest.update(b"ctx-warp-nativepath-inventory-observation-v1\0");
        digest.update((source_revision.len() as u64).to_be_bytes());
        digest.update(source_revision.as_bytes());
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
        source_revision = format!("inventory-observation-sha256-v1:{:x}", digest.finalize());
    }
    Ok(WarpPublicationContext {
        machine_id: adapter.machine_id.clone(),
        raw_source_path: inputs.canonical_route.display().to_string(),
        source_root: configured_root.display().to_string(),
        imported_at: adapter.imported_at,
        history_record_id,
        locator_identity,
        cursor_stream,
        proposed_source_identity,
        source_revision,
        replacement_prior_source_id: replacement_prior_source_identity
            .as_deref()
            .map(warp_source_id),
        replacement_prior_source_identity,
    })
}

fn warp_canonical_source_identity(
    machine_id: &str,
    source_identity: &WarpNativeSourceIdentity,
) -> Result<String> {
    let source_identity_wire = serde_json::to_string(source_identity)?;
    Ok(format!(
        "warp-nativepath:{}",
        stable_capture_uuid(
            &format!(
                "{machine_id}:{}:{source_identity_wire}",
                WARP_SQLITE_SOURCE_FORMAT
            ),
            "canonical-source"
        )
    ))
}

struct WarpCoreStoreSink<'a> {
    store: &'a mut Store,
    committed: &'a Store,
    bulk_guard: &'a EventSearchBulkGuard,
    context: &'a WarpPublicationContext,
    inputs: &'a WarpNativePreparationInputs,
    work_limit: CaptureWorkLimit,
    pages_committed: usize,
    stopped: bool,
    summary: ProviderImportSummary,
}

impl WarpNativeSink for WarpCoreStoreSink<'_> {
    fn push_page(&mut self, page: WarpNativePage) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let state = self
            .inputs
            .persisted_state_at(page.next_safe_frontier.clone())?;
        let summary = publish_core_page(
            self.store,
            self.committed,
            self.bulk_guard,
            self.context,
            &page,
            &state,
        )?;
        self.summary.merge_from(summary);
        self.pages_committed = self.pages_committed.saturating_add(1);
        if self.work_limit == CaptureWorkLimit::OneSafeGroup {
            self.stopped = true;
        }
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        page.receipt()
    }
}

impl WarpCoreStoreSink<'_> {
    fn publish_terminal(&mut self, authority: &WarpNativeSourceAuthority) -> Result<()> {
        let summary = publish_terminal_observation(
            self.store,
            self.committed,
            self.bulk_guard,
            self.context,
            &authority.persisted_state,
        )?;
        self.summary.merge_from(summary);
        Ok(())
    }
}

fn publish_core_page(
    store: &mut Store,
    committed: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &WarpPublicationContext,
    page: &WarpNativePage,
    state: &WarpNativePersistedState,
) -> Result<ProviderImportSummary> {
    let transition = cursor_transition(
        store,
        context,
        encode_warp_cursor(state, context.replacement_prior_source_identity.as_deref())?,
    )?;
    let publication_id = core_publication_id(
        context,
        Some(&page.identity.0),
        std::slice::from_ref(&transition),
    );
    let accounting = NativePathGroupAccounting::new(1, 1, page.estimated_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(skipped_page_summary(page));
    }

    let (source_id, canonical_source_identity) = reconcile_source(&mut group, committed, context)?;
    let mut summary = ProviderImportSummary::default();
    let mut sessions = BTreeMap::new();
    for fact in &page.sessions {
        let session = canonical_session(
            committed,
            context,
            fact,
            source_id,
            &canonical_source_identity,
        )?;
        ensure_relationship_placeholders(
            &mut group,
            committed,
            context,
            fact,
            source_id,
            &canonical_source_identity,
            &session,
        )?;
        let existed = committed.get_session(session.id).is_ok();
        group.upsert_session(&session)?;
        sessions.insert(fact.conversation_id.clone(), session);
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    for edge in &page.hierarchy_edges {
        publish_edge(
            &mut group,
            committed,
            context,
            edge,
            source_id,
            &canonical_source_identity,
            &sessions,
            &mut summary,
        )?;
    }
    for event in &page.events {
        publish_event(
            &mut group,
            committed,
            context,
            event,
            source_id,
            &canonical_source_identity,
            &sessions,
            &mut summary,
        )?;
    }
    record_rejections(&mut summary, &page.rejections);
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn publish_terminal_observation(
    store: &mut Store,
    committed: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &WarpPublicationContext,
    state: &WarpNativePersistedState,
) -> Result<ProviderImportSummary> {
    let encoded = encode_warp_cursor(state, None)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &context.cursor_stream)?;
    if stored
        .as_ref()
        .and_then(|cursor| provider_cursor(&cursor.cursor))
        .is_some_and(|cursor| cursor == encoded)
        && persisted_source_revision_matches(committed, context)?
    {
        return Ok(noop_summary());
    }
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(context, encoded),
    );
    let publication_id = core_publication_id(context, None, std::slice::from_ref(&transition));
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(noop_summary());
    }
    reconcile_source(&mut group, committed, context)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn persisted_source_revision_matches(
    committed: &Store,
    context: &WarpPublicationContext,
) -> Result<bool> {
    match committed.get_capture_source(warp_source_id(&context.proposed_source_identity)) {
        Ok(source) => Ok(source.descriptor.source_identity.as_deref()
            == Some(context.proposed_source_identity.as_str())
            && source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str)
                == Some(context.source_revision.as_str())),
        Err(StoreError::NotFound(_)) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn reconcile_source(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
) -> Result<(Uuid, String)> {
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Warp,
            source_format: WARP_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: context.locator_identity.clone(),
            cursor_stream: context.cursor_stream.clone(),
            proposed_source_identity: context.proposed_source_identity.clone(),
            raw_source_path: Some(context.raw_source_path.clone()),
            source_revision: context.source_revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = warp_source_id(&resolution.canonical_source_identity);
    let started_at = match committed.get_capture_source(source_id) {
        Ok(source) => source.started_at,
        Err(StoreError::NotFound(_)) => context.imported_at,
        Err(error) => return Err(error.into()),
    };
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Warp,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(WARP_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: None,
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": resolution.canonical_source_identity,
                "source_revision": context.source_revision,
                "warp_native_locator_identity": context.locator_identity,
                "warp_native_cursor_stream": context.cursor_stream,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    Ok((source_id, resolution.canonical_source_identity))
}

fn warp_source_id(canonical_source_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!("warp-nativepath-source:{canonical_source_identity}"),
        "source",
    )
}

fn canonical_session(
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = warp_session_id(
        committed,
        context,
        &fact.conversation_id,
        source_id,
        source_identity,
    )?;
    let parent_session_id = fact
        .parent_conversation_id
        .as_deref()
        .map(|parent| warp_session_id(committed, context, parent, source_id, source_identity))
        .transpose()?;
    let root_session_id = warp_session_id(
        committed,
        context,
        &fact.root_conversation_id,
        source_id,
        source_identity,
    )?;
    let observed_at = fact.modified_at.unwrap_or(context.imported_at);
    Ok(Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id: (root_session_id != id).then_some(root_session_id),
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Warp,
        external_session_id: Some(fact.conversation_id.clone()),
        external_agent_id: Some("warp-agent".to_owned()),
        agent_type: if parent_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(
            if parent_session_id.is_some() {
                "subagent"
            } else {
                "primary"
            }
            .to_owned(),
        ),
        is_primary: parent_session_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: observed_at,
        ended_at: fact.modified_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.conversation_id,
                "parent_provider_session_id": fact.parent_conversation_id,
                "root_provider_session_id": fact.root_conversation_id,
                "parent_present": fact.parent_present,
                "title": fact.title,
                "metadata": fact.metadata,
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
            }),
        ),
    })
}

fn warp_session_id(
    committed: &Store,
    context: &WarpPublicationContext,
    external_session_id: &str,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Uuid> {
    if let Some(prior_source_id) = context.replacement_prior_source_id {
        if prior_source_id != source_id {
            if let Some(existing) = committed.session_by_capture_source_and_external_session(
                prior_source_id,
                CaptureProvider::Warp,
                external_session_id,
            )? {
                return Ok(existing.id);
            }
        }
    }
    provider_import_session_uuid(
        committed,
        CaptureProvider::Warp,
        external_session_id,
        source_id,
        Some(source_identity),
    )
}

fn ensure_relationship_placeholders(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeSession,
    source_id: Uuid,
    source_identity: &str,
    session: &Session,
) -> Result<()> {
    let mut facts = BTreeSet::new();
    if let Some(parent) = fact.parent_conversation_id.as_deref() {
        facts.insert(parent);
    }
    if fact.root_conversation_id != fact.conversation_id {
        facts.insert(fact.root_conversation_id.as_str());
    }
    for external_id in facts {
        let id = warp_session_id(committed, context, external_id, source_id, source_identity)?;
        if id != session.id && committed.get_session(id).is_err() {
            group.insert_session_if_absent(&relationship_placeholder(
                context,
                source_id,
                id,
                external_id,
            ))?;
        }
    }
    Ok(())
}

fn relationship_placeholder(
    context: &WarpPublicationContext,
    source_id: Uuid,
    id: Uuid,
    external_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Warp,
        external_session_id: Some(external_id.to_owned()),
        external_agent_id: Some("warp-agent".to_owned()),
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_id,
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_edge(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeHierarchyEdge,
    source_id: Uuid,
    source_identity: &str,
    sessions: &BTreeMap<String, Session>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let child = if let Some(session) = sessions.get(&fact.child_conversation_id) {
        session.clone()
    } else {
        let id = warp_session_id(
            committed,
            context,
            &fact.child_conversation_id,
            source_id,
            source_identity,
        )?;
        committed.get_session(id)?
    };
    let parent_id = warp_session_id(
        committed,
        context,
        &fact.parent_conversation_id,
        source_id,
        source_identity,
    )?;
    if committed.get_session(parent_id).is_err() {
        group.insert_session_if_absent(&relationship_placeholder(
            context,
            source_id,
            parent_id,
            &fact.parent_conversation_id,
        ))?;
    }
    let edge = SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "warp-nativepath:{source_identity}:{}:parent_child",
                fact.child_conversation_id
            ),
            "session-edge",
        ),
        from_session_id: child.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: if fact.parent_present {
            Confidence::Explicit
        } else {
            Confidence::High
        },
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.child_conversation_id,
                "parent_provider_session_id": fact.parent_conversation_id,
                "parent_present": fact.parent_present,
                "source_format": WARP_SQLITE_SOURCE_FORMAT,
            }),
        ),
    };
    let existed = committed.session_edge_exists(edge.id)?;
    group.upsert_projection_neutral_session_edge(&canonical_actor(&child), &edge)?;
    if existed {
        summary.skipped_edges = summary.skipped_edges.saturating_add(1);
    } else {
        summary.imported_edges = summary.imported_edges.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed: &Store,
    context: &WarpPublicationContext,
    fact: &WarpNativeEvent,
    source_id: Uuid,
    source_identity: &str,
    sessions: &BTreeMap<String, Session>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let session = if let Some(session) = sessions.get(&fact.identity.conversation_id) {
        session.clone()
    } else {
        let id = warp_session_id(
            committed,
            context,
            &fact.identity.conversation_id,
            source_id,
            source_identity,
        )?;
        committed.get_session(id)?
    };
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let sequence_index = fact.native_order.provider_event_index;
    let identity_index = warp_event_identity_index(fact);
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed,
        CaptureProvider::Warp,
        provider_session_id,
        source_id,
        identity_index,
        sequence_index,
        &fact.content_hash,
        None,
        Some(sequence_index),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Warp,
                provider_session_id,
            ),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &fact.content_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let native_record_id = match &fact.identity.message {
        WarpNativeMessageIdentity::ProviderId(id) => id.clone(),
        WarpNativeMessageIdentity::MessageOrdinal(ordinal) => {
            format!("{}:{ordinal}", fact.identity.task_id)
        }
    };
    let mut sync_details = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": sequence_index,
        "provider_event_identity_index": identity_index,
        "provider_event_hash": fact.content_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "source_format": WARP_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "native_record_id": native_record_id,
        "task_rowid": fact.native_order.task_rowid,
        "task_key": fact.native_order.task_key,
        "message_ordinal": fact.native_order.message_ordinal,
        "native_identity": {
            "conversation_id": fact.identity.conversation_id,
            "task_id": fact.identity.task_id,
        },
    });
    attach_complete_content_locator(fact, &native_record_id, &mut sync_details)?;
    let occurred_at = fact.occurred_at.unwrap_or(session.started_at);
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: fact.event_type,
        role: fact.role,
        occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Warp.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": sequence_index,
            "provider_event_identity_index": identity_index,
            "provider_event_hash": fact.content_hash,
            "native_record_id": native_record_id,
            "kind": fact.kind,
            "request_id": fact.request_id,
            "result_outcome": fact.result_outcome.map(|outcome| format!("{outcome:?}").to_lowercase()),
            "call_id": fact.call_id,
            "body": fact.body,
            "preview": fact.preview,
            "searchable_text": fact.body,
            "artifacts": [],
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_details),
    };
    if group.reconcile_provider_event(
        &normalized,
        ProviderEventHashAuthority::NormalizedPayloadFallback,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

fn warp_event_identity_index(event: &WarpNativeEvent) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let message_identity = match &event.identity.message {
        WarpNativeMessageIdentity::ProviderId(id) => id.clone(),
        WarpNativeMessageIdentity::MessageOrdinal(ordinal) => {
            format!("{}:{ordinal}", event.identity.task_id)
        }
    };
    let mut hash = OFFSET;
    for component in [
        b"ctx-warp-message-v1".as_slice(),
        event.identity.conversation_id.as_bytes(),
        event.identity.task_id.as_bytes(),
        message_identity.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn attach_complete_content_locator(
    event: &WarpNativeEvent,
    native_record_id: &str,
    metadata: &mut Value,
) -> Result<()> {
    let Some(content_ref) = event.complete_content_ref.clone() else {
        return Ok(());
    };
    if event.event_type != EventType::Message {
        return Ok(());
    }
    let Some(profile) = verified_content_profile(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        CompleteContentSourceFamily::Sqlite,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "Warp complete-content profile is not registered",
        ));
    };
    let mut locator_value = Vec::with_capacity(12);
    locator_value.extend_from_slice(&event.native_order.task_rowid.to_be_bytes());
    locator_value.extend_from_slice(&event.native_order.message_ordinal.to_be_bytes());
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Sqlite,
        WARP_CONTENT_LOCATOR_KIND,
        &locator_value,
        native_record_id,
        event.source_record_digest.clone(),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Warp complete-content locator exceeded its typed bounds",
    ))?;
    attach_verified_content_locator(metadata, locator).ok_or(CaptureError::SystemInvariant(
        "Warp complete-content metadata exceeded its typed bounds",
    ))?;
    if metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .is_none()
    {
        return Err(CaptureError::SystemInvariant(
            "Warp complete-content metadata attachment was lost",
        ));
    }
    Ok(())
}

fn canonical_actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

fn record_rejections(summary: &mut ProviderImportSummary, rejections: &[WarpNativeRejection]) {
    for (index, rejection) in rejections.iter().enumerate() {
        summary.record_failure(ProviderImportFailure {
            line: index.saturating_add(1),
            error: format!(
                "Warp {:?} record {} rejected: {}",
                rejection.kind, rejection.native_key, rejection.reason
            ),
        });
    }
}

fn skipped_page_summary(page: &WarpNativePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_sessions = page.sessions.len();
    summary.skipped_events = page.events.len();
    summary.skipped_edges = page.hierarchy_edges.len();
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events)
        .saturating_add(summary.skipped_edges);
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

fn noop_summary() -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

fn cursor_transition(
    store: &Store,
    context: &WarpPublicationContext,
    provider_cursor: String,
) -> Result<NativePathCursorTransition> {
    let stored = store.get_sync_cursor(None, &context.machine_id, &context.cursor_stream)?;
    Ok(NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(context, provider_cursor),
    ))
}

fn provider_sync_cursor(context: &WarpPublicationContext, cursor: String) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Warp.as_str(),
                context.machine_id,
                context.cursor_stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: context.cursor_stream.clone(),
        cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    }
}

fn core_publication_id(
    context: &WarpPublicationContext,
    page_identity: Option<&[u8; 32]>,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(WARP_PUBLICATION_DOMAIN);
    digest.update(context.locator_identity.as_bytes());
    digest.update((context.source_revision.len() as u64).to_be_bytes());
    digest.update(context.source_revision.as_bytes());
    if let Some(identity) = page_identity {
        digest.update([1]);
        digest.update(identity);
    } else {
        digest.update([0]);
    }
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        }
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("warp-nativepath-v1:{:x}", digest.finalize())
}

fn encode_warp_cursor(
    state: &WarpNativePersistedState,
    replacement_prior_source_identity: Option<&str>,
) -> Result<String> {
    Ok(serde_json::to_string(&WarpCursorWire {
        version: WARP_CURSOR_VERSION,
        kind: WARP_CURSOR_KIND.to_owned(),
        state: state.clone(),
        replacement_prior_source_identity: replacement_prior_source_identity.map(str::to_owned),
    })?)
}

fn provider_cursor(encoded: &str) -> Option<String> {
    ctx_history_store::decode_native_path_committed_cursor(encoded)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .ok()
        .or_else(|| Some(encoded.to_owned()))
}

fn decode_warp_cursor(encoded: &str) -> Result<Option<DecodedWarpCursor>> {
    let committed = ctx_history_store::decode_native_path_committed_cursor(encoded);
    let provider = match &committed {
        Ok(committed) => committed.provider_cursor(),
        Err(_) => {
            // Released certified pre-NativePath cursors are migration input
            // only. They grant no NativePath resume authority and are replaced
            // after one complete NativePath scan.
            return match CertifiedProviderCursor::decode_if_certified(encoded)? {
                Some(certified)
                    if certified.native_position().kind()
                        == "warp-conversation-task-keyset-v4"
                        && certified.parser_revision() == 5
                        && certified.policy_revision() == 7
                        && certified.source_revision().starts_with(
                            "warp-sqlite-snapshot-v1:capture=5;policy=7;schema=",
                        ) =>
                {
                    Ok(None)
                }
                Some(_) => Err(CaptureError::InvalidPayload(
                    "Warp migration cursor does not match the released Warp cursor authority"
                        .to_owned(),
                )),
                None => Err(CaptureError::InvalidPayload(
                    "Warp cursor is neither a committed NativePath cursor nor a released certified migration cursor"
                        .to_owned(),
                )),
            };
        }
    };
    let wire: WarpCursorWire = serde_json::from_str(provider).map_err(|_| {
        CaptureError::InvalidPayload("Warp committed NativePath cursor is malformed".to_owned())
    })?;
    if wire.version != WARP_CURSOR_VERSION
        || wire.kind != WARP_CURSOR_KIND
        || wire.state.parser_revision != WARP_NATIVE_PARSER_REVISION
        || wire.state.policy_revision != WARP_NATIVE_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Warp committed NativePath cursor has unsupported authority revisions".to_owned(),
        ));
    }
    Ok(Some(DecodedWarpCursor {
        state: wire.state,
        replacement_prior_source_identity: wire.replacement_prior_source_identity,
    }))
}

fn known_warp_route(
    store: &Store,
    machine_id: &str,
    path: &Path,
    cursor_stream: &str,
    cursor: Option<&SyncCursor>,
    previous: Option<&WarpNativePersistedState>,
) -> Result<Option<KnownWarpRoute>> {
    let Some(cursor) = cursor else {
        return Ok(None);
    };
    let expected_path = match path.canonicalize() {
        Ok(path) => path,
        Err(_) if path.is_absolute() => path.to_path_buf(),
        Err(_) => std::env::current_dir()?.join(path),
    }
    .display()
    .to_string();
    let expected_locator = previous
        .map(|state| {
            warp_canonical_source_identity(machine_id, &state.source_identity)
                .map(|identity| format!("{cursor_stream}#{identity}"))
        })
        .transpose()?;
    let mut found = None;
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Warp
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(WARP_SQLITE_SOURCE_FORMAT)
            || source.descriptor.raw_source_path.as_deref() != Some(expected_path.as_str())
        {
            continue;
        }
        let Some(locator_identity) = source
            .sync
            .metadata
            .get("warp_native_locator_identity")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(stored_stream) = source
            .sync
            .metadata
            .get("warp_native_cursor_stream")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(canonical_source_identity) = source.descriptor.source_identity.as_deref() else {
            continue;
        };
        if stored_stream != cursor_stream {
            continue;
        }
        if expected_locator
            .as_deref()
            .is_some_and(|expected| expected != locator_identity)
        {
            continue;
        }
        let route = KnownWarpRoute {
            locator_identity: locator_identity.to_owned(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            cursor: cursor.clone(),
        };
        if found.replace(route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Warp persisted duplicate current routes for one SQLite source",
            ));
        }
    }
    Ok(found)
}

fn retire_known_route(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownWarpRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let provider_cursor = provider_cursor(&route.cursor.cursor).ok_or(
        CaptureError::SystemInvariant("Warp cursor could not be decoded"),
    )?;
    let context = WarpPublicationContext {
        machine_id: machine_id.to_owned(),
        raw_source_path: String::new(),
        source_root: String::new(),
        imported_at: retired_at,
        history_record_id: None,
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor.stream.clone(),
        proposed_source_identity: route.canonical_source_identity.clone(),
        source_revision: route.source_revision.clone(),
        replacement_prior_source_id: None,
        replacement_prior_source_identity: None,
    };
    let transition = NativePathCursorTransition::new(
        Some(route.cursor.cursor.clone()),
        provider_sync_cursor(&context, provider_cursor),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Warp,
        source_format: WARP_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: route.cursor.stream.clone(),
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let mut digest = Sha256::new();
    digest.update(WARP_RETIREMENT_DOMAIN);
    digest.update(route.locator_identity.as_bytes());
    digest.update(route.source_revision.as_bytes());
    digest.update([reason as u8]);
    let publication_id = format!("warp-nativepath-retirement-v1:{:x}", digest.finalize());
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
        summary.set_work_result(if changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        });
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

fn replay_outputs_or_mark_behind(
    path: &Path,
    adapter: &ProviderAdapterContext,
    sink: Option<&dyn ProOutputSink>,
    expected_core: Option<&WarpNativePersistedState>,
) {
    let Some(sink) = sink else {
        return;
    };
    let Some(expected_core) = expected_core else {
        sink.mark_behind(ProOutputSinkError::new(
            "warp_nativepath_core_unavailable",
            "Warp output replay requires a committed NativePath Core generation",
        ));
        return;
    };
    if let Err(error) = replay_outputs(path, adapter, sink, expected_core) {
        sink.mark_behind(ProOutputSinkError::new(
            "warp_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_outputs(
    path: &Path,
    adapter: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
    expected_core: &WarpNativePersistedState,
) -> Result<()> {
    let mut prepared = match prepare_warp_nativepath_lifecycle(path, &[]) {
        WarpNativePreparationOutcome::Ready(prepared) => prepared,
        WarpNativePreparationOutcome::ExactNoOp { .. } => {
            return Err(CaptureError::SystemInvariant(
                "Warp output replay unexpectedly trusted a persisted terminal checkpoint",
            ));
        }
        WarpNativePreparationOutcome::Incomplete(failure)
        | WarpNativePreparationOutcome::Failed(failure) => {
            return Err(preparation_error(failure));
        }
    };
    if prepared.inputs.source_identity != expected_core.source_identity
        || prepared.inputs.snapshot_revision != expected_core.snapshot_revision
        || prepared.inputs.capability_digest != expected_core.capability_digest
        || prepared.inputs.parser_revision != expected_core.parser_revision
        || prepared.inputs.policy_revision != expected_core.policy_revision
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let source = OutputSourceIdentity {
        provider: CaptureProvider::Warp.as_str().to_owned(),
        namespace_id: adapter.machine_id.clone(),
        source_id: warp_canonical_source_identity(
            &adapter.machine_id,
            &prepared.inputs.source_identity,
        )?,
    };
    let observed_revision = format!(
        "warp-nativepath-output-v1:parser={};policy={};capability={};snapshot={}",
        prepared.inputs.parser_revision,
        prepared.inputs.policy_revision,
        prepared.inputs.capability_digest,
        prepared.inputs.snapshot_revision,
    );
    let progress = sink.observe_source(&source).map_err(|error| {
        CaptureError::InvalidPayload(format!("Warp output progress failed: {error}"))
    })?;
    let resume_frontier = progress
        .as_ref()
        .filter(|progress| {
            progress.observed_revision == observed_revision
                && progress.parser_revision == WARP_OUTPUT_PARSER_REVISION
                && progress.materializer_revision == sink.materializer_revision()
        })
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == WARP_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<WarpNativeFrontier>(&cursor.payload).ok())
        .filter(WarpNativeFrontier::is_persistable);
    if progress.as_ref().is_some_and(|progress| {
        progress.terminal
            && progress.observed_revision == observed_revision
            && progress.parser_revision == WARP_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && resume_frontier.is_some()
    }) {
        return Ok(());
    }
    if let Some(frontier) = resume_frontier {
        prepared.inputs.resume_frontier = Some(frontier);
        prepared.inputs.action =
            super::nativepath::WarpNativePreparationAction::ResumeExactSnapshot;
    }
    let mut output = WarpOutputStoreSink::new(
        sink,
        source,
        observed_revision,
        progress,
        prepared.inputs.resume_frontier.clone().unwrap_or_default(),
    )?;
    if output.terminal_noop {
        return Ok(());
    }
    let outcome =
        scan_prepared_warp_nativepath(*prepared, WarpNativeProfile::CoreAndPro, &mut output)?;
    if let WarpNativeScanOutcome::Complete(authority) = outcome {
        output.finish(authority.persisted_state.checkpoint_frontier().clone());
    }
    Ok(())
}

struct WarpOutputStoreSink<'a> {
    sink: &'a dyn ProOutputSink,
    source: OutputSourceIdentity,
    observed_revision: String,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_cursor: Option<OutputNativeCursor>,
    disposition: ProOutputSourceDisposition,
    failed: bool,
    terminal_noop: bool,
}

impl<'a> WarpOutputStoreSink<'a> {
    fn new(
        sink: &'a dyn ProOutputSink,
        source: OutputSourceIdentity,
        observed_revision: String,
        progress: Option<ProOutputProgress>,
        _initial_frontier: WarpNativeFrontier,
    ) -> Result<Self> {
        let exact = progress.as_ref().is_some_and(|progress| {
            progress.observed_revision == observed_revision
                && progress.parser_revision == WARP_OUTPUT_PARSER_REVISION
                && progress.materializer_revision == sink.materializer_revision()
                && progress.cursor.as_ref().is_some_and(valid_output_cursor)
        });
        let terminal_noop = exact && progress.as_ref().is_some_and(|progress| progress.terminal);
        let (source_epoch, expected_source_epoch, expected_cursor, disposition) =
            if let Some(progress) = progress {
                if exact {
                    (
                        progress.source_epoch,
                        Some(progress.source_epoch),
                        progress.cursor,
                        ProOutputSourceDisposition::AppendOrResume,
                    )
                } else {
                    (
                        progress.source_epoch.checked_add(1).ok_or(
                            CaptureError::SystemInvariant("Warp output source epoch exhausted"),
                        )?,
                        Some(progress.source_epoch),
                        progress.cursor,
                        ProOutputSourceDisposition::Rewrite,
                    )
                }
            } else {
                (0, None, None, ProOutputSourceDisposition::NewSource)
            };
        Ok(Self {
            sink,
            source,
            observed_revision,
            source_epoch,
            expected_source_epoch,
            expected_cursor,
            disposition,
            failed: false,
            terminal_noop,
        })
    }

    fn finish(&mut self, frontier: WarpNativeFrontier) {
        if self.failed || self.terminal_noop {
            return;
        }
        let next = match output_cursor(&frontier) {
            Ok(cursor) => cursor,
            Err(error) => {
                self.mark_failed("warp_output_terminal_cursor", error.to_string());
                return;
            }
        };
        self.materialize(Vec::new(), next, true);
    }

    fn materialize(
        &mut self,
        observations: Vec<crate::ProOutputObservation>,
        next: OutputNativeCursor,
        terminal: bool,
    ) {
        if self.failed {
            return;
        }
        let page = ProOutputMaterializationPage {
            inventory_generation: self.sink.inventory_generation(),
            source: self.source.clone(),
            source_epoch: self.source_epoch,
            observed_revision: self.observed_revision.clone(),
            parser_revision: WARP_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: self.sink.materializer_revision().to_owned(),
            disposition: self.disposition,
            expected_prior_source_epoch: self.expected_source_epoch,
            expected_prior_cursor: self.expected_cursor.clone(),
            next_safe_cursor: next.clone(),
            terminal,
            observations,
        };
        match self.sink.materialize_page(page) {
            Ok(result)
                if result.source_epoch == self.source_epoch && result.committed_cursor == next =>
            {
                self.expected_source_epoch = Some(self.source_epoch);
                self.expected_cursor = Some(next);
                self.disposition = ProOutputSourceDisposition::AppendOrResume;
            }
            Ok(_) => self.mark_failed(
                "warp_output_receipt_mismatch",
                "Warp output sink acknowledged a different source frontier",
            ),
            Err(error) => {
                self.sink.mark_behind(error);
                self.failed = true;
            }
        }
    }

    fn mark_failed(&mut self, code: &'static str, message: impl Into<String>) {
        self.sink
            .mark_behind(ProOutputSinkError::new(code, message.into()));
        self.failed = true;
    }
}

impl WarpNativeSink for WarpOutputStoreSink<'_> {
    fn push_page(&mut self, _page: WarpNativePage) -> Result<()> {
        Ok(())
    }

    fn push_pro_output_page(
        &mut self,
        page: WarpNativeProOutputPage,
    ) -> WarpNativeProOutputPageReceipt {
        let receipt = page.receipt();
        if !self.failed {
            match output_cursor(&page.next_safe_frontier) {
                Ok(cursor) => self.materialize(page.outputs, cursor, false),
                Err(error) => {
                    self.mark_failed("warp_output_cursor", error.to_string());
                }
            }
        }
        receipt
    }
}

fn output_cursor(frontier: &WarpNativeFrontier) -> Result<OutputNativeCursor> {
    Ok(OutputNativeCursor {
        version: WARP_OUTPUT_FRONTIER_VERSION,
        payload: serde_json::to_vec(frontier)?,
    })
}

fn valid_output_cursor(cursor: &OutputNativeCursor) -> bool {
    cursor.version == WARP_OUTPUT_FRONTIER_VERSION
        && serde_json::from_slice::<WarpNativeFrontier>(&cursor.payload)
            .is_ok_and(|frontier| frontier.is_persistable())
}
