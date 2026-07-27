use std::collections::BTreeMap;

use ctx_history_capture::complete_content::{
    jsonl::JsonlCompleteContentResolver, sqlite::SqliteCompleteContentResolver,
    structured::StructuredCompleteContentResolver, verified_content_route_matches,
    AuthorizedSourceRoute, BrokeredSourceAccess, ResultContentRequest,
    ResultContentResolverRegistry, SourceAccessBroker, VerifiedContentLocatorsV1,
    VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
};
use ctx_history_core::{ContentRef, EventType};
use ctx_history_store::Store;
use ctx_pro_host_protocol::{
    journal_sync_envelope_bytes, JournalEntityKind, JournalOperation, JournalRecord,
    JournalSyncRequest, ResultContentSidecar, MAX_JOURNAL_SYNC_ENVELOPE_BYTES,
    MAX_RESULT_CONTENT_BYTES_PER_ITEM, MAX_RESULT_CONTENT_TOTAL_BYTES,
};
use uuid::Uuid;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResultHydrationCounts {
    pub(super) hydrated: u64,
    pub(super) omitted: u64,
    pub(super) resolver_batches: u64,
}

pub(super) fn hydrate_result_contents(
    store: &Store,
    request: &mut JournalSyncRequest,
) -> ResultHydrationCounts {
    hydrate_result_contents_with_admission_observer(store, request, |_| {})
}

fn hydrate_result_contents_with_admission_observer(
    store: &Store,
    request: &mut JournalSyncRequest,
    mut observe_admission_attempt: impl FnMut(Uuid),
) -> ResultHydrationCounts {
    request.result_contents.clear();
    let mut resolvers = ResultContentResolverRegistry::new();
    resolvers.register(JsonlCompleteContentResolver::new());
    resolvers.register(StructuredCompleteContentResolver::new());
    resolvers.register(SqliteCompleteContentResolver::new());
    let mut counts = ResultHydrationCounts::default();
    let mut records = request.records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| record.sequence);
    let mut pending = Vec::new();
    for record in records {
        let Some(content_ref) = record_content_ref(record) else {
            continue;
        };
        let Some(content_bytes) = usize::try_from(content_ref.byte_len()).ok() else {
            counts.omitted = counts.omitted.saturating_add(1);
            continue;
        };
        if content_bytes > MAX_RESULT_CONTENT_BYTES_PER_ITEM {
            counts.omitted = counts.omitted.saturating_add(1);
            continue;
        }
        pending.push(PendingResultContent {
            declared_bytes: content_bytes,
            journal_sequence: record.sequence,
            stable_entity_id: record.stable_entity_id,
            content_ref,
        });
    }

    let mut total_bytes = 0_usize;
    // Each wave reserves only the remaining aggregate budget. A successful
    // resolution commits its reservation only after the complete serialized
    // host envelope admits it; every other outcome releases capacity so
    // deferred records can backfill without allowing unbounded source reads.
    while !pending.is_empty() {
        let mut reserved_total_bytes = total_bytes;
        let mut selected = Vec::new();
        let mut deferred = Vec::new();
        for candidate in pending {
            let Some(next_reserved) = reserved_total_bytes.checked_add(candidate.declared_bytes)
            else {
                deferred.push(candidate);
                continue;
            };
            if next_reserved > MAX_RESULT_CONTENT_TOTAL_BYTES {
                deferred.push(candidate);
                continue;
            }
            reserved_total_bytes = next_reserved;
            selected.push(candidate);
        }

        if selected.is_empty() {
            counts.omitted = counts
                .omitted
                .saturating_add(u64::try_from(deferred.len()).unwrap_or(u64::MAX));
            break;
        }

        // Source access is admitted only after this wave has reserved declared
        // content bytes. The cache is wave-local, so omitted candidates cannot
        // retain snapshots while later waves backfill released capacity.
        let mut admitted = BTreeMap::<Uuid, BrokeredSourceAccess>::new();
        let mut groups = BTreeMap::<Uuid, Vec<AdmittedResultContent>>::new();
        for candidate in selected {
            observe_admission_attempt(candidate.stable_entity_id);
            let Some((source_id, source_request)) = result_content_request(
                store,
                candidate.stable_entity_id,
                candidate.content_ref.clone(),
                &mut admitted,
            ) else {
                counts.omitted = counts.omitted.saturating_add(1);
                continue;
            };
            groups
                .entry(source_id)
                .or_default()
                .push(AdmittedResultContent {
                    journal_sequence: candidate.journal_sequence,
                    stable_entity_id: candidate.stable_entity_id,
                    content_ref: candidate.content_ref,
                    request: source_request,
                });
        }

        let mut candidates = Vec::new();
        for group in groups.values_mut() {
            group.sort_by_key(|pending| {
                (
                    pending.request.source_record_ordinal,
                    pending.request.source_record_subrecord_index,
                )
            });
            counts.resolver_batches = counts.resolver_batches.saturating_add(1);
            let source_requests = group
                .iter()
                .map(|pending| pending.request.clone())
                .collect::<Vec<_>>();
            let resolved = resolvers.resolve(&source_requests);
            if resolved.len() != group.len() {
                counts.omitted = counts
                    .omitted
                    .saturating_add(u64::try_from(group.len()).unwrap_or(u64::MAX));
                continue;
            }
            for (pending, resolved) in group.iter().zip(resolved) {
                match resolved {
                    Ok(content) => candidates.push(ResultContentSidecar {
                        journal_sequence: pending.journal_sequence,
                        stable_entity_id: pending.stable_entity_id,
                        content_ref: pending.content_ref.clone(),
                        content: content.content,
                    }),
                    Err(_) => counts.omitted = counts.omitted.saturating_add(1),
                }
            }
        }

        candidates.sort_by_key(|sidecar| sidecar.journal_sequence);
        for sidecar in candidates {
            let Some(next_total) = total_bytes.checked_add(sidecar.content.len()) else {
                counts.omitted = counts.omitted.saturating_add(1);
                continue;
            };
            if next_total > MAX_RESULT_CONTENT_TOTAL_BYTES {
                counts.omitted = counts.omitted.saturating_add(1);
                continue;
            }
            request.result_contents.push(sidecar);
            if journal_sync_envelope_bytes(request)
                .ok()
                .is_none_or(|bytes| bytes > MAX_JOURNAL_SYNC_ENVELOPE_BYTES)
            {
                request.result_contents.pop();
                counts.omitted = counts.omitted.saturating_add(1);
                continue;
            }
            total_bytes = next_total;
            counts.hydrated = counts.hydrated.saturating_add(1);
        }
        pending = deferred;
    }
    request
        .result_contents
        .sort_by_key(|sidecar| sidecar.journal_sequence);
    debug_assert!(request
        .result_contents
        .windows(2)
        .all(|pair| pair[0].journal_sequence < pair[1].journal_sequence));
    counts
}

#[derive(Debug, Clone)]
struct PendingResultContent {
    declared_bytes: usize,
    journal_sequence: u64,
    stable_entity_id: Uuid,
    content_ref: ContentRef,
}

#[derive(Debug, Clone)]
struct AdmittedResultContent {
    journal_sequence: u64,
    stable_entity_id: Uuid,
    content_ref: ContentRef,
    request: ResultContentRequest,
}

fn record_content_ref(record: &JournalRecord) -> Option<ContentRef> {
    (record.entity_kind == JournalEntityKind::Event && record.operation == JournalOperation::Upsert)
        .then_some(record.canonical_payload.as_ref())
        .flatten()
        .and_then(|payload| payload.pointer("/result/content_ref"))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

pub(super) fn result_content_request(
    store: &Store,
    event_id: Uuid,
    content_ref: ContentRef,
    admitted: &mut BTreeMap<Uuid, BrokeredSourceAccess>,
) -> Option<(Uuid, ResultContentRequest)> {
    let event = store.get_event(event_id).ok()?;
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return None;
    }
    let route = store.authorized_source_route_for_event(event_id).ok()?;
    let source = store.get_capture_source(route.capture_source_id()).ok()?;
    let locator = event
        .sync
        .metadata
        .get(VERIFIED_CONTENT_LOCATORS_METADATA_KEY)
        .and_then(VerifiedContentLocatorsV1::from_metadata_value)?
        .locator(VerifiedContentRole::ResultBody)?
        .clone();
    if locator.content_ref() != &content_ref {
        return None;
    }
    let source_id = route.capture_source_id();
    if event.capture_source_id != Some(source_id) {
        return None;
    }
    let source_format = route.source_format().to_owned();
    if !verified_content_route_matches(
        locator.content_profile(),
        route.provider(),
        &source_format,
        locator.family(),
        VerifiedContentRole::ResultBody,
        locator.kind(),
    ) {
        return None;
    }
    let source_access = if let Some(access) = admitted.get(&source_id) {
        access.clone()
    } else {
        let access = SourceAccessBroker::new()
            .admit(
                AuthorizedSourceRoute {
                    source_id,
                    provider: route.provider(),
                    source_format: source_format.clone(),
                    family: locator.family(),
                    raw_source_path: route.path().to_path_buf(),
                    source_root: source
                        .descriptor
                        .source_root
                        .as_deref()
                        .map(std::path::PathBuf::from)
                        .filter(|root| route.path().starts_with(root)),
                    source_identity: Some(route.canonical_source_identity().to_owned()),
                    source_snapshot: crate::complete_content::source_snapshot(
                        &source.sync.metadata,
                    ),
                },
                event_id,
            )
            .ok()?;
        admitted.insert(source_id, access.clone());
        access
    };
    Some((
        source_id,
        ResultContentRequest {
            event_id,
            provider: route.provider(),
            source_format,
            source_access,
            source_family: locator.family(),
            content_profile: locator.content_profile().to_owned(),
            source_locator: locator.source_locator()?,
            source_record_ordinal: event.sync.metadata.get("source_record_ordinal")?.as_u64()?,
            source_record_subrecord_index: event
                .sync
                .metadata
                .get("source_record_subrecord_index")?
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())?,
            expected_native_record_id: locator.native_record_id().to_owned(),
            expected_record_digest: locator.record_sha256().clone(),
            expected_content_ref: content_ref,
        },
    ))
}

#[cfg(test)]
#[path = "client_result_content_tests.rs"]
mod tests;
