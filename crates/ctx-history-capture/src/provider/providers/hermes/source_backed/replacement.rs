use std::sync::Mutex;

use super::*;
use crate::provider::source_backed::{
    family::document::{
        ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
        ObservedDocumentLeaf, ReplacementDocumentTree,
    },
    route_error, SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
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
        if std::fs::symlink_metadata(self.path()).is_err() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "selected Hermes database is unavailable",
            ));
        }
        let (sqlite_authority, snapshot) =
            open_root_authorized_snapshot(&self.data_root, self.path()).map_err(route_error)?;
        let opening_evidence = snapshot.evidence().clone();
        snapshot.revalidate().map_err(route_error)?;
        let fingerprint = DocumentLeafFingerprint::new(*opening_evidence.revision());
        Ok(CompleteDocumentTree::new(
            hermes_tree_fingerprint(&self.source),
            vec![ObservedDocumentLeaf::with_durable_replay(
                fingerprint,
                self.source.clone(),
                false,
            )],
            HermesTreeAuthority {
                opening_evidence,
                _sqlite_authority: sqlite_authority,
                terminal_revalidate: snapshot.terminal_revalidator(),
                snapshot: Mutex::new(Some(snapshot)),
            },
        ))
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
        let mut sink_error = None;
        let scan = project_hermes_snapshot(
            self,
            snapshot.connection().map_err(route_error)?,
            &mut |page| {
                if let Err(error) = sink.report_completed_bytes(page.completed_bytes) {
                    let detail = error.to_string();
                    sink_error = Some(error);
                    return Err(HermesSourceBackedError::Capture(
                        CaptureError::InvalidPayload(detail),
                    ));
                }
                for record in page.records {
                    if let HermesSourceBackedRecord::Event(document) = record {
                        if let Err(error) = sink.emit_core_record(document) {
                            let detail = error.to_string();
                            sink_error = Some(error);
                            return Err(HermesSourceBackedError::Capture(
                                CaptureError::InvalidPayload(detail),
                            ));
                        }
                    }
                }
                Ok(())
            },
        );
        if let Some(error) = sink_error {
            return Err(error);
        }
        let scan = scan.map_err(route_error)?;
        let counts = scan.certificate.counts();
        if scan.decoded_rows != counts.complete_records
            || scan.peak_buffered_records > 64
            || (counts.complete_records == 0) != (scan.emitted_pages == 0)
            || scan.native_candidate_query_batches == 0
            || scan.native_hydration_query_batches > scan.native_candidate_query_batches
            || scan.max_native_rows_per_set > 64
            || scan.direct_context_query_batches > scan.native_candidate_query_batches
            || scan.ancestry_query_batches
                > scan
                    .direct_context_query_batches
                    .saturating_mul(NATIVE_INGESTION_PAGE_MAX_UNITS as u64)
            || scan.max_context_query_batches_per_page
                > (NATIVE_INGESTION_PAGE_MAX_UNITS + 1) as u64
            || scan.max_direct_context_rows_per_query > 64
            || scan.max_ancestry_rows_per_query > ancestry::HERMES_ANCESTRY_QUERY_MAX_ROWS as u64
            || scan.max_direct_context_bytes_per_query
                > ancestry::HERMES_DIRECT_CONTEXT_RESIDENT_MAX_BYTES as u64
            || scan.max_ancestry_bytes_per_query
                > ancestry::HERMES_ANCESTRY_RESIDENT_MAX_BYTES as u64
            || scan.peak_context_cache_rows > ancestry::HERMES_CONTEXT_CACHE_MAX_ROWS as u64
            || scan.peak_context_cache_bytes > ancestry::HERMES_CONTEXT_CACHE_MAX_BYTES as u64
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
        snapshot.revalidate().map_err(route_error)?;
        restore_snapshot(&authority.snapshot, snapshot)?;
        Ok(document_terminal(scan.certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let snapshot = take_snapshot(&tree.authority.snapshot)?;
        let evidence = snapshot.finish().map_err(route_error)?;
        if evidence != tree.authority.opening_evidence {
            return Err(hermes_changed(format!(
                "{}: physical source changed before commit",
                HermesSourceBackedError::SourceChanged
            )));
        }
        (tree.authority.terminal_revalidate)().map_err(route_error)?;
        Ok(tree.tree_fingerprint)
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
