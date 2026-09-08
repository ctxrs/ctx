use super::*;

pub enum SourceBackedRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

pub type SourceBackedScanCallback<L> = dyn for<'writer> Fn(&mut SourceBackedGenerationSink<'writer, L>) -> SourceBackedRouteResult<()>
    + Send
    + Sync;
pub type SourcePredicate = dyn Fn(&SourceKey) -> SourceBackedRouteResult<bool> + Send + Sync;
pub type RevalidationCallback = dyn for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> SourceBackedRouteResult<bool>
    + Send
    + Sync;
pub type CompleteInventoryRevalidationCallback =
    dyn Fn(&CertifiedSourceInventory) -> SourceBackedRouteResult<bool> + Send + Sync;
pub type RoutePublicationRevalidationCallback = dyn Fn() -> bool + Send + Sync;
pub type RoutePublicationControlCallback =
    dyn Fn() -> SourceBackedRouteResult<Option<Vec<u8>>> + Send + Sync;
pub type WatchTargetsCallback = dyn Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync;

#[derive(Debug, Clone, Default)]
pub struct SourceBackedRouteWatchTargets {
    pub sqlite_databases: BTreeSet<PathBuf>,
    pub authority_paths: BTreeSet<PathBuf>,
}

/// Closure bundle at the coordinator boundary. This deliberately does not
/// pretend provider scanners share a provider-local trait.
pub struct SourceBackedRouteDriver<L: CaptureLifecycleSink, C> {
    pub scan: Arc<SourceBackedScanCallback<L>>,
    pub owns_source: Arc<SourcePredicate>,
    pub revalidate: Arc<RevalidationCallback>,
    pub revalidate_complete_inventory: Option<Arc<CompleteInventoryRevalidationCallback>>,
    pub revalidate_at_publication: Option<Arc<RoutePublicationRevalidationCallback>>,
    pub publication_control: Option<Arc<RoutePublicationControlCallback>>,
    pub watch_targets: Option<Arc<WatchTargetsCallback>>,
    pub route_control_expectation: Option<C>,
    pub uses_parallel_leaf_workers: bool,
}

impl<L: CaptureLifecycleSink, C: Clone> Clone for SourceBackedRouteDriver<L, C> {
    fn clone(&self) -> Self {
        Self {
            scan: Arc::clone(&self.scan),
            owns_source: Arc::clone(&self.owns_source),
            revalidate: Arc::clone(&self.revalidate),
            revalidate_complete_inventory: self.revalidate_complete_inventory.clone(),
            revalidate_at_publication: self.revalidate_at_publication.clone(),
            publication_control: self.publication_control.clone(),
            watch_targets: self.watch_targets.clone(),
            route_control_expectation: self.route_control_expectation.clone(),
            uses_parallel_leaf_workers: self.uses_parallel_leaf_workers,
        }
    }
}

impl<L: CaptureLifecycleSink, C> fmt::Debug for SourceBackedRouteDriver<L, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceBackedRouteDriver")
    }
}

impl<L: CaptureLifecycleSink, C> SourceBackedRouteDriver<L, C> {
    pub fn new(
        scan: impl for<'writer> Fn(
                &mut SourceBackedGenerationSink<'writer, L>,
            ) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self::new_fallible(
            scan,
            move |source| Ok(owns_source(source)),
            move |target| Ok(revalidate(target)),
        )
    }

    pub fn new_fallible(
        scan: impl for<'writer> Fn(
                &mut SourceBackedGenerationSink<'writer, L>,
            ) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> SourceBackedRouteResult<bool> + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> SourceBackedRouteResult<bool>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        Self {
            scan: Arc::new(scan),
            owns_source: Arc::new(owns_source),
            revalidate: Arc::new(revalidate),
            revalidate_complete_inventory: None,
            revalidate_at_publication: None,
            publication_control: None,
            watch_targets: None,
            route_control_expectation: None,
            uses_parallel_leaf_workers: false,
        }
    }

    pub fn with_parallel_leaf_workers(mut self) -> Self {
        self.uses_parallel_leaf_workers = true;
        self
    }

    pub fn with_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_complete_inventory =
            Some(Arc::new(move |inventory| Ok(revalidate(inventory))));
        self
    }

    pub fn with_fallible_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> SourceBackedRouteResult<bool>
            + Send
            + Sync
            + 'static,
    ) -> Self {
        self.revalidate_complete_inventory = Some(Arc::new(revalidate));
        self
    }

    pub fn with_publication_revalidation(
        mut self,
        revalidate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_at_publication = Some(Arc::new(revalidate));
        self
    }

    pub fn with_publication_control(
        mut self,
        control: impl Fn() -> SourceBackedRouteResult<Option<Vec<u8>>> + Send + Sync + 'static,
    ) -> Self {
        self.publication_control = Some(Arc::new(control));
        self
    }

    pub fn with_route_control_expectation(mut self, expectation: C) -> Self {
        self.route_control_expectation = Some(expectation);
        self
    }

    pub fn scan(
        &self,
        sink: &mut SourceBackedGenerationSink<'_, L>,
    ) -> SourceBackedRouteResult<()> {
        (self.scan)(sink)
    }

    pub fn owns_source(&self, source: &SourceKey) -> SourceBackedRouteResult<bool> {
        (self.owns_source)(source)
    }

    pub fn revalidate(
        &self,
        target: SourceBackedRevalidationTarget<'_>,
    ) -> SourceBackedRouteResult<bool> {
        (self.revalidate)(target)
    }

    pub fn revalidate_complete_inventory(
        &self,
        inventory: &CertifiedSourceInventory,
    ) -> Option<SourceBackedRouteResult<bool>> {
        self.revalidate_complete_inventory
            .as_ref()
            .map(|revalidate| revalidate(inventory))
    }

    pub fn publication_revalidation(&self) -> Option<bool> {
        self.revalidate_at_publication
            .as_ref()
            .map(|revalidate| revalidate())
    }

    pub fn publication_control(&self) -> Option<SourceBackedRouteResult<Option<Vec<u8>>>> {
        self.publication_control.as_ref().map(|control| control())
    }

    pub fn watch_targets(&self) -> Option<SourceBackedRouteWatchTargets> {
        self.watch_targets.as_ref().and_then(|observe| observe())
    }

    pub fn route_control_expectation(&self) -> Option<&C> {
        self.route_control_expectation.as_ref()
    }

    pub fn uses_parallel_leaf_workers(&self) -> bool {
        self.uses_parallel_leaf_workers
    }

    pub fn scan_callback(&self) -> Arc<SourceBackedScanCallback<L>> {
        Arc::clone(&self.scan)
    }

    pub fn replace_scan_callback(&mut self, scan: Arc<SourceBackedScanCallback<L>>) {
        self.scan = scan;
    }

    pub fn revalidation_callback(&self) -> Arc<RevalidationCallback> {
        Arc::clone(&self.revalidate)
    }

    pub fn replace_revalidation_callback(&mut self, revalidate: Arc<RevalidationCallback>) {
        self.revalidate = revalidate;
    }

    pub fn replace_complete_inventory_revalidation_callback(
        &mut self,
        revalidate: Option<Arc<CompleteInventoryRevalidationCallback>>,
    ) {
        self.revalidate_complete_inventory = revalidate;
    }

    pub fn set_watch_targets(
        &mut self,
        observe: impl Fn() -> Option<SourceBackedRouteWatchTargets> + Send + Sync + 'static,
    ) {
        self.watch_targets = Some(Arc::new(observe));
    }
}
