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

    fn logical_tables(&self) -> &'static [&'static str] {
        &["sessions", "messages"]
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let inventory = bind_crush_inventory(
            &self.data_root,
            self.inventory.observe().map_err(route_error)?,
        )
        .map_err(route_error)?;
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
            .map_err(route_error)?;
        #[cfg(test)]
        self.inventory.record_projection_pass();
        let certificate = scan_crush_source(&source, sink).map_err(route_error)?;
        if !finish_crush_source(source).map_err(route_error)? {
            return Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "Crush source changed while its logical replacement was staged",
            ));
        }
        Ok(certificate)
    }

    fn hydrate(
        &self,
        request: &BatchHydrationRequest,
    ) -> Result<BatchHydrationResult, HydrationFailure> {
        let resolver = CrushLocatorResolverV0::discover(&self.data_root, self.inventory.as_ref())
            .map_err(|error| {
            hydration_failure(HydrationFailureKind::TemporarilyUnavailable, error)
        })?;
        let locators = request
            .events()
            .iter()
            .map(EventHydrationRequest::locator)
            .collect::<Vec<_>>();
        let hydrated = resolver
            .hydrate_locators(&locators)
            .map_err(|error| hydration_failure(HydrationFailureKind::StaleRecordEvidence, error))?;
        let records = request
            .events()
            .iter()
            .zip(hydrated)
            .map(|(event, hydrated)| {
                let provider_bytes = hydrated
                    .decoded_display_text
                    .ok_or_else(|| {
                        hydration_failure(
                            HydrationFailureKind::UnsupportedParserRevision,
                            "Crush record has no exact display text",
                        )
                    })?
                    .into_bytes();
                Ok(HydratedProviderRecord {
                    event_id: event.event_id(),
                    provider_bytes,
                })
            })
            .collect::<Result<Vec<_>, HydrationFailure>>()?;
        BatchHydrationResult::new(records)
            .map_err(|error| hydration_failure(HydrationFailureKind::InvalidLocator, error))
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
    let _legacy_compatibility = (
        crush_exact_replay_matches,
        closing_crush_observation,
        open_crush_source,
        CRUSH_DISCOVERY_REVISION,
        CRUSH_FRONTIER_KIND,
        CRUSH_SOURCE_SCHEMA_VARIANT,
    );
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
