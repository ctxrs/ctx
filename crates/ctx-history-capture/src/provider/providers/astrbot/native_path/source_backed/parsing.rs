use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ctx_history_core::{CertifiedSource, EventRole, EventType, ScannedSourceCounts, TypedKey};
use ctx_history_index::LexicalDocument;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    provider::normalization::{provider_json_text, provider_timestamp_millis, provider_value_text},
    provider::sqlite::sqlite_schema_fingerprint,
    CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::super::super::{
    model::{
        checkpoint_id, item_is_output, item_role, item_text, provider_session_id, ConversationRow,
        PlatformMessageLink, PlatformMessageRow,
    },
    source::{
        fetch_candidate, hydrate_conversation, hydrate_platform_message, AstrBotSql, RowCandidate,
    },
};
use super::{
    discovery::{
        open_root_authorized_snapshot, revision_digest, source_observation,
        AstrBotSourceBackedSourceV0,
    },
    identity::{
        conversation_document, logical_values_digest, platform_document, EventFact, SessionFact,
    },
    AstrBotSourceBackedErrorV0, AstrBotSourceBackedResultV0, PARSER_REVISION,
};

type PlatformUnitProjection = (Option<CoreUnit>, Option<String>, [u8; 32], Option<String>);

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
    fn emit(&mut self, document: LexicalDocument) -> AstrBotSourceBackedResultV0<()>;
}

impl<F> AstrBotSourceBackedSinkV0 for F
where
    F: FnMut(LexicalDocument) -> AstrBotSourceBackedResultV0<()>,
{
    fn emit(&mut self, document: LexicalDocument) -> AstrBotSourceBackedResultV0<()> {
        self(document)
    }
}

pub(crate) fn scan_astrbot_source_backed_v0(
    source: &AstrBotSourceBackedSourceV0,
    sink: &mut impl AstrBotSourceBackedSinkV0,
) -> AstrBotSourceBackedResultV0<CertifiedSource> {
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(&source.path)?;
    let opening_evidence = sqlite_snapshot.evidence().clone();
    let conn = sqlite_snapshot.connection()?;
    let sql = AstrBotSql::new(conn)?;
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
    let opening = source_observation(
        &source.source_key,
        &opening_evidence,
        user_version,
        &schema_fingerprint,
    )?;
    let source_revision_digest = revision_digest(&opening);
    let revision_scope = TypedKey::bytes(source_revision_digest.to_vec())?;
    let mut counts = ScannedSourceCounts::default();
    let mut content_chain = [0_u8; 32];
    let mut native_ordinal = 0_u64;
    let mut conversation_after = None;
    let mut pending_documents = Vec::new();
    let mut checkpoint_links = BTreeMap::new();

    loop {
        let Some(candidate) = fetch_candidate(
            conn,
            &sql.conversation_candidate_initial,
            &sql.conversation_candidate_after,
            conversation_after,
        )?
        else {
            break;
        };
        conversation_after = Some(candidate.physical_rowid);
        add_certified_bytes(&mut counts, candidate.observed_bytes()?)?;
        if candidate.observed_bytes()?
            > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
        {
            content_chain = chain_hash(
                content_chain,
                candidate_hash(
                    b"astrbot-source-backed-conversation-oversize-v0\0",
                    candidate,
                ),
            );
            add_complete(&mut counts)?;
            add_rejected(&mut counts)?;
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
            continue;
        }

        let row =
            hydrate_conversation(conn, &sql.conversation_hydration, candidate.physical_rowid)?;
        let row_digest = logical_values_digest(&super::super::super::model::conversation_values(
            row.clone(),
        ));
        content_chain = chain_hash(content_chain, row_digest);
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
            add_complete(&mut counts)?;
            let item = items.get(item_index);
            let event =
                source_backed_conversation_event(&row, item, content_is_array, native_ordinal);
            if let Some(event) = event {
                let complete_text = if content_is_array {
                    item.and_then(item_text)
                        .filter(|text| !text.trim().is_empty())
                        .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?
                } else {
                    item.and_then(provider_value_text)
                        .filter(|text| !text.trim().is_empty())
                        .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?
                };
                let session = conversation_session_fact(&row);
                let document = conversation_document(
                    source,
                    &source_revision_digest,
                    &revision_scope,
                    candidate.physical_rowid,
                    item_index,
                    row_digest,
                    item,
                    &session,
                    &event,
                    &complete_text,
                )?;
                pending_documents.push(document);
                add_retained(&mut counts)?;
            } else {
                add_ignored(&mut counts)?;
            }
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
        }
    }

    if let (Some(initial), Some(after)) = (
        sql.platform_message_candidate_initial.as_deref(),
        sql.platform_message_candidate_after.as_deref(),
    ) {
        let mut platform_after = None;
        loop {
            let Some(candidate) = fetch_candidate(conn, initial, after, platform_after)? else {
                break;
            };
            platform_after = Some(candidate.physical_rowid);
            add_certified_bytes(&mut counts, candidate.observed_bytes()?)?;
            add_complete(&mut counts)?;
            if candidate.observed_bytes()?
                > u64::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).unwrap_or(u64::MAX)
            {
                content_chain = chain_hash(
                    content_chain,
                    candidate_hash(b"astrbot-source-backed-platform-oversize-v0\0", candidate),
                );
                add_rejected(&mut counts)?;
            } else {
                let (unit, rejection, row_digest, complete_text) = source_backed_platform_unit(
                    conn,
                    &sql,
                    candidate,
                    native_ordinal,
                    &checkpoint_links,
                )?;
                content_chain = chain_hash(content_chain, row_digest);
                if rejection.is_some() {
                    add_rejected(&mut counts)?;
                } else if let Some(unit) = unit {
                    if let Some(event) = unit.event {
                        let document = platform_document(
                            source,
                            &source_revision_digest,
                            candidate.physical_rowid,
                            candidate.legacy_order.logical_id,
                            row_digest,
                            &unit.session,
                            &event,
                            complete_text
                                .as_deref()
                                .ok_or(AstrBotSourceBackedErrorV0::ExactConversationMismatch)?,
                        )?;
                        pending_documents.push(document);
                        add_retained(&mut counts)?;
                    } else {
                        add_ignored(&mut counts)?;
                    }
                } else {
                    add_ignored(&mut counts)?;
                }
            }
            native_ordinal = native_ordinal
                .checked_add(1)
                .ok_or(AstrBotSourceBackedErrorV0::CountOverflow)?;
        }
    }

    let closing_evidence = sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    let closing = source_observation(
        &source.source_key,
        &closing_evidence,
        user_version,
        &schema_fingerprint,
    )?;
    let mut digest = Sha256::new();
    digest.update(b"ctx-astrbot-source-backed-content-v0\0");
    digest.update(content_chain);
    digest.update(counts.complete_records.to_be_bytes());
    digest.update(counts.certified_bytes.to_be_bytes());
    let certificate = CertifiedSource::certify(
        opening,
        closing,
        PARSER_REVISION,
        digest.finalize().into(),
        counts,
    )?;
    for document in pending_documents {
        sink.emit(document)?;
    }
    Ok(certificate)
}

fn source_backed_platform_unit(
    conn: &rusqlite::Connection,
    sql: &AstrBotSql,
    candidate: RowCandidate,
    native_ordinal: u64,
    checkpoint_links: &BTreeMap<String, PlatformMessageLink>,
) -> AstrBotSourceBackedResultV0<PlatformUnitProjection> {
    let hydration =
        sql.platform_message_hydration
            .as_deref()
            .ok_or(CaptureError::SystemInvariant(
                "AstrBot platform-message hydration SQL is missing",
            ))?;
    let row = hydrate_platform_message(conn, hydration, candidate.physical_rowid)?;
    let row_sha256 = serialized_hash(b"astrbot-platform-row-v1\0", &row)?;
    let link = row
        .llm_checkpoint_id
        .as_ref()
        .and_then(|checkpoint| checkpoint_links.get(checkpoint));
    let Some(text) = row
        .content
        .as_deref()
        .map(provider_json_text)
        .as_ref()
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
    else {
        return Ok((None, None, row_sha256, None));
    };
    let session = platform_session_fact(&row, link);
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
        Some(text),
    ))
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
