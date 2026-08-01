use std::sync::Mutex;

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
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
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
        let fingerprint = DocumentLeafFingerprint::new(scanner.logical_fingerprint());
        let terminal_revalidate = scanner.terminal_revalidator();
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::new(fingerprint, source)],
            DeepAgentsTreeAuthority {
                scanner: Mutex::new(Some(scanner)),
                terminal_revalidate,
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
                sink.emit_core_record(document)?;
            }
        }
        let opening_evidence = scanner.evidence.clone();
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
        if scan.terminal_fence.evidence != opening_evidence {
            return Err(deepagents_changed(
                "Deep Agents terminal evidence changed while sealing the snapshot",
            ));
        }
        Ok(terminal)
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        if let Some(scanner) = tree
            .authority
            .scanner
            .lock()
            .map_err(|_| deepagents_internal("Deep Agents scanner lock was poisoned"))?
            .take()
        {
            scanner.seal_unscanned().map_err(route_error)?;
        }
        (tree.authority.terminal_revalidate)().map_err(route_error)?;
        Ok(tree.tree_fingerprint)
    }
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

fn deepagents_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn deepagents_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
