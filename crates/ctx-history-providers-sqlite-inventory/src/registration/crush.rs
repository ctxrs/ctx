use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::lifecycle::{
    CaptureLifecycleSink, ChangedDocumentSink, DocumentRecordSpool, ReplacementDocumentTree,
    SourceBackedRouteError, SourceBackedRouteErrorKind, SourceBackedRouteResult,
    SourceBackedRouteSelection, SourceBackedSelectorAuthority,
};
use crate::provider::providers::crush::native_path::source_backed::BoundDatabase;
use crate::provider::providers::crush::native_path::source_backed::{
    bind_inventory_scoped as bind_crush_inventory_scoped,
    finish_opened_source as finish_crush_source, scan_source as scan_crush_source,
    CrushSourceBackedErrorV0, CRUSH_PARSER_REVISION,
};
use crate::provider::source_backed::combine_primary_and_cleanup_route_errors;
use crate::{
    CrushProjectDatabaseV0, CrushProjectInventorySourceV0, ProviderSource,
    CRUSH_SQLITE_SOURCE_FORMAT,
};
use ctx_history_core::{CaptureProvider, CertifiedSource, SourceAnchorScope};

use super::shared::{
    sqlite_inventory_authority_fingerprint, SqliteInventoryCatalog, SqliteInventoryCatalogLeaf,
    SqliteInventoryDocumentAdapter, SqliteInventoryProvider,
};
use super::{
    sqlite_capture_route_error, sqlite_inventory_watch_targets, sqlite_source_route_error,
    sqlite_source_route_error_kind, SqliteInventoryCoverage, SqliteInventoryRegistration,
};
use crate::provider_sources::SqliteSourceReadSnapshot;

#[cfg(test)]
use super::shared::SqliteInventorySnapshotCounters;

pub struct CrushInventoryProvider<I> {
    data_root: PathBuf,
    inventory: Arc<I>,
    source_scope: SourceAnchorScope,
}

impl<I, L, S> SqliteInventoryProvider<L, S> for CrushInventoryProvider<I>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    type Leaf = BoundDatabase;

    fn parser_revision(&self) -> &'static str {
        CRUSH_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = bind_crush_inventory_scoped(
            &self.data_root,
            self.inventory.observe().map_err(crush_route_error)?,
            self.source_scope,
        )
        .map_err(crush_route_error)?;
        let authority_fingerprint = sqlite_inventory_authority_fingerprint(&inventory.observation)?;
        let leaves = inventory
            .databases
            .into_iter()
            .map(|database| SqliteInventoryCatalogLeaf {
                source: database.source_key.clone(),
                physical_locator: database.canonical_path.clone(),
                provider_leaf: database,
            })
            .collect();
        Ok(SqliteInventoryCatalog {
            authority_fingerprint,
            leaves,
        })
    }

    fn scan(
        &self,
        leaf: &Self::Leaf,
        snapshot: SqliteSourceReadSnapshot,
        sink: &mut ChangedDocumentSink<'_, '_, L, S>,
    ) -> SourceBackedRouteResult<CertifiedSource> {
        let source =
            crate::provider::providers::crush::native_path::source_backed::open_source_snapshot(
                leaf.clone(),
                snapshot,
            )
            .map_err(crush_route_error)?;
        #[cfg(test)]
        self.inventory.record_projection_pass();
        let certificate = match scan_crush_source(&source, sink) {
            Ok(certificate) => certificate,
            Err(primary) => {
                return Err(crush_route_error(
                    crate::provider::providers::crush::native_path::source_backed::abort_opened_source(
                        source, primary,
                    ),
                ));
            }
        };
        if !finish_crush_source(source).map_err(crush_route_error)? {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "Crush source changed while its logical replacement was staged",
            ));
        }
        Ok(certificate)
    }

    #[cfg(test)]
    fn observe_snapshot_counters(&self, counters: SqliteInventorySnapshotCounters) {
        self.inventory.record_snapshot_work(
            crate::provider::providers::crush::native_path::source_backed::CrushSnapshotWorkV0 {
                immutable_snapshot_opens: counters.immutable_snapshot_opens,
                copied_snapshot_opens: counters.copied_snapshot_opens,
                source_bytes_copied: counters.source_bytes_copied,
                terminal_fences: counters.terminal_fences,
                terminal_revalidations: counters.terminal_revalidations,
                max_active_snapshots: counters.max_active_snapshots,
            },
        );
    }
}

fn crush_route_error(error: CrushSourceBackedErrorV0) -> SourceBackedRouteError {
    if let CrushSourceBackedErrorV0::Route(error) = error {
        return error;
    }
    if let CrushSourceBackedErrorV0::SnapshotCleanup { primary, cleanup } = error {
        return combine_primary_and_cleanup_route_errors(
            crush_route_error(*primary),
            sqlite_source_route_error(cleanup.into_source()),
        );
    }
    let kind = match &error {
        CrushSourceBackedErrorV0::SqliteSource(error) => {
            sqlite_source_route_error_kind(error.source())
        }
        CrushSourceBackedErrorV0::Capture(error) => {
            sqlite_capture_route_error(error).unwrap_or(SourceBackedRouteErrorKind::InvalidSource)
        }
        CrushSourceBackedErrorV0::Sqlite(error)
            if crate::provider_sources::rusqlite_resource_failure(error) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        CrushSourceBackedErrorV0::Io(error)
            if crate::provider_sources::resource_exhaustion_io_error(error) =>
        {
            SourceBackedRouteErrorKind::ResourceUnavailable
        }
        CrushSourceBackedErrorV0::Sqlite(_) | CrushSourceBackedErrorV0::Io(_) => {
            SourceBackedRouteErrorKind::Internal
        }
        _ => SourceBackedRouteErrorKind::InvalidSource,
    };
    SourceBackedRouteError::new(kind, error.to_string())
}

/// Registers Crush's selector-owned finite project inventory as logical SQLite
/// leaves. Each admitted project database is acquired once, observed before
/// replay, and projected only when its logical tables changed.
pub fn crush_registration<I, L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory: Arc<I>,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    crush_registration_scoped(
        source,
        selection,
        data_root,
        inventory,
        SourceAnchorScope::Unqualified,
        SqliteInventoryCoverage::Complete,
    )
}

pub fn crush_registration_scoped<I, L, S>(
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory: Arc<I>,
    source_scope: SourceAnchorScope,
    coverage: SqliteInventoryCoverage,
) -> SqliteInventoryRegistration<
    impl ReplacementDocumentTree<
        Lifecycle = L,
        Spool = S,
        RouteControl = crate::ProviderRouteControlExpectation,
    >,
>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let watch_inventory = Arc::clone(&inventory);
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        CrushInventoryProvider {
            data_root: data_root.to_path_buf(),
            inventory,
            source_scope,
        },
    )
    .with_coverage(coverage);
    SqliteInventoryRegistration::new(
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        adapter,
        Some(Box::new(move || {
            let inventory = watch_inventory.observe().ok()?;
            Some(sqlite_inventory_watch_targets(
                inventory
                    .databases()
                    .iter()
                    .map(CrushProjectDatabaseV0::path),
            ))
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_sources::{
        SqliteArtifactKind, SqliteCleanupStatus, SqliteFailurePhase, SqliteSourceAccessError,
    };
    use rusqlite::ffi;

    #[test]
    fn crush_sqlite_source_change_remains_typed() {
        let error = crush_route_error(SqliteSourceAccessError::SourceChanged.into());
        assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    }

    #[test]
    fn crush_production_mapper_preserves_sqlite_resource_and_corruption_taxonomy() {
        for code in [ffi::SQLITE_BUSY, ffi::SQLITE_LOCKED] {
            let diagnosed = |artifact| {
                SqliteSourceAccessError::SqliteControl {
                    operation: "using the production Crush SQLite snapshot",
                    code,
                }
                .with_diagnostic(
                    SqliteFailurePhase::Projection,
                    artifact,
                    4,
                    16_384,
                    SqliteCleanupStatus::NotRequired,
                )
            };
            assert_eq!(
                crush_route_error(diagnosed(SqliteArtifactKind::ProviderDatabase).into()).kind,
                SourceBackedRouteErrorKind::Unavailable
            );
            assert_eq!(
                crush_route_error(diagnosed(SqliteArtifactKind::PrivateSourceCopy).into()).kind,
                SourceBackedRouteErrorKind::Internal
            );
            let raw = rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
            assert_eq!(
                crush_route_error(CrushSourceBackedErrorV0::Sqlite(raw)).kind,
                SourceBackedRouteErrorKind::Internal
            );
        }

        for code in [ffi::SQLITE_FULL, ffi::SQLITE_NOMEM] {
            let error = SqliteSourceAccessError::SqliteControl {
                operation: "using the production Crush SQLite snapshot",
                code,
            }
            .with_diagnostic(
                SqliteFailurePhase::Projection,
                SqliteArtifactKind::PrivateSourceCopy,
                4,
                16_384,
                SqliteCleanupStatus::NotRequired,
            );
            assert_eq!(
                crush_route_error(error.into()).kind,
                SourceBackedRouteErrorKind::ResourceUnavailable
            );
            let raw = rusqlite::Error::SqliteFailure(ffi::Error::new(code), None);
            assert_eq!(
                crush_route_error(CrushSourceBackedErrorV0::Sqlite(raw)).kind,
                SourceBackedRouteErrorKind::ResourceUnavailable
            );
        }

        let provider = SqliteSourceAccessError::SqliteControl {
            operation: "querying the exact Crush provider copy",
            code: ffi::SQLITE_CORRUPT,
        }
        .with_diagnostic(
            SqliteFailurePhase::Projection,
            SqliteArtifactKind::PrivateSourceCopy,
            4,
            16_384,
            SqliteCleanupStatus::NotRequired,
        )
        .with_exact_provider_content_provenance();
        assert_eq!(
            crush_route_error(provider.into()).kind,
            SourceBackedRouteErrorKind::InvalidSource
        );

        let private = SqliteSourceAccessError::SqliteControl {
            operation: "querying a damaged ctx-owned Crush copy",
            code: ffi::SQLITE_CORRUPT,
        }
        .with_diagnostic(
            SqliteFailurePhase::Projection,
            SqliteArtifactKind::PrivateSourceCopy,
            4,
            16_384,
            SqliteCleanupStatus::NotRequired,
        );
        assert_eq!(
            crush_route_error(private.into()).kind,
            SourceBackedRouteErrorKind::Internal
        );

        #[cfg(unix)]
        assert_eq!(
            crush_route_error(CrushSourceBackedErrorV0::Io(
                std::io::Error::from_raw_os_error(libc::EIO),
            ))
            .kind,
            SourceBackedRouteErrorKind::Internal
        );
    }
}
