use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventType, Fidelity, FileChangeKind, FileTouched, Run, Session,
    SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::structured::STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            MAX_PROVIDER_FILE_TOUCHES_PER_EVENT, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        importer::{
            compact_provider_result_payload, provider_command_run,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_edge_uuid,
            provider_source_identity, provider_source_root, provider_sync_metadata, timestamps,
            CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativeIngestionPageError, NativeOutputProFailure,
            NativePageAccounting, NativeProOutputPage, NativeProReplayPage, NativeSafeFrontier,
            NativeSourceIdentity,
        },
        normalization::{
            provider_block_text, provider_capped_json_value, provider_local_preview,
            provider_message_id, provider_output_event_is_failure,
            provider_result_outcome_evidence, provider_string_field,
            provider_timestamp_from_fields,
        },
        tool_input,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
    ROVODEV_SOURCE_FORMAT,
};

use super::rovodev_result_content;
use super::{
    event::{rovodev_event, rovodev_event_type, RovoDevCoreEvent},
    source::{
        discover_rovodev_session_sources, RovoDevDiscovery, RovoDevSessionObservation,
        RovoDevSessionSource,
    },
};

mod lifecycle;
mod manifest;
mod model;
mod parse;
mod projection;
mod publication;
mod source;
#[allow(
    dead_code,
    reason = "provider adapter awaits central source-backed registration"
)]
mod source_backed;

use lifecycle::*;
use manifest::*;
use model::*;
use parse::*;
use projection::*;
use publication::*;
use source::*;

#[allow(
    unused_imports,
    reason = "provider adapter awaits central source-backed registration"
)]
pub(crate) use source_backed::{
    discover_rovodev_source_backed, hydrate_rovodev_source_record, RovoDevHydratedSourceRecord,
    RovoDevSourceBackedDisposition, RovoDevSourceBackedError, RovoDevSourceBackedInventory,
    RovoDevSourceBackedLeaf, RovoDevSourceBackedPage, RovoDevSourceBackedReader,
    RovoDevSourceBackedResult, RovoDevSourceBackedScan,
};

pub(super) const ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS: usize = 65_536;

pub(crate) fn import_rovodev_native_path(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let root_identity = root_identity(path)?;
    let root_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_ROOT_CURSOR_FORMAT,
        &root_identity,
    );
    let prior_root_cursor = store.get_sync_cursor(None, &context.machine_id, &root_stream)?;
    let mut manifest = load_manifest(prior_root_cursor.as_ref(), &root_identity)?;
    let discovery = discover_rovodev_session_sources(path)?;
    if discovery.sources().is_empty() && prior_root_cursor.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: if discovery.root_exists() {
                "no Rovo Dev session_context.json files found"
            } else {
                "Rovo Dev session root does not exist"
            },
        });
    }

    let configured_source_root = context
        .source_root
        .as_deref()
        .or(context.source_path.as_deref())
        .unwrap_or(path)
        .to_path_buf();
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut live_entries = BTreeMap::new();

        for source in discovery.sources() {
            let published = import_source(
                store,
                &committed_store,
                &bulk_guard,
                source,
                &configured_source_root,
                &root_stream,
                &mut manifest,
                &context,
                &options,
            )?;
            changed_groups = changed_groups.saturating_add(published.groups_changed);
            live_entries.insert(
                published.cursor.source_identity.clone(),
                manifest_entry(store, source, &published.cursor)?,
            );
            summary.merge_from(published.summary);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0 {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        let live_identities = live_entries.keys().cloned().collect::<BTreeSet<_>>();
        let mut missing = manifest
            .sources
            .iter()
            .filter(|entry| !live_identities.contains(&entry.source_identity))
            .cloned()
            .collect::<Vec<_>>();
        missing.sort_by(|left, right| left.source_identity.cmp(&right.source_identity));
        for entry in missing {
            let retirement = retire_source(
                store,
                &bulk_guard,
                &context,
                &root_stream,
                &manifest,
                &entry,
                if discovery.root_exists() {
                    ProviderSourceRouteRetirementReason::SourceMissing
                } else {
                    ProviderSourceRouteRetirementReason::RootMissing
                },
            )?;
            manifest = retirement.0;
            changed_groups = changed_groups.saturating_add(retirement.1);
            summary.merge_from(retirement.2);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0 {
                summary.work_remaining = manifest
                    .sources
                    .iter()
                    .any(|source| !live_identities.contains(&source.source_identity));
                return Ok(summary);
            }
        }

        for entry in live_entries.into_values() {
            match manifest
                .sources
                .iter_mut()
                .find(|prior| prior.source_identity == entry.source_identity)
            {
                Some(prior) => *prior = entry,
                None => manifest.sources.push(entry),
            }
        }
        manifest
            .sources
            .sort_by(|left, right| left.source_identity.cmp(&right.source_identity));

        revalidate_discovery(path, &discovery)?;
        let manifest_summary = publish_manifest(
            store,
            &bulk_guard,
            &context,
            &root_stream,
            prior_root_cursor.as_ref(),
            &manifest,
        )?;
        summary.merge_from(manifest_summary);
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
