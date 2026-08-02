use std::sync::Mutex;

use super::*;
use crate::provider::source_backed::{
    family::document::{
        ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
        ObservedDocumentLeaf, ReplacementDocumentTree,
    },
    route_error as default_route_error, SourceBackedCurrentSourceProgress,
    SourceBackedCurrentSourceProgressStage, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};

pub(crate) struct HermesTreeAuthority {
    opening_evidence: SqliteSourceEvidence,
    _sqlite_authority: SqliteSourceDirectoryAuthority,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
}

impl ReplacementDocumentTree for HermesSourceCandidate {
    type Leaf = SourceKey;
    type TreeAuthority = HermesTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        HERMES_SOURCE_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Hermes.as_str()
            && source.source_format() == HERMES_SQLITE_SOURCE_FORMAT
            && source.schema_variant() == HERMES_SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        discover_hermes_tree(self, &mut |_| Ok(()))
    }

    fn discover_complete_with_progress(
        &self,
        _base_sources: &[CertifiedSource],
        report_progress: &mut dyn FnMut(
            SourceBackedCurrentSourceProgress,
        ) -> SourceBackedRouteResult<()>,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        discover_hermes_tree(self, report_progress)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        source: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        if !self.source.exact_descriptor_eq(source) {
            return Err(hermes_changed(
                "Hermes logical source changed after physical discovery",
            ));
        }
        let snapshot = take_snapshot(&authority.snapshot)?;
        sink.begin_source(source.clone())?;
        let scan = project_hermes_snapshot_with_progress(
            self,
            snapshot.connection().map_err(default_route_error)?,
            &mut |output| {
                match output {
                    HermesSnapshotProjectionOutput::Page(page) => {
                        for record in page.records {
                            if let HermesSourceBackedRecord::Event(document) = record {
                                sink.emit_core_record(document)?;
                            }
                        }
                    }
                    HermesSnapshotProjectionOutput::Progress {
                        rows_scanned,
                        certified_bytes,
                    } => sink.report_progress(hermes_logical_progress(
                        SourceBackedCurrentSourceProgressStage::LogicalScan,
                        rows_scanned,
                        certified_bytes,
                    ))?,
                }
                Ok(())
            },
        );
        let scan = scan.map_err(hermes_route_error)?;
        let counts = scan.certificate.counts();
        if scan.decoded_rows != counts.complete_records
            || scan.peak_buffered_records > 64
            || (counts.complete_records == 0) != (scan.emitted_pages == 0)
            || scan.native_candidate_query_batches == 0
            || scan.native_hydration_query_batches > scan.native_candidate_query_batches
            || scan.max_native_rows_per_set > 64
        {
            return Err(hermes_internal(
                "Hermes scan violated its one-pass bounded-page receipt",
            ));
        }
        if snapshot.evidence() != &authority.opening_evidence {
            return Err(hermes_changed(
                "Hermes source changed between physical discovery and logical scan",
            ));
        }
        snapshot.revalidate().map_err(default_route_error)?;
        restore_snapshot(&authority.snapshot, snapshot)?;
        Ok(document_terminal(scan.certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let snapshot = take_snapshot(&tree.authority.snapshot)?;
        let evidence = snapshot.finish().map_err(default_route_error)?;
        if evidence != tree.authority.opening_evidence {
            return Err(hermes_changed(format!(
                "{}: physical source changed before commit",
                HermesSourceBackedError::SourceChanged
            )));
        }
        (tree.authority.terminal_revalidate)().map_err(default_route_error)?;
        Ok(tree.tree_fingerprint)
    }
}

fn discover_hermes_tree(
    candidate: &HermesSourceCandidate,
    report_progress: &mut dyn FnMut(
        SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()>,
) -> SourceBackedRouteResult<CompleteDocumentTree<SourceKey, HermesTreeAuthority>> {
    if std::fs::symlink_metadata(candidate.path()).is_err() {
        return Err(SourceBackedRouteError::new(
            SourceBackedRouteErrorKind::Unavailable,
            "selected Hermes database is unavailable",
        ));
    }
    let (sqlite_authority, snapshot) = open_root_authorized_snapshot_with_progress(
        &candidate.data_root,
        candidate.path(),
        report_progress,
    )
    .map_err(hermes_route_error)?;
    let opening_evidence = snapshot.evidence().clone();
    let fingerprint = observe_hermes_logical_snapshot_with_progress(
        snapshot.connection().map_err(default_route_error)?,
        report_progress,
    )
    .map_err(hermes_route_error)?;
    snapshot.revalidate().map_err(default_route_error)?;
    let fingerprint = DocumentLeafFingerprint::new(fingerprint);
    Ok(CompleteDocumentTree::new(
        fingerprint.as_bytes(),
        vec![ObservedDocumentLeaf::new(
            fingerprint,
            candidate.source.clone(),
        )],
        HermesTreeAuthority {
            opening_evidence,
            _sqlite_authority: sqlite_authority,
            terminal_revalidate: snapshot.terminal_revalidator(),
            snapshot: Mutex::new(Some(snapshot)),
        },
    ))
}

fn hermes_route_error(error: HermesSourceBackedError) -> SourceBackedRouteError {
    match error {
        HermesSourceBackedError::Route(error) => error,
        error => default_route_error(error),
    }
}

fn take_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
) -> SourceBackedRouteResult<SqliteSourceReadSnapshot> {
    slot.lock()
        .map_err(|_| hermes_internal("Hermes SQLite snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| hermes_internal("Hermes SQLite snapshot was already consumed"))
}

fn restore_snapshot(
    slot: &Mutex<Option<SqliteSourceReadSnapshot>>,
    snapshot: SqliteSourceReadSnapshot,
) -> SourceBackedRouteResult<()> {
    let mut slot = slot
        .lock()
        .map_err(|_| hermes_internal("Hermes SQLite snapshot lock was poisoned"))?;
    if slot.replace(snapshot).is_some() {
        return Err(hermes_internal(
            "Hermes SQLite snapshot slot was already occupied",
        ));
    }
    Ok(())
}

fn document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: HERMES_SOURCE_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn hermes_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn hermes_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
