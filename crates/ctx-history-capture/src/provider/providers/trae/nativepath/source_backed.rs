use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    derive_event_id, derive_session_id, AgentType, CaptureProvider, CertifiedSource, CoreRecord,
    CoreRecordError, EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceKey, SubrecordSelector,
    TypedKey,
};
use thiserror::Error;

use super::scanner::{
    absolute_trae_path, acquire_source, packed_native_index, TraeCoreRecord, TraeFrontier,
    TraeScanner, TraeSourceAuthority,
};
use crate::{
    provider_sources::{SqliteLogicalSnapshot, SqliteSourceEvidence},
    CaptureError,
};

use super::super::TRAE_STATE_VSCDB_SOURCE_FORMAT;

mod replacement;

pub(crate) use replacement::TraeReplacementTree;

const TRAE_SOURCE_ANCHOR_NAMESPACE: &str = "trae.workspace-storage";
const TRAE_SOURCE_SCHEMA_VARIANT: &str = "trae-itemtable-json-v1";
const TRAE_SOURCE_BACKED_PARSER_REVISION: &str = "trae-itemtable-source-backed-v1";
const TRAE_NATIVE_SESSION_NAMESPACE: &str = "trae.itemtable-session-v1";
const TRAE_SESSION_POSITION_KIND: &str = "trae.itemtable-session-position-v1";
const TRAE_NATIVE_ITEM_NAMESPACE: &str = "trae.itemtable-key-v1";
const TRAE_NATIVE_MESSAGE_NAMESPACE: &str = "trae.itemtable-message-v1";
const TRAE_MESSAGE_POSITION_KIND: &str = "trae.itemtable-message-position-v1";
const TRAE_LOGICAL_SESSION_KIND: &str = "trae-session";
const TRAE_LOGICAL_EVENT_KIND: &str = "trae-message";
const TRAE_SOURCE_BACKED_PAGE_ROWS: usize = 64;

#[derive(Debug, Error)]
pub(crate) enum TraeSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error("Trae source-backed adapter requires an explicit state.vscdb leaf")]
    ExplicitLeafRequired,
    #[error("Trae source-backed scan counters overflowed or did not reconcile")]
    CountMismatch,
}

pub(crate) type TraeSourceBackedResultV0<T> = std::result::Result<T, TraeSourceBackedErrorV0>;

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedPageV0 {
    pub(crate) documents: Vec<CoreRecord>,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceBackedScanV0 {
    pub(crate) source: CertifiedSource,
    pub(crate) terminal_fence: TraeSourceTerminalFence,
    pub(crate) row_decode_passes: u64,
    pub(crate) decoded_rows: u64,
    pub(crate) emitted_pages: u64,
    pub(crate) peak_buffered_documents: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct TraeSourceTerminalFence {
    evidence: SqliteSourceEvidence,
}

pub(super) fn scan_trae_authority(
    canonical_path: &Path,
    authority: &TraeSourceAuthority,
    emit: &mut dyn FnMut(TraeSourceBackedPageV0) -> TraeSourceBackedResultV0<()>,
) -> TraeSourceBackedResultV0<TraeSourceBackedScanV0> {
    let source = source_key(authority)?;
    let mut scanner = TraeScanner::new(authority, TraeFrontier::default());
    let mut counts = ScannedSourceCounts::default();
    let mut emitted_pages = 0_u64;
    let mut peak_buffered_documents = 0_u64;
    while let Some(page) = scanner.next_page()? {
        let complete_records = u64::try_from(page.logical_units)
            .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let rejected_records = u64::try_from(page.rejections.len())
            .map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let mut documents = Vec::with_capacity(page.core.len());
        for record in page.core {
            if let Some(document) = core_record(&source, authority, record)? {
                documents.push(document);
            }
        }
        let retained_records =
            u64::try_from(documents.len()).map_err(|_| TraeSourceBackedErrorV0::CountMismatch)?;
        let ignored_records = complete_records
            .checked_sub(
                retained_records
                    .checked_add(rejected_records)
                    .ok_or(TraeSourceBackedErrorV0::CountMismatch)?,
            )
            .ok_or(TraeSourceBackedErrorV0::CountMismatch)?;

        counts.complete_records = checked_add(counts.complete_records, complete_records)?;
        counts.retained_records = checked_add(counts.retained_records, retained_records)?;
        counts.rejected_records = checked_add(counts.rejected_records, rejected_records)?;
        counts.ignored_records = checked_add(counts.ignored_records, ignored_records)?;
        counts.indexed_documents = checked_add(counts.indexed_documents, retained_records)?;
        peak_buffered_documents = peak_buffered_documents.max(retained_records);
        if !documents.is_empty() {
            if documents.len() > TRAE_SOURCE_BACKED_PAGE_ROWS {
                return Err(TraeSourceBackedErrorV0::CountMismatch);
            }
            emitted_pages = checked_add(emitted_pages, 1)?;
            emit(TraeSourceBackedPageV0 { documents })?;
        }
    }

    let terminal_evidence = authority.database.seal(canonical_path)?;
    counts.certified_bytes = scanner.certified_source_bytes();
    let decoded_rows = scanner.decoded_rows();
    let source = SqliteLogicalSnapshot::new(
        TRAE_SOURCE_BACKED_PARSER_REVISION,
        &authority.schema_evidence,
        scanner.source_content_digest(),
        counts,
    )
    .certify(source)?;
    Ok(TraeSourceBackedScanV0 {
        source,
        terminal_fence: TraeSourceTerminalFence {
            evidence: terminal_evidence,
        },
        row_decode_passes: 1,
        decoded_rows,
        emitted_pages,
        peak_buffered_documents,
    })
}

fn core_record(
    source: &SourceKey,
    authority: &TraeSourceAuthority,
    record: TraeCoreRecord,
) -> TraeSourceBackedResultV0<Option<CoreRecord>> {
    let body = record.lexical_text;
    if body.is_empty() {
        return Ok(None);
    }
    let revision_scope = TypedKey::bytes(record.value_digest.to_vec())?;
    let session_key = if record.native_session_id_from_provider {
        NativeSessionKey::composite(
            TRAE_NATIVE_SESSION_NAMESPACE,
            vec![
                TypedKey::utf8(record.chat_key)?,
                TypedKey::utf8(&record.native_session_id)?,
            ],
        )?
    } else {
        NativeSessionKey::revision_scoped_position(
            TRAE_SESSION_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::from(record.key_index)),
                TypedKey::U64(u64::from(record.raw_session_index)),
            ])?,
            revision_scope.clone(),
        )?
    };
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: TRAE_LOGICAL_SESSION_KIND,
        native_session_key: &session_key,
    })?;
    let item_key =
        NativeItemKey::native_id(TRAE_NATIVE_ITEM_NAMESPACE, TypedKey::utf8(record.chat_key)?)?;
    let subrecord = if record.native_message_id_from_provider {
        SubrecordSelector::composite(
            TRAE_NATIVE_MESSAGE_NAMESPACE,
            vec![
                TypedKey::utf8(&record.native_session_id)?,
                TypedKey::utf8(&record.native_message_id)?,
            ],
        )?
    } else {
        SubrecordSelector::revision_scoped_position(
            TRAE_MESSAGE_POSITION_KIND,
            TypedKey::composite(vec![
                TypedKey::U64(u64::from(record.raw_session_index)),
                TypedKey::U64(u64::from(record.message_index)),
            ])?,
            revision_scope,
        )?
    };
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: TRAE_LOGICAL_EVENT_KIND,
        native_item_key: &item_key,
        subrecord_selector: Some(&subrecord),
    })?;
    let native_event_id = TypedKey::composite(vec![
        TypedKey::utf8(record.chat_key)?,
        TypedKey::U64(u64::from(record.raw_session_index)),
        TypedKey::U64(u64::from(record.message_index)),
        TypedKey::utf8(&record.provider_session_id)?,
    ])?;
    let event_sequence = packed_native_index(
        record.key_index,
        record.raw_session_index,
        record.message_index,
    )?;
    let mut projected = CoreRecord::new_selected(
        event_id,
        session_id,
        session_id,
        source.clone(),
        event_sequence,
        record.event_type.as_str(),
        AgentType::Primary.as_str(),
        true,
        TRAE_SOURCE_BACKED_PARSER_REVISION,
        body,
    )?;
    projected.provider_session_id = Some(record.provider_session_id);
    projected.native_event_id = Some(native_event_id);
    projected.occurred_at_unix_ms = Some(record.occurred_at.timestamp_millis());
    projected.role = record.role.map(|role| role.as_str().to_owned());
    projected.workspace = authority
        .workspace_folder
        .clone()
        .or_else(|| Some(authority.workspace_id.clone()));
    projected.cwd = authority.workspace_folder.clone();
    projected.validate_contract()?;
    Ok(Some(projected))
}

fn source_key(authority: &TraeSourceAuthority) -> TraeSourceBackedResultV0<SourceKey> {
    source_key_for_workspace(&authority.workspace_id)
}

fn source_key_for_workspace(workspace_id: &str) -> TraeSourceBackedResultV0<SourceKey> {
    let anchor =
        SourceAnchor::provider_native(TRAE_SOURCE_ANCHOR_NAMESPACE, TypedKey::utf8(workspace_id)?)?;
    Ok(SourceKey::derive(
        CaptureProvider::Trae.as_str(),
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        TRAE_SOURCE_SCHEMA_VARIANT,
        1,
        anchor,
    )?)
}

pub(super) fn explicit_trae_leaf(path: &Path) -> TraeSourceBackedResultV0<PathBuf> {
    crate::common::io::ensure_provider_path_parents_are_not_symlinks(path)?;
    let metadata = fs::symlink_metadata(path).map_err(CaptureError::from)?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_file()
        || path.file_name().and_then(|name| name.to_str()) != Some("state.vscdb")
    {
        return Err(TraeSourceBackedErrorV0::ExplicitLeafRequired);
    }
    Ok(absolute_trae_path(path)?)
}

fn checked_add(left: u64, right: u64) -> TraeSourceBackedResultV0<u64> {
    left.checked_add(right)
        .ok_or(TraeSourceBackedErrorV0::CountMismatch)
}

#[cfg(test)]
mod tests {
    #[test]
    fn direct_core_projection_is_self_contained() {
        let production = include_str!("source_backed.rs")
            .split("\n#[cfg(test)]\nmod tests {")
            .next()
            .expect("production source");
        assert!(production.contains("CoreRecord::new_selected"));
        assert!(production.contains("native_event_id = Some"));
        assert!(production.contains("TRAE_SOURCE_BACKED_PARSER_REVISION"));
        assert!(production.contains("let body = record.lexical_text"));
        assert!(production.contains("validate_contract"));
        assert!(!production.contains("body.truncate"));
        assert!(!production.contains("body.chars().take"));
        for removed_api in [
            concat!("Lexical", "Document"),
            concat!("SourceRecord", "Locator"),
            concat!("hyd", "rate_"),
            concat!("resol", "ver"),
        ] {
            assert!(!production.contains(removed_api), "found {removed_api}");
        }
    }
}
