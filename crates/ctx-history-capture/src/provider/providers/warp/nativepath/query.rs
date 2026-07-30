mod conversations;
mod tasks;

use conversations::{emit_sessions_and_hierarchy, scan_conversations};
use tasks::scan_tasks;

use std::collections::{BTreeMap, BTreeSet, HashSet};

use chrono::{DateTime, NaiveDateTime, Utc};
use rusqlite::{types::ValueRef, Connection, Statement};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::super::schema::{warp_quote_identifier, WarpSqliteSchema};
use super::decode::{
    decode_warp_native_task, WarpDecodeCounters, WarpDecodedMessage, WarpDecodedMessagePayload,
};
use super::model::{
    hex_digest, WarpNativeCounters, WarpNativeDigestChain, WarpNativeEvent, WarpNativeEventDraft,
    WarpNativeHierarchyEdge, WarpNativeMessageIdentity, WarpNativePageAccumulator,
    WarpNativeRejection, WarpNativeRejectionKind, WarpNativeSession, WarpNativeSink,
    WarpNativeUnit, WARP_NATIVE_PAGE_MAX_BYTES, WARP_SOURCE_DIGEST_DOMAIN,
};
use crate::provider::sqlite::SqliteLengthPreflightGuard;
use crate::{
    complete_content::CompleteContentBodyDigest, CaptureError, OutputOutcome, Result,
    MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

const WARP_SESSION_METADATA_MAX_BYTES: usize = 64 * 1024;
const WARP_ORDERING_KEY_MAX_BYTES: usize = 240 * 1024;
const WARP_NATIVE_SQLITE_CONVERSATION_ROW_OVERHEAD_BYTES: u64 = 64 * 4;
const WARP_NATIVE_SQLITE_ROW_OVERHEAD_BYTES: u64 = 64 * 5;

#[derive(Debug)]
struct WarpHierarchyNode {
    parent_conversation_id: Option<String>,
    root_conversation_id: String,
    root_resolved: bool,
    parent_present: bool,
    title: String,
    modified_at: Option<DateTime<Utc>>,
    metadata: Value,
    rejections: Vec<WarpNativeRejection>,
}

#[derive(Debug)]
enum WarpConversationEmission {
    Session {
        conversation_id: String,
        source_digest: [u8; 32],
    },
    Rejection {
        rejection: WarpNativeRejection,
        source_digest: [u8; 32],
    },
}

#[derive(Debug)]
struct WarpTaskCellMetadata {
    storage_class: String,
    bytes: i64,
}

#[derive(Debug)]
struct WarpConversationCandidate {
    rowid: i64,
    conversation_id: WarpTaskCellMetadata,
    conversation_data: WarpTaskCellMetadata,
    last_modified_at: WarpTaskCellMetadata,
    hydrated_conversation_id: Option<String>,
    hydrated_conversation_data: Option<String>,
    hydrated_last_modified_at: Option<String>,
}

#[derive(Debug)]
struct WarpTaskCandidate {
    rowid: i64,
    conversation_id: WarpTaskCellMetadata,
    task_id: WarpTaskCellMetadata,
    task: WarpTaskCellMetadata,
    last_modified_at: WarpTaskCellMetadata,
    hydrated_conversation_id: Option<String>,
    hydrated_task_id: Option<String>,
    hydrated_task: Option<Vec<u8>>,
    hydrated_last_modified_at: Option<String>,
}

pub(super) struct WarpNativeQueryResult {
    pub(super) counters: WarpNativeCounters,
    pub(super) source_integrity_digest: String,
}

/// Scans through a read transaction already pinned by the caller.
///
/// Root-bound source-backed readers use this entry point so their provider-wide
/// compound guard, rather than a per-database wrapper, owns transaction finish.
pub(super) fn scan_warp_native_pinned_snapshot(
    conn: &Connection,
    schema: &WarpSqliteSchema,
    sink: &mut dyn WarpNativeSink,
) -> Result<WarpNativeQueryResult> {
    let mut builder = WarpNativePageEmitter::new(sink);
    let mut counters = WarpNativeCounters::default();

    let (hierarchy, conversation_emissions) = scan_conversations(conn, &mut counters)?;
    counters.hierarchy_nodes_retained = u64::try_from(hierarchy.len()).unwrap_or(u64::MAX);
    counters.hierarchy_edges = hierarchy
        .values()
        .filter(|node| node.parent_conversation_id.is_some())
        .count()
        .try_into()
        .unwrap_or(u64::MAX);
    emit_sessions_and_hierarchy(
        &hierarchy,
        conversation_emissions,
        &mut builder,
        &mut counters,
    )?;
    scan_tasks(conn, schema, &hierarchy, &mut builder, &mut counters)?;
    let source_integrity_digest = builder.finish()?;
    Ok(WarpNativeQueryResult {
        counters,
        source_integrity_digest,
    })
}

struct WarpNativePageEmitter<'a> {
    sink: &'a mut dyn WarpNativeSink,
    page: WarpNativePageAccumulator,
    source_hasher: WarpNativeDigestChain,
    retained_events: u64,
    legacy_indexed_events: u64,
}

impl<'a> WarpNativePageEmitter<'a> {
    fn new(sink: &'a mut dyn WarpNativeSink) -> Self {
        let source_hasher = WarpNativeDigestChain::new(WARP_SOURCE_DIGEST_DOMAIN);
        Self {
            sink,
            page: WarpNativePageAccumulator::new(),
            source_hasher,
            retained_events: 0,
            legacy_indexed_events: 0,
        }
    }

    fn record_source(&mut self, label: &[u8], digest: [u8; 32]) -> Result<()> {
        self.source_hasher.push(label, digest)
    }

    fn retained_events(&self) -> u64 {
        self.retained_events
    }

    fn legacy_indexed_events(&self) -> u64 {
        self.legacy_indexed_events
    }

    fn advance_legacy_index(&mut self) -> Result<()> {
        self.legacy_indexed_events =
            self.legacy_indexed_events
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp released event index overflowed",
                ))?;
        Ok(())
    }

    fn push(
        &mut self,
        mut unit: WarpNativeUnit,
        native_key: String,
        counters: &mut WarpNativeCounters,
    ) -> Result<()> {
        if unit.estimated_bytes() > WARP_NATIVE_PAGE_MAX_BYTES {
            counters.oversized_normalized_units =
                counters.oversized_normalized_units.saturating_add(1);
            unit = unit.into_oversized_rejection(
                WarpNativeRejectionKind::OversizedNormalizedUnit,
                native_key,
            )?;
        }
        let core_unit = unit.into_core();
        if !self.page.can_accept(&core_unit) {
            self.flush_core()?;
        }
        let retained = u64::try_from(core_unit.retained_event_count())
            .map_err(|_| CaptureError::SystemInvariant("Warp event count exceeds u64"))?;
        self.page.push(core_unit)?;
        self.retained_events =
            self.retained_events
                .checked_add(retained)
                .ok_or(CaptureError::SystemInvariant(
                    "Warp retained event count overflowed",
                ))?;
        if self.page.is_full() {
            self.flush_core()?;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<String> {
        self.flush_core()?;
        Ok(hex_digest(self.source_hasher.state()))
    }

    fn flush_core(&mut self) -> Result<()> {
        let next = WarpNativePageAccumulator::new();
        let page = std::mem::replace(&mut self.page, next);
        let Some(page) = page.finish()? else {
            return Ok(());
        };
        self.sink.push_page(page)?;
        Ok(())
    }
}

fn record_retained_event_counters(counters: &mut WarpNativeCounters, event: &WarpNativeEvent) {
    counters.retained_events = counters.retained_events.saturating_add(1);
    counters.retained_body_bytes = counters
        .retained_body_bytes
        .saturating_add(u64::try_from(event.lexical_body.len()).unwrap_or(u64::MAX));
}

fn hash_rejected_task_candidate(hasher: &mut Sha256, candidate: &WarpTaskCandidate) -> Result<()> {
    hasher.update(b"task-preflight-rejection\0");
    hasher.update(candidate.rowid.to_le_bytes());
    for cell in [
        &candidate.conversation_id,
        &candidate.task_id,
        &candidate.task,
        &candidate.last_modified_at,
    ] {
        hash_bytes(hasher, cell.storage_class.as_bytes())?;
        hasher.update(cell.bytes.to_le_bytes());
    }
    Ok(())
}

fn rejected_task_candidate_digest(candidate: &WarpTaskCandidate) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-task-preflight-evidence-v1\0");
    hash_rejected_task_candidate(&mut hasher, candidate)?;
    Ok(hasher.finalize().into())
}

fn hash_rejected_conversation_candidate(
    hasher: &mut Sha256,
    candidate: &WarpConversationCandidate,
) -> Result<()> {
    hasher.update(b"conversation-preflight-rejection\0");
    hasher.update(candidate.rowid.to_le_bytes());
    for cell in [
        &candidate.conversation_id,
        &candidate.conversation_data,
        &candidate.last_modified_at,
    ] {
        hash_bytes(hasher, cell.storage_class.as_bytes())?;
        hasher.update(cell.bytes.to_le_bytes());
    }
    Ok(())
}

fn rejected_conversation_candidate_digest(
    candidate: &WarpConversationCandidate,
) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-conversation-preflight-evidence-v1\0");
    hash_rejected_conversation_candidate(&mut hasher, candidate)?;
    Ok(hasher.finalize().into())
}

fn source_text_row_digest<const N: usize>(label: &[u8], values: [&str; N]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-native-row-evidence-v1\0");
    hash_source_text_row(&mut hasher, label, values)?;
    Ok(hasher.finalize().into())
}

fn hash_source_text_row<const N: usize>(
    hasher: &mut Sha256,
    label: &[u8],
    values: [&str; N],
) -> Result<()> {
    hasher.update(label);
    for value in values {
        hasher.update([3]);
        hash_bytes(hasher, value.as_bytes())?;
    }
    Ok(())
}

fn merge_decode_counters(counters: &mut WarpNativeCounters, decoded: WarpDecodeCounters) {
    counters.unknown_fields = counters
        .unknown_fields
        .saturating_add(decoded.unknown_fields);
    counters.unknown_oneofs = counters
        .unknown_oneofs
        .saturating_add(decoded.unknown_oneofs);
    counters.native_result_records = counters
        .native_result_records
        .saturating_add(decoded.native_result_records);
    counters.native_result_envelope_bytes = counters
        .native_result_envelope_bytes
        .saturating_add(decoded.native_result_envelope_bytes);
    counters.native_result_body_bytes_observed = counters
        .native_result_body_bytes_observed
        .saturating_add(decoded.native_result_body_bytes_observed);
    counters.native_results_success = counters
        .native_results_success
        .saturating_add(decoded.native_results_success);
    counters.native_results_failure = counters
        .native_results_failure
        .saturating_add(decoded.native_results_failure);
    counters.native_results_timeout = counters
        .native_results_timeout
        .saturating_add(decoded.native_results_timeout);
    counters.native_results_unknown = counters
        .native_results_unknown
        .saturating_add(decoded.native_results_unknown);
    counters.malformed_output_records = counters
        .malformed_output_records
        .saturating_add(decoded.malformed_output_records);
}

fn parse_warp_timestamp(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(raw) {
        return Ok(timestamp.with_timezone(&Utc));
    }
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f").map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "Warp timestamp is not RFC3339 or SQLite UTC text: {raw:?}"
        ))
    })?;
    Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

fn bounded_session_metadata(conversation_data: &Value) -> Result<Value> {
    let bounded_text = |field: &str| {
        conversation_data
            .get(field)
            .and_then(Value::as_str)
            .map(|value| truncate_chars(value, 1_024))
    };
    let mut metadata = json!({
        "agent_name": bounded_text("agent_name"),
        "run_id": bounded_text("run_id"),
        "parent_conversation_id": bounded_text("parent_conversation_id"),
        "server_conversation_token_present": conversation_data
            .get("server_conversation_token")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "forked_from_server_conversation_token_present": conversation_data
            .get("forked_from_server_conversation_token")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "conversation_usage_metadata": conversation_data
            .get("conversation_usage_metadata")
            .cloned()
            .unwrap_or(Value::Null),
    });
    if serde_json::to_vec(&metadata)?.len() > WARP_SESSION_METADATA_MAX_BYTES {
        metadata["conversation_usage_metadata"] =
            json!({"truncated": true, "reason": "bounded_nativepath_metadata"});
    }
    Ok(metadata)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        value.chars().take(limit).collect()
    }
}

fn hash_source_row(hasher: &mut Sha256, label: &[u8], values: &[ValueRef<'_>]) -> Result<()> {
    hasher.update(label);
    for value in values {
        match value {
            ValueRef::Null => hasher.update([0]),
            ValueRef::Integer(value) => {
                hasher.update([1]);
                hasher.update(value.to_le_bytes());
            }
            ValueRef::Real(value) => {
                hasher.update([2]);
                hasher.update(value.to_bits().to_le_bytes());
            }
            ValueRef::Text(value) => {
                hasher.update([3]);
                hash_bytes(hasher, value)?;
            }
            ValueRef::Blob(value) => {
                hasher.update([4]);
                hash_bytes(hasher, value)?;
            }
        }
    }
    Ok(())
}

fn source_row_digest(label: &[u8], values: &[ValueRef<'_>]) -> Result<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-warp-native-row-evidence-v1\0");
    hash_source_row(&mut hasher, label, values)?;
    Ok(hasher.finalize().into())
}

fn complete_content_record_digest(values: &[ValueRef<'_>]) -> Result<CompleteContentBodyDigest> {
    const DOMAIN: &[u8] = b"ctx-complete-content-sqlite-logical-row-v1\0";
    let mut digest = Sha256::new();
    digest.update(DOMAIN);
    digest.update(
        u64::try_from(values.len())
            .map_err(|_| CaptureError::SystemInvariant("Warp SQLite value count overflowed"))?
            .to_be_bytes(),
    );
    for value in values {
        match value {
            ValueRef::Null => digest.update([0]),
            ValueRef::Integer(value) => {
                digest.update([1]);
                digest.update(value.to_be_bytes());
            }
            ValueRef::Real(value) => {
                digest.update([2]);
                digest.update(value.to_bits().to_be_bytes());
            }
            ValueRef::Text(value) => {
                digest.update([3]);
                digest.update(
                    u64::try_from(value.len())
                        .map_err(|_| {
                            CaptureError::SystemInvariant("Warp SQLite text length overflowed")
                        })?
                        .to_be_bytes(),
                );
                digest.update(value);
            }
            ValueRef::Blob(value) => {
                digest.update([4]);
                digest.update(
                    u64::try_from(value.len())
                        .map_err(|_| {
                            CaptureError::SystemInvariant("Warp SQLite blob length overflowed")
                        })?
                        .to_be_bytes(),
                );
                digest.update(value);
            }
        }
    }
    CompleteContentBodyDigest::parse(format!("{:x}", digest.finalize())).ok_or(
        CaptureError::SystemInvariant("Warp complete-content digest was not canonical SHA-256"),
    )
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<()> {
    let length = u64::try_from(value.len()).map_err(|_| {
        CaptureError::SystemInvariant("Warp source digest field length exceeds u64")
    })?;
    hasher.update(length.to_le_bytes());
    hasher.update(value);
    Ok(())
}
