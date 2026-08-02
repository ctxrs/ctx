use std::{path::PathBuf, sync::Arc};

use crate::provider::providers::crush::native_path::source_backed::BoundDatabase;

#[cfg(test)]
use crate::provider_sources::SqliteSourceAccessError;

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
        let certificate = scan_crush_source(&source, sink).map_err(crush_route_error)?;
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
    let kind = match &error {
        CrushSourceBackedErrorV0::SqliteSourceChanged => SourceBackedRouteErrorKind::SourceChanged,
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crush_sqlite_source_change_remains_typed() {
        let error = crush_route_error(SqliteSourceAccessError::SourceChanged.into());
        assert_eq!(error.kind, SourceBackedRouteErrorKind::SourceChanged);
    }
}
