use super::*;

pub fn astrbot_released_registration_scoped<L, S>(
    source: ProviderSource,
    identity_source: ProviderSource,
    identity_home: &Path,
    data_root: &Path,
    source_scope: SourceAnchorScope,
) -> SourceBackedRouteResult<
    SqliteInventoryRegistration<
        impl ReplacementDocumentTree<
            Lifecycle = L,
            Spool = S,
            RouteControl = crate::ProviderRouteControlExpectation,
        >,
    >,
>
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    let watch_primary = source.path.clone();
    let inventory = AstrBotSourceBackedInventoryV0::released_scoped(
        identity_home,
        &identity_source,
        &source.path,
        source_scope,
    )
    .map_err(astrbot_inventory_route_error)?;
    let adapter = SqliteInventoryDocumentAdapter::new(
        data_root,
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        AstrBotReleasedInventoryProvider { inventory },
    );
    Ok(SqliteInventoryRegistration::new(
        source,
        SourceBackedRouteSelection::Automatic,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        adapter,
        Some(Box::new(move || {
            Some(sqlite_inventory_watch_targets([watch_primary.as_path()]))
        })),
    ))
}

pub struct AstrBotReleasedInventoryProvider {
    inventory: AstrBotSourceBackedInventoryV0,
}

impl<L, S> SqliteInventoryProvider<L, S> for AstrBotReleasedInventoryProvider
where
    L: CaptureLifecycleSink + 'static,
    S: DocumentRecordSpool,
{
    type Leaf = AstrBotSourceBackedSourceV0;

    fn parser_revision(&self) -> &'static str {
        ASTRBOT_SOURCE_BACKED_PARSER_REVISION
    }

    fn discover(&self) -> SourceBackedRouteResult<SqliteInventoryCatalog<Self::Leaf>> {
        let authority_fingerprint =
            sqlite_inventory_authority_fingerprint(self.inventory.observation())?;
        let leaves = self
            .inventory
            .sources()
            .iter()
            .cloned()
            .map(|leaf| SqliteInventoryCatalogLeaf {
                source: leaf.source_key().clone(),
                physical_locator: leaf.path().to_path_buf(),
                provider_leaf: leaf,
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
        scan_astrbot_inventory_leaf(leaf, snapshot, sink)
    }
}
