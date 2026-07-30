use std::sync::Mutex;

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, HydratedProviderRecord, HydrationFailure,
    HydrationFailureKind,
};

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
    fence: Mutex<Option<HermesSourceTerminalFence>>,
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
        let (root, snapshot) = open_root_authorized_snapshot(self.path()).map_err(route_error)?;
        let evidence = snapshot.finish().map_err(route_error)?;
        root.revalidate().map_err(route_error)?;
        let fingerprint = DocumentLeafFingerprint::new(*evidence.revision());
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::with_durable_replay(
                fingerprint,
                self.source.clone(),
                false,
            )],
            HermesTreeAuthority {
                opening_evidence: evidence,
                fence: Mutex::new(None),
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
        sink.begin_source(source.clone())?;
        let mut sink_error = None;
        let scan = scan_hermes_source_backed(self, |page| {
            for record in page.records {
                if let HermesSourceBackedRecord::Event(document) = record {
                    if let Err(error) = sink.emit_document(document) {
                        let detail = error.to_string();
                        sink_error = Some(error);
                        return Err(HermesSourceBackedError::Capture(
                            CaptureError::InvalidPayload(detail),
                        ));
                    }
                }
            }
            Ok(())
        });
        if let Some(error) = sink_error {
            return Err(error);
        }
        let scan = scan.map_err(route_error)?;
        let counts = scan.certificate.counts();
        if scan.row_decode_passes != 1
            || scan.decoded_rows != counts.complete_records
            || scan.peak_buffered_records > 64
            || (counts.complete_records == 0) != (scan.emitted_pages == 0)
        {
            return Err(hermes_internal(
                "Hermes scan violated its one-pass bounded-page receipt",
            ));
        }
        if scan.terminal_fence.evidence != authority.opening_evidence {
            return Err(hermes_changed(
                "Hermes source changed between physical discovery and logical scan",
            ));
        }
        *authority
            .fence
            .lock()
            .map_err(|_| hermes_internal("Hermes terminal fence lock was poisoned"))? =
            Some(scan.terminal_fence);
        Ok(document_terminal(scan.certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let fence = tree
            .authority
            .fence
            .lock()
            .map_err(|_| hermes_internal("Hermes terminal fence lock was poisoned"))?
            .clone()
            .ok_or_else(|| hermes_changed("Hermes scan has no terminal fence"))?;
        if terminal_fence_matches(self.path(), &fence).map_err(route_error)? {
            Ok(tree.tree_fingerprint)
        } else {
            Err(hermes_changed(
                "Hermes physical source changed before commit",
            ))
        }
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let locators = request
            .events()
            .iter()
            .map(|event| event.locator())
            .collect::<Vec<_>>();
        let hydrated = HermesLocatorResolver::new(self.path.clone(), self.source.clone())
            .hydrate_locators(&locators)
            .map_err(hermes_hydration_failure)?;
        let records = request
            .events()
            .iter()
            .zip(hydrated)
            .map(|(event, hydrated)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes: hydrated.provider_bytes,
            })
            .collect();
        BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: error.to_string(),
        })
    }
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

fn hermes_hydration_failure(error: HermesSourceBackedError) -> HydrationFailure {
    let kind = match error {
        HermesSourceBackedError::InvalidLocator | HermesSourceBackedError::Resolver(_) => {
            HydrationFailureKind::InvalidLocator
        }
        HermesSourceBackedError::MissingRecord => HydrationFailureKind::MissingRecord,
        HermesSourceBackedError::StaleRecordEvidence
        | HermesSourceBackedError::StaleSourceEvidence => HydrationFailureKind::StaleRecordEvidence,
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

fn hermes_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn hermes_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
