use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ctx_history_core::{
    CaptureProvider, CertifiedSource, CertifiedSourceDeletion, CertifiedSourceInventory,
    ScannedSourceCounts, SourceKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::{
    canonical_row_bytes, firebender_document, firebender_session_id, firebender_source_key,
    firebender_workspace, increment, FirebenderSourceBackedError, FirebenderSourceBackedResult,
};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    provider::{
        source_backed::{
            certify_captured_route_inventory, executable_route, provider_format_scope,
            route_coordinator_error, route_error, SourceBackedCoordinatorResult,
            SourceBackedGenerationSink, SourceBackedProviderRegistry,
            SourceBackedRevalidationTarget, SourceBackedRouteDriver, SourceBackedRouteError,
            SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
            SourceBackedSelectorAuthority,
        },
        sqlite::{sqlite_schema_fingerprint, sqlite_table_columns, SqliteLengthPreflightGuard},
    },
    provider_sources::{
        open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
        SqliteLogicalSnapshot, SqliteSourceEvidence, SqliteSourceReadSnapshot,
    },
    CaptureError, ProviderSource, FIREBENDER_SQLITE_SOURCE_FORMAT,
};

use super::super::{
    firebender_path_identity, firebender_raw_row_digest, validate_schema, FirebenderRow,
    FIREBENDER_PAGE_OVERHEAD_BYTES, FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES,
};
use super::hydration::FirebenderExactResolver;

const DIRECT_PAGE_DOCUMENTS: usize = 64;
const CONTENT_DIGEST_DOMAIN: &[u8] = b"ctx-firebender-logical-content-v2\0";
const OVERSIZE_DIGEST_DOMAIN: &[u8] = b"ctx-firebender-oversize-row-v1\0";
const DIRECT_PARSER_REVISION: &str = "firebender-source-backed-v2";

#[derive(Clone, Debug)]
enum TerminalFence {
    Present(SqliteSourceEvidence),
    Missing(Arc<MissingLeafFence>),
}

#[derive(Debug)]
struct TerminalEvidence {
    certificate: Option<CertifiedSource>,
    deletion: Option<CertifiedSourceDeletion>,
    inventory: CertifiedSourceInventory,
    fence: TerminalFence,
}

#[derive(Debug, Default)]
struct TerminalState {
    evidence: Option<TerminalEvidence>,
    fence_result: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct FirebenderDirectScan {
    source: SourceKey,
    certificate: CertifiedSource,
    terminal_fence: SqliteSourceEvidence,
    emitted_pages: u64,
    row_decode_passes: u64,
    decoded_rows: u64,
    peak_buffered_documents: u64,
}

#[cfg(test)]
impl FirebenderDirectScan {
    pub(crate) fn certificate(&self) -> &CertifiedSource {
        &self.certificate
    }

    pub(crate) fn work_counters(&self) -> (u64, u64, u64, u64) {
        (
            self.row_decode_passes,
            self.decoded_rows,
            self.emitted_pages,
            self.peak_buffered_documents,
        )
    }
}

enum FirebenderScanOutcome {
    Missing {
        source: Box<SourceKey>,
        terminal_fence: Arc<MissingLeafFence>,
    },
    Present(Box<FirebenderDirectScan>),
}

pub(crate) fn register_source_backed_route(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let driver = source_backed_route_driver(&source);
    registry.register(executable_route(
        source,
        selection,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        driver,
    )?);
    Ok(())
}

fn source_backed_route_driver(route: &ProviderSource) -> SourceBackedRouteDriver {
    let path = route.path.clone();
    let scan_path = path.clone();
    let source_revalidation_path = path.clone();
    let inventory_revalidation_path = path.clone();
    let hydration_path = path.clone();
    let batch_hydration_path = path;
    let inventory_route = route.clone();
    let terminal = Arc::new(Mutex::new(TerminalState::default()));
    let scan_terminal = Arc::clone(&terminal);
    let source_terminal = Arc::clone(&terminal);
    let inventory_terminal = terminal;

    SourceBackedRouteDriver::new(
        move |sink| {
            reset_terminal(&scan_terminal)?;
            scan_replacement(&scan_path, &inventory_route, sink, &scan_terminal)
        },
        provider_format_scope(CaptureProvider::Firebender, FIREBENDER_SQLITE_SOURCE_FORMAT),
        move |target| revalidate_target(&source_revalidation_path, target, &source_terminal),
        move |request| FirebenderExactResolver::new(hydration_path.clone()).hydrate_event(request),
    )
    .with_complete_inventory_revalidation(move |expected| {
        revalidate_inventory(&inventory_revalidation_path, expected, &inventory_terminal)
    })
    .with_batch_hydration(move |request| {
        FirebenderExactResolver::new(batch_hydration_path.clone()).hydrate_batch(request)
    })
}

fn scan_replacement(
    path: &Path,
    route: &ProviderSource,
    sink: &mut SourceBackedGenerationSink<'_>,
    terminal: &Mutex<TerminalState>,
) -> SourceBackedRouteResult<()> {
    let mut began = false;
    let mut sink_failure = None;
    let outcome = scan_source(path, &mut |page| {
        stream_page(sink, page, &mut began, &mut sink_failure)
    })
    .map_err(route_error)?;
    if let Some(error) = sink_failure {
        return Err(error);
    }

    let evidence = match outcome {
        FirebenderScanOutcome::Present(scan) => {
            let scan = *scan;
            validate_scan_receipt(&scan)?;
            if !began {
                sink.begin_source(scan.source.clone())
                    .map_err(route_coordinator_error)?;
            }
            sink.certify_source(scan.certificate.clone())
                .map_err(route_coordinator_error)?;
            let inventory =
                certify_captured_route_inventory(route, std::slice::from_ref(&scan.certificate))?;
            sink.certify_complete_inventory(inventory.clone())
                .map_err(route_coordinator_error)?;
            TerminalEvidence {
                certificate: Some(scan.certificate),
                deletion: None,
                inventory,
                fence: TerminalFence::Present(scan.terminal_fence),
            }
        }
        FirebenderScanOutcome::Missing {
            source,
            terminal_fence,
        } => {
            let source = *source;
            let inventory = certify_captured_route_inventory(route, &[])?;
            let deletion = sink
                .base_source(&source)
                .map(|_| CertifiedSourceDeletion::from_inventory(source.clone(), &inventory));
            let deletion = deletion.transpose().map_err(route_error)?;
            if let Some(deletion) = deletion.clone() {
                sink.delete_source(deletion, inventory.clone())
                    .map_err(route_coordinator_error)?;
            }
            sink.certify_complete_inventory(inventory.clone())
                .map_err(route_coordinator_error)?;
            TerminalEvidence {
                certificate: None,
                deletion,
                inventory,
                fence: TerminalFence::Missing(terminal_fence),
            }
        }
    };
    let mut terminal = terminal.lock().map_err(|_| terminal_lock_error())?;
    terminal.evidence = Some(evidence);
    terminal.fence_result = None;
    Ok(())
}

fn stream_page(
    sink: &mut SourceBackedGenerationSink<'_>,
    page: Vec<LexicalDocument>,
    began: &mut bool,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> FirebenderSourceBackedResult<()> {
    for document in page {
        if !*began {
            if let Err(error) = sink.begin_source(document.source.clone()) {
                return bridge_sink_error(error, sink_failure);
            }
            *began = true;
        }
        if let Err(error) = sink.add_document(document) {
            return bridge_sink_error(error, sink_failure);
        }
    }
    Ok(())
}

fn bridge_sink_error(
    error: crate::provider::source_backed::SourceBackedCoordinatorError,
    sink_failure: &mut Option<SourceBackedRouteError>,
) -> FirebenderSourceBackedResult<()> {
    let route_error = route_coordinator_error(error);
    let detail = route_error.to_string();
    *sink_failure = Some(route_error);
    Err(FirebenderSourceBackedError::Capture(
        CaptureError::InvalidPayload(detail),
    ))
}

fn validate_scan_receipt(scan: &FirebenderDirectScan) -> SourceBackedRouteResult<()> {
    let indexed = scan.certificate.counts().indexed_documents;
    let page_size = DIRECT_PAGE_DOCUMENTS as u64;
    let expected_pages = indexed / page_size + u64::from(!indexed.is_multiple_of(page_size));
    if scan.row_decode_passes != 1
        || scan.emitted_pages != expected_pages
        || scan.peak_buffered_documents != indexed.min(page_size)
        || (scan.decoded_rows == 0 && scan.certificate.counts().certified_bytes != 0)
    {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Internal,
            "Firebender scan violated the one-pass 64-document stream contract",
        ));
    }
    Ok(())
}

fn reset_terminal(terminal: &Mutex<TerminalState>) -> SourceBackedRouteResult<()> {
    *terminal.lock().map_err(|_| terminal_lock_error())? = TerminalState::default();
    Ok(())
}

fn terminal_lock_error() -> SourceBackedRouteError {
    SourceBackedRouteError::new(
        SourceBackedRouteErrorKind::Internal,
        "Firebender terminal evidence lock was poisoned",
    )
}

fn revalidate_target(
    path: &Path,
    target: SourceBackedRevalidationTarget<'_>,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(path, terminal, |evidence| match target {
        SourceBackedRevalidationTarget::Source(expected) => {
            evidence.certificate.as_ref() == Some(expected)
        }
        SourceBackedRevalidationTarget::Deletion(expected) => {
            evidence.deletion.as_ref() == Some(expected)
        }
    })
}

fn revalidate_inventory(
    path: &Path,
    expected: &CertifiedSourceInventory,
    terminal: &Mutex<TerminalState>,
) -> bool {
    revalidate_terminal(path, terminal, |evidence| evidence.inventory == *expected)
}

fn revalidate_terminal(
    path: &Path,
    terminal: &Mutex<TerminalState>,
    expected_matches: impl FnOnce(&TerminalEvidence) -> bool,
) -> bool {
    let Ok(mut terminal) = terminal.lock() else {
        return false;
    };
    let Some(evidence) = terminal.evidence.as_ref() else {
        return false;
    };
    if !expected_matches(evidence) {
        return false;
    }
    let fence = evidence.fence.clone();
    cache_terminal_fence_result(&mut terminal.fence_result, || terminal_fence(path, &fence))
}

pub(crate) fn cache_terminal_fence_result(
    cached: &mut Option<bool>,
    evaluate: impl FnOnce() -> bool,
) -> bool {
    if let Some(result) = *cached {
        return result;
    }
    let result = evaluate();
    *cached = Some(result);
    result
}

fn terminal_fence(path: &Path, expected: &TerminalFence) -> bool {
    let Ok((database_path, _)) = firebender_database_path_and_source(path) else {
        return false;
    };
    match expected {
        TerminalFence::Present(expected) => open_snapshot(&database_path)
            .and_then(OpenedSnapshot::finish)
            .is_ok_and(|current| &current == expected),
        TerminalFence::Missing(fence) => fence.revalidate(),
    }
}

fn scan_source(
    explicit_path: &Path,
    emit: &mut dyn FnMut(Vec<LexicalDocument>) -> FirebenderSourceBackedResult<()>,
) -> FirebenderSourceBackedResult<FirebenderScanOutcome> {
    let identity = firebender_path_identity(explicit_path)?;
    let source = firebender_source_key(&identity.route_identity)?;
    let database_path = identity.canonical_database_path;
    let snapshot = match open_database_leaf(&database_path)? {
        OpenDatabaseLeaf::Present(snapshot) => snapshot,
        OpenDatabaseLeaf::Missing(terminal_fence) => {
            return Ok(FirebenderScanOutcome::Missing {
                source: Box::new(source),
                terminal_fence: Arc::new(terminal_fence),
            });
        }
    };
    let scan = {
        let connection = snapshot.connection()?;
        validate_schema(connection, &database_path)?;
        let schema_fingerprint = sqlite_schema_fingerprint(connection)?;
        let include_deleted_filter =
            sqlite_table_columns(connection, "chat_sessions")?.contains("deleted_at");
        scan_rows(
            connection,
            &database_path,
            source.clone(),
            include_deleted_filter,
            emit,
        )?
        .with_schema_fingerprint(schema_fingerprint)
    };
    let terminal_fence = snapshot.finish()?;
    let logical = SqliteLogicalSnapshot::new(
        DIRECT_PARSER_REVISION,
        scan.schema_fingerprint.as_bytes(),
        scan.content_digest,
        scan.counts,
    );
    let certificate = logical.certify(source.clone())?;
    Ok(FirebenderScanOutcome::Present(Box::new(
        FirebenderDirectScan {
            source,
            certificate,
            terminal_fence,
            emitted_pages: scan.emitted_pages,
            row_decode_passes: 1,
            decoded_rows: scan.decoded_rows,
            peak_buffered_documents: scan.peak_buffered_documents,
        },
    )))
}

#[derive(Debug)]
pub(super) struct MissingLeafFence {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    leaf: OsString,
}

impl MissingLeafFence {
    pub(super) fn revalidate(&self) -> bool {
        if self.root.revalidate().is_err() || self.directory.revalidate().is_err() {
            return false;
        }
        let missing = matches!(
            self.directory.open_child(&self.leaf),
            Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound
        );
        missing && self.directory.revalidate().is_ok() && self.root.revalidate().is_ok()
    }
}

pub(super) enum OpenDatabaseLeaf {
    Present(Box<OpenedSnapshot>),
    Missing(MissingLeafFence),
}

pub(super) fn open_database_leaf(path: &Path) -> FirebenderSourceBackedResult<OpenDatabaseLeaf> {
    let parent = database_parent(path)?;
    let leaf = database_leaf(path)?;
    let root = ProviderSourceRoot::open(parent)?;
    let directory = root.directory()?;
    root.revalidate()?;
    directory.revalidate()?;
    match directory.open_child(leaf) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            file.revalidate()?;
            directory.revalidate()?;
            root.revalidate()?;
            open_snapshot_from_authority(parent, leaf, root, directory)
                .map(Box::new)
                .map(OpenDatabaseLeaf::Present)
        }
        Ok(OpenedProviderSourcePath::Directory(_)) => Err(invalid_database_leaf(path).into()),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            directory.revalidate()?;
            root.revalidate()?;
            Ok(OpenDatabaseLeaf::Missing(MissingLeafFence {
                root,
                directory,
                leaf: leaf.to_os_string(),
            }))
        }
        Err(error) => Err(error.into()),
    }
}

pub(super) struct OpenedSnapshot {
    root: ProviderSourceRoot,
    snapshot: Option<SqliteSourceReadSnapshot>,
}

impl OpenedSnapshot {
    pub(super) fn connection(&self) -> FirebenderSourceBackedResult<&Connection> {
        self.snapshot
            .as_ref()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?
            .connection()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()).into())
    }

    pub(super) fn finish(mut self) -> FirebenderSourceBackedResult<SqliteSourceEvidence> {
        let snapshot = self
            .snapshot
            .take()
            .ok_or(FirebenderSourceBackedError::Capture(
                CaptureError::SystemInvariant("Firebender SQLite snapshot is inactive"),
            ))?;
        let evidence = snapshot
            .finish()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        self.root.revalidate()?;
        Ok(evidence)
    }
}

pub(super) fn open_snapshot(path: &Path) -> FirebenderSourceBackedResult<OpenedSnapshot> {
    match open_database_leaf(path)? {
        OpenDatabaseLeaf::Present(snapshot) => Ok(*snapshot),
        OpenDatabaseLeaf::Missing(_) => Err(CaptureError::Io(io::Error::new(
            io::ErrorKind::NotFound,
            "Firebender SQLite database leaf is absent from the retained provider root",
        ))
        .into()),
    }
}

fn open_snapshot_from_authority(
    parent: &Path,
    leaf: &OsStr,
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
) -> FirebenderSourceBackedResult<OpenedSnapshot> {
    let handle = directory
        .try_clone_authority_handle()
        .map_err(CaptureError::Io)?;
    let authority = retain_sqlite_source_directory_authority(&handle, parent)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let snapshot = open_root_handle_sqlite_source_snapshot(&authority, leaf)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    snapshot
        .revalidate()
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    directory.revalidate()?;
    root.revalidate()?;
    Ok(OpenedSnapshot {
        root,
        snapshot: Some(snapshot),
    })
}

fn database_parent(path: &Path) -> FirebenderSourceBackedResult<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Firebender SQLite source must have a parent directory",
            }
            .into()
        })
}

fn database_leaf(path: &Path) -> FirebenderSourceBackedResult<&OsStr> {
    path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Firebender SQLite source must have a database leaf name",
        }
        .into()
    })
}

fn invalid_database_leaf(path: &Path) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Firebender SQLite source must be a regular non-symlink file",
    }
}

struct WorkingScan {
    counts: ScannedSourceCounts,
    content_digest: [u8; 32],
    schema_fingerprint: String,
    emitted_pages: u64,
    decoded_rows: u64,
    peak_buffered_documents: u64,
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
    emit: &mut dyn FnMut(Vec<LexicalDocument>) -> FirebenderSourceBackedResult<()>,
) -> FirebenderSourceBackedResult<WorkingScan> {
    let source_path = database_path.display().to_string();
    let workspace = firebender_workspace(database_path);
    let mut after = None;
    let mut hasher = Sha256::new();
    hasher.update(CONTENT_DIGEST_DOMAIN);
    let mut counts = ScannedSourceCounts::default();
    let mut page = Vec::with_capacity(DIRECT_PAGE_DOCUMENTS);
    let mut emitted_pages = 0_u64;
    let mut decoded_rows = 0_u64;
    let mut peak_buffered_documents = 0_u64;

    while let Some(decoded) = next_row(connection, after, include_deleted_filter)? {
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
                let row_digest = firebender_raw_row_digest(&row.logical_values());
                for (message_index, message) in row.messages.iter().enumerate() {
                    increment(&mut counts.complete_records, 1)?;
                    let Some(document) = firebender_document(
                        &source,
                        session_id,
                        &source_path,
                        workspace.as_deref(),
                        &row,
                        message_index,
                        message,
                        row_digest,
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

fn next_row(
    connection: &Connection,
    after: Option<(i64, i64)>,
    include_deleted_filter: bool,
) -> FirebenderSourceBackedResult<Option<DecodedRow>> {
    let deleted_filter = if include_deleted_filter {
        "deleted_at is null and"
    } else {
        ""
    };
    let sql = format!(
        "with candidate as (
             select rowid source_rowid, cast(updated_at as integer) source_updated_at,
                    cast(created_at as integer) source_created_at,
                    length(cast(id as blob)) id_bytes,
                    length(cast(name as blob)) name_bytes,
                    length(cast(messages_json as blob)) messages_bytes,
                    length(cast(metadata_json as blob)) metadata_bytes,
                    cast(id as text) source_id, cast(name as text) source_name,
                    cast(messages_json as text) source_messages,
                    cast(metadata_json as text) source_metadata
             from chat_sessions
             where {deleted_filter}
                   (?1 = 0 or cast(updated_at as integer) > ?2 or
                    (cast(updated_at as integer) = ?2 and rowid > ?3))
             order by cast(updated_at as integer), rowid
             limit 1
         )
         select source_rowid, source_updated_at, source_created_at,
                id_bytes, name_bytes, messages_bytes, metadata_bytes,
                case when id_bytes + name_bytes + messages_bytes + metadata_bytes <= ?4
                     then source_id end,
                case when id_bytes + name_bytes + messages_bytes + metadata_bytes <= ?4
                     then source_name end,
                case when id_bytes + name_bytes + messages_bytes + metadata_bytes <= ?4
                     then source_messages end,
                case when id_bytes + name_bytes + messages_bytes + metadata_bytes <= ?4
                     then source_metadata end
         from candidate"
    );
    let (has_after, updated_at, rowid) = after.map_or((0_i64, 0_i64, 0_i64), |(updated, row)| {
        (1_i64, updated, row)
    });
    let maximum_payload = FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES
        .checked_sub(FIREBENDER_PAGE_OVERHEAD_BYTES)
        .ok_or(FirebenderSourceBackedError::CountOverflow)?;
    let max_bytes =
        i64::try_from(maximum_payload).map_err(|_| FirebenderSourceBackedError::CountOverflow)?;
    let _guard = SqliteLengthPreflightGuard::new(connection);
    connection
        .query_row(
            &sql,
            params![has_after, updated_at, rowid, max_bytes],
            |row| {
                let source_rowid = row.get(0)?;
                let source_updated_at = row.get(1)?;
                let source_created_at = row.get(2)?;
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
                let id = row.get::<_, Option<String>>(7)?;
                let name = row.get::<_, Option<String>>(8)?;
                let messages_json = row.get::<_, Option<String>>(9)?;
                let metadata_json = row.get::<_, Option<String>>(10)?;
                let within_bound = retained_bytes <= FIREBENDER_SOURCE_BACKED_PAGE_MAX_BYTES as u64;
                if !within_bound {
                    return Ok(DecodedRow {
                        rowid: source_rowid,
                        updated_at: source_updated_at,
                        row: None,
                        rejection: Some("Firebender row exceeds the bounded scan limit".to_owned()),
                        retained_bytes: FIREBENDER_PAGE_OVERHEAD_BYTES as u64,
                        lengths,
                    });
                }
                let (id, name, messages_json, metadata_json) =
                    match (id, name, messages_json, metadata_json) {
                        (Some(id), Some(name), Some(messages), Some(metadata)) => {
                            (id, name, messages, metadata)
                        }
                        _ => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                7,
                                "chat_sessions logical values".to_owned(),
                                rusqlite::types::Type::Null,
                            ));
                        }
                    };
                let parsed = serde_json::from_str::<Vec<serde_json::Value>>(&messages_json);
                let (messages, rejection) = match parsed {
                    Ok(messages) => (messages, None),
                    Err(error) => (
                        Vec::new(),
                        Some(format!(
                            "Firebender session {id} messages_json is invalid: {error}"
                        )),
                    ),
                };
                Ok(DecodedRow {
                    rowid: source_rowid,
                    updated_at: source_updated_at,
                    row: Some(FirebenderRow {
                        rowid: source_rowid,
                        id,
                        name,
                        created_at: source_created_at,
                        updated_at: source_updated_at,
                        messages_json,
                        metadata_json,
                        messages,
                    }),
                    rejection,
                    retained_bytes,
                    lengths,
                })
            },
        )
        .optional()
        .map_err(|error| FirebenderSourceBackedError::Capture(CaptureError::from(error)))
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

pub(super) fn firebender_database_path_and_source(
    explicit_path: &Path,
) -> FirebenderSourceBackedResult<(PathBuf, SourceKey)> {
    let identity = firebender_path_identity(explicit_path)?;
    let source = firebender_source_key(&identity.route_identity)?;
    Ok((identity.canonical_database_path, source))
}

#[cfg(test)]
pub(crate) fn scan_for_test(
    explicit_path: &Path,
    emit: &mut dyn FnMut(Vec<LexicalDocument>) -> FirebenderSourceBackedResult<()>,
) -> FirebenderSourceBackedResult<FirebenderDirectScan> {
    match scan_source(explicit_path, emit)? {
        FirebenderScanOutcome::Present(scan) => Ok(*scan),
        FirebenderScanOutcome::Missing { .. } => Err(FirebenderSourceBackedError::Capture(
            CaptureError::InvalidPayload("expected present Firebender test source".to_owned()),
        )),
    }
}

#[cfg(test)]
pub(crate) fn revalidate_missing_after_for_test(
    explicit_path: &Path,
    mutate: impl FnOnce(),
) -> FirebenderSourceBackedResult<bool> {
    let (database_path, _) = firebender_database_path_and_source(explicit_path)?;
    let OpenDatabaseLeaf::Missing(fence) = open_database_leaf(&database_path)? else {
        return Err(CaptureError::InvalidPayload(
            "expected missing Firebender test source".to_owned(),
        )
        .into());
    };
    mutate();
    Ok(fence.revalidate())
}
