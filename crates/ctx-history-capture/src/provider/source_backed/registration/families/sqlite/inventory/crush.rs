use std::{path::PathBuf, sync::Arc};

use crate::provider::providers::crush::native_path::source_backed::BoundDatabase;

#[cfg(test)]
use super::shared::SqliteInventorySnapshotCounters;
use super::*;

struct CrushInventoryProvider<I> {
    data_root: PathBuf,
    inventory: Arc<I>,
}

impl<I> SqliteInventoryProvider for CrushInventoryProvider<I>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    type Leaf = BoundDatabase;

    fn parser_revision(&self) -> &'static str {
        CRUSH_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = bind_crush_inventory(
            &self.data_root,
            self.inventory.observe().map_err(crush_route_error)?,
        )
        .map_err(crush_route_error)?;
        let authority_fingerprint = sqlite_inventory_authority_fingerprint(&inventory.observation)?;
        let leaves = inventory
            .databases
            .into_iter()
            .map(|database| SqliteInventoryCatalogLeaf {
                source: database.source_key.clone(),
                path: database.canonical_path.clone(),
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
        sink: &mut ChangedDocumentSink<'_, '_>,
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
            if crate::provider_sources::rusqlite_resource_failure(error)
                || crate::provider_sources::rusqlite_busy_or_locked(error) =>
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
pub fn register_crush_source_backed_route<I>(
    registry: &mut SourceBackedProviderRegistry,
    source: ProviderSource,
    selection: SourceBackedRouteSelection,
    data_root: &Path,
    inventory: Arc<I>,
) -> SourceBackedCoordinatorResult<()>
where
    I: CrushProjectInventorySourceV0 + Send + Sync + 'static,
{
    let watch_source = source.clone();
    let watch_inventory = Arc::clone(&inventory);
    SqliteInventoryDocumentAdapter::register_replacement_document_tree_route_with_authority(
        registry,
        source,
        selection,
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit,
        data_root,
        CaptureProvider::Crush,
        "crush_sqlite",
        CrushInventoryProvider {
            data_root: data_root.to_path_buf(),
            inventory,
        },
    )?;
    registry.attach_route_watch_targets(&watch_source, move || {
        let inventory = watch_inventory.observe().ok()?;
        Some(sqlite_inventory_watch_targets(
            inventory
                .databases()
                .iter()
                .map(CrushProjectDatabaseV0::path),
        ))
    })
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
        for code in [
            ffi::SQLITE_BUSY,
            ffi::SQLITE_LOCKED,
            ffi::SQLITE_FULL,
            ffi::SQLITE_NOMEM,
        ] {
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
