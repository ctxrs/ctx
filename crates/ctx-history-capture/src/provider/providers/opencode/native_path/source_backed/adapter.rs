use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{
    BatchHydrationRequest, BatchHydrationResult, CaptureProvider, ContentSourceResolver,
    HydrationFailure, SourceKey,
};
use sha2::{Digest, Sha256};

use super::{
    observe_logical_source, open_root_authorized_snapshot_retained,
    opencode_family_source_backed_registrations, scan_pinned_source, OpenCodeLogicalObservation,
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
    provider_sources::{
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
    },
    CaptureError, ProviderSource,
};

#[derive(Debug)]
struct OpenCodeDocumentTreeAdapter {
    data_root: PathBuf,
    registration: OpenCodeSourceBackedRegistration,
    path: PathBuf,
    #[cfg(test)]
    work_observer: Option<OpenCodeWorkObserver>,
}

#[derive(Debug)]
enum OpenCodeTreeAuthority {
    Present,
    Missing {
        source_root: ProviderSourceRoot,
        database_leaf: OsString,
        tree_fingerprint: [u8; 32],
    },
}

struct OpenCodeDocumentLeaf {
    observation: OpenCodeLogicalObservation,
    source_root: ProviderSourceRoot,
    sqlite_authority: SqliteSourceDirectoryAuthority,
    snapshot: Mutex<Option<SqliteSourceReadSnapshot>>,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
    work: Mutex<OpenCodeSqliteWorkCounters>,
}

type OpenCodeDocumentTree = CompleteDocumentTree<OpenCodeDocumentLeaf, OpenCodeTreeAuthority>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct OpenCodeSqliteWorkCounters {
    pub(super) snapshot_opens: u64,
    pub(super) immutable_snapshot_opens: u64,
    pub(super) copied_snapshot_opens: u64,
    pub(super) source_bytes_copied: u64,
    pub(super) logical_observation_passes: u64,
    pub(super) logical_rows_observed: u64,
    pub(super) projection_passes: u64,
    pub(super) logical_rows_projected: u64,
    pub(super) documents_staged: u64,
    pub(super) max_buffered_documents: u64,
    pub(super) exact_replays: u64,
    pub(super) terminal_fences: u64,
    pub(super) terminal_revalidations: u64,
    pub(super) active_snapshots: u64,
    pub(super) max_active_snapshots: u64,
}

#[cfg(test)]
type OpenCodeWorkObserver = std::sync::Arc<Mutex<Vec<OpenCodeSqliteWorkCounters>>>;

impl ReplacementDocumentTree for OpenCodeDocumentTreeAdapter {
    type Leaf = OpenCodeDocumentLeaf;
    type TreeAuthority = OpenCodeTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        self.registration.owns_source(source)
    }

    fn discover_complete(&self) -> SourceBackedRouteResult<OpenCodeDocumentTree> {
        discover_document_tree(&self.data_root, &self.path, self.registration.dialect)
            .map_err(route_error)
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let OpenCodeTreeAuthority::Present = authority else {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                "missing OpenCode-family tree unexpectedly contained a leaf",
            ));
        };
        let snapshot = leaf
            .snapshot
            .lock()
            .map_err(|_| source_internal("OpenCode-family snapshot lock was poisoned"))?
            .take()
            .ok_or_else(|| source_internal("OpenCode-family snapshot was already consumed"))?;
        let scan = scan_pinned_source(
            &self.path,
            self.registration.dialect,
            &leaf.observation,
            snapshot,
            &mut |output| match output {
                OpenCodeScanOutput::Begin(source) => sink.begin_source(source).map_err(Into::into),
                OpenCodeScanOutput::Document(document) => {
                    sink.emit_document(document).map_err(Into::into)
                }
            },
        )
        .map_err(route_error)?;
        if !scan.source.exact_descriptor_eq(&leaf.observation.source)
            || scan.certificate.counts().complete_records != leaf.observation.logical_rows
        {
            return Err(source_changed(
                "OpenCode-family projection did not match its logical observation",
            ));
        }
        {
            let mut work = leaf
                .work
                .lock()
                .map_err(|_| source_internal("OpenCode-family work counter lock was poisoned"))?;
            work.projection_passes = 1;
            work.logical_rows_projected = scan.certificate.counts().complete_records;
            work.documents_staged = scan.certificate.counts().indexed_documents;
            work.max_buffered_documents =
                u64::from(scan.certificate.counts().indexed_documents != 0);
        }
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
            OpenCodeTreeAuthority::Present => {
                let [observed] = tree.leaves.as_slice() else {
                    return Err(source_internal(
                        "present OpenCode-family tree must contain exactly one leaf",
                    ));
                };
                let leaf = &observed.provider_leaf;
                let exact_replay = if let Some(snapshot) = leaf
                    .snapshot
                    .lock()
                    .map_err(|_| source_internal("OpenCode-family snapshot lock was poisoned"))?
                    .take()
                {
                    snapshot
                        .finish()
                        .map_err(|error| route_error(error.into()))?;
                    true
                } else {
                    false
                };
                leaf.source_root
                    .revalidate()
                    .map_err(|error| route_error(error.into()))?;
                (leaf.terminal_revalidate)().map_err(|error| route_error(error.into()))?;
                let counters = finalize_work_counters(leaf, exact_replay)?;
                #[cfg(test)]
                if let Some(observer) = &self.work_observer {
                    observer.lock().unwrap().push(counters);
                }
                #[cfg(not(test))]
                let _ = counters;
                Ok(tree.tree_fingerprint)
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
            .exact_resolver(self.data_root.clone(), self.path.clone())
            .hydrate_batch(request)
    }
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    register_adapter(
        registry,
        source,
        selection,
        data_root,
        #[cfg(test)]
        None,
    )
}

#[cfg(test)]
pub(super) fn register_with_work_observer(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    work_observer: OpenCodeWorkObserver,
) -> SourceBackedCoordinatorResult<()> {
    register_adapter(registry, source, selection, data_root, Some(work_observer))
}

fn register_adapter(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    #[cfg(test)] work_observer: Option<OpenCodeWorkObserver>,
) -> SourceBackedCoordinatorResult<()> {
    let registration = registration_for_provider(source.provider).ok_or_else(|| {
        invalid_route(
            source.provider,
            "provider is not part of the OpenCode SQLite family",
        )
    })?;
    let adapter = OpenCodeDocumentTreeAdapter {
        data_root: data_root.to_path_buf(),
        registration,
        path: source.path.clone(),
        #[cfg(test)]
        work_observer,
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
    data_root: &Path,
    path: &std::path::Path,
    dialect: &'static crate::provider::providers::opencode::OpenCodeSqliteDialect,
) -> OpenCodeSourceBackedResult<OpenCodeDocumentTree> {
    match observe_present_document_tree(data_root, path, dialect) {
        Ok(tree) => Ok(tree),
        Err(error) if source_missing(&error) => observe_missing_document_tree(path),
        Err(error) => Err(error),
    }
}

fn observe_present_document_tree(
    data_root: &Path,
    path: &std::path::Path,
    dialect: &'static crate::provider::providers::opencode::OpenCodeSqliteDialect,
) -> OpenCodeSourceBackedResult<OpenCodeDocumentTree> {
    let authorized = open_root_authorized_snapshot_retained(data_root, path)?;
    let observation = observe_logical_source(authorized.sqlite_snapshot.connection()?, dialect)?;
    let terminal_revalidate = authorized.sqlite_snapshot.terminal_revalidator();
    let leaf_fingerprint = DocumentLeafFingerprint::new(observation.fingerprint);
    let tree_fingerprint = leaf_fingerprint.as_bytes();
    let logical_rows = observation.logical_rows;
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        vec![ObservedDocumentLeaf::new(
            leaf_fingerprint,
            OpenCodeDocumentLeaf {
                observation,
                source_root: authorized.source_root,
                sqlite_authority: authorized.sqlite_authority,
                snapshot: Mutex::new(Some(authorized.sqlite_snapshot)),
                terminal_revalidate,
                work: Mutex::new(OpenCodeSqliteWorkCounters {
                    logical_observation_passes: 1,
                    logical_rows_observed: logical_rows,
                    ..OpenCodeSqliteWorkCounters::default()
                }),
            },
        )],
        OpenCodeTreeAuthority::Present,
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

fn finalize_work_counters(
    leaf: &OpenCodeDocumentLeaf,
    exact_replay: bool,
) -> SourceBackedRouteResult<OpenCodeSqliteWorkCounters> {
    let snapshot = leaf.sqlite_authority.snapshot_counters();
    let mut work = leaf
        .work
        .lock()
        .map_err(|_| source_internal("OpenCode-family work counter lock was poisoned"))?;
    work.immutable_snapshot_opens = snapshot.immutable_snapshot_opens();
    work.copied_snapshot_opens = snapshot.copied_snapshot_opens();
    work.snapshot_opens = work
        .immutable_snapshot_opens
        .checked_add(work.copied_snapshot_opens)
        .ok_or_else(|| source_internal("OpenCode-family snapshot open count overflowed"))?;
    work.source_bytes_copied = snapshot.source_bytes_copied();
    work.terminal_fences = snapshot.terminal_fences();
    work.terminal_revalidations = snapshot.terminal_revalidations();
    work.active_snapshots = snapshot.active_snapshots();
    work.max_active_snapshots = snapshot.max_active_snapshots();
    work.exact_replays = u64::from(exact_replay);
    let counters = *work;
    if counters.snapshot_opens != 1
        || counters.logical_observation_passes != 1
        || counters.terminal_fences != 1
        || counters.terminal_revalidations < 2
        || counters.active_snapshots != 0
        || counters.max_active_snapshots != 1
        || counters.projection_passes + counters.exact_replays != 1
        || counters.max_buffered_documents > 1
        || (exact_replay
            && (counters.projection_passes != 0
                || counters.logical_rows_projected != 0
                || counters.documents_staged != 0
                || counters.max_buffered_documents != 0))
        || (!exact_replay
            && (counters.projection_passes != 1
                || counters.logical_rows_projected != counters.logical_rows_observed
                || counters.documents_staged > counters.logical_rows_projected))
    {
        return Err(source_internal(
            "OpenCode-family lifecycle violated its one-snapshot bounded-work contract",
        ));
    }
    Ok(counters)
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

fn source_internal(detail: impl Into<String>) -> SourceBackedRouteError {
    SourceBackedRouteError::new(SourceBackedRouteErrorKind::Internal, detail)
}

fn unavailable_io(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
    )
}
