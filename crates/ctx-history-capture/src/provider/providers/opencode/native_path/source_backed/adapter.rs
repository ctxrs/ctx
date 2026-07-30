use std::{ffi::OsString, path::PathBuf};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, CaptureProvider, ContentSourceResolver,
    HydrationFailure, SourceKey,
};
use sha2::{Digest, Sha256};

use super::{
    open_root_authorized_snapshot, opencode_family_source_backed_registrations, scan_source,
    OpenCodeScanOutput, OpenCodeSourceBackedError, OpenCodeSourceBackedRegistration,
    OpenCodeSourceBackedResult, PARSER_REVISION, SQLITE_SOURCE_INVALID_REASON,
};
use crate::{
    common::io::ProviderSourceRoot,
    provider::source_backed::{
        family::document::{
            register_replacement_document_tree_route, ChangedDocumentSink, CompleteDocumentTree,
            DocumentLeafFingerprint, DocumentSourceTerminal, ObservedDocumentLeaf,
            ReplacementDocumentTree,
        },
        invalid_route, SourceBackedCoordinatorResult, SourceBackedProviderRegistry,
        SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
        SourceBackedRouteSelection,
    },
    provider_sources::{SqliteSourceAccessError, SqliteSourceEvidence},
    CaptureError, ProviderSource,
};

#[derive(Debug)]
struct OpenCodeDocumentTreeAdapter {
    registration: OpenCodeSourceBackedRegistration,
    path: PathBuf,
}

#[derive(Debug)]
enum OpenCodeTreeAuthority {
    Present(SqliteSourceEvidence),
    Missing {
        source_root: ProviderSourceRoot,
        database_leaf: OsString,
        tree_fingerprint: [u8; 32],
    },
}

type OpenCodeDocumentTree = CompleteDocumentTree<(), OpenCodeTreeAuthority>;

impl ReplacementDocumentTree for OpenCodeDocumentTreeAdapter {
    type Leaf = ();
    type TreeAuthority = OpenCodeTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.registration.owns_source(source)
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<OpenCodeDocumentTree> {
        discover_document_tree(&self.path).map_err(route_error)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        _leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let OpenCodeTreeAuthority::Present(physical_evidence) = authority else {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "missing OpenCode-family tree unexpectedly contained a leaf",
            ));
        };
        let scan = scan_source(
            &self.path,
            self.registration.dialect,
            Some(physical_evidence),
            &mut |output| match output {
                OpenCodeScanOutput::Begin(source) => sink.begin_source(source).map_err(Into::into),
                OpenCodeScanOutput::Document(document) => {
                    sink.emit_document(document).map_err(Into::into)
                }
            },
        )
        .map_err(route_error)?;
        let observation = scan.certificate.observation().clone();
        Ok(DocumentSourceTerminal {
            source: scan.source,
            opening: observation.clone(),
            closing: observation,
            parser_revision: PARSER_REVISION,
            content_digest: *scan.certificate.content_digest(),
            counts: scan.certificate.counts(),
        })
    }

    fn revalidate_complete(
        &self,
        tree: &OpenCodeDocumentTree,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            OpenCodeTreeAuthority::Present(expected) => {
                let current = observe_present_document_tree(&self.path).map_err(route_error)?;
                let OpenCodeTreeAuthority::Present(current_evidence) = current.authority else {
                    return Err(source_changed(
                        "OpenCode-family SQLite database disappeared before publication",
                    ));
                };
                if current_evidence != *expected {
                    return Err(source_changed(
                        "OpenCode-family physical SQLite family changed before publication",
                    ));
                }
                Ok(current.tree_fingerprint)
            }
            OpenCodeTreeAuthority::Missing {
                source_root,
                database_leaf,
                tree_fingerprint,
            } => {
                revalidate_missing_database(source_root, database_leaf).map_err(route_error)?;
                Ok(*tree_fingerprint)
            }
        }
    }

    fn hydrate_group(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        self.registration
            .exact_resolver(self.path.clone())
            .hydrate_batch(request)
    }
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
) -> SourceBackedCoordinatorResult<()> {
    let registration = registration_for_provider(source.provider).ok_or_else(|| {
        invalid_route(
            source.provider,
            "provider is not part of the OpenCode SQLite family",
        )
    })?;
    let adapter = OpenCodeDocumentTreeAdapter {
        registration,
        path: source.path.clone(),
    };
    register_replacement_document_tree_route(registry, source, selection, adapter)
}

fn registration_for_provider(
    provider: CaptureProvider,
) -> Option<OpenCodeSourceBackedRegistration> {
    opencode_family_source_backed_registrations()
        .into_iter()
        .find(|registration| registration.provider() == provider)
}

fn discover_document_tree(
    path: &std::path::Path,
) -> OpenCodeSourceBackedResult<OpenCodeDocumentTree> {
    match observe_present_document_tree(path) {
        Ok(tree) => Ok(tree),
        Err(error) if source_missing(&error) => observe_missing_document_tree(path),
        Err(error) => Err(error),
    }
}

fn observe_present_document_tree(
    path: &std::path::Path,
) -> OpenCodeSourceBackedResult<OpenCodeDocumentTree> {
    let (source_root, sqlite_snapshot) = open_root_authorized_snapshot(path)?;
    let opening = sqlite_snapshot.evidence().clone();
    let closing = sqlite_snapshot.finish()?;
    source_root.revalidate()?;
    if opening != closing {
        return Err(CaptureError::SourceChangedDuringCapture.into());
    }
    let leaf_fingerprint = DocumentLeafFingerprint::new(*closing.revision());
    let tree_fingerprint = leaf_fingerprint.as_bytes();
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        vec![ObservedDocumentLeaf::with_durable_replay(
            leaf_fingerprint,
            (),
            false,
        )],
        OpenCodeTreeAuthority::Present(closing),
    ))
}

fn observe_missing_document_tree(
    path: &std::path::Path,
) -> OpenCodeSourceBackedResult<OpenCodeDocumentTree> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let database_leaf =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: SQLITE_SOURCE_INVALID_REASON,
            })?;
    let source_root = ProviderSourceRoot::open(parent)?;
    revalidate_missing_database(&source_root, database_leaf)?;
    let mut tree = Sha256::new();
    tree.update(b"ctx.opencode-family-missing-sqlite-tree-v1\0");
    tree.update(source_root.authority_fingerprint());
    tree.update((database_leaf.as_encoded_bytes().len() as u64).to_be_bytes());
    tree.update(database_leaf.as_encoded_bytes());
    let tree_fingerprint = tree.finalize().into();
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        Vec::new(),
        OpenCodeTreeAuthority::Missing {
            source_root,
            database_leaf: database_leaf.to_os_string(),
            tree_fingerprint,
        },
    ))
}

fn revalidate_missing_database(
    source_root: &ProviderSourceRoot,
    database_leaf: &std::ffi::OsStr,
) -> OpenCodeSourceBackedResult<()> {
    let directory = source_root.directory()?;
    match directory.open_child(database_leaf) {
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
        Ok(_) => return Err(CaptureError::SourceChangedDuringCapture.into()),
    }
    directory.revalidate()?;
    source_root.revalidate()?;
    Ok(())
}

fn source_missing(error: &OpenCodeSourceBackedError) -> bool {
    match error {
        OpenCodeSourceBackedError::Capture(CaptureError::Io(error)) => {
            error.kind() == std::io::ErrorKind::NotFound
        }
        OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Io { source, .. }) => {
            source.kind() == std::io::ErrorKind::NotFound
        }
        _ => false,
    }
}

fn route_error(error: OpenCodeSourceBackedError) -> SourceBackedRouteError {
    let error = match error {
        OpenCodeSourceBackedError::Route(error) => return error,
        error => error,
    };
    let kind =
        match &error {
            OpenCodeSourceBackedError::Capture(CaptureError::SourceChangedDuringCapture)
            | OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::SourceChanged) => {
                SourceBackedRouteErrorKind::SourceChanged
            }
            OpenCodeSourceBackedError::Capture(CaptureError::Io(error))
                if unavailable_io(error.kind()) =>
            {
                SourceBackedRouteErrorKind::Unavailable
            }
            OpenCodeSourceBackedError::SqliteSource(SqliteSourceAccessError::Io {
                source, ..
            }) if unavailable_io(source.kind()) => SourceBackedRouteErrorKind::Unavailable,
            OpenCodeSourceBackedError::SqliteSource(
                SqliteSourceAccessError::SnapshotUnavailable { .. }
                | SqliteSourceAccessError::UnsupportedSidecarIdentity { .. },
            ) => SourceBackedRouteErrorKind::Unavailable,
            _ => SourceBackedRouteErrorKind::InvalidSource,
        };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn source_changed(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::SourceChanged, detail)
}

fn unavailable_io(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}
