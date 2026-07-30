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

#[derive(Debug, Clone)]
pub(crate) struct TraeReplacementTree {
    data_root: PathBuf,
    path: PathBuf,
}

impl TraeReplacementTree {
    pub(crate) fn new(data_root: impl Into<PathBuf>, path: impl Into<PathBuf>) -> Self {
        Self {
            data_root: data_root.into(),
            path: path.into(),
        }
    }
}

pub(crate) struct TraeTreeAuthority {
    canonical_path: PathBuf,
    source: TraeSourceAuthority,
    fence: Mutex<Option<TraeSourceTerminalFence>>,
}

impl ReplacementDocumentTree for TraeReplacementTree {
    type Leaf = SourceKey;
    type TreeAuthority = TraeTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        TRAE_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Trae.as_str()
            && source.source_format() == TRAE_STATE_VSCDB_SOURCE_FORMAT
            && source.schema_variant() == TRAE_SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let canonical_path = explicit_trae_leaf(&self.path).map_err(|error| {
            SourceBackedRouteError::new(SourceBackedRouteErrorKind::Unavailable, error.to_string())
        })?;
        let authority = acquire_source(
            &self.data_root,
            &canonical_path,
            DateTime::<Utc>::UNIX_EPOCH,
        )
        .map_err(route_error)?;
        let source = source_key(&authority).map_err(route_error)?;
        let fingerprint = DocumentLeafFingerprint::new(*authority.database.evidence().revision());
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::with_durable_replay(
                fingerprint,
                source,
                false,
            )],
            TraeTreeAuthority {
                canonical_path,
                source: authority,
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
        sink.begin_source(source.clone())?;
        let mut sink_error = None;
        let scan = scan_trae_authority(&authority.canonical_path, &authority.source, &mut |page| {
            for document in page.documents {
                if let Err(error) = sink.emit_document(document) {
                    let detail = error.to_string();
                    sink_error = Some(error);
                    return Err(TraeSourceBackedErrorV0::Capture(
                        CaptureError::InvalidPayload(detail),
                    ));
                }
            }
            Ok(())
        });
        if let Some(error) = sink_error {
            return Err(error);
        }
        let scan = scan.map_err(route_error)?;
        let counts = scan.source.counts();
        if scan.row_decode_passes != 1
            || scan.decoded_rows > counts.complete_records
            || scan.peak_buffered_documents > TRAE_SOURCE_BACKED_PAGE_ROWS as u64
            || (counts.indexed_documents == 0) != (scan.emitted_pages == 0)
        {
            return Err(trae_internal(
                "Trae scan violated its one-pass bounded-page receipt",
            ));
        }
        if !scan
            .source
            .observation()
            .source()
            .exact_descriptor_eq(source)
            || scan.terminal_fence.evidence != *authority.source.database.evidence()
        {
            return Err(trae_changed(
                "Trae source changed between physical discovery and logical scan",
            ));
        }
        *authority
            .fence
            .lock()
            .map_err(|_| trae_internal("Trae terminal fence lock was poisoned"))? =
            Some(scan.terminal_fence);
        Ok(document_terminal(scan.source))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let fence = tree
            .authority
            .fence
            .lock()
            .map_err(|_| trae_internal("Trae terminal fence lock was poisoned"))?
            .clone()
            .ok_or_else(|| trae_changed("Trae scan has no terminal fence"))?;
        let current = acquire_source(
            &self.data_root,
            &tree.authority.canonical_path,
            DateTime::<Utc>::UNIX_EPOCH,
        )
        .map_err(route_error)?;
        current.database.revalidate().map_err(route_error)?;
        if current.database.evidence() == &fence.evidence {
            Ok(tree.tree_fingerprint)
        } else {
            Err(trae_changed("Trae physical source changed before commit"))
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
        let hydrated = TraeLocatorResolverV0::new(self.data_root.clone(), self.path.clone())
            .hydrate_locators(&locators)
            .map_err(trae_hydration_failure)?;
        let records = request
            .events()
            .iter()
            .zip(hydrated)
            .map(|(event, hydrated)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes: hydrated.exact_text.into_bytes(),
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
        parser_revision: TRAE_SOURCE_BACKED_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn trae_hydration_failure(error: TraeSourceBackedErrorV0) -> HydrationFailure {
    let kind = match error {
        TraeSourceBackedErrorV0::InvalidLocator
        | TraeSourceBackedErrorV0::LocatorSourceMismatch
        | TraeSourceBackedErrorV0::SourceRevisionMismatch
        | TraeSourceBackedErrorV0::Resolver(_) => HydrationFailureKind::InvalidLocator,
        TraeSourceBackedErrorV0::LocatorValueMissing
        | TraeSourceBackedErrorV0::LocatorMessageMissing => HydrationFailureKind::MissingRecord,
        TraeSourceBackedErrorV0::LocatorValueDigestMismatch => {
            HydrationFailureKind::StaleRecordEvidence
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

fn trae_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn trae_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
