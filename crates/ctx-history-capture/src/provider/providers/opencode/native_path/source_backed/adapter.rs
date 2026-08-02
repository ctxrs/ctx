use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{CaptureProvider, SourceKey};
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
        SqliteSourceAccessError, SqliteSourceDirectoryAuthority, SqliteSourceEvidence,
        SqliteSourceReadSnapshot,
    },
    CaptureError, ProviderSource,
};

#[derive(Debug)]
struct OpenCodeDocumentTreeAdapter {
    data_root: PathBuf,
    registration: OpenCodeSourceBackedRegistration,
    path: PathBuf,
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
    pub(super) logical_online_backup_opens: u64,
    pub(super) source_bytes_copied: u64,
    pub(super) schema_probe_passes: u64,
    pub(super) schema_event_validation_traversals: u64,
    pub(super) logical_fingerprint_passes: u64,
    pub(super) logical_row_traversals: u64,
    pub(super) projection_passes: u64,
    pub(super) logical_rows_projected: u64,
    pub(super) documents_staged: u64,
    pub(super) max_buffered_documents: u64,
    pub(super) session_rows_scanned: u64,
    pub(super) session_metadata_loads: u64,
    pub(super) max_buffered_session_metadata: u64,
    pub(super) max_session_ancestry_depth: u64,
    pub(super) fallback_payload_hydrations: u64,
    pub(super) max_buffered_payload_rows: u64,
    pub(super) fallback_disk_sort: bool,
    pub(super) exact_replays: u64,
    pub(super) terminal_fences: u64,
    pub(super) terminal_revalidations: u64,
    pub(super) active_snapshots: u64,
    pub(super) max_active_snapshots: u64,
}

#[cfg(test)]
thread_local! {
    static LAST_WORK_COUNTERS: std::cell::RefCell<Option<OpenCodeSqliteWorkCounters>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn take_last_work_counters() -> Option<OpenCodeSqliteWorkCounters> {
    LAST_WORK_COUNTERS.with(|slot| slot.borrow_mut().take())
}

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
                OpenCodeScanOutput::CompletedBytes(bytes) => {
                    sink.report_completed_bytes(bytes).map_err(Into::into)
                }
                OpenCodeScanOutput::Document(document) => {
                    sink.emit_core_record(document).map_err(Into::into)
                }
            },
        )
        .map_err(route_error)?;
        if !scan.source.exact_descriptor_eq(&leaf.observation.source) {
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
            work.logical_row_traversals = 1;
            work.logical_rows_projected = scan.certificate.counts().complete_records;
            work.documents_staged = scan.certificate.counts().indexed_documents;
            work.max_buffered_documents =
                u64::from(scan.certificate.counts().indexed_documents != 0);
            work.session_rows_scanned = scan.bounds.session_rows_scanned;
            work.session_metadata_loads = scan.bounds.session_metadata_loads;
            work.max_buffered_session_metadata = scan.bounds.max_buffered_session_metadata;
            work.max_session_ancestry_depth = scan.bounds.max_session_ancestry_depth;
            work.fallback_payload_hydrations = scan.bounds.fallback_payload_hydrations;
            work.max_buffered_payload_rows = scan.bounds.max_buffered_payload_rows;
            work.fallback_disk_sort = scan.bounds.fallback_disk_sort;
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
                finalize_work_counters(leaf, exact_replay)?;
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
}

pub(crate) fn register(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
) -> SourceBackedCoordinatorResult<()> {
    register_adapter(registry, source, selection, data_root)
}

fn register_adapter(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
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
    let leaf_fingerprint = DocumentLeafFingerprint::new(admitted_leaf_fingerprint(
        &observation.source,
        authorized.sqlite_snapshot.evidence(),
    ));
    let replay_from_frontier = authorized
        .sqlite_snapshot
        .admitted_revision_is_replay_safe();
    let schema_event_validation_traversals = observation.schema.event_validation_traversals;
    let tree_fingerprint = leaf_fingerprint.as_bytes();
    Ok(CompleteDocumentTree::new(
        tree_fingerprint,
        vec![ObservedDocumentLeaf::with_durable_replay(
            leaf_fingerprint,
            OpenCodeDocumentLeaf {
                observation,
                source_root: authorized.source_root,
                sqlite_authority: authorized.sqlite_authority,
                snapshot: Mutex::new(Some(authorized.sqlite_snapshot)),
                terminal_revalidate,
                work: Mutex::new(OpenCodeSqliteWorkCounters {
                    schema_probe_passes: 1,
                    schema_event_validation_traversals,
                    ..OpenCodeSqliteWorkCounters::default()
                }),
            },
            replay_from_frontier,
        )],
        OpenCodeTreeAuthority::Present,
    ))
}

fn admitted_leaf_fingerprint(source: &SourceKey, evidence: &SqliteSourceEvidence) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.opencode-family-admitted-sqlite-revision-v1\0");
    digest.update(source.exact_descriptor_digest());
    digest.update((PARSER_REVISION.len() as u64).to_be_bytes());
    digest.update(PARSER_REVISION.as_bytes());
    digest.update(evidence.revision());
    digest.finalize().into()
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
    work.logical_online_backup_opens = snapshot.logical_online_backup_opens();
    work.snapshot_opens = work
        .immutable_snapshot_opens
        .checked_add(work.copied_snapshot_opens)
        .and_then(|opens| opens.checked_add(work.logical_online_backup_opens))
        .ok_or_else(|| source_internal("OpenCode-family snapshot open count overflowed"))?;
    work.source_bytes_copied = snapshot.source_bytes_copied();
    work.terminal_fences = snapshot.terminal_fences();
    work.terminal_revalidations = snapshot.terminal_revalidations();
    work.active_snapshots = snapshot.active_snapshots();
    work.max_active_snapshots = snapshot.max_active_snapshots();
    work.exact_replays = u64::from(exact_replay);
    let counters = *work;
    if counters.snapshot_opens != 1
        || counters.logical_online_backup_opens != 1
        || counters.schema_probe_passes != 1
        || counters.logical_fingerprint_passes != 0
        || counters.terminal_fences != 1
        || counters.terminal_revalidations < 2
        || counters.active_snapshots != 0
        || counters.max_active_snapshots != 1
        || counters.projection_passes + counters.exact_replays != 1
        || counters.max_buffered_documents > 1
        || counters.max_buffered_session_metadata > 1
        || counters.max_session_ancestry_depth > 64
        || counters.max_buffered_payload_rows > 1
        || counters.session_metadata_loads > counters.session_rows_scanned
        || (counters.fallback_disk_sort
            && counters.fallback_payload_hydrations != counters.logical_rows_projected)
        || (!counters.fallback_disk_sort && counters.fallback_payload_hydrations != 0)
        || (leaf.observation.schema.message_part_indexed_streaming
            && counters.schema_event_validation_traversals != 2)
        || (exact_replay
            && (counters.projection_passes != 0
                || counters.logical_row_traversals != 0
                || counters.logical_rows_projected != 0
                || counters.documents_staged != 0
                || counters.max_buffered_documents != 0))
        || (!exact_replay
            && (counters.projection_passes != 1
                || counters.logical_row_traversals != 1
                || counters.documents_staged > counters.logical_rows_projected))
    {
        return Err(source_internal(
            "OpenCode-family lifecycle violated its one-snapshot bounded-work contract",
        ));
    }
    #[cfg(test)]
    LAST_WORK_COUNTERS.with(|slot| slot.replace(Some(counters)));
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
