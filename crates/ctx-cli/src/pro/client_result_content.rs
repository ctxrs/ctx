use std::collections::BTreeMap;

use ctx_history_capture::complete_content::{
    jsonl::JsonlCompleteContentResolver, PersistedCompleteContentLocatorV1, ResultContentRequest,
    RESULT_CONTENT_LOCATOR_METADATA_KEY,
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
    request.result_contents.clear();
    let resolver = JsonlCompleteContentResolver::new();
    let mut counts = ResultHydrationCounts::default();
    let mut groups = BTreeMap::<Uuid, Vec<PendingResultContent>>::new();
    let mut records = request.records.iter().collect::<Vec<_>>();
    records.sort_by_key(|record| record.sequence);
    let mut reserved_total_bytes = 0_usize;
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
        let Some((source_id, source_request)) =
            result_content_request(store, record.stable_entity_id, content_ref.clone())
        else {
            counts.omitted = counts.omitted.saturating_add(1);
            continue;
        };
        let Some(next_reserved) = reserved_total_bytes.checked_add(content_bytes) else {
            counts.omitted = counts.omitted.saturating_add(1);
            continue;
        };
        if next_reserved > MAX_RESULT_CONTENT_TOTAL_BYTES {
            counts.omitted = counts.omitted.saturating_add(1);
            continue;
        }
        reserved_total_bytes = next_reserved;
        groups
            .entry(source_id)
            .or_default()
            .push(PendingResultContent {
                journal_sequence: record.sequence,
                stable_entity_id: record.stable_entity_id,
                content_ref,
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
        let resolved = resolver.resolve_results(&source_requests);
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
    let mut total_bytes = 0_usize;
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
    counts
}

#[derive(Debug, Clone)]
struct PendingResultContent {
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

fn result_content_request(
    store: &Store,
    event_id: Uuid,
    content_ref: ContentRef,
) -> Option<(Uuid, ResultContentRequest)> {
    let event = store.get_event(event_id).ok()?;
    if !matches!(
        event.event_type,
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return None;
    }
    let source = store.get_capture_source(event.capture_source_id?).ok()?;
    let locator = event
        .sync
        .metadata
        .get(RESULT_CONTENT_LOCATOR_METADATA_KEY)
        .and_then(PersistedCompleteContentLocatorV1::from_metadata_value)?;
    if locator.body_sha256().as_str() != content_ref.sha256() {
        return None;
    }
    let source_id = source.id;
    let source_identity = source
        .descriptor
        .source_identity
        .filter(|identity| !identity.is_empty())?;
    Some((
        source_id,
        ResultContentRequest {
            event_id,
            provider: source.descriptor.provider,
            source_format: source.descriptor.source_format?,
            raw_source_path: source.descriptor.raw_source_path?.into(),
            source_root: source.descriptor.source_root.map(Into::into),
            source_identity: Some(source_identity),
            source_locator: locator.source_locator()?,
            source_snapshot: crate::complete_content::source_snapshot(&source.sync.metadata),
            source_record_ordinal: event.sync.metadata.get("source_record_ordinal")?.as_u64()?,
            source_record_subrecord_index: event
                .sync
                .metadata
                .get("source_record_subrecord_index")?
                .as_u64()
                .and_then(|value| u32::try_from(value).ok())?,
            expected_record_digest: locator.record_sha256().clone(),
            expected_content_ref: content_ref,
        },
    ))
}
