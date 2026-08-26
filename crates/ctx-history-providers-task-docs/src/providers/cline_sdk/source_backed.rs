use std::{io, path::PathBuf};

use ctx_history_core::{
    ScannedSourceCounts, SourceAnchorScope, SourceKey, SourceObservation, StableEntityId,
};
use ctx_history_source_io::{ProviderSourceRoot, SourceIoError};
use sha2::{Digest, Sha256};

use super::{
    projection::{
        cline_session_id, cline_source_key_scoped, owns_cline_source, project_messages,
        PARSER_REVISION,
    },
    source::{discover_cline_sdk_tree, read_bound_leaf_files, ClineSdkError, SessionLeaf},
};
use crate::{
    CaptureLifecycleSink, ChangedDocumentSink, CompleteDocumentTree, DocumentLeafExecutionPolicy,
    DocumentLeafFingerprint, DocumentRecordSpool, DocumentSourceTerminal, ObservedDocumentLeaf,
    ReplacementDocumentTree, SourceBackedRouteError, SourceBackedRouteErrorKind,
    SourceBackedRouteResult,
};

const SOURCE_REVISION_KIND: &str = "cline-sdk-compound-source-revision-v1";
const REVISION_DOMAIN: &[u8] = b"ctx.cline.sdk.source-revision.v1\0";

#[derive(Debug, Clone)]
pub struct ClineSdkDocumentLeaf {
    snapshot: SessionLeaf,
    source_key: SourceKey,
    session_id: StableEntityId,
    source_anchor_scope: SourceAnchorScope,
}

#[derive(Debug)]
pub struct ClineSdkTreeAuthority {
    root: ProviderSourceRoot,
    selected_root: PathBuf,
    data_root: PathBuf,
}

type ClineSdkDocumentTree = CompleteDocumentTree<ClineSdkDocumentLeaf, ClineSdkTreeAuthority>;

pub struct ClineSdkDocumentTreeAdapter<L, S, C> {
    selected_root: PathBuf,
    data_root: PathBuf,
    source_anchor_scope: SourceAnchorScope,
    _lifecycle: crate::ProviderLifecycleMarker<L, S, C>,
}

impl<L, S, C> ClineSdkDocumentTreeAdapter<L, S, C> {
    pub fn new(selected_root: PathBuf, data_root: PathBuf) -> Self {
        Self::new_scoped(selected_root, data_root, SourceAnchorScope::Unqualified)
    }

    pub fn new_scoped(
        selected_root: PathBuf,
        data_root: PathBuf,
        source_anchor_scope: SourceAnchorScope,
    ) -> Self {
        Self {
            selected_root,
            data_root,
            source_anchor_scope,
            _lifecycle: std::marker::PhantomData,
        }
    }
}

impl<L, S, C> ReplacementDocumentTree for ClineSdkDocumentTreeAdapter<L, S, C>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
    C: Send + Sync + 'static,
{
    type Lifecycle = L;
    type Spool = S;
    type RouteControl = C;
    type Leaf = ClineSdkDocumentLeaf;
    type TreeAuthority = ClineSdkTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        owns_cline_source(source)
    }

    fn leaf_execution_policy(&self) -> DocumentLeafExecutionPolicy {
        DocumentLeafExecutionPolicy::Independent
    }

    fn independent_leaf_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<SourceKey> {
        Ok(leaf.source_key.clone())
    }

    fn durable_replay_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        Ok(Some(leaf.source_key.clone()))
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<ClineSdkDocumentTree> {
        bind_tree_scoped(
            &self.selected_root,
            &self.data_root,
            self.source_anchor_scope,
        )
        .map_err(route_error)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        scan_leaf(authority, leaf, sink)
    }

    fn revalidate_complete(
        &self,
        tree: &ClineSdkDocumentTree,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        tree.authority
            .root
            .revalidate_same_object()
            .map_err(|error| route_error(error.into()))?;
        let current =
            discover_cline_sdk_tree(&tree.authority.selected_root, &tree.authority.data_root)
                .map_err(route_error)?;
        if !tree.authority.root.same_object_as(&current.authority)
            || current.tree_fingerprint != tree.tree_fingerprint
        {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "Cline SDK compound source changed during terminal revalidation",
            ));
        }
        Ok(current.tree_fingerprint)
    }
}

#[cfg(test)]
fn bind_tree(
    selected_root: &std::path::Path,
    data_root: &std::path::Path,
) -> Result<ClineSdkDocumentTree, ClineSdkError> {
    bind_tree_scoped(selected_root, data_root, SourceAnchorScope::Unqualified)
}

fn bind_tree_scoped(
    selected_root: &std::path::Path,
    data_root: &std::path::Path,
    source_anchor_scope: SourceAnchorScope,
) -> Result<ClineSdkDocumentTree, ClineSdkError> {
    let snapshot = discover_cline_sdk_tree(selected_root, data_root)?;
    let mut observed = Vec::with_capacity(snapshot.leaves.len());
    for leaf in snapshot.leaves {
        let source_key = cline_source_key_scoped(&leaf.provider_session_id, source_anchor_scope)
            .map_err(|error| ClineSdkError::Invalid(error.to_string()))?;
        let session_id = cline_session_id(&source_key, &leaf.provider_session_id)
            .map_err(|error| ClineSdkError::Invalid(error.to_string()))?;
        observed.push(ObservedDocumentLeaf::new(
            DocumentLeafFingerprint::new(leaf.fingerprint()),
            ClineSdkDocumentLeaf {
                snapshot: leaf,
                source_key,
                session_id,
                source_anchor_scope,
            },
        ));
    }
    Ok(CompleteDocumentTree::new(
        snapshot.tree_fingerprint,
        observed,
        ClineSdkTreeAuthority {
            root: snapshot.authority,
            selected_root: selected_root.to_path_buf(),
            data_root: data_root.to_path_buf(),
        },
    ))
}

fn scan_leaf<L, S>(
    authority: &ClineSdkTreeAuthority,
    leaf: &ClineSdkDocumentLeaf,
    sink: &mut ChangedDocumentSink<'_, '_, L, S>,
) -> SourceBackedRouteResult<DocumentSourceTerminal>
where
    L: CaptureLifecycleSink,
    S: DocumentRecordSpool,
{
    if let Some(detail) = leaf.snapshot.catalog_binding_failure.as_deref() {
        return Err(invalid_route(detail));
    }
    let (manifest, messages) =
        read_bound_leaf_files(&authority.root, &leaf.snapshot).map_err(route_error)?;
    let source_revision = source_revision(&leaf.snapshot, manifest.as_deref(), messages.as_deref());
    let observation = SourceObservation::new(
        leaf.source_key.clone(),
        SOURCE_REVISION_KIND,
        source_revision.to_vec(),
    )
    .map_err(|error| invalid_route(error.to_string()))?;
    sink.begin_source(leaf.source_key.clone())?;

    let mut counts = ScannedSourceCounts::default();
    match messages.as_deref() {
        None => counts.rejected_records = 1,
        Some(bytes) => match project_messages(
            &leaf.snapshot,
            &leaf.source_key,
            leaf.session_id,
            leaf.source_anchor_scope,
            source_revision,
            bytes,
        ) {
            Err(_) => counts.rejected_records = 1,
            Ok(projected) => {
                counts.rejected_records = projected.rejected;
                counts.ignored_records = projected.ignored;
                for record in projected.records {
                    sink.emit_core_record(record)?;
                    counts.retained_records = checked_add(counts.retained_records, 1)?;
                    counts.indexed_documents = checked_add(counts.indexed_documents, 1)?;
                }
            }
        },
    }
    counts.complete_records = counts
        .retained_records
        .checked_add(counts.rejected_records)
        .and_then(|value| value.checked_add(counts.ignored_records))
        .ok_or_else(|| invalid_route("Cline SDK count overflow"))?;
    counts.certified_bytes = manifest
        .as_ref()
        .map_or(0, |bytes| bytes.len() as u64)
        .checked_add(messages.as_ref().map_or(0, |bytes| bytes.len() as u64))
        .ok_or_else(|| invalid_route("Cline SDK byte count overflow"))?;
    authority
        .root
        .revalidate_same_object()
        .map_err(|error| route_error(error.into()))?;
    Ok(DocumentSourceTerminal {
        source: leaf.source_key.clone(),
        opening: observation.clone(),
        closing: observation,
        parser_revision: PARSER_REVISION,
        content_digest: source_revision,
        counts,
    })
}

fn source_revision(
    leaf: &SessionLeaf,
    manifest: Option<&[u8]>,
    messages: Option<&[u8]>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(REVISION_DOMAIN);
    digest.update(leaf.catalog_evidence);
    hash_optional_bytes(&mut digest, manifest);
    hash_optional_bytes(&mut digest, messages);
    digest.finalize().into()
}

fn hash_optional_bytes(digest: &mut Sha256, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            digest.update([1]);
            digest.update((bytes.len() as u64).to_be_bytes());
            digest.update(Sha256::digest(bytes));
        }
        None => digest.update([0]),
    }
}

fn checked_add(left: u64, right: u64) -> SourceBackedRouteResult<u64> {
    left.checked_add(right)
        .ok_or_else(|| invalid_route("Cline SDK count overflow"))
}

fn route_error(error: ClineSdkError) -> SourceBackedRouteError {
    let kind = match &error {
        ClineSdkError::MissingCatalog => SourceBackedRouteErrorKind::Unavailable,
        ClineSdkError::SourceChanged
        | ClineSdkError::SourceIo(SourceIoError::SourceChangedDuringCapture) => {
            SourceBackedRouteErrorKind::SourceChanged
        }
        ClineSdkError::Io(error) | ClineSdkError::SourceIo(SourceIoError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            SourceBackedRouteErrorKind::SourceChanged
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

fn invalid_route(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, detail)
}

#[cfg(test)]
pub(super) fn test_bind_tree(
    root: &std::path::Path,
    data_root: &std::path::Path,
) -> Result<ClineSdkDocumentTree, ClineSdkError> {
    bind_tree(root, data_root)
}

#[cfg(test)]
pub(super) fn test_project_leaf(
    leaf: &ClineSdkDocumentLeaf,
    bytes: &[u8],
) -> std::result::Result<Vec<ctx_history_core::CoreRecord>, String> {
    project_messages(
        &leaf.snapshot,
        &leaf.source_key,
        leaf.session_id,
        leaf.source_anchor_scope,
        [7; 32],
        bytes,
    )
    .map(|projected| projected.records)
    .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(super) fn test_source_revision(
    leaf: &SessionLeaf,
    manifest: Option<&[u8]>,
    messages: Option<&[u8]>,
) -> [u8; 32] {
    source_revision(leaf, manifest, messages)
}
