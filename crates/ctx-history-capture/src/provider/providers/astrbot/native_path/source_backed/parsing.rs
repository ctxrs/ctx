use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ctx_history_core::{CertifiedSource, CoreRecord, EventRole, EventType, ScannedSourceCounts};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    provider::normalization::{provider_json_text, provider_timestamp_millis, provider_value_text},
    provider::sqlite::sqlite_schema_fingerprint,
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceReadSnapshot},
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::super::{
    model::{
        checkpoint_id, item_is_output, item_role, item_text, provider_session_id, ConversationRow,
        PlatformMessageLink, PlatformMessageRow,
    },
    source::{
        fetch_candidates, visit_conversations, visit_platform_messages, AstrBotSql, RowCandidate,
    },
    ASTRBOT_CAPTURE_REVISION, ASTRBOT_POLICY_REVISION,
};
#[cfg(test)]
use super::discovery::open_root_authorized_snapshot;
use super::{
    discovery::AstrBotSourceBackedSourceV0,
    identity::{
        conversation_document, logical_values_digest, platform_document, EventFact, SessionFact,
    },
    AstrBotSourceBackedErrorV0, AstrBotSourceBackedResultV0, PARSER_REVISION,
};

type PlatformUnitProjection = (
    Option<CoreUnit>,
    Option<String>,
    [u8; 32],
    Option<(String, Value)>,
);
const SOURCE_BACKED_PAGE_ROWS: usize = 64;

#[derive(Debug)]
struct CoreUnit {
    session: SessionFact,
    event: Option<EventFact>,
}

pub(super) fn conversation_items(raw: &str) -> (Vec<Value>, bool) {
    match provider_json_text(raw) {
        Value::Array(items) => (items, true),
        value => (vec![value], false),
    }
}

fn conversation_session_fact(row: &ConversationRow) -> SessionFact {
    SessionFact {
        provider_session_id: provider_session_id(row),
        started_at: timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH),
    }
}

pub(super) fn platform_session_fact(
    row: &PlatformMessageRow,
    link: Option<&PlatformMessageLink>,
) -> SessionFact {
    let provider_session_id = link
        .map(|link| link.provider_session_id.clone())
        .unwrap_or_else(|| {
            format!(
                "platform/{}/{}",
                row.platform_id.as_deref().unwrap_or("unknown"),
                row.user_id.as_deref().unwrap_or("unknown")
            )
        });
    let started_at = link
        .and_then(|link| link.parent_created_at)
        .map(|value| timestamp(Some(value), DateTime::<Utc>::UNIX_EPOCH))
        .unwrap_or_else(|| timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH));
    SessionFact {
        provider_session_id,
        started_at,
    }
}

fn source_backed_conversation_event(
    row: &ConversationRow,
    item: Option<&Value>,
    content_is_array: bool,
    native_ordinal: u64,
) -> Option<EventFact> {
    let item = item?;
    if checkpoint_id(item).is_some() {
        return None;
    }
    let text = if content_is_array {
        item_text(item)
    } else {
        provider_value_text(item)
    }?;
    if text.trim().is_empty() {
        return None;
    }
    let event_type = if item_is_output(item) {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    Some(EventFact {
        source_record_ordinal: native_ordinal,
        event_type,
        role: item_role(item),
        occurred_at: timestamp(row.created_at, DateTime::<Utc>::UNIX_EPOCH),
    })
}

pub(super) fn serialized_hash(
    value_domain: &[u8],
    value: &impl Serialize,
) -> std::result::Result<[u8; 32], CaptureError> {
    let encoded = serde_json::to_vec(value).map_err(CaptureError::from)?;
    let mut hash = Sha256::new();
    hash.update(value_domain);
    hash_field(&mut hash, &encoded);
    Ok(hash.finalize().into())
}

fn candidate_hash(domain: &[u8], candidate: RowCandidate) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(candidate.physical_rowid.to_le_bytes());
    hash.update(candidate.retained_bytes.to_le_bytes());
    hash.update(candidate.legacy_order.logical_id.to_le_bytes());
    hash.update(candidate.legacy_order.timestamp.to_le_bytes());
    hash.finalize().into()
}

fn chain_hash(prior: [u8; 32], row: [u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ctx-astrbot-prefix-chain-v1\0");
    hash.update(prior);
    hash.update(row);
    hash.finalize().into()
}

fn hash_field(hash: &mut Sha256, value: &[u8]) {
    hash.update((value.len() as u64).to_le_bytes());
    hash.update(value);
}

fn timestamp(value: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_millis(value, fallback)
}

pub(crate) trait AstrBotSourceBackedSinkV0 {
    fn emit(&mut self, record: CoreRecord) -> AstrBotSourceBackedResultV0<()>;
}

impl<F> AstrBotSourceBackedSinkV0 for F
where
    F: FnMut(CoreRecord) -> AstrBotSourceBackedResultV0<()>,
{
    fn emit(&mut self, record: CoreRecord) -> AstrBotSourceBackedResultV0<()> {
        self(record)
    }
}

#[cfg(test)]
pub(crate) fn scan_astrbot_source_backed_v0(
    data_root: &std::path::Path,
    source: &AstrBotSourceBackedSourceV0,
    sink: &mut impl AstrBotSourceBackedSinkV0,
) -> AstrBotSourceBackedResultV0<CertifiedSource> {
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(data_root, &source.path)?;
    let certificate = scan_astrbot_snapshot_v0(source, sqlite_snapshot, sink)?;
    source_root.revalidate()?;
    Ok(certificate)
}

pub(crate) fn scan_astrbot_snapshot_v0(
    source: &AstrBotSourceBackedSourceV0,
    sqlite_snapshot: SqliteSourceReadSnapshot,
    sink: &mut impl AstrBotSourceBackedSinkV0,
) -> AstrBotSourceBackedResultV0<CertifiedSource> {
    let scan = (|| {
        let conn = sqlite_snapshot.connection()?;
        let sql = AstrBotSql::new(conn)?;
        let user_version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(CaptureError::from)?;
        let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
        let mut counts = ScannedSourceCounts::default();
        let mut content_chain = [0_u8; 32];
        let mut native_ordinal = 0_u64;
        let mut conversation_after = None;
        let mut page = Vec::with_capacity(SOURCE_BACKED_PAGE_ROWS);
        let mut checkpoint_links = BTreeMap::new();

        loop {
            let candidates = fetch_candidates(
                conn,
                &sql.conversation_candidate_initial,
                &sql.conversation_candidate_after,
                conversation_after,
                SOURCE_BACKED_PAGE_ROWS,
            )?;
            if candidates.is_empty() {
                break;
            }
            let mut rowids = Vec::with_capacity(candidates.len());
            for candidate in &candidates {
                if !candidate_is_oversize(*candidate)? {
                    rowids.push(candidate.physical_rowid);
                }
            }
            let mut candidate_index = 0;
            visit_conversations(
                conn,
                &sql.conversation_rows,
                &rowids,
                |physical_rowid, row| {
                    process_oversize_run(
                        &candidates,
                        &mut candidate_index,
                        b"astrbot-source-backed-conversation-oversize-v0\0",
                        &mut counts,
                        &mut content_chain,
                        &mut native_ordinal,
                    )?;
                    let candidate = candidates.get(candidate_index).copied().ok_or(
                        AstrBotSourceBackedErrorV0::Capture(
                            CaptureError::SourceChangedDuringCapture,
                        ),
                    )?;
                    candidate_index += 1;
                    if candidate.physical_rowid != physical_rowid {
                        return Err(AstrBotSourceBackedErrorV0::Capture(
                            CaptureError::SourceChangedDuringCapture,
                        ));
                    }
                    process_conversation_row(
                        source,
                        candidate,
                        row,
                        sink,
                        &mut page,
                        &mut checkpoint_links,
                        &mut counts,
                        &mut content_chain,
                        &mut native_ordinal,
                    )
                },
            )?;
            process_oversize_run(
                &candidates,
                &mut candidate_index,
                b"astrbot-source-backed-conversation-oversize-v0\0",
                &mut counts,
                &mut content_chain,
                &mut native_ordinal,
            )?;
            if candidate_index != candidates.len() {
                return Err(CaptureError::SourceChangedDuringCapture.into());
            }
            conversation_after = candidates.last().map(|candidate| candidate.physical_rowid);
        }

        if let (Some(initial), Some(after), Some(rows_sql)) = (
            sql.platform_message_candidate_initial.as_deref(),
            sql.platform_message_candidate_after.as_deref(),
            sql.platform_message_rows.as_deref(),
        ) {
            let mut platform_after = None;
            loop {
                let candidates = fetch_candidates(
                    conn,
                    initial,
                    after,
                    platform_after,
                    SOURCE_BACKED_PAGE_ROWS,
                )?;
                if candidates.is_empty() {
                    break;
                }
                let mut rowids = Vec::with_capacity(candidates.len());
                for candidate in &candidates {
                    if !candidate_is_oversize(*candidate)? {
                        rowids.push(candidate.physical_rowid);
                    }
                }
                let mut candidate_index = 0;
                visit_platform_messages(conn, rows_sql, &rowids, |physical_rowid, row| {
                    process_oversize_run(
                        &candidates,
                        &mut candidate_index,
                        b"astrbot-source-backed-platform-oversize-v0\0",
                        &mut counts,
                        &mut content_chain,
                        &mut native_ordinal,
                    )?;
                    let candidate = candidates.get(candidate_index).copied().ok_or(
                        AstrBotSourceBackedErrorV0::Capture(
                            CaptureError::SourceChangedDuringCapture,
                        ),
                    )?;
                    candidate_index += 1;
                    if candidate.physical_rowid != physical_rowid {
                        return Err(AstrBotSourceBackedErrorV0::Capture(
                            CaptureError::SourceChangedDuringCapture,
                        ));
                    }
                    process_platform_row(
                        source,
                        candidate,
                        row,
                        &checkpoint_links,
                        sink,
                        &mut page,
                        &mut counts,
                        &mut content_chain,
                        &mut native_ordinal,
                    )
                })?;
                process_oversize_run(
                    &candidates,
                    &mut candidate_index,
                    b"astrbot-source-backed-platform-oversize-v0\0",
                    &mut counts,
                    &mut content_chain,
                    &mut native_ordinal,
                )?;
                if candidate_index != candidates.len() {
                    return Err(CaptureError::SourceChangedDuringCapture.into());
                }
                platform_after = candidates.last().map(|candidate| candidate.physical_rowid);
            }
        }

        for document in page {
            sink.emit(document)?;
        }
        let mut digest = Sha256::new();
        digest.update(b"ctx-astrbot-source-backed-content-v0\0");
        digest.update(content_chain);
        digest.update(counts.complete_records.to_be_bytes());
        digest.update(counts.certified_bytes.to_be_bytes());
        let schema_evidence = format!(
            "capture={ASTRBOT_CAPTURE_REVISION}\0policy={ASTRBOT_POLICY_REVISION}\0\
         user_version={user_version}\0schema={schema_fingerprint}"
        );
        SqliteLogicalSnapshot::new(
            PARSER_REVISION,
            schema_evidence.as_bytes(),
            digest.finalize().into(),
            counts,
        )
        .certify(source.source_key.clone())
        .map_err(Into::into)
    })();
    match scan {
        Ok(certificate) => {
            sqlite_snapshot.finish()?;
            Ok(certificate)
        }
        Err(primary) => match sqlite_snapshot.abort() {
            Ok(()) => Err(primary),
            Err(cleanup) => Err(AstrBotSourceBackedErrorV0::SnapshotCleanup {
                primary: Box::new(primary),
                cleanup,
            }),
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn process_conversation_row(
    source: &AstrBotSourceBackedSourceV0,
    candidate: RowCandidate,
    row: ConversationRow,
    sink: &mut impl AstrBotSourceBackedSinkV0,
    page: &mut Vec<CoreRecord>,
    checkpoint_links: &mut BTreeMap<String, PlatformMessageLink>,
    counts: &mut ScannedSourceCounts,
    content_chain: &mut [u8; 32],
    native_ordinal: &mut u64,
) -> AstrBotSourceBackedResultV0<()> {
    add_certified_bytes(counts, candidate.observed_bytes()?)?;
    let row_digest = logical_values_digest(&super::super::super::model::conversation_values(
        row.clone(),
    ));
    *content_chain = chain_hash(*content_chain, row_digest);
    let (items, content_is_array) = conversation_items(&row.content);
    let provider_session_id = provider_session_id(&row);
    for item in &items {
        if let Some(checkpoint) = checkpoint_id(item) {
            checkpoint_links.insert(
                checkpoint,
                PlatformMessageLink {
                    provider_session_id: provider_session_id.clone(),
                    parent_created_at: row.created_at,
                },
            );
        }
    }
    let item_count = items.len().max(1);
    for item_index in 0..item_count {
        add_complete(counts)?;
        let item = items.get(item_index);
        let event = source_backed_conversation_event(&row, item, content_is_array, *native_ordinal);
        if let Some(event) = event {
            let complete_text = if content_is_array {
                item.and_then(item_text)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or(AstrBotSourceBackedErrorV0::MissingSelectedContent)?
            } else {
                item.and_then(provider_value_text)
                    .filter(|text| !text.trim().is_empty())
                    .ok_or(AstrBotSourceBackedErrorV0::MissingSelectedContent)?
            };
            let session = conversation_session_fact(&row);
            let document = conversation_document(
                source,
                candidate.physical_rowid,
                item_index,
                row_digest,
                item,
                &session,
                &event,
                &complete_text,
            )?;
            emit_bounded(sink, page, document)?;
            add_retained(counts)?;
        } else {
            add_ignored(counts)?;
        }
        *native_ordinal = native_ordinal
            .checked_add(1)
            .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    }
    Ok(())
}

fn emit_bounded(
    sink: &mut impl AstrBotSourceBackedSinkV0,
    page: &mut Vec<CoreRecord>,
    record: CoreRecord,
) -> AstrBotSourceBackedResultV0<()> {
    page.push(record);
    if page.len() == SOURCE_BACKED_PAGE_ROWS {
        for record in page.drain(..) {
            sink.emit(record)?;
        }
    }
    Ok(())
}

fn source_backed_platform_unit(
    row: &PlatformMessageRow,
    native_ordinal: u64,
    checkpoint_links: &BTreeMap<String, PlatformMessageLink>,
) -> AstrBotSourceBackedResultV0<PlatformUnitProjection> {
    let row_sha256 = serialized_hash(b"astrbot-platform-row-v1\0", &row)?;
    let link = row
        .llm_checkpoint_id
        .as_ref()
        .and_then(|checkpoint| checkpoint_links.get(checkpoint));
    let Some(provider_content) = row.content.as_deref().map(provider_json_text) else {
        return Ok((None, None, row_sha256, None));
    };
    let Some(text) = provider_value_text(&provider_content).filter(|text| !text.trim().is_empty())
    else {
        return Ok((None, None, row_sha256, None));
    };
    let session = platform_session_fact(row, link);
    let role = if row.sender_id.as_deref() == row.user_id.as_deref() {
        Some(EventRole::User)
    } else {
        Some(EventRole::Assistant)
    };
    let event_type = EventType::Message;
    let occurred_at = timestamp(row.created_at, session.started_at);
    Ok((
        Some(CoreUnit {
            session,
            event: Some(EventFact {
                source_record_ordinal: native_ordinal,
                event_type,
                role,
                occurred_at,
            }),
        }),
        None,
        row_sha256,
        Some((text, provider_content)),
    ))
}

#[allow(clippy::too_many_arguments)]
fn process_platform_row(
    source: &AstrBotSourceBackedSourceV0,
    candidate: RowCandidate,
    row: PlatformMessageRow,
    checkpoint_links: &BTreeMap<String, PlatformMessageLink>,
    sink: &mut impl AstrBotSourceBackedSinkV0,
    page: &mut Vec<CoreRecord>,
    counts: &mut ScannedSourceCounts,
    content_chain: &mut [u8; 32],
    native_ordinal: &mut u64,
) -> AstrBotSourceBackedResultV0<()> {
    add_certified_bytes(counts, candidate.observed_bytes()?)?;
    add_complete(counts)?;
    let (unit, rejection, row_digest, selected_content) =
        source_backed_platform_unit(&row, *native_ordinal, checkpoint_links)?;
    *content_chain = chain_hash(*content_chain, row_digest);
    if rejection.is_some() {
        add_rejected(counts)?;
    } else if let Some(unit) = unit {
        if let Some(event) = unit.event {
            let (complete_text, provider_content) = selected_content
                .as_ref()
                .ok_or(AstrBotSourceBackedErrorV0::MissingSelectedContent)?;
            let document = platform_document(
                source,
                candidate.legacy_order.logical_id,
                &unit.session,
                &event,
                complete_text,
                provider_content,
            )?;
            emit_bounded(sink, page, document)?;
            add_retained(counts)?;
        } else {
            add_ignored(counts)?;
        }
    } else {
        add_ignored(counts)?;
    }
    *native_ordinal = native_ordinal
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn candidate_is_oversize(candidate: RowCandidate) -> AstrBotSourceBackedResultV0<bool> {
    Ok(candidate.observed_bytes()?
        > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX))
}

fn process_oversize_run(
    candidates: &[RowCandidate],
    index: &mut usize,
    hash_domain: &[u8],
    counts: &mut ScannedSourceCounts,
    content_chain: &mut [u8; 32],
    native_ordinal: &mut u64,
) -> AstrBotSourceBackedResultV0<()> {
    while candidates
        .get(*index)
        .copied()
        .map(candidate_is_oversize)
        .transpose()?
        == Some(true)
    {
        process_oversize_candidate(
            candidates[*index],
            hash_domain,
            counts,
            content_chain,
            native_ordinal,
        )?;
        *index += 1;
    }
    Ok(())
}

fn process_oversize_candidate(
    candidate: RowCandidate,
    hash_domain: &[u8],
    counts: &mut ScannedSourceCounts,
    content_chain: &mut [u8; 32],
    native_ordinal: &mut u64,
) -> AstrBotSourceBackedResultV0<()> {
    add_certified_bytes(counts, candidate.observed_bytes()?)?;
    *content_chain = chain_hash(*content_chain, candidate_hash(hash_domain, candidate));
    add_complete(counts)?;
    add_rejected(counts)?;
    *native_ordinal = native_ordinal
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_complete(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.complete_records = counts
        .complete_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_retained(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.retained_records = counts
        .retained_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    counts.indexed_documents = counts
        .indexed_documents
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_rejected(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.rejected_records = counts
        .rejected_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_ignored(counts: &mut ScannedSourceCounts) -> AstrBotSourceBackedResultV0<()> {
    counts.ignored_records = counts
        .ignored_records
        .checked_add(1)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}

fn add_certified_bytes(
    counts: &mut ScannedSourceCounts,
    bytes: u64,
) -> AstrBotSourceBackedResultV0<()> {
    counts.certified_bytes = counts
        .certified_bytes
        .checked_add(bytes)
        .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
    Ok(())
}
