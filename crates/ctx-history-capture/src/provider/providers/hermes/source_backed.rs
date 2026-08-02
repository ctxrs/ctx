//! Provider-local source-backed Hermes adapter.
//!
//! This module deliberately stops at discovery, bounded native projection,
//! source certification, and complete direct Core projection. Publication,
//! replacement/deletion lifecycle, and projection fanout remain shared
//! responsibilities.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, CaptureProvider, CertifiedSource, CoreRecord, CoreRecordError,
    EventIdentityInput, NativeItemKey, ProjectionContractError, ScannedSourceCounts, SourceAnchor,
    SourceKey, TypedKey,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    common::io::ProviderSourceRoot,
    provider::{
        native_ingestion::{NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS},
        sqlite::sqlite_schema_fingerprint,
    },
    provider_sources::{
        retain_sqlite_source_directory_authority, ProviderSource, SqliteLogicalSnapshot,
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, HERMES_SQLITE_SOURCE_FORMAT, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    hermes_layout_record_digest, hermes_native_event,
    layout::{HermesMessageRow, HermesSchema, HermesSessionRow},
    sqlite::{HermesNativeRecord, HermesNativeRow, HermesPhase, HermesRowReader},
    HERMES_CAPTURE_REVISION, HERMES_POLICY_REVISION,
};

const HERMES_SOURCE_ANCHOR_NAMESPACE: &str = "hermes.profile";
const HERMES_SESSION_NAMESPACE: &str = "hermes.session";
const HERMES_MESSAGE_NAMESPACE: &str = "hermes.message";
const HERMES_LOGICAL_SESSION_KIND: &str = "hermes-session";
const HERMES_LOGICAL_EVENT_KIND: &str = "hermes-message";
const HERMES_SOURCE_SCHEMA_VARIANT: &str = "hermes-state-db-v1";
const SQLITE_SOURCE_INVALID_REASON: &str =
    "Hermes SQLite source must have an authorized parent and database leaf";
#[cfg(test)]
const HERMES_LEGACY_SOURCE_PARSER_REVISION: &str = "hermes-source-backed-v1";
const HERMES_SOURCE_PARSER_REVISION: &str = "hermes-source-backed-v2";
const HERMES_SOURCE_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-snapshot-v1\0";
const HERMES_TREE_FINGERPRINT_DOMAIN: &[u8] = b"ctx-hermes-source-inventory-v1\0";
const HERMES_SESSION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-session-v1\0";
const HERMES_REJECTION_DIGEST_DOMAIN: &[u8] = b"ctx-hermes-source-backed-rejection-v1\0";

mod ancestry;
mod contracts;
mod replacement;

use ancestry::{HermesSessionContext, HermesSessionContextMemo, HermesSessionResolution};
pub(crate) use contracts::*;

struct HermesSnapshotProjection {
    certificate: CertifiedSource,
    decoded_rows: u64,
    emitted_pages: u64,
    peak_buffered_records: u64,
    native_candidate_query_batches: u64,
    native_hydration_query_batches: u64,
    max_native_rows_per_set: u64,
    direct_context_query_batches: u64,
    ancestry_query_batches: u64,
    max_context_query_batches_per_page: u64,
    max_direct_context_rows_per_query: u64,
    max_ancestry_rows_per_query: u64,
    max_direct_context_bytes_per_query: u64,
    max_ancestry_bytes_per_query: u64,
    peak_context_cache_rows: u64,
    peak_context_cache_bytes: u64,
}

fn project_hermes_snapshot(
    candidate: &HermesSourceCandidate,
    conn: &rusqlite::Connection,
    emit: &mut dyn FnMut(HermesSourceBackedPage) -> HermesSourceBackedResult<()>,
) -> HermesSourceBackedResult<HermesSnapshotProjection> {
    candidate.source.validate_contract()?;
    let source_path = candidate
        .path
        .to_str()
        .ok_or_else(|| HermesSourceBackedError::InvalidProfilePath(candidate.path.clone()))?
        .to_owned();
    let sqlite_user_version = conn
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(CaptureError::from)?;
    let schema_fingerprint = sqlite_schema_fingerprint(conn)?;
    let schema = HermesSchema::detect(conn)?;
    let schema_evidence = hermes_schema_evidence(sqlite_user_version, &schema_fingerprint);

    let mut reader = HermesRowReader::new(conn, &schema)?;
    let mut context_memo = HermesSessionContextMemo::new(conn, &schema, &candidate.source);
    let operation: HermesSourceBackedResult<(ScannedSourceCounts, [u8; 32], u64, u64, u64)> =
        (|| {
            let mut frontier = super::sqlite::HermesFrontier::initial();
            let mut digest = Sha256::new();
            digest.update(HERMES_SOURCE_DIGEST_DOMAIN);
            let mut counts = ScannedSourceCounts::default();
            let mut page_records = Vec::new();
            let mut page_owned_bytes = 0_usize;
            let mut page_completed_bytes = 0_u64;
            let mut decoded_rows = 0_u64;
            let mut emitted_pages = 0_u64;
            let mut peak_buffered_records = 0_u64;

            loop {
                let native_page = reader.next_page(frontier)?;
                if native_page.is_empty() {
                    break;
                }
                frontier = native_page
                    .last()
                    .map(|native| native.next_frontier)
                    .unwrap_or(frontier);
                let requested_sessions = native_page
                    .iter()
                    .filter_map(|native| match &native.record {
                        HermesNativeRecord::Session(row) => Some(row.id.clone()),
                        HermesNativeRecord::Message { row, .. } => Some(row.session_id.clone()),
                        HermesNativeRecord::Rejected(_) => None,
                    })
                    .collect::<BTreeSet<_>>();
                let session_contexts = context_memo.resolve_page(&requested_sessions)?;
                for native in native_page {
                    decoded_rows = checked_add(decoded_rows, 1)?;
                    counts.complete_records = checked_add(counts.complete_records, 1)?;
                    let observed_bytes = u64::try_from(native.observed_bytes)
                        .map_err(|_| HermesSourceBackedError::CountOverflow)?;
                    counts.certified_bytes = checked_add(counts.certified_bytes, observed_bytes)?;

                    let logical_digest = native_record_digest(&native)?;
                    digest.update([match native.locator.phase {
                        HermesPhase::Sessions => 1,
                        HermesPhase::Messages => 2,
                    }]);
                    digest.update(native.ordinal.to_be_bytes());
                    digest.update(observed_bytes.to_be_bytes());
                    digest.update(logical_digest);

                    let record = project_native_row(
                        &candidate.source,
                        &source_path,
                        native,
                        &session_contexts,
                    )?;
                    let (record, owned_bytes) = bound_projected_record(record)?;

                    match &record {
                        HermesSourceBackedRecord::Session(_) => {
                            counts.retained_records = checked_add(counts.retained_records, 1)?;
                        }
                        HermesSourceBackedRecord::Event(_) => {
                            counts.retained_records = checked_add(counts.retained_records, 1)?;
                            counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                        }
                        HermesSourceBackedRecord::Rejected(_) => {
                            counts.rejected_records = checked_add(counts.rejected_records, 1)?;
                        }
                    }

                    if !page_records.is_empty()
                        && (page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS
                            || page_owned_bytes.saturating_add(owned_bytes)
                                > NATIVE_INGESTION_PAGE_MAX_BYTES)
                    {
                        let records = std::mem::take(&mut page_records);
                        peak_buffered_records = peak_buffered_records.max(
                            u64::try_from(records.len())
                                .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                        );
                        emit(HermesSourceBackedPage {
                            records,
                            completed_bytes: page_completed_bytes,
                        })?;
                        emitted_pages = checked_add(emitted_pages, 1)?;
                        page_owned_bytes = 0;
                        page_completed_bytes = 0;
                    }
                    page_owned_bytes = page_owned_bytes.saturating_add(owned_bytes);
                    page_completed_bytes = checked_add(page_completed_bytes, observed_bytes)?;
                    page_records.push(record);
                    if page_records.len() == NATIVE_INGESTION_PAGE_MAX_UNITS {
                        let records = std::mem::take(&mut page_records);
                        peak_buffered_records = peak_buffered_records.max(
                            u64::try_from(records.len())
                                .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                        );
                        emit(HermesSourceBackedPage {
                            records,
                            completed_bytes: page_completed_bytes,
                        })?;
                        emitted_pages = checked_add(emitted_pages, 1)?;
                        page_owned_bytes = 0;
                        page_completed_bytes = 0;
                    }
                }
            }
            if !page_records.is_empty() {
                peak_buffered_records = peak_buffered_records.max(
                    u64::try_from(page_records.len())
                        .map_err(|_| HermesSourceBackedError::CountOverflow)?,
                );
                emit(HermesSourceBackedPage {
                    records: page_records,
                    completed_bytes: page_completed_bytes,
                })?;
                emitted_pages = checked_add(emitted_pages, 1)?;
            }
            Ok((
                counts,
                digest.finalize().into(),
                decoded_rows,
                emitted_pages,
                peak_buffered_records,
            ))
        })();
    let reader_counters = reader.counters();
    let context_counters = context_memo.counters();
    drop(reader);
    drop(context_memo);

    let (counts, content_digest, decoded_rows, emitted_pages, peak_buffered_records) = operation?;
    let certificate = SqliteLogicalSnapshot::new(
        HERMES_SOURCE_PARSER_REVISION,
        &schema_evidence,
        content_digest,
        counts,
    )
    .certify(candidate.source.clone())?;
    #[cfg(test)]
    record_logical_row_traversal();
    Ok(HermesSnapshotProjection {
        certificate,
        decoded_rows,
        emitted_pages,
        peak_buffered_records,
        native_candidate_query_batches: reader_counters.candidate_query_batches,
        native_hydration_query_batches: reader_counters.hydration_query_batches,
        max_native_rows_per_set: reader_counters.max_hydration_rows,
        direct_context_query_batches: context_counters.direct_query_batches,
        ancestry_query_batches: context_counters.ancestry_query_batches,
        max_context_query_batches_per_page: context_counters.max_query_batches_per_page,
        max_direct_context_rows_per_query: context_counters.max_direct_rows_per_query,
        max_ancestry_rows_per_query: context_counters.max_ancestry_rows_per_query,
        max_direct_context_bytes_per_query: context_counters.max_direct_bytes_per_query,
        max_ancestry_bytes_per_query: context_counters.max_ancestry_bytes_per_query,
        peak_context_cache_rows: context_counters.peak_cache_rows,
        peak_context_cache_bytes: context_counters.peak_cache_bytes,
    })
}

fn checked_add(left: u64, right: u64) -> HermesSourceBackedResult<u64> {
    left.checked_add(right)
        .ok_or(HermesSourceBackedError::CountOverflow)
}

fn open_root_authorized_snapshot(
    data_root: &Path,
    path: &Path,
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    open_root_authorized_snapshot_with_hook(data_root, path, || {})
}

fn open_root_authorized_snapshot_with_hook(
    data_root: &Path,
    path: &Path,
    after_authorize: impl FnOnce(),
) -> HermesSourceBackedResult<(SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let admission_root = ProviderSourceRoot::open(parent)?;
    let admission_directory = admission_root.directory()?;
    let parent_handle = admission_directory
        .try_clone_authority_handle()
        .map_err(CaptureError::from)?;
    let sqlite_authority =
        retain_sqlite_source_directory_authority(data_root, &parent_handle, parent)?;
    let sqlite_snapshot = sqlite_authority.open_logical_online_backup_snapshot(database_leaf)?;
    after_authorize();
    sqlite_snapshot.revalidate()?;
    let connection = sqlite_snapshot.connection()?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES)
        .map_err(|_| HermesSourceBackedError::CountOverflow)?;
    connection.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(CaptureError::from)?;
    Ok((sqlite_authority, sqlite_snapshot))
}

fn hermes_schema_evidence(sqlite_user_version: i64, schema_fingerprint: &str) -> Vec<u8> {
    format!(
        "hermes-logical-schema-v1:capture={HERMES_CAPTURE_REVISION};\
         policy={HERMES_POLICY_REVISION};user_version={sqlite_user_version};\
         schema={schema_fingerprint}",
    )
    .into_bytes()
}

fn hermes_tree_fingerprint(source: &SourceKey) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HERMES_TREE_FINGERPRINT_DOMAIN);
    digest.update(source.exact_descriptor_digest());
    digest.finalize().into()
}

#[cfg(test)]
std::thread_local! {
    static HERMES_LOGICAL_ROW_TRAVERSALS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn reset_logical_row_traversals() {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(|count| count.set(0));
}

#[cfg(test)]
pub(crate) fn logical_row_traversals() -> u64 {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(std::cell::Cell::get)
}

#[cfg(test)]
fn record_logical_row_traversal() {
    HERMES_LOGICAL_ROW_TRAVERSALS.with(|count| {
        count.set(count.get().saturating_add(1));
    });
}

fn project_native_row(
    source: &SourceKey,
    source_path: &str,
    native: HermesNativeRow,
    session_contexts: &BTreeMap<String, HermesSessionResolution>,
) -> HermesSourceBackedResult<HermesSourceBackedRecord> {
    let ordinal = native.ordinal;
    match native.record {
        HermesNativeRecord::Session(row) => {
            let context = match session_contexts.get(&row.id) {
                Some(HermesSessionResolution::Context(context)) => context,
                Some(HermesSessionResolution::Rejected(reason)) => {
                    return Ok(rejected(reason.to_string()));
                }
                Some(HermesSessionResolution::Missing) | None => {
                    return Ok(rejected(format!(
                        "Hermes session {} disappeared during projection",
                        row.id
                    )));
                }
            };
            match project_session(source_path, row, context) {
                Ok(session) => Ok(HermesSourceBackedRecord::Session(session)),
                Err(error) => Ok(rejected(error.to_string())),
            }
        }
        HermesNativeRecord::Message {
            row,
            values: _,
            prepared,
        } => {
            let context = match session_contexts.get(&row.session_id) {
                Some(HermesSessionResolution::Context(context)) => context,
                Some(HermesSessionResolution::Rejected(reason)) => {
                    return Ok(rejected(reason.to_string()));
                }
                Some(HermesSessionResolution::Missing) | None => {
                    return Ok(rejected(format!(
                        "Hermes message {} depends on missing session {}",
                        row.id, row.session_id
                    )));
                }
            };
            match project_message(source, ordinal, row, prepared, context) {
                Ok(document) => Ok(HermesSourceBackedRecord::Event(document)),
                Err(error) => Ok(rejected(error.to_string())),
            }
        }
        HermesNativeRecord::Rejected(reason) => Ok(rejected(reason)),
    }
}

fn rejected(reason: String) -> HermesSourceBackedRecord {
    HermesSourceBackedRecord::Rejected(HermesSourceBackedRejection { reason })
}

fn project_session(
    source_path: &str,
    row: HermesSessionRow,
    context: &HermesSessionContext,
) -> HermesSourceBackedResult<HermesSourceBackedSession> {
    Ok(HermesSourceBackedSession {
        provider_session_id: row.id,
        provider_parent_session_id: row.parent_session_id,
        branch: context.branch.clone(),
        source_path: source_path.to_owned(),
        agent_type: context.agent_type.clone(),
        workspace: context.workspace.clone(),
        cwd: context.cwd.clone(),
    })
}

fn project_message(
    source: &SourceKey,
    ordinal: u64,
    row: HermesMessageRow,
    prepared: Option<super::HermesPreparedCoreMessage>,
    session: &HermesSessionContext,
) -> HermesSourceBackedResult<CoreRecord> {
    let native = match prepared {
        Some(prepared) => prepared.native,
        None => hermes_native_event(&row, ordinal)?,
    };
    let body = native.complete_text;
    let native_item_key = NativeItemKey::composite(
        HERMES_MESSAGE_NAMESPACE,
        vec![TypedKey::utf8(&row.session_id)?, TypedKey::I64(row.id)],
    )?;
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id: session.session_id,
        logical_item_kind: HERMES_LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(&row.session_id)?,
        TypedKey::I64(row.id),
    ])?;
    let native_tool = (row.tool_name.is_some()
        || row.tool_call_id.is_some()
        || row.tool_calls.is_some())
    .then(|| {
        serde_json::json!({
            "name": row.tool_name,
            "call_id": row.tool_call_id,
            "calls": row.tool_calls,
        })
    });
    let mut record = CoreRecord::new_selected(
        event_id,
        session.session_id,
        session.root_session_id,
        source.clone(),
        native.provider_event_index,
        native.event_type.as_str(),
        session.agent_type.clone(),
        session.is_primary,
        HERMES_SOURCE_PARSER_REVISION,
        body,
    )?;
    record.parent_session_id = session.parent_session_id;
    record.provider_session_id = Some(row.session_id);
    record.native_event_id = Some(native_event_id);
    record.occurred_at_unix_ms = Some(native.occurred_at.timestamp_millis());
    record.role = native.role.map(|role| role.as_str().to_owned());
    record.branch = session.branch.clone();
    record.workspace = session.workspace.clone();
    record.cwd = session.cwd.clone();
    if let Some(native_tool) = native_tool {
        record.content.structured_content = Some(serde_json::json!({
            "provider_native_tool": native_tool,
        }));
    }
    record.validate_contract()?;
    Ok(record)
}

fn bound_projected_record(
    record: HermesSourceBackedRecord,
) -> HermesSourceBackedResult<(HermesSourceBackedRecord, usize)> {
    let owned_bytes = projected_owned_bytes(&record)?;
    if owned_bytes <= NATIVE_INGESTION_PAGE_MAX_BYTES {
        return Ok((record, owned_bytes));
    }
    let record = rejected(format!(
        "Hermes projected row requires {owned_bytes} bytes and exceeds the {}-byte page limit",
        NATIVE_INGESTION_PAGE_MAX_BYTES
    ));
    let owned_bytes = projected_owned_bytes(&record)?;
    Ok((record, owned_bytes))
}

fn projected_owned_bytes(record: &HermesSourceBackedRecord) -> Result<usize, serde_json::Error> {
    let fixed = 1024_usize;
    match record {
        HermesSourceBackedRecord::Session(session) => Ok(fixed
            .saturating_add(session.provider_session_id.len())
            .saturating_add(
                session
                    .provider_parent_session_id
                    .as_deref()
                    .map(str::len)
                    .unwrap_or(0),
            )
            .saturating_add(session.branch.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.source_path.len())
            .saturating_add(session.agent_type.len())
            .saturating_add(session.workspace.as_deref().map(str::len).unwrap_or(0))
            .saturating_add(session.cwd.as_deref().map(str::len).unwrap_or(0))),
        HermesSourceBackedRecord::Event(event) => {
            Ok(fixed.saturating_add(serde_json::to_vec(event)?.len()))
        }
        HermesSourceBackedRecord::Rejected(rejection) => {
            Ok(fixed.saturating_add(rejection.reason.len()))
        }
    }
}

fn native_record_digest(native: &HermesNativeRow) -> HermesSourceBackedResult<[u8; 32]> {
    match &native.record {
        HermesNativeRecord::Session(row) => Ok(session_record_digest(row)),
        HermesNativeRecord::Message {
            values, prepared, ..
        } => {
            if !values.is_empty() {
                decode_sha256(hermes_layout_record_digest(values).as_str())
            } else if let Some(prepared) = prepared {
                decode_sha256(prepared.record_digest.as_str())
            } else {
                Err(HermesSourceBackedError::InvalidLogicalDigest)
            }
        }
        HermesNativeRecord::Rejected(reason) => {
            let mut digest = Sha256::new();
            digest.update(HERMES_REJECTION_DIGEST_DOMAIN);
            digest.update(reason.as_bytes());
            Ok(digest.finalize().into())
        }
    }
}

fn session_record_digest(row: &HermesSessionRow) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(HERMES_SESSION_DIGEST_DOMAIN);
    hash_text(&mut digest, &row.id);
    hash_text(&mut digest, &row.source);
    hash_optional_text(&mut digest, row.parent_session_id.as_deref());
    hash_optional_text(&mut digest, row.model.as_deref());
    hash_optional_text(&mut digest, row.model_config.as_deref());
    digest.update(row.started_at.to_bits().to_be_bytes());
    hash_optional_f64(&mut digest, row.ended_at);
    hash_optional_text(&mut digest, row.end_reason.as_deref());
    digest.update(row.message_count.to_be_bytes());
    digest.update(row.tool_call_count.to_be_bytes());
    digest.update(row.input_tokens.to_be_bytes());
    digest.update(row.output_tokens.to_be_bytes());
    digest.update(row.cache_read_tokens.to_be_bytes());
    digest.update(row.cache_write_tokens.to_be_bytes());
    digest.update(row.reasoning_tokens.to_be_bytes());
    hash_optional_text(&mut digest, row.cwd.as_deref());
    hash_optional_text(&mut digest, row.git_branch.as_deref());
    hash_optional_text(&mut digest, row.git_repo_root.as_deref());
    hash_optional_text(&mut digest, row.billing_provider.as_deref());
    hash_optional_text(&mut digest, row.billing_base_url.as_deref());
    hash_optional_text(&mut digest, row.billing_mode.as_deref());
    hash_optional_f64(&mut digest, row.estimated_cost_usd);
    hash_optional_f64(&mut digest, row.actual_cost_usd);
    hash_optional_text(&mut digest, row.title.as_deref());
    digest.update(row.archived.to_be_bytes());
    digest.finalize().into()
}

fn hash_text(digest: &mut Sha256, value: &str) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
}

fn hash_optional_text(digest: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            digest.update([1]);
            hash_text(digest, value);
        }
        None => digest.update([0]),
    }
}

fn hash_optional_f64(digest: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            digest.update([1]);
            digest.update(value.to_bits().to_be_bytes());
        }
        None => digest.update([0]),
    }
}

fn decode_sha256(value: &str) -> HermesSourceBackedResult<[u8; 32]> {
    if value.len() != 64 {
        return Err(HermesSourceBackedError::InvalidLogicalDigest);
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        digest[index] = (high << 4) | low;
    }
    Ok(digest)
}

fn decode_hex_nibble(value: u8) -> HermesSourceBackedResult<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(HermesSourceBackedError::InvalidLogicalDigest),
    }
}

#[cfg(test)]
mod tests;
