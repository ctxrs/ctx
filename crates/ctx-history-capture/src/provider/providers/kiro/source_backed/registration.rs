use std::{
    ffi::{OsStr, OsString},
    io,
    path::{Path, PathBuf},
};

use super::{
    hydration::hydration_failure_from_error, kiro_source_key, scan_kiro_source_backed,
    KiroLocatorResolverV0, KiroSourceBackedErrorV0, KiroSourceBackedScan,
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
    provider_sources::SqliteSourceEvidence,
    CaptureError, ProviderSource, KIRO_SQLITE_SOURCE_FORMAT,
};
use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, ContentSourceResolver, HydrationFailure, SourceKey,
};

use super::super::{absolute_kiro_path, KiroSqliteDatabase};

#[derive(Debug)]
enum KiroTreeAuthority {
    Present(SqliteSourceEvidence),
    Missing(KiroMissingLeafFence),
}

#[derive(Debug)]
struct KiroDocumentTreeAdapter {
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
        match observe_kiro_inventory(&self.path).map_err(route_error)? {
            KiroPhysicalInventory::Present(evidence) => {
                let fingerprint = *evidence.revision();
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    vec![ObservedDocumentLeaf::with_durable_replay(
                        DocumentLeafFingerprint::new(fingerprint),
                        source,
                        false,
                    )],
                    KiroTreeAuthority::Present(evidence),
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
        let KiroTreeAuthority::Present(expected_physical) = authority else {
            return Err(internal_error(
                "Kiro missing inventory unexpectedly contained a document leaf",
            ));
        };
        sink.begin_source(leaf.clone())?;
        let scan = scan_kiro_source_backed(&self.path, KIRO_SQLITE_SOURCE_FORMAT, &mut |page| {
            page.into_iter()
                .try_for_each(|document| sink.emit_document(document).map_err(Into::into))
        })
        .map_err(kiro_scan_error)?;
        validate_scan_receipt(&scan)?;
        if !scan.source.exact_descriptor_eq(leaf) || &scan.terminal_fence != expected_physical {
            return Err(source_changed(
                "Kiro SQLite physical inventory changed during logical projection",
            ));
        }
        Ok(document_terminal(scan))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            KiroTreeAuthority::Present(expected) => {
                let path = absolute_kiro_path(&self.path).map_err(route_error)?;
                let current = KiroSqliteDatabase::open(&path)
                    .and_then(|database| database.finish(&path))
                    .map_err(route_error)?;
                if &current != expected {
                    return Err(source_changed(
                        "Kiro SQLite physical inventory changed before commit",
                    ));
                }
            }
            KiroTreeAuthority::Missing(fence) if !fence.revalidate() => {
                return Err(source_changed("Kiro SQLite absence changed before commit"));
            }
            KiroTreeAuthority::Missing(_) => {}
        }
        Ok(tree.tree_fingerprint)
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        KiroLocatorResolverV0::discover(&self.path, KIRO_SQLITE_SOURCE_FORMAT)
            .map_err(hydration_failure_from_error)?
            .hydrate_batch(request)
    }
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let adapter = KiroDocumentTreeAdapter {
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
    if scan.row_decode_passes != 1
        || scan.decoded_rows > complete
        || (scan.decoded_rows == 0) != (complete == 0)
        || scan.emitted_pages != expected_pages
        || scan.peak_buffered_rows != indexed.min(page_rows)
    {
        return Err(internal_error(
            "Kiro scan receipt violated the one-pass bounded-stream contract",
        ));
    }
    Ok(())
}

enum KiroPhysicalInventory {
    Present(SqliteSourceEvidence),
    Missing(KiroMissingLeafFence),
}

fn observe_kiro_inventory(path: &Path) -> super::KiroSourceBackedResultV0<KiroPhysicalInventory> {
    let path = absolute_kiro_path(path)?;
    let parent = database_parent(&path)?;
    let leaf = database_leaf(&path)?;
    let root = ProviderSourceRoot::open(parent)?;
    let directory = root.directory()?;
    root.revalidate()?;
    directory.revalidate()?;
    match directory.open_child(leaf) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            file.revalidate()?;
            directory.revalidate()?;
            root.revalidate()?;
            drop(file);
            let database = KiroSqliteDatabase::open(&path)?;
            let evidence = database.finish(&path)?;
            Ok(KiroPhysicalInventory::Present(evidence))
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

fn kiro_scan_error(error: KiroSourceBackedErrorV0) -> SourceBackedRouteError {
    match error {
        KiroSourceBackedErrorV0::Route(error) => error,
        error => route_error(error),
    }
}

fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn internal_error(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
