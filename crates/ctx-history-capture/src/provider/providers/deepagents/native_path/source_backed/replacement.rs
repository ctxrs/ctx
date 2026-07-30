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

pub(crate) struct DeepAgentsTreeAuthority {
    scanner: Mutex<Option<DeepAgentsSourceBackedScannerV0>>,
    fence: Mutex<Option<DeepAgentsSourceTerminalFence>>,
}

impl ReplacementDocumentTree for DeepAgentsDatabaseSelectionV0 {
    type Leaf = SourceKey;
    type TreeAuthority = DeepAgentsTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        DEEPAGENTS_SOURCE_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::DeepAgents.as_str()
            && source.source_format() == DEEPAGENTS_SQLITE_SOURCE_FORMAT
            && source.schema_variant() == DEEPAGENTS_SOURCE_SCHEMA_VARIANT
            && source.provider_identity_version() == 1
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        if fs::symlink_metadata(self.path()).is_err() {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Unavailable,
                "selected Deep Agents database is unavailable",
            ));
        }
        let scanner =
            DeepAgentsSourceBackedScannerV0::open(self.clone(), DateTime::<Utc>::UNIX_EPOCH)
                .map_err(route_error)?;
        let source = scanner.source().clone();
        let fingerprint = DocumentLeafFingerprint::new(*scanner.evidence.revision());
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::with_durable_replay(
                fingerprint,
                source,
                false,
            )],
            DeepAgentsTreeAuthority {
                scanner: Mutex::new(Some(scanner)),
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
        let mut scanner = authority
            .scanner
            .lock()
            .map_err(|_| deepagents_internal("Deep Agents scanner lock was poisoned"))?
            .take()
            .ok_or_else(|| deepagents_internal("Deep Agents logical leaf was scanned twice"))?;
        if scanner.source() != source {
            return Err(deepagents_changed(
                "Deep Agents source changed after physical discovery",
            ));
        }
        sink.begin_source(source.clone())?;
        while let Some(page) = scanner.next_page().map_err(route_error)? {
            for document in page {
                sink.emit_document(document)?;
            }
        }
        let scan = scanner.finish().map_err(route_error)?;
        let counts = scan.certificate.counts();
        if !scan.source.exact_descriptor_eq(source)
            || scan.row_decode_passes != 1
            || scan.decoded_rows > counts.complete_records
            || scan.peak_buffered_documents > DEEPAGENTS_PAGE_MAX_DOCUMENTS as u64
        {
            return Err(deepagents_internal(
                "Deep Agents scan violated its one-pass bounded-page receipt",
            ));
        }
        let terminal = document_terminal(scan.certificate);
        *authority
            .fence
            .lock()
            .map_err(|_| deepagents_internal("Deep Agents fence lock was poisoned"))? =
            Some(scan.terminal_fence);
        Ok(terminal)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        let fence = tree
            .authority
            .fence
            .lock()
            .map_err(|_| deepagents_internal("Deep Agents fence lock was poisoned"))?
            .clone()
            .ok_or_else(|| deepagents_changed("Deep Agents scan has no terminal fence"))?;
        if terminal_fence_matches(self.path(), &fence).map_err(route_error)? {
            Ok(tree.tree_fingerprint)
        } else {
            Err(deepagents_changed(
                "Deep Agents physical source changed before commit",
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
        let hydrated = DeepAgentsLocatorResolverV0::explicit(self.path.clone())
            .hydrate_locators(&locators)
            .map_err(deepagents_hydration_failure)?;
        let records = request
            .events()
            .iter()
            .zip(hydrated)
            .map(|(event, hydrated)| HydratedProviderRecord {
                event_id: event.event_id(),
                provider_bytes: hydrated.text.into_bytes(),
            })
            .collect();
        BatchHydrationResult::new(records).map_err(|error| HydrationFailure {
            kind: HydrationFailureKind::InvalidLocator,
            detail: error.to_string(),
        })
    }
}

fn terminal_fence_matches(
    path: &Path,
    expected: &DeepAgentsSourceTerminalFence,
) -> DeepAgentsSourceBackedResultV0<bool> {
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(path)?;
    let current = sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    Ok(current == expected.evidence)
}

fn document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: DEEPAGENTS_SOURCE_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn deepagents_hydration_failure(error: DeepAgentsSourceBackedErrorV0) -> HydrationFailure {
    let kind = match error {
        DeepAgentsSourceBackedErrorV0::InvalidLocator
        | DeepAgentsSourceBackedErrorV0::Resolver(_) => HydrationFailureKind::InvalidLocator,
        DeepAgentsSourceBackedErrorV0::MissingRecord => HydrationFailureKind::MissingRecord,
        DeepAgentsSourceBackedErrorV0::StaleRecordEvidence
        | DeepAgentsSourceBackedErrorV0::StaleSourceEvidence => {
            HydrationFailureKind::StaleRecordEvidence
        }
        _ => HydrationFailureKind::TemporarilyUnavailable,
    };
    HydrationFailure {
        kind,
        detail: error.to_string(),
    }
}

fn deepagents_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn deepagents_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
