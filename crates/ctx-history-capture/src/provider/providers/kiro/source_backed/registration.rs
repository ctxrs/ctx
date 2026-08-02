use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use super::{
    kiro_source_key, observe_kiro_logical_snapshot, require_legacy_sqlite_format,
    scan_kiro_snapshot, KiroSourceBackedErrorV0, KiroSourceBackedScan,
    KIRO_SOURCE_BACKED_PARSER_REVISION, SOURCE_BACKED_PAGE_ROWS,
};
use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    provider::source_backed::{
        family::document::{
            register_replacement_document_tree_route, ChangedDocumentSink, CompleteDocumentTree,
            DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
            ReplacementDocumentTree,
        },
        route_error, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        SourceBackedRouteSelection,
    },
    provider_sources::{
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
    },
    CaptureError, ProviderSource, KIRO_SQLITE_SOURCE_FORMAT,
};
use ctx_history_core::SourceKey;

use super::super::{
    absolute_kiro_path,
    scan::{
        KIRO_HYDRATION_BATCH_BYTES, KIRO_HYDRATION_BATCH_ROWS, KIRO_HYDRATION_SINGLETON_MAX_BYTES,
        KIRO_KEY_BATCH_BYTES, KIRO_KEY_BATCH_ROWS, KIRO_KEY_SINGLETON_MEMORY_BYTES,
        KIRO_ORDER_SCRATCH_MAX_BYTES,
    },
    KiroSqliteDatabase,
};

enum KiroTreeAuthority {
    Present(Box<KiroPresentAuthority>),
    Missing(KiroMissingLeafFence),
}

struct KiroPresentAuthority {
    opening_evidence: SqliteSourceEvidence,
    _sqlite_authority: SqliteSourceDirectoryAuthority,
    database: Mutex<Option<KiroSqliteDatabase>>,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
}

#[derive(Debug)]
struct KiroDocumentTreeAdapter {
    data_root: PathBuf,
    path: PathBuf,
}

impl ReplacementDocumentTree for KiroDocumentTreeAdapter {
    type Leaf = SourceKey;
    type TreeAuthority = KiroTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        KIRO_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        kiro_source_key().is_ok_and(|owned| owned.exact_descriptor_eq(source))
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let source = kiro_source_key().map_err(route_error)?;
        match observe_kiro_inventory(&self.data_root, &self.path).map_err(kiro_scan_error)? {
            KiroPhysicalInventory::Present(present) => {
                let fingerprint = present.logical_fingerprint;
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    vec![ObservedDocumentLeaf::new(
                        DocumentLeafFingerprint::new(fingerprint),
                        source,
                    )],
                    KiroTreeAuthority::Present(Box::new(KiroPresentAuthority {
                        opening_evidence: present.database.evidence().clone(),
                        _sqlite_authority: present.database.sqlite_authority(),
                        terminal_revalidate: present.database.terminal_revalidator(),
                        database: Mutex::new(Some(present.database)),
                    })),
                ))
            }
            KiroPhysicalInventory::Missing(fence) => {
                let fingerprint = fence.fingerprint();
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    Vec::new(),
                    KiroTreeAuthority::Missing(fence),
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
        let KiroTreeAuthority::Present(authority) = authority else {
            return Err(internal_error(
                "Kiro missing inventory unexpectedly contained a document leaf",
            ));
        };
        let path = absolute_kiro_path(&self.path).map_err(route_error)?;
        let database = take_database(&authority.database)?;
        sink.begin_source(leaf.clone())?;
        let scan = database
            .with_private_scratch_database(
                "kiro-order-",
                KIRO_ORDER_SCRATCH_MAX_BYTES,
                |scratch, scratch_path| {
                    scan_kiro_snapshot(
                        database.connection(&path)?,
                        scratch,
                        scratch_path,
                        leaf.clone(),
                        authority.opening_evidence.clone(),
                        &mut |page| {
                            page.into_iter().try_for_each(|document| {
                                sink.emit_core_record(document).map_err(Into::into)
                            })
                        },
                    )
                },
            )
            .map_err(kiro_scan_error)?;
        validate_scan_receipt(&scan)?;
        if !scan.source.exact_descriptor_eq(leaf)
            || scan.terminal_fence != authority.opening_evidence
        {
            return Err(source_changed(
                "Kiro SQLite physical inventory changed during logical projection",
            ));
        }
        database.revalidate(&path).map_err(route_error)?;
        restore_database(&authority.database, database)?;
        Ok(document_terminal(scan))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            KiroTreeAuthority::Present(authority) => {
                let path = absolute_kiro_path(&self.path).map_err(route_error)?;
                let database = take_database(&authority.database)?;
                let evidence = database.finish(&path).map_err(route_error)?;
                if evidence != authority.opening_evidence {
                    return Err(source_changed(
                        "Kiro SQLite physical inventory changed before commit",
                    ));
                }
                (authority.terminal_revalidate)().map_err(route_error)?;
            }
            KiroTreeAuthority::Missing(fence) if !fence.revalidate() => {
                return Err(source_changed("Kiro SQLite absence changed before commit"));
            }
            KiroTreeAuthority::Missing(_) => {}
        }
        Ok(tree.tree_fingerprint)
    }
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = KiroDocumentTreeAdapter {
        data_root: data_root.to_path_buf(),
        path: source.path.clone(),
    };
    register_replacement_document_tree_route(registry, source, selection, adapter)
}

fn document_terminal(scan: KiroSourceBackedScan) -> DocumentSourceTerminal {
    let observation = scan.certificate.observation().clone();
    DocumentSourceTerminal {
        source: scan.source,
        opening: observation.clone(),
        closing: observation,
        parser_revision: KIRO_SOURCE_BACKED_PARSER_REVISION,
        content_digest: *scan.certificate.content_digest(),
        counts: scan.certificate.counts(),
    }
}

fn validate_scan_receipt(scan: &KiroSourceBackedScan) -> SourceBackedRouteResult<()> {
    let indexed = scan.certificate.counts().indexed_documents;
    let page_rows = SOURCE_BACKED_PAGE_ROWS as u64;
    let expected_pages = indexed / page_rows + u64::from(!indexed.is_multiple_of(page_rows));
    let complete = scan.certificate.counts().complete_records;
    let ordering = scan.ordering;
    let expected_statements = ordering
        .phases
        .checked_mul(4)
        .and_then(|fixed| fixed.checked_add(ordering.key_batches))
        .and_then(|total| total.checked_add(ordering.hydration_batches));
    if scan.row_decode_passes != 1
        || scan.decoded_rows > complete
        || (scan.decoded_rows == 0) != (complete == 0)
        || scan.emitted_pages != expected_pages
        || scan.peak_buffered_rows != indexed.min(page_rows)
        || ordering.rows != scan.decoded_rows
        || ordering.max_key_batch_rows > KIRO_KEY_BATCH_ROWS as u64
        || ordering.max_key_batch_bytes > KIRO_KEY_SINGLETON_MEMORY_BYTES as u64
        || (ordering.max_key_batch_bytes > KIRO_KEY_BATCH_BYTES as u64
            && ordering.max_key_batch_rows != 1)
        || ordering.max_hydration_batch_rows > KIRO_HYDRATION_BATCH_ROWS as u64
        || ordering.max_hydration_batch_bytes > KIRO_HYDRATION_SINGLETON_MAX_BYTES
        || (ordering.max_hydration_batch_bytes > KIRO_HYDRATION_BATCH_BYTES
            && ordering.max_hydration_batch_rows != 1)
        || expected_statements != Some(ordering.data_statements)
    {
        return Err(internal_error(
            "Kiro scan receipt violated the one-pass bounded-stream contract",
        ));
    }
    Ok(())
}

enum KiroPhysicalInventory {
    Present(Box<KiroPresentInventory>),
    Missing(KiroMissingLeafFence),
}

struct KiroPresentInventory {
    logical_fingerprint: [u8; 32],
    database: KiroSqliteDatabase,
}

fn observe_kiro_inventory(
    data_root: &Path,
    path: &Path,
) -> super::KiroSourceBackedResultV0<KiroPhysicalInventory> {
    let path = absolute_kiro_path(path)?;
    let parent = database_parent(&path)?;
    let leaf = database_leaf(&path)?;
    let root = ProviderSourceRoot::open(parent)?;
    let directory = root.directory()?;
    root.revalidate()?;
    directory.revalidate()?;
    match directory.open_child(leaf) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            require_legacy_sqlite_format(&path, KIRO_SQLITE_SOURCE_FORMAT)?;
            file.revalidate()?;
            directory.revalidate()?;
            root.revalidate()?;
            drop(file);
            let database = KiroSqliteDatabase::open(data_root, &path)?;
            let logical_fingerprint = database.with_private_scratch_database(
                "kiro-order-",
                KIRO_ORDER_SCRATCH_MAX_BYTES,
                |scratch, scratch_path| {
                    observe_kiro_logical_snapshot(
                        database.connection(&path)?,
                        scratch,
                        scratch_path,
                    )
                },
            )?;
            database.revalidate(&path)?;
            Ok(KiroPhysicalInventory::Present(Box::new(
                KiroPresentInventory {
                    logical_fingerprint,
                    database,
                },
            )))
        }
        Ok(OpenedProviderSourcePath::Directory(_)) => Err(invalid_database_leaf(&path).into()),
        Err(CaptureError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            directory.revalidate()?;
            root.revalidate()?;
            Ok(KiroPhysicalInventory::Missing(KiroMissingLeafFence {
                root,
                directory,
                leaf: leaf.to_os_string(),
            }))
        }
        Err(error) => Err(error.into()),
    }
}

fn take_database(
    slot: &Mutex<Option<KiroSqliteDatabase>>,
) -> SourceBackedRouteResult<KiroSqliteDatabase> {
    slot.lock()
        .map_err(|_| internal_error("Kiro SQLite snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| internal_error("Kiro SQLite snapshot was already consumed"))
}

fn restore_database(
    slot: &Mutex<Option<KiroSqliteDatabase>>,
    database: KiroSqliteDatabase,
) -> SourceBackedRouteResult<()> {
    let mut slot = slot
        .lock()
        .map_err(|_| internal_error("Kiro SQLite snapshot lock was poisoned"))?;
    if slot.replace(database).is_some() {
        return Err(internal_error(
            "Kiro SQLite snapshot slot was already occupied",
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct KiroMissingLeafFence {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    leaf: OsString,
}

impl KiroMissingLeafFence {
    fn fingerprint(&self) -> [u8; 32] {
        self.root.authority_fingerprint()
    }

    fn revalidate(&self) -> bool {
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

fn database_parent(path: &Path) -> super::KiroSourceBackedResultV0<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Kiro SQLite source must have a parent directory",
            }
            .into()
        })
}

fn database_leaf(path: &Path) -> super::KiroSourceBackedResultV0<&OsStr> {
    path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Kiro SQLite source must have a database leaf name",
        }
        .into()
    })
}

fn invalid_database_leaf(path: &Path) -> CaptureError {
    CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Kiro SQLite source must be a regular non-symlink file",
    }
}

pub(super) fn kiro_scan_error(error: KiroSourceBackedErrorV0) -> SourceBackedRouteError {
    match error {
        KiroSourceBackedErrorV0::Route(error) => error,
        KiroSourceBackedErrorV0::SqliteSource(error)
            if error.is_retryable_resource_unavailable() =>
        {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
        }
        error => route_error(error),
    }
}

fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn internal_error(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
