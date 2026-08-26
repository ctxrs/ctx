use std::{
    marker::PhantomData,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_capture_model::ProviderSource;
use ctx_history_capture_runtime::{
    ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint, DocumentSourceTerminal,
    ObservedDocumentLeaf, ReplacementDocumentTree, SourceBackedRouteError,
    SourceBackedRouteErrorKind, SourceBackedRouteResult, SourceBackedRouteSelection,
    SourceBackedSelectorAuthority,
};
use ctx_history_core::{
    CaptureProvider, CertifiedSource, ScannedSourceCounts, SourceAnchorScope, SourceKey,
};
use thiserror::Error;

use crate::{
    providers::{
        deepagents::native_path::source_backed::DeepAgentsRouteAdapter,
        forgecode::nativepath::source_backed::ForgeCodeRouteAdapter,
        opencode::native_path::source_backed::source_backed_adapter_scoped,
        zed::native_path::{
            source_backed::{
                acquire_snapshot, decode_sha256_hex, scan_zed_native_snapshot,
                snapshot_revision_digest, source_observation, zed_source_key_scoped,
                ZedSourceBackedSinkV0, ZED_PARSER_REVISION,
            },
            ZedImmutableSqliteSnapshot, ZedNativePathError,
        },
    },
    CaptureError, LogicalSqliteRuntimeBinding, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

pub enum LogicalSqliteRoutePlan<B: LogicalSqliteRuntimeBinding> {
    DeepAgents {
        source: ProviderSource,
        adapter: DeepAgentsRouteAdapter<B>,
    },
    ForgeCode {
        source: ProviderSource,
        adapter: ForgeCodeRouteAdapter<B>,
        authority: SourceBackedSelectorAuthority,
    },
    OpenCodeFamily {
        source: ProviderSource,
        adapter:
            crate::providers::opencode::native_path::source_backed::OpenCodeDocumentTreeAdapter<B>,
    },
    Zed {
        source: ProviderSource,
        adapter: ZedRouteAdapter<B>,
    },
}

impl<B: LogicalSqliteRuntimeBinding> LogicalSqliteRoutePlan<B> {
    pub fn selector_authority(&self) -> SourceBackedSelectorAuthority {
        match self {
            Self::ForgeCode { authority, .. } => *authority,
            Self::DeepAgents { .. } | Self::OpenCodeFamily { .. } | Self::Zed { .. } => {
                SourceBackedSelectorAuthority::DiscoveredWinner
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum LogicalSqliteRegistrationError {
    #[error("{0} is not owned by the logical SQLite provider pack")]
    UnsupportedProvider(&'static str),
    #[error("manual ForgeCode registration requires explicit catalog lineage")]
    ForgeCodeLineageRequired,
    #[error("invalid logical SQLite route: {0}")]
    InvalidRoute(&'static str),
}

pub fn logical_sqlite_route_plan<B: LogicalSqliteRuntimeBinding>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> Result<LogicalSqliteRoutePlan<B>, LogicalSqliteRegistrationError> {
    logical_sqlite_route_plan_scoped(source, selection, data_root, SourceAnchorScope::Unqualified)
}

pub fn logical_sqlite_route_plan_scoped<B: LogicalSqliteRuntimeBinding>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    source_scope: SourceAnchorScope,
) -> Result<LogicalSqliteRoutePlan<B>, LogicalSqliteRegistrationError> {
    match source.provider {
        CaptureProvider::DeepAgents => {
            let adapter =
                DeepAgentsRouteAdapter::new_scoped(data_root, source.path.clone(), source_scope);
            Ok(LogicalSqliteRoutePlan::DeepAgents { source, adapter })
        }
        CaptureProvider::ForgeCode => {
            if selection != SourceBackedRouteSelection::Automatic {
                return Err(LogicalSqliteRegistrationError::ForgeCodeLineageRequired);
            }
            let adapter = ForgeCodeRouteAdapter::selected_scoped(
                data_root,
                source.path.clone(),
                source_scope,
            );
            Ok(LogicalSqliteRoutePlan::ForgeCode {
                source,
                adapter,
                authority: SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
            })
        }
        CaptureProvider::OpenCode | CaptureProvider::Kilo | CaptureProvider::MiMoCode => {
            let adapter = source_backed_adapter_scoped(source.clone(), data_root, source_scope)
                .map_err(LogicalSqliteRegistrationError::InvalidRoute)?;
            Ok(LogicalSqliteRoutePlan::OpenCodeFamily { source, adapter })
        }
        CaptureProvider::Zed => {
            let adapter = ZedRouteAdapter::new_scoped(data_root, source.path.clone(), source_scope);
            Ok(LogicalSqliteRoutePlan::Zed { source, adapter })
        }
        provider => Err(LogicalSqliteRegistrationError::UnsupportedProvider(
            provider.as_str(),
        )),
    }
}

pub fn explicit_forgecode_route_plan<B: LogicalSqliteRuntimeBinding>(
    source: ProviderSource,
    data_root: &Path,
    catalog_lineage: [u8; 32],
) -> Result<LogicalSqliteRoutePlan<B>, LogicalSqliteRegistrationError> {
    explicit_forgecode_route_plan_scoped(
        source,
        data_root,
        catalog_lineage,
        SourceAnchorScope::Unqualified,
    )
}

pub fn explicit_forgecode_route_plan_scoped<B: LogicalSqliteRuntimeBinding>(
    source: ProviderSource,
    data_root: &Path,
    catalog_lineage: [u8; 32],
    source_scope: SourceAnchorScope,
) -> Result<LogicalSqliteRoutePlan<B>, LogicalSqliteRegistrationError> {
    if source.provider != CaptureProvider::ForgeCode {
        return Err(LogicalSqliteRegistrationError::UnsupportedProvider(
            source.provider.as_str(),
        ));
    }
    let adapter = ForgeCodeRouteAdapter::explicit_scoped(
        data_root,
        source.path.clone(),
        catalog_lineage,
        source_scope,
    );
    Ok(LogicalSqliteRoutePlan::ForgeCode {
        source,
        adapter,
        authority: SourceBackedSelectorAuthority::ExplicitPath,
    })
}

#[derive(Debug, Clone)]
pub struct ZedRouteAdapter<B> {
    data_root: PathBuf,
    path: PathBuf,
    source_scope: SourceAnchorScope,
    binding: PhantomData<fn() -> B>,
}

impl<B> ZedRouteAdapter<B> {
    pub fn new(data_root: &Path, path: PathBuf) -> Self {
        Self::new_scoped(data_root, path, SourceAnchorScope::Unqualified)
    }

    pub fn new_scoped(data_root: &Path, path: PathBuf, source_scope: SourceAnchorScope) -> Self {
        Self {
            data_root: data_root.to_path_buf(),
            path,
            source_scope,
            binding: PhantomData,
        }
    }
}

pub struct ZedTreeAuthority {
    snapshot: Mutex<Option<ZedImmutableSqliteSnapshot>>,
    terminal_revalidate: Box<dyn Fn() -> Result<(), ZedNativePathError> + Send + Sync + 'static>,
}

impl<B: LogicalSqliteRuntimeBinding> ReplacementDocumentTree for ZedRouteAdapter<B> {
    type Lifecycle = B::Lifecycle;
    type Spool = B::Spool;
    type RouteControl = B::RouteControl;
    type Leaf = SourceKey;
    type TreeAuthority = ZedTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        ZED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        source.provider() == CaptureProvider::Zed.as_str()
            && source.source_format() == ZED_THREADS_SQLITE_SOURCE_FORMAT
    }

    fn durable_replay_source(
        &self,
        _authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
    ) -> SourceBackedRouteResult<Option<SourceKey>> {
        Ok(Some(leaf.clone()))
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let snapshot = acquire_snapshot(&self.data_root, &self.path).map_err(route_error)?;
        let fingerprint =
            DocumentLeafFingerprint::new(snapshot_revision_digest(&snapshot.snapshot_revision));
        let terminal_revalidate = snapshot.terminal_revalidator().map_err(route_error)?;
        let source = zed_source_key_scoped(self.source_scope).map_err(route_error)?;
        Ok(CompleteDocumentTree::new(
            fingerprint.as_bytes(),
            vec![ObservedDocumentLeaf::new(fingerprint, source)],
            ZedTreeAuthority {
                snapshot: Mutex::new(Some(snapshot)),
                terminal_revalidate,
            },
        ))
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        source: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, B::Lifecycle, B::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let mut snapshot = authority
            .snapshot
            .lock()
            .map_err(|_| zed_internal("Zed snapshot lock was poisoned"))?
            .take()
            .ok_or_else(|| zed_internal("Zed snapshot was consumed twice"))?;
        let snapshot_revision = snapshot.snapshot_revision.clone();
        sink.begin_source(source.clone())?;
        let connection = snapshot.connection().map_err(route_error)?;
        let mut sink_failure = None;
        let mut projection =
            ZedSourceBackedSinkV0::with_emitter(connection, source.clone(), |record| {
                sink.emit_core_record(record).map_err(|error| {
                    let detail = error.to_string();
                    sink_failure = Some(error);
                    CaptureError::InvalidPayload(detail).into()
                })
            })
            .map_err(route_error)?;
        let scan = scan_zed_native_snapshot(connection, &snapshot_revision, &mut projection);
        let projection_failure = projection.take_failure();
        let staged_core_records = projection.staged_core_records();
        drop(projection);
        if let Some(error) = sink_failure {
            return Err(error);
        }
        if let Some(error) = projection_failure {
            return Err(route_error(error));
        }
        let scan = scan.map_err(route_error)?;
        snapshot.finish().map_err(route_error)?;
        if staged_core_records != scan.counters.retained_events {
            return Err(zed_internal("Zed source-backed counts do not reconcile"));
        }
        let complete_records = scan
            .counters
            .retained_events
            .checked_add(scan.counters.rejected_threads)
            .ok_or_else(|| zed_internal("Zed source-backed counts overflowed"))?;
        let counts = ScannedSourceCounts {
            complete_records,
            retained_records: scan.counters.retained_events,
            rejected_records: scan.counters.rejected_threads,
            ignored_records: 0,
            indexed_documents: staged_core_records,
            certified_bytes: scan.counters.certified_logical_bytes,
        };
        let observation = source_observation(source, &snapshot_revision).map_err(route_error)?;
        let certificate = CertifiedSource::certify(
            observation.clone(),
            observation,
            ZED_PARSER_REVISION,
            decode_sha256_hex(&scan.source_integrity_digest).map_err(route_error)?,
            counts,
        )
        .map_err(route_error)?;
        Ok(zed_document_terminal(certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        if let Some(mut snapshot) = tree
            .authority
            .snapshot
            .lock()
            .map_err(|_| zed_internal("Zed snapshot lock was poisoned"))?
            .take()
        {
            snapshot.finish().map_err(route_error)?;
        }
        (tree.authority.terminal_revalidate)().map_err(route_error)?;
        Ok(tree.tree_fingerprint)
    }
}

fn zed_document_terminal(certificate: CertifiedSource) -> DocumentSourceTerminal {
    DocumentSourceTerminal {
        source: certificate.observation().source().clone(),
        opening: certificate.observation().clone(),
        closing: certificate.observation().clone(),
        parser_revision: ZED_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

fn route_error(error: impl std::fmt::Display) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::InvalidSource, error.to_string())
}

fn zed_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}
