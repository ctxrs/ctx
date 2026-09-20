//! The Devin document-tree route.
//!
//! Devin keeps one database at one exact path, so the tree is a single leaf.
//! Discovery may still observe the file absent — a fresh install has no
//! history — and that absence is fenced rather than treated as an error, so a
//! refresh can publish a stable no-op generation until the file appears.

use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use ctx_history_core::{SourceAnchorScope, SourceKey};

use crate::{
    common::io::{OpenedProviderSourcePath, ProviderSourceDirectory, ProviderSourceRoot},
    document_inventory_authority,
    provider::source_backed::{
        route_error, ChangedDocumentSink, CompleteDocumentTree, DocumentLeafFingerprint,
        DocumentSourceTerminal, ObservedDocumentLeaf, ReplacementDocumentTree,
        SourceBackedRouteDriver, SourceBackedRouteError, SourceBackedRouteResult,
    },
    provider_sources::{
        SqliteFailurePhase, SqliteSourceAccessError, SqliteSourceDirectoryAuthority,
        SqliteSourceEvidence,
    },
    sqlite_common::{internal_error, source_changed},
    SelectedSqliteCaptureBinding, DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::{
    database::{absolute_devin_path, require_devin_sqlite_format, DevinSqliteDatabase},
    schema::DevinNativeSchema,
    source_backed::{
        devin_missing_tree_fingerprint, devin_route_error, devin_source_key_scoped,
        scan_devin_snapshot, DevinResult, DevinSourceBackedError,
        DEVIN_SOURCE_BACKED_PARSER_REVISION, DEVIN_SOURCE_PATH_REASONS,
    },
};

enum DevinTreeAuthority {
    Present(Box<DevinPresentAuthority>),
    Missing(DevinMissingLeafFence),
}

struct DevinPresentAuthority {
    opening_evidence: SqliteSourceEvidence,
    _sqlite_authority: SqliteSourceDirectoryAuthority,
    database: Mutex<Option<DevinSqliteDatabase>>,
    terminal_revalidate:
        Box<dyn Fn() -> Result<(), SqliteSourceAccessError> + Send + Sync + 'static>,
}

#[derive(Debug)]
struct DevinDocumentTreeAdapter<B> {
    data_root: PathBuf,
    path: PathBuf,
    source_scope: SourceAnchorScope,
    binding: std::marker::PhantomData<fn() -> B>,
}

impl<B: SelectedSqliteCaptureBinding> ReplacementDocumentTree for DevinDocumentTreeAdapter<B> {
    type Lifecycle = B::Lifecycle;
    type Spool = B::Spool;
    type RouteControl = B::RouteControl;
    type Leaf = SourceKey;
    type TreeAuthority = DevinTreeAuthority;

    fn parser_revision(&self) -> &'static str {
        DEVIN_SOURCE_BACKED_PARSER_REVISION
    }

    fn owns_source(&self, source: &SourceKey) -> bool {
        devin_source_key_scoped(self.source_scope)
            .is_ok_and(|owned| owned.exact_descriptor_eq(source))
    }

    fn discover_complete(
        &self,
    ) -> SourceBackedRouteResult<CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>> {
        let source = devin_source_key_scoped(self.source_scope).map_err(route_error)?;
        match observe_devin_inventory(&self.data_root, &self.path).map_err(devin_route_error)? {
            DevinPhysicalInventory::Present(present) => {
                let fingerprint = present.logical_fingerprint;
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    vec![ObservedDocumentLeaf::new(
                        DocumentLeafFingerprint::new(fingerprint),
                        source,
                    )],
                    DevinTreeAuthority::Present(Box::new(DevinPresentAuthority {
                        opening_evidence: present.database.evidence().clone(),
                        _sqlite_authority: present.database.sqlite_authority(),
                        terminal_revalidate: present.database.terminal_revalidator(),
                        database: Mutex::new(Some(present.database)),
                    })),
                ))
            }
            DevinPhysicalInventory::Missing(fence) => {
                let fingerprint = fence.fingerprint();
                Ok(CompleteDocumentTree::new(
                    fingerprint,
                    Vec::new(),
                    DevinTreeAuthority::Missing(fence),
                ))
            }
        }
    }

    fn scan_changed(
        &self,
        authority: &Self::TreeAuthority,
        leaf: &Self::Leaf,
        sink: &mut ChangedDocumentSink<'_, '_, B::Lifecycle, B::Spool>,
    ) -> SourceBackedRouteResult<DocumentSourceTerminal> {
        let DevinTreeAuthority::Present(authority) = authority else {
            return Err(internal_error(
                "Devin missing inventory unexpectedly contained a document leaf",
            ));
        };
        let database = take_database(&authority.database)?;
        let scan = (|| {
            sink.begin_source(leaf.clone())?;
            let connection = database.connection().map_err(devin_route_error)?;
            let schema = DevinNativeSchema::probe(connection)
                .map_err(|error| devin_route_error(error.into()))?;
            let scan = scan_devin_snapshot(connection, &schema, leaf, &mut |record| {
                sink.emit_core_record(record)
                    .map_err(DevinSourceBackedError::Route)
            })
            .map_err(|error| {
                database.diagnose_provider_query_error(error, SqliteFailurePhase::Projection)
            })
            .map_err(devin_route_error)?;
            // The snapshot must still be the one this scan opened, or the
            // records just emitted describe a source that no longer exists.
            if database.evidence() != &authority.opening_evidence {
                return Err(source_changed(
                    "Devin SQLite physical inventory changed during logical projection",
                ));
            }
            database.revalidate().map_err(devin_route_error)?;
            Ok(scan)
        })();
        let scan = match scan {
            Ok(scan) => scan,
            Err(error) => return Err(database.abort(error)),
        };
        if let Err(failure) = restore_database(&authority.database, database) {
            let (error, database) = *failure;
            return Err(database.abort(error));
        }
        let certificate = scan.certify(leaf.clone()).map_err(devin_route_error)?;
        Ok(document_terminal(&certificate))
    }

    fn revalidate_complete(
        &self,
        tree: &CompleteDocumentTree<Self::Leaf, Self::TreeAuthority>,
    ) -> SourceBackedRouteResult<[u8; 32]> {
        match &tree.authority {
            DevinTreeAuthority::Present(authority) => {
                let database = take_database(&authority.database)?;
                let evidence = database.finish().map_err(devin_route_error)?;
                if evidence != authority.opening_evidence {
                    return Err(source_changed(
                        "Devin SQLite physical inventory changed before commit",
                    ));
                }
                (authority.terminal_revalidate)().map_err(|error| {
                    crate::provider::source_backed::sqlite_source_route_error(error)
                })?;
            }
            DevinTreeAuthority::Missing(fence) if !fence.revalidate() => {
                return Err(source_changed("Devin SQLite absence changed before commit"));
            }
            DevinTreeAuthority::Missing(_) => {}
        }
        Ok(tree.tree_fingerprint)
    }
}

pub(crate) fn source_backed_driver_scoped<B: SelectedSqliteCaptureBinding>(
    provider: &str,
    source_format: &str,
    source_path: &Path,
    data_root: &Path,
    source_scope: SourceAnchorScope,
) -> SourceBackedRouteDriver<B::Lifecycle, B::RouteControl> {
    let adapter = DevinDocumentTreeAdapter::<B> {
        data_root: data_root.to_path_buf(),
        path: source_path.to_path_buf(),
        source_scope,
        binding: std::marker::PhantomData,
    };
    ctx_history_capture_runtime::replacement_document_tree_driver(
        document_inventory_authority(provider, source_format, source_path),
        adapter,
    )
}

fn document_terminal(certificate: &ctx_history_core::CertifiedSource) -> DocumentSourceTerminal {
    let observation = certificate.observation().clone();
    DocumentSourceTerminal {
        source: observation.source().clone(),
        opening: observation.clone(),
        closing: observation,
        parser_revision: DEVIN_SOURCE_BACKED_PARSER_REVISION,
        content_digest: *certificate.content_digest(),
        counts: certificate.counts(),
    }
}

enum DevinPhysicalInventory {
    Present(Box<DevinPresentInventory>),
    Missing(DevinMissingLeafFence),
}

struct DevinPresentInventory {
    logical_fingerprint: [u8; 32],
    database: DevinSqliteDatabase,
}

/// Observes the database, or fences its absence.
///
/// The logical fingerprint is computed here, before any record is emitted, so
/// a refresh can decide the source is unchanged without projecting it.
fn observe_devin_inventory(data_root: &Path, path: &Path) -> DevinResult<DevinPhysicalInventory> {
    let path = absolute_devin_path(path)?;
    let parent = crate::sqlite_common::database_parent(&path, &DEVIN_SOURCE_PATH_REASONS)?;
    let leaf = crate::sqlite_common::database_leaf(&path, &DEVIN_SOURCE_PATH_REASONS)?;
    let root = ProviderSourceRoot::open(parent)?;
    let directory = root.directory()?;
    root.revalidate()?;
    directory.revalidate()?;
    match directory.open_child(leaf) {
        Ok(OpenedProviderSourcePath::File(file)) => {
            require_devin_sqlite_format(&path, DEVIN_CLI_SESSIONS_SQLITE_SOURCE_FORMAT)?;
            file.revalidate()?;
            directory.revalidate()?;
            root.revalidate()?;
            drop(file);
            let database = DevinSqliteDatabase::open(data_root, &path)?;
            let observed = (|| {
                let connection = database.connection()?;
                let schema = DevinNativeSchema::probe(connection)?;
                let scan =
                    scan_devin_snapshot(connection, &schema, &placeholder_source()?, &mut |_| {
                        Ok(())
                    })?;
                Ok::<[u8; 32], DevinSourceBackedError>(scan.logical_fingerprint)
            })();
            let logical_fingerprint = match observed {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    let error =
                        database.diagnose_provider_query_error(error, SqliteFailurePhase::Schema);
                    return Err(DevinSourceBackedError::Route(
                        database.abort(devin_route_error(error)),
                    ));
                }
            };
            if let Err(error) = database.revalidate() {
                return Err(DevinSourceBackedError::Route(
                    database.abort(devin_route_error(error)),
                ));
            }
            Ok(DevinPhysicalInventory::Present(Box::new(
                DevinPresentInventory {
                    logical_fingerprint,
                    database,
                },
            )))
        }
        Ok(OpenedProviderSourcePath::Directory(_)) => Err(
            crate::sqlite_common::invalid_database_leaf(&path, &DEVIN_SOURCE_PATH_REASONS).into(),
        ),
        Err(ctx_history_source_io::SourceIoError::Io(error))
            if error.kind() == io::ErrorKind::NotFound =>
        {
            directory.revalidate()?;
            root.revalidate()?;
            Ok(DevinPhysicalInventory::Missing(DevinMissingLeafFence {
                root,
                directory,
                leaf: leaf.to_os_string(),
            }))
        }
        Err(error) => Err(error.into()),
    }
}

/// The source key used while observing, before a leaf is published.
///
/// Fingerprinting reads the same rows regardless of scope, and the scope only
/// affects identity derivation, which observation does not publish.
fn placeholder_source() -> DevinResult<SourceKey> {
    devin_source_key_scoped(SourceAnchorScope::Unqualified)
}

fn take_database(
    slot: &Mutex<Option<DevinSqliteDatabase>>,
) -> SourceBackedRouteResult<DevinSqliteDatabase> {
    slot.lock()
        .map_err(|_| internal_error("Devin SQLite snapshot lock was poisoned"))?
        .take()
        .ok_or_else(|| internal_error("Devin SQLite snapshot was already consumed"))
}

fn restore_database(
    slot: &Mutex<Option<DevinSqliteDatabase>>,
    database: DevinSqliteDatabase,
) -> Result<(), Box<(SourceBackedRouteError, DevinSqliteDatabase)>> {
    let mut slot = match slot.lock() {
        Ok(slot) => slot,
        Err(_) => {
            return Err(Box::new((
                internal_error("Devin SQLite snapshot lock was poisoned"),
                database,
            )));
        }
    };
    if slot.is_some() {
        return Err(Box::new((
            internal_error("Devin SQLite snapshot slot was already occupied"),
            database,
        )));
    }
    *slot = Some(database);
    Ok(())
}

/// Holds the observation that the database is absent.
///
/// Revalidation re-checks the whole chain — root, directory, and the still
/// missing leaf — so a file that appears between discovery and commit forces a
/// rescan instead of publishing an empty generation over real history.
#[derive(Debug)]
struct DevinMissingLeafFence {
    root: ProviderSourceRoot,
    directory: ProviderSourceDirectory,
    leaf: OsString,
}

impl DevinMissingLeafFence {
    fn fingerprint(&self) -> [u8; 32] {
        devin_missing_tree_fingerprint_for(&self.root)
    }

    fn revalidate(&self) -> bool {
        if self.root.revalidate().is_err() || self.directory.revalidate().is_err() {
            return false;
        }
        let missing = matches!(
            self.directory.open_child(&self.leaf),
            Err(ctx_history_source_io::SourceIoError::Io(error))
                if error.kind() == io::ErrorKind::NotFound
        );
        missing && self.directory.revalidate().is_ok() && self.root.revalidate().is_ok()
    }
}

fn devin_missing_tree_fingerprint_for(root: &ProviderSourceRoot) -> [u8; 32] {
    devin_source_key_scoped(SourceAnchorScope::Unqualified)
        .map(|source| devin_missing_tree_fingerprint(&source))
        .unwrap_or_else(|_| root.authority_fingerprint())
}
