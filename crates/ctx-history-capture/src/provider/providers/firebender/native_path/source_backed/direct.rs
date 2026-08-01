use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{CertifiedSource, CoreRecord, ScannedSourceCounts, SourceKey};
use rusqlite::{params_from_iter, Connection};
use sha2::{Digest, Sha256};

use super::{
    canonical_row_bytes, firebender_core_record, firebender_session_id, firebender_source_key,
    firebender_workspace, increment, FirebenderSourceBackedError, FirebenderSourceBackedResult,
};
use crate::{
    provider::{
        source_backed::{
            family::document::{
                register_replacement_document_tree_route, ChangedDocumentSink,
                CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
                ObservedDocumentLeaf, ReplacementDocumentTree,
            },
            route_error, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
            SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
            SourceBackedRouteSelection,
        },
        sqlite::{sqlite_schema_fingerprint, sqlite_table_columns, SqliteLengthPreflightGuard},
    },
    provider_sources::{
        SqliteLogicalSnapshot, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    },
    CaptureError, ProviderSource,
};

use super::super::{
    firebender_database_path, firebender_raw_row_digest, validate_schema, FirebenderRow,
    FIREBENDER_PAGE_OVERHEAD_BYTES, FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES,
};
pub(super) use super::direct_snapshot::{open_database_leaf, OpenDatabaseLeaf};
use super::direct_snapshot::{MissingLeafFence, OpenedSnapshot};

const DIRECT_PAGE_DOCUMENTS: usize = 64;
const CONTENT_DIGEST_DOMAIN: &[u8] = b"ctx-firebender-logical-content-v2\0";
const LOGICAL_FINGERPRINT_DOMAIN: &[u8] = b"ctx-firebender-logical-fingerprint-v1\0";
const OVERSIZE_DIGEST_DOMAIN: &[u8] = b"ctx-firebender-oversize-row-v1\0";
pub(super) const DIRECT_PARSER_REVISION: &str = "firebender-source-backed-v3";

#[derive(Debug)]
pub(crate) struct FirebenderDirectScan {
    source: SourceKey,
    certificate: CertifiedSource,
    terminal_fence: SqliteSourceEvidence,
    emitted_pages: u64,
    row_decode_passes: u64,
    decoded_rows: u64,
    peak_buffered_documents: u64,
    candidate_query_batches: u64,
    row_set_queries: u64,
    max_rows_per_set: u64,
}

enum FirebenderTreeAuthority {
    Present(Box<FirebenderPresentAuthority>),
    Missing(MissingLeafFence),
}

struct FirebenderPresentAuthority {
    opening_evidence: SqliteSourceEvidence,
    _sqlite_authority: SqliteSourceDirectoryAuthority,
    snapshot: Mutex<Option<Box<OpenedSnapshot>>>,
    terminal_revalidate: Box<
        dyn Fn() -> Result<(), crate::provider_sources::SqliteSourceAccessError>
            + Send
            + Sync
            + 'static,
    >,
}

#[derive(Debug)]
struct FirebenderDocumentTreeAdapter {
    data_root: PathBuf,
    path: PathBuf,
}

impl ReplacementDocumentTree for FirebenderDocumentTreeAdapter {
    type Leaf = SourceKey;
    type TreeAuthority = FirebenderTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        DIRECT_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        firebender_database_path_and_source(&self.path)
            .is_ok_and(|(_, owned)| owned.exact_descriptor_eq(source))
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let (database_path, source) =
            firebender_database_path_and_source(&self.path).map_err(route_error)?;
        match open_database_leaf(&self.data_root, &database_path).map_err(route_error)? {
            OpenDatabaseLeaf::Present(snapshot) => {
                let opening_evidence = snapshot.evidence().map_err(route_error)?.clone();
                let fingerprint = observe_logical_snapshot(
                    snapshot.connection().map_err(route_error)?,
                    &database_path,
                )
                .map_err(route_error)?;
                snapshot.revalidate().map_err(route_error)?;
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    vec![ObservedDocumentLeaf::new(
                        DocumentLeafFingerprint::new(fingerprint),
                        source,
                    )],
                    FirebenderTreeAuthority::Present(Box::new(FirebenderPresentAuthority {
                        opening_evidence,
                        _sqlite_authority: snapshot.sqlite_authority(),
                        terminal_revalidate: snapshot
                            .terminal_revalidator()
                            .map_err(route_error)?,
                        snapshot: Mutex::new(Some(snapshot)),
                    })),
                ))
            }
            OpenDatabaseLeaf::Missing(fence) => {
                let fingerprint = fence.fingerprint();
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    Vec::new(),
                    FirebenderTreeAuthority::Missing(fence),
                ))
            }
        }
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let FirebenderTreeAuthority::Present(authority) = authority else {
            return Err(internal_error(
                "Firebender missing inventory unexpectedly contained a document leaf",
            ));
        };
        let (database_path, _) =
            firebender_database_path_and_source(&self.path).map_err(route_error)?;
        let snapshot = take_opened_snapshot(&authority.snapshot)?;
        sink.begin_source(leaf.clone())?;
        let scan = scan_opened_snapshot(&snapshot, &database_path, leaf.clone(), &mut |page| {
            page.into_iter()
                .try_for_each(|document| sink.emit_core_record(document).map_err(Into::into))
        })
        .map_err(firebender_scan_error)?;
        validate_scan_receipt(&scan)?;
        if !scan.source.exact_descriptor_eq(leaf)
            || scan.terminal_fence != authority.opening_evidence
        {
            return Err(source_changed(
                "Firebender SQLite physical inventory changed during logical projection",
            ));
        }
        snapshot.revalidate().map_err(route_error)?;
        restore_opened_snapshot(&authority.snapshot, snapshot)?;
        Ok(document_terminal(scan))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            FirebenderTreeAuthority::Present(authority) => {
                let snapshot = take_opened_snapshot(&authority.snapshot)?;
                let evidence = snapshot.finish().map_err(route_error)?;
                if evidence != authority.opening_evidence {
                    return Err(source_changed(
                        "Firebender SQLite physical inventory changed before commit",
                    ));
                }
                (authority.terminal_revalidate)().map_err(route_error)?;
            }
            FirebenderTreeAuthority::Missing(fence) if !fence.revalidate() => {
                return Err(source_changed(
                    "Firebender SQLite absence changed before commit",
                ));
            }
            FirebenderTreeAuthority::Missing(_) => {}
        }
        Ok(tree.tree_fingerprint)
    }
}

pub(crate) fn register_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = FirebenderDocumentTreeAdapter {
        data_root: data_root.to_path_buf(),
        path: source.path.clone(),
    };
    register_replacement_document_tree_route(registry, source, selection, adapter)
}

fn take_opened_snapshot(
    slot: &Mutex<Option<Box<OpenedSnapshot>>>,
) -> SourceBackedRouteResult<Box<OpenedSnapshot>> {
    slot.lock()
        .map_err(|_| internal_error("Firebender SQLite snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| internal_error("Firebender SQLite snapshot was already consumed"))
}

fn restore_opened_snapshot(
    slot: &Mutex<Option<Box<OpenedSnapshot>>>,
    snapshot: Box<OpenedSnapshot>,
) -> SourceBackedRouteResult<()> {
    let mut slot = slot
        .lock()
        .map_err(|_| internal_error("Firebender SQLite snapshot lock was poisoned"))?;
    if slot.replace(snapshot).is_some() {
        return Err(internal_error(
            "Firebender SQLite snapshot slot was already occupied",
        ));
    }
    Ok(())
}

fn document_terminal(scan: FirebenderDirectScan) -> DocumentSourceTerminal {
    let observation = scan.certificate.observation().clone();
    DocumentSourceTerminal {
        source: scan.source,
        opening: observation.clone(),
        closing: observation,
        parser_revision: DIRECT_PARSER_REVISION,
        content_digest: *scan.certificate.content_digest(),
        counts: scan.certificate.counts(),
    }
}

fn validate_scan_receipt(scan: &FirebenderDirectScan) -> SourceBackedRouteResult<()> {
    let indexed = scan.certificate.counts().indexed_documents;
    let page_size = DIRECT_PAGE_DOCUMENTS as u64;
    let expected_pages = indexed / page_size + u64::from(!indexed.is_multiple_of(page_size));
    if scan.row_decode_passes != 1
        || scan.emitted_pages != expected_pages
        || scan.peak_buffered_documents != indexed.min(page_size)
        || (scan.decoded_rows == 0 && scan.certificate.counts().certified_bytes != 0)
        || scan.candidate_query_batches == 0
        || scan.row_set_queries > scan.candidate_query_batches
        || scan.max_rows_per_set > DIRECT_PAGE_DOCUMENTS as u64
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Firebender scan violated the one-pass 64-document stream contract",
        ));
    }
    Ok(())
}

fn scan_opened_snapshot(
    snapshot: &OpenedSnapshot,
    database_path: &Path,
    source: SourceKey,
    emit: &mut dyn FnMut(Vec<CoreRecord>) -> FirebenderSourceBackedResult<()>,
) -> FirebenderSourceBackedResult<FirebenderDirectScan> {
    let connection = snapshot.connection()?;
    validate_schema(connection, database_path)?;
    let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
    let include_deleted_filter =
        sqlite_table_columns(connection, "chat_sessions")?.contains("deleted_at");
    let scan = scan_rows(
        connection,
        database_path,
        source.clone(),
        include_deleted_filter,
        emit,
    )?
    .with_schema_fingerprint(schema_fingerprint);
    let certificate = SqliteLogicalSnapshot::new(
        DIRECT_PARSER_REVISION,
        scan.schema_fingerprint.as_bytes(),
        scan.content_digest,
        scan.counts,
    )
    .certify(source.clone())?;
    Ok(FirebenderDirectScan {
        source,
        certificate,
        terminal_fence: snapshot.evidence()?.clone(),
        emitted_pages: scan.emitted_pages,
        row_decode_passes: 1,
        decoded_rows: scan.decoded_rows,
        peak_buffered_documents: scan.peak_buffered_documents,
        candidate_query_batches: scan.row_reads.candidate_queries,
        row_set_queries: scan.row_reads.row_set_queries,
        max_rows_per_set: scan.row_reads.max_rows_per_set,
    })
}

fn observe_logical_snapshot(
    connection: &Connection,
    database_path: &Path,
) -> FirebenderSourceBackedResult<[u8; 32]> {
    validate_schema(connection, database_path)?;
    let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
    let include_deleted_filter =
        sqlite_table_columns(connection, "chat_sessions")?.contains("deleted_at");
    let mut digest = Sha256::new();
    digest.update(LOGICAL_FINGERPRINT_DOMAIN);
    digest.update((DIRECT_PARSER_REVISION.len() as u64).to_be_bytes());
    digest.update(DIRECT_PARSER_REVISION.as_bytes());
    digest.update((schema_fingerprint.len() as u64).to_be_bytes());
    digest.update(schema_fingerprint.as_bytes());
    let mut after = None;
    let mut rows = 0_u64;
    let mut row_reads = RowReadCounters::default();
    loop {
        let page = next_rows(connection, after, include_deleted_filter, &mut row_reads)?;
        if page.is_empty() {
            break;
        }
        for decoded in page {
            after = Some((decoded.updated_at, decoded.rowid));
            increment(&mut rows, 1)?;
            hash_decoded_row(&mut digest, &decoded);
        }
    }
    digest.update(rows.to_be_bytes());
    Ok(digest.finalize().into())
}

struct WorkingScan {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    schema_fingerprint: String,
    emitted_pages: u64,
    decoded_rows: u64,
    peak_buffered_documents: u64,
    row_reads: RowReadCounters,
}

impl WorkingScan {
    fn with_schema_fingerprint(mut self, schema_fingerprint: String) -> Self {
        self.schema_fingerprint = schema_fingerprint;
        self
    }
}

fn scan_rows(
    connection: &Connection,
    database_path: &Path,
    source: SourceKey,
    include_deleted_filter: bool,
    emit: &mut dyn FnMut(Vec<CoreRecord>) -> FirebenderSourceBackedResult<()>,
) -> FirebenderSourceBackedResult<WorkingScan> {
    let workspace = firebender_workspace(database_path);
    let mut after = None;
    let mut hasher = Sha256::new();
    hasher.update(CONTENT_DIGEST_DOMAIN);
    let mut counts = ScannedSourceCounts::default();
    let mut page = Vec::with_capacity(DIRECT_PAGE_DOCUMENTS);
    let mut emitted_pages = 0_u64;
    let mut decoded_rows = 0_u64;
    let mut peak_buffered_documents = 0_u64;
    let mut row_reads = RowReadCounters::default();

    loop {
        let rows = next_rows(connection, after, include_deleted_filter, &mut row_reads)?;
        if rows.is_empty() {
            break;
        }
        for decoded in rows {
            after = Some((decoded.updated_at, decoded.rowid));
            increment(&mut decoded_rows, 1)?;
            hash_decoded_row(&mut hasher, &decoded);
            match decoded.row {
                None => {
                    increment(&mut counts.complete_records, 1)?;
                    increment(&mut counts.rejected_records, 1)?;
                    increment(&mut counts.certified_bytes, decoded.retained_bytes)?;
                }
                Some(row) => {
                    increment(&mut counts.certified_bytes, canonical_row_bytes(&row)?)?;
                    if decoded.rejection.is_some() {
                        increment(&mut counts.complete_records, 1)?;
                        increment(&mut counts.rejected_records, 1)?;
                        continue;
                    }
                    let session_id = firebender_session_id(&source, &row.id)?;
                    for (message_index, message) in row.messages.iter().enumerate() {
                        increment(&mut counts.complete_records, 1)?;
                        let Some(document) = firebender_core_record(
                            &source,
                            session_id,
                            workspace.as_deref(),
                            &row,
                            message_index,
                            message,
                        )?
                        else {
                            increment(&mut counts.ignored_records, 1)?;
                            continue;
                        };
                        increment(&mut counts.retained_records, 1)?;
                        increment(&mut counts.indexed_documents, 1)?;
                        page.push(document);
                        peak_buffered_documents = peak_buffered_documents.max(
                            u64::try_from(page.len())
                                .map_err(|_| FirebenderSourceBackedError::CountOverflow)?,
                        );
                        if page.len() == DIRECT_PAGE_DOCUMENTS {
                            emit(std::mem::take(&mut page))?;
                            page = Vec::with_capacity(DIRECT_PAGE_DOCUMENTS);
                            increment(&mut emitted_pages, 1)?;
                        }
                    }
                }
            }
        }
    }
    if !page.is_empty() {
        emit(page)?;
        increment(&mut emitted_pages, 1)?;
    }
    Ok(WorkingScan {
        counts,
        content_digest: hasher.finalize().into(),
        schema_fingerprint: String::new(),
        emitted_pages,
        decoded_rows,
        peak_buffered_documents,
        row_reads,
    })
}

struct DecodedRow {
    rowid: i64,
    updated_at: i64,
    row: Option<FirebenderRow>,
    rejection: Option<String>,
    retained_bytes: u64,
    lengths: [u64; 4],
}

#[derive(Clone, Copy, Debug, Default)]
struct RowReadCounters {
    candidate_queries: u64,
    row_set_queries: u64,
    max_rows_per_set: u64,
}

struct RowCandidate {
    rowid: i64,
    updated_at: i64,
    created_at: i64,
    retained_bytes: u64,
    lengths: [u64; 4],
}

fn next_rows(
    connection: &Connection,
    after: Option<(i64, i64)>,
    include_deleted_filter: bool,
    counters: &mut RowReadCounters,
) -> FirebenderSourceBackedResult<Vec<DecodedRow>> {
    let deleted_filter = if include_deleted_filter {
        "deleted_at is null and"
    } else {
        ""
    };
    let sql = format!(
        "select rowid, cast(updated_at as integer), cast(created_at as integer),
                length(cast(id as blob)), length(cast(name as blob)),
                length(cast(messages_json as blob)), length(cast(metadata_json as blob))
         from chat_sessions
         where {deleted_filter}
               (?1 = 0 or cast(updated_at as integer) > ?2 or
                (cast(updated_at as integer) = ?2 and rowid > ?3))
         order by cast(updated_at as integer), rowid
         limit ?4"
    );
    let (has_after, updated_at, rowid) = after.map_or((0_i64, 0_i64, 0_i64), |(updated, row)| {
        (1_i64, updated, row)
    });
    increment(&mut counters.candidate_queries, 1)?;
    let _guard = SqliteLengthPreflightGuard::new(connection);
    let mut statement = connection.prepare(&sql).map_err(CaptureError::from)?;
    let rows = statement
        .query_map(
            [has_after, updated_at, rowid, DIRECT_PAGE_DOCUMENTS as i64],
            |row| {
                let raw_lengths = [
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ];
                let lengths = raw_lengths.map(|length| u64::try_from(length).unwrap_or(u64::MAX));
                let retained_bytes = lengths
                    .into_iter()
                    .try_fold(FIREBENDER_PAGE_OVERHEAD_BYTES as u64, |total, length| {
                        total.checked_add(length)
                    })
                    .unwrap_or(u64::MAX);
                Ok(RowCandidate {
                    rowid: row.get(0)?,
                    updated_at: row.get(1)?,
                    created_at: row.get(2)?,
                    retained_bytes,
                    lengths,
                })
            },
        )
        .map_err(CaptureError::from)?;
    let candidates = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(CaptureError::from)?;
    drop(statement);
    drop(_guard);
    let mut selected = Vec::new();
    let mut loaded_bytes = 0_u64;
    for candidate in candidates {
        let next = loaded_bytes
            .checked_add(candidate.retained_bytes)
            .ok_or(FirebenderSourceBackedError::CountOverflow)?;
        if !selected.is_empty() && next > FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES as u64 {
            break;
        }
        loaded_bytes = next;
        selected.push(candidate);
    }
    let safe_rowids = selected
        .iter()
        .map(|candidate| candidate.rowid)
        .collect::<Vec<_>>();
    let mut values = BTreeMap::new();
    if !safe_rowids.is_empty() {
        increment(&mut counters.row_set_queries, 1)?;
        counters.max_rows_per_set = counters.max_rows_per_set.max(safe_rowids.len() as u64);
        let placeholders = (1..=safe_rowids.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let deleted_filter = if include_deleted_filter {
            " and deleted_at is null"
        } else {
            ""
        };
        let sql = format!(
            "select rowid, cast(id as text), cast(name as text),
                    cast(messages_json as text), cast(metadata_json as text)
             from chat_sessions
             where rowid in ({placeholders}){deleted_filter}
             order by cast(updated_at as integer), rowid"
        );
        let mut statement = connection.prepare(&sql).map_err(CaptureError::from)?;
        let rows = statement
            .query_map(params_from_iter(&safe_rowids), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    (
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ),
                ))
            })
            .map_err(CaptureError::from)?;
        values = rows
            .collect::<rusqlite::Result<BTreeMap<_, _>>>()
            .map_err(CaptureError::from)?;
    }
    selected
        .into_iter()
        .map(|candidate| {
            let (id, name, messages_json, metadata_json) =
                values.remove(&candidate.rowid).ok_or_else(|| {
                    FirebenderSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture)
                })?;
            let (messages, rejection) =
                match serde_json::from_str::<Vec<serde_json::Value>>(&messages_json) {
                    Ok(messages) => (messages, None),
                    Err(error) => (
                        Vec::new(),
                        Some(format!(
                            "Firebender session {id} messages_json is invalid: {error}"
                        )),
                    ),
                };
            Ok(DecodedRow {
                rowid: candidate.rowid,
                updated_at: candidate.updated_at,
                row: Some(FirebenderRow {
                    rowid: candidate.rowid,
                    id,
                    name,
                    created_at: candidate.created_at,
                    updated_at: candidate.updated_at,
                    messages_json,
                    metadata_json,
                    messages,
                }),
                rejection,
                retained_bytes: candidate.retained_bytes,
                lengths: candidate.lengths,
            })
        })
        .collect()
}

fn hash_decoded_row(hasher: &mut Sha256, decoded: &DecodedRow) {
    hasher.update(decoded.rowid.to_le_bytes());
    hasher.update(decoded.updated_at.to_le_bytes());
    if let Some(row) = decoded.row.as_ref() {
        hasher.update(firebender_raw_row_digest(&row.logical_values()));
    } else {
        hasher.update(OVERSIZE_DIGEST_DOMAIN);
        for length in decoded.lengths {
            hasher.update(length.to_le_bytes());
        }
    }
}

pub(in crate::provider::providers::firebender::native_path) fn firebender_database_path_and_source(
    explicit_path: &Path,
) -> FirebenderSourceBackedResult<(PathBuf, SourceKey)> {
    let database_path = firebender_database_path(explicit_path)?;
    let source = firebender_source_key()?;
    Ok((database_path, source))
}

fn firebender_scan_error(error: FirebenderSourceBackedError) -> SourceBackedRouteError {
    match error {
        FirebenderSourceBackedError::Route(error) => error,
        error => route_error(error),
    }
}

fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn internal_error(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indivisible_tool_result_larger_than_page_target_is_retained() {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "create table chat_sessions (
                    id text not null,
                    name text not null,
                    created_at integer not null,
                    updated_at integer not null,
                    messages_json text not null,
                    metadata_json text not null
                );",
            )
            .unwrap();
        let body = format!(
            "firebender-large-head-{}-firebender-large-tail",
            "x".repeat(8 * 1024 * 1024)
        );
        let messages = serde_json::json!([{
            "id": "large-result",
            "role": "tool",
            "tool_call_id": "large-call",
            "content": body,
            "status": "success"
        }])
        .to_string();
        connection
            .execute(
                "insert into chat_sessions
                 (id, name, created_at, updated_at, messages_json, metadata_json)
                 values ('large-session', 'large', 1, 2, ?1, '{}')",
                [&messages],
            )
            .unwrap();

        let source = firebender_source_key().unwrap();
        let mut emitted = Vec::new();
        let scan = scan_rows(
            &connection,
            Path::new("/tmp/chat_history.db"),
            source,
            false,
            &mut |page| {
                emitted.extend(page);
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(scan.counts.rejected_records, 0);
        assert_eq!(emitted.len(), 1);
        let retained = emitted[0].content.meaningful_text();
        assert!(retained.starts_with("firebender-large-head-"));
        assert!(retained.ends_with("-firebender-large-tail"));
        assert!(retained.len() > FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES);
    }
}
