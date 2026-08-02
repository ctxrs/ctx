use super::super::*;

pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

/// Three independently committed complete inventories bound transient
/// automatic-source absence while preserving prompt eventual deletion.
pub const AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES: u32 = 3;
pub const MAX_RECORDED_SOURCE_BACKED_FAILURES: usize = 64;
pub const MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES: usize = 512;
pub const MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRefreshOutcome {
    Completed,
    CompletedWithSourceFailures,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedSourceFailureClass {
    Unavailable,
    SourceChanged,
    Unreadable,
    Incompatible,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedSourceFailure {
    pub source_identity: String,
    pub provider: CaptureProvider,
    pub class: SourceBackedSourceFailureClass,
    pub carried_forward: bool,
    pub source_selector: String,
    pub detail: String,
}

impl SourceBackedSourceFailure {
    pub(in super::super) fn from_route(
        route: &SourceBackedRoute,
        class: SourceBackedSourceFailureClass,
        carried_forward: bool,
        detail: impl AsRef<str>,
    ) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"ctx.source-failure-identity-v1\0");
        digest.update(route.metadata.source.provider.as_str().as_bytes());
        digest.update([0]);
        digest.update(route.metadata.certified_source_format.as_bytes());
        digest.update([0]);
        let path = route.metadata.source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Self {
            source_identity: format!("{:x}", digest.finalize()),
            provider: route.metadata.source.provider,
            class,
            carried_forward,
            source_selector: bounded_text(
                &route.metadata.source.path.display().to_string(),
                MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES,
            ),
            detail: bounded_text(detail.as_ref(), MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES),
        }
    }
}

fn bounded_text(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value[..boundary].to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceBackedSourceFailures {
    failures: Vec<SourceBackedSourceFailure>,
    omitted: usize,
}

impl SourceBackedSourceFailures {
    pub fn failures(&self) -> &[SourceBackedSourceFailure] {
        &self.failures
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn total(&self) -> usize {
        self.failures.len().saturating_add(self.omitted)
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub(in super::super) fn record(&mut self, failure: SourceBackedSourceFailure) {
        if self.failures.len() < MAX_RECORDED_SOURCE_BACKED_FAILURES {
            self.failures.push(failure);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedDeletionDisposition {
    Deferred,
    Deleted,
}

/// Runtime metadata for one selected source route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedRouteMetadata {
    pub source: ProviderSource,
    pub certified_source_format: &'static str,
    pub selection: Option<SourceBackedRouteSelection>,
    pub selector_authority: SourceBackedSelectorAuthority,
    pub unsupported_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedRouteErrorKind {
    Unavailable,
    SourceChanged,
    InvalidSource,
    Unsupported,
    Internal,
}

impl SourceBackedRouteErrorKind {
    pub fn source_failure_class(self) -> Option<SourceBackedSourceFailureClass> {
        match self {
            Self::Unavailable => Some(SourceBackedSourceFailureClass::Unavailable),
            Self::SourceChanged => Some(SourceBackedSourceFailureClass::SourceChanged),
            Self::InvalidSource => Some(SourceBackedSourceFailureClass::Unreadable),
            Self::Unsupported => Some(SourceBackedSourceFailureClass::Incompatible),
            Self::Internal => None,
        }
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
#[error("{kind:?}: {detail}")]
pub struct SourceBackedRouteError {
    pub kind: SourceBackedRouteErrorKind,
    pub detail: String,
}

impl SourceBackedRouteError {
    pub fn new(kind: SourceBackedRouteErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SourceBackedCoordinatorError {
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error("invalid source-backed route for {provider}: {detail}")]
    InvalidRoute {
        provider: CaptureProvider,
        detail: String,
    },
    #[error("source-backed scan failed for {provider}: {source}")]
    RouteScan {
        provider: CaptureProvider,
        #[source]
        source: SourceBackedRouteError,
    },
    #[error("source {source_id} was staged by more than one provider route")]
    DuplicateSourceOwner { source_id: String },
    #[error("base source {source_id} was not claimed by any provider route in this refresh")]
    UnclaimedBaseSource { source_id: String },
    #[error("source deletion was not certified by its supplied authoritative inventory")]
    InvalidDeletionWitness,
    #[error("retained source deletion {source_id} could not be recertified: {detail}")]
    RetainedDeletionRecertification { source_id: String, detail: String },
    #[error("source-backed refresh progress callback failed: {0}")]
    Progress(SourceBackedRouteError),
    #[error("source-backed refresh completed with source failures but no usable source route")]
    NoUsableSourceRoutes {
        failures: SourceBackedSourceFailures,
    },
}

/// The only write surface provider drivers receive. It exposes staging and
/// certification, but never generation commit.
pub struct SourceBackedGenerationSink<'writer> {
    pub(in super::super) writer: &'writer mut GenerationWriter,
    pub(in super::super) owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    pub(in super::super) complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
    pub(in super::super) route_index: usize,
    pub(in super::super) leaf_worker_budget: usize,
    pub(in super::super) automatic_missing_observed_at_unix_ms: Option<u64>,
    pub(in super::super) report_current_source_progress:
        &'writer mut (dyn FnMut(SourceBackedCurrentSourceProgress) -> SourceBackedRouteResult<()>
                          + 'writer),
}

#[derive(Clone)]
pub(in super::super) struct SourceOwner {
    pub(in super::super) route_index: usize,
    pub(in super::super) source: SourceKey,
}

#[derive(Clone)]
pub(in super::super) struct CompleteInventoryOwner {
    pub(in super::super) route_index: usize,
    pub(in super::super) inventory: CertifiedSourceInventory,
}

impl SourceBackedGenerationSink<'_> {
    pub fn report_current_source_progress(
        &mut self,
        progress: SourceBackedCurrentSourceProgress,
    ) -> SourceBackedRouteResult<()> {
        (self.report_current_source_progress)(progress).map_err(|error| {
            SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::Internal,
                format!("source-backed progress callback failed: {error}"),
            )
        })
    }

    pub fn base_source(&self, source: &SourceKey) -> Option<&CertifiedSource> {
        self.writer.base_manifest().and_then(|manifest| {
            manifest
                .sources
                .iter()
                .find(|candidate| candidate.observation().source().exact_descriptor_eq(source))
        })
    }

    pub fn begin_source(&mut self, source: SourceKey) -> SourceBackedCoordinatorResult<()> {
        self.claim(&source)?;
        self.writer.begin_source(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource> {
        self.claim(&source)?;
        Ok(self.writer.begin_source_append(source)?)
    }

    pub fn add_core_record(&mut self, record: CoreRecord) -> SourceBackedCoordinatorResult<()> {
        self.writer.add_core_record(record)?;
        Ok(())
    }

    pub fn certify_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_source(certificate)?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_source_append(append)?;
        Ok(())
    }

    pub fn retain_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim(certificate.observation().source())?;
        self.writer.retain_source(certificate)?;
        Ok(())
    }

    pub fn certify_complete_inventory(
        &mut self,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<()> {
        self.writer.certify_complete_inventory(inventory.clone())?;
        self.complete_inventories.push(CompleteInventoryOwner {
            route_index: self.route_index,
            inventory,
        });
        Ok(())
    }

    pub fn delete_source(
        &mut self,
        deletion: CertifiedSourceDeletion,
        inventory: CertifiedSourceInventory,
    ) -> SourceBackedCoordinatorResult<SourceBackedDeletionDisposition> {
        if !deletion.verifies(&inventory) {
            return Err(SourceBackedCoordinatorError::InvalidDeletionWitness);
        }
        self.claim(deletion.source())?;
        if let Some(observed_at_unix_ms) = self.automatic_missing_observed_at_unix_ms {
            let deleted = self.writer.observe_automatic_source_missing(
                deletion,
                inventory,
                observed_at_unix_ms,
                AUTOMATIC_SOURCE_DELETION_MISSING_INVENTORIES,
            )?;
            return Ok(if deleted {
                SourceBackedDeletionDisposition::Deleted
            } else {
                SourceBackedDeletionDisposition::Deferred
            });
        }
        self.writer.delete_source(deletion, inventory)?;
        Ok(SourceBackedDeletionDisposition::Deleted)
    }

    pub fn replace_source(
        &mut self,
        certificate: CertifiedSource,
        core_records: impl IntoIterator<Item = CoreRecord>,
    ) -> SourceBackedCoordinatorResult<()> {
        self.begin_source(certificate.observation().source().clone())?;
        for record in core_records {
            self.add_core_record(record)?;
        }
        self.certify_source(certificate)
    }

    pub(in super::super) fn claim(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<()> {
        let digest = source.identity().digest();
        match self.owners.get(&digest) {
            Some(owner)
                if owner.route_index != self.route_index
                    || !owner.source.exact_descriptor_eq(source) =>
            {
                return Err(SourceBackedCoordinatorError::DuplicateSourceOwner {
                    source_id: source.identity().to_string(),
                });
            }
            Some(_) => {}
            None => {
                self.owners.insert(
                    digest,
                    SourceOwner {
                        route_index: self.route_index,
                        source: source.clone(),
                    },
                );
            }
        }
        Ok(())
    }
}

pub enum SourceBackedRevalidationTarget<'a> {
    Source(&'a CertifiedSource),
    Deletion(&'a CertifiedSourceDeletion),
}

type ScanCallback = dyn for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
    + Send
    + Sync;
pub(super) type SourcePredicate = dyn Fn(&SourceKey) -> bool + Send + Sync;
type RevalidationCallback =
    dyn for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync;
type CompleteInventoryRevalidationCallback =
    dyn Fn(&CertifiedSourceInventory) -> bool + Send + Sync;
type SuccessfulPublicationCallback = dyn Fn() + Send + Sync;

/// Closure bundle at the coordinator boundary. This deliberately does not
/// pretend provider scanners share a provider-local trait.
#[derive(Clone)]
pub struct SourceBackedRouteDriver {
    pub(in super::super) scan: Arc<ScanCallback>,
    pub(in super::super) owns_source: Arc<SourcePredicate>,
    pub(in super::super) revalidate: Arc<RevalidationCallback>,
    pub(in super::super) revalidate_complete_inventory:
        Option<Arc<CompleteInventoryRevalidationCallback>>,
    pub(in super::super) after_successful_publication: Option<Arc<SuccessfulPublicationCallback>>,
}

impl fmt::Debug for SourceBackedRouteDriver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceBackedRouteDriver")
    }
}

impl SourceBackedRouteDriver {
    pub fn new(
        scan: impl for<'writer> Fn(&mut SourceBackedGenerationSink<'writer>) -> SourceBackedRouteResult<()>
            + Send
            + Sync
            + 'static,
        owns_source: impl Fn(&SourceKey) -> bool + Send + Sync + 'static,
        revalidate: impl for<'a> Fn(SourceBackedRevalidationTarget<'a>) -> bool + Send + Sync + 'static,
    ) -> Self {
        Self {
            scan: Arc::new(scan),
            owns_source: Arc::new(owns_source),
            revalidate: Arc::new(revalidate),
            revalidate_complete_inventory: None,
            after_successful_publication: None,
        }
    }

    pub fn with_complete_inventory_revalidation(
        mut self,
        revalidate: impl Fn(&CertifiedSourceInventory) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.revalidate_complete_inventory = Some(Arc::new(revalidate));
        self
    }

    /// Installs best-effort work that may run only after atomic publication.
    ///
    /// The callback cannot affect the committed generation and must suppress
    /// its own cache or observation failures.
    pub(crate) fn with_successful_publication(
        mut self,
        after_publication: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.after_successful_publication = Some(Arc::new(after_publication));
        self
    }
}

#[derive(Debug, Clone)]
pub struct SourceBackedRoute {
    pub(in super::super) metadata: SourceBackedRouteMetadata,
    pub(in super::super) driver: Option<SourceBackedRouteDriver>,
}

impl SourceBackedRoute {
    pub fn automatic(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
            },
            driver: Some(driver),
        })
    }

    pub fn explicit_manual(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
        driver: SourceBackedRouteDriver,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::ExplicitManual,
            selector_authority,
        )?;
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::ExplicitManual),
                selector_authority,
                unsupported_reason: None,
            },
            driver: Some(driver),
        })
    }

    pub fn unsupported(source: ProviderSource, reason: impl Into<String>) -> Self {
        let certified_source_format = landed_format_route(source.provider, source.source_format)
            .map_or(source.source_format, |route| route.certified_source_format);
        Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format,
                selection: None,
                selector_authority: SourceBackedSelectorAuthority::ExplicitPath,
                unsupported_reason: Some(reason.into()),
            },
            driver: None,
        }
    }

    pub fn metadata(&self) -> &SourceBackedRouteMetadata {
        &self.metadata
    }
}

#[derive(Debug, Clone, Default)]
pub struct SourceBackedProviderRegistry {
    pub(in super::super) routes: Vec<SourceBackedRoute>,
}

impl SourceBackedProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, route: SourceBackedRoute) {
        self.routes.push(route);
    }

    pub fn routes(&self) -> impl ExactSizeIterator<Item = &SourceBackedRouteMetadata> {
        self.routes.iter().map(SourceBackedRoute::metadata)
    }

    pub fn executable_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_some())
            .count()
    }

    pub fn unsupported_route_count(&self) -> usize {
        self.routes
            .iter()
            .filter(|route| route.driver.is_none())
            .count()
    }
}
