use super::super::*;

pub const MAX_RECORDED_SOURCE_BACKED_FAILURES: usize = 64;
pub const MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES: usize = 512;
pub const MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in super::super) struct SourceBackedRecordProgressDelta {
    pub(in super::super) accepted_records: u64,
    pub(in super::super) completed_bytes: u64,
}

pub type SourceBackedCoordinatorResult<T> = Result<T, SourceBackedCoordinatorError>;
pub type SourceBackedRouteResult<T> = Result<T, SourceBackedRouteError>;

/// Three independently committed certified whole-route absences bound grace.
pub const AUTOMATIC_ROUTE_DELETION_MISSING_OBSERVATIONS: u32 =
    ctx_history_index::policy::AUTOMATIC_ROUTE_DELETION_GRACE_OBSERVATIONS;

/// Selects the provider routes incorporated into one global generation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SourceBackedRefreshScope {
    #[default]
    All,
    Exact(BTreeSet<SourceRouteIdentity>),
}

impl SourceBackedRefreshScope {
    pub fn exact(route_identities: impl IntoIterator<Item = SourceRouteIdentity>) -> Self {
        Self::Exact(route_identities.into_iter().collect())
    }
}

/// Narrow source-authority failures that may be isolated to one route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceBackedSourceFailureClass {
    Unavailable,
    SourceChanged,
    Unreadable,
    Incompatible,
}

impl SourceBackedSourceFailureClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::SourceChanged => "source_changed",
            Self::Unreadable => "unreadable",
            Self::Incompatible => "incompatible",
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Unavailable => 0,
            Self::SourceChanged => 1,
            Self::Unreadable => 2,
            Self::Incompatible => 3,
        }
    }
}

#[cfg(test)]
mod bounded_failure_tests {
    use super::*;

    #[test]
    fn bounded_diagnostics_retain_exact_aggregate_class_totals() {
        let failures = (0..70).map(|index| {
            SourceBackedFailedRoute::new(
                SourceRouteIdentity::from_sha256(format!("{index:064x}")).unwrap(),
                format!("source-{index}"),
                CaptureProvider::OpenCode,
                if index % 2 == 0 {
                    SourceBackedSourceFailureClass::Unreadable
                } else {
                    SourceBackedSourceFailureClass::Incompatible
                },
                false,
                format!("selector-{index}"),
                format!("detail-{index}"),
            )
        });
        let bounded = SourceBackedSourceFailures::from_failures(failures);

        assert_eq!(
            bounded.failures().len(),
            MAX_RECORDED_SOURCE_BACKED_FAILURES
        );
        assert_eq!(bounded.omitted(), 6);
        assert_eq!(bounded.total(), 70);
        assert_eq!(
            bounded.class_total(SourceBackedSourceFailureClass::Unreadable),
            35
        );
        assert_eq!(
            bounded.class_total(SourceBackedSourceFailureClass::Incompatible),
            35
        );
    }
}

/// Content-free identity and class for one route-local failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedFailedRoute {
    pub route_identity: SourceRouteIdentity,
    pub source_identity: String,
    pub provider: CaptureProvider,
    pub class: SourceBackedSourceFailureClass,
    pub carried_forward: bool,
    pub source_selector: String,
    pub detail: String,
}

/// Compact lifecycle result retained for every failed route. Human-readable
/// selector/detail diagnostics live only in [`SourceBackedSourceFailures`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceBackedFailedRouteOutcome {
    pub route_identity: SourceRouteIdentity,
    pub source_identity: String,
    pub provider: CaptureProvider,
    pub class: SourceBackedSourceFailureClass,
    pub carried_forward: bool,
}

impl From<&SourceBackedFailedRoute> for SourceBackedFailedRouteOutcome {
    fn from(failure: &SourceBackedFailedRoute) -> Self {
        Self {
            route_identity: failure.route_identity.clone(),
            source_identity: failure.source_identity.clone(),
            provider: failure.provider,
            class: failure.class,
            carried_forward: failure.carried_forward,
        }
    }
}

impl SourceBackedFailedRoute {
    pub fn new(
        route_identity: SourceRouteIdentity,
        source_identity: String,
        provider: CaptureProvider,
        class: SourceBackedSourceFailureClass,
        carried_forward: bool,
        source_selector: impl AsRef<str>,
        detail: impl AsRef<str>,
    ) -> Self {
        Self {
            route_identity,
            source_identity,
            provider,
            class,
            carried_forward,
            source_selector: bounded_text(
                source_selector.as_ref(),
                MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES,
            ),
            detail: bounded_text(detail.as_ref(), MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES),
        }
    }

    pub(in super::super) fn from_route(
        route: &SourceBackedRoute,
        class: SourceBackedSourceFailureClass,
        carried_forward: bool,
        detail: impl AsRef<str>,
    ) -> SourceBackedCoordinatorResult<Self> {
        let route_identity = route.metadata.route_identity.clone().ok_or_else(|| {
            SourceBackedCoordinatorError::InvalidRoute {
                provider: route.metadata.source.provider,
                detail: "failed executable route has no route identity".to_owned(),
            }
        })?;
        let mut digest = Sha256::new();
        digest.update(b"ctx.source-failure-identity-v1\0");
        digest.update(route.metadata.source.provider.as_str().as_bytes());
        digest.update([0]);
        digest.update(route.metadata.certified_source_format.as_bytes());
        digest.update([0]);
        let path = route.metadata.source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
        Ok(Self::new(
            route_identity,
            format!("{:x}", digest.finalize()),
            route.metadata.source.provider,
            class,
            carried_forward,
            route.metadata.source.path.display().to_string(),
            detail,
        ))
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
    failures: Vec<SourceBackedFailedRoute>,
    omitted: usize,
    class_totals: [usize; 4],
}

impl SourceBackedSourceFailures {
    pub fn from_failures(failures: impl IntoIterator<Item = SourceBackedFailedRoute>) -> Self {
        let mut bounded = Self::default();
        for failure in failures {
            bounded.record(failure);
        }
        bounded
    }

    pub fn failures(&self) -> &[SourceBackedFailedRoute] {
        &self.failures
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn total(&self) -> usize {
        self.failures.len().saturating_add(self.omitted)
    }

    pub fn class_total(&self, class: SourceBackedSourceFailureClass) -> usize {
        self.class_totals[class.index()]
    }

    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    pub(in super::super) fn record(&mut self, failure: SourceBackedFailedRoute) {
        self.class_totals[failure.class.index()] =
            self.class_totals[failure.class.index()].saturating_add(1);
        if self.failures.len() < MAX_RECORDED_SOURCE_BACKED_FAILURES {
            self.failures.push(failure);
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    pub fn extend(&mut self, failures: impl IntoIterator<Item = SourceBackedFailedRoute>) {
        for failure in failures {
            self.record(failure);
        }
    }
}

impl std::ops::Deref for SourceBackedSourceFailures {
    type Target = [SourceBackedFailedRoute];

    fn deref(&self) -> &Self::Target {
        self.failures()
    }
}

impl fmt::Display for SourceBackedSourceFailures {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const MAX_DISPLAYED_FAILURES: usize = 3;

        for (index, failure) in self
            .failures
            .iter()
            .take(MAX_DISPLAYED_FAILURES)
            .enumerate()
        {
            if index != 0 {
                formatter.write_str("; ")?;
            }
            write!(
                formatter,
                "source-backed scan failed for {} at {}: {}: {}",
                failure.provider.as_str(),
                failure.source_selector,
                failure.class.display_label(),
                failure.detail,
            )?;
        }
        let undisplayed = self
            .total()
            .saturating_sub(self.failures.len().min(MAX_DISPLAYED_FAILURES));
        if undisplayed != 0 {
            write!(
                formatter,
                "; {undisplayed} additional source failure(s) omitted"
            )?;
        }
        Ok(())
    }
}

impl SourceBackedSourceFailureClass {
    fn display_label(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable (provider source unavailable)",
            Self::SourceChanged => {
                "source_changed (provider source changed during bounded capture)"
            }
            Self::Unreadable => "invalid_source (invalid capture payload)",
            Self::Incompatible => "unsupported (unsupported provider schema)",
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
    pub route_identity: Option<SourceRouteIdentity>,
    pub watch_target_kind: SourceBackedWatchTargetKind,
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
    #[error("source-backed refresh has an unknown or unavailable route for {provider}: {detail}")]
    UnavailableRoute {
        provider: CaptureProvider,
        detail: String,
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
    #[error("selected source-backed route {route_id} is unknown or not executable")]
    InvalidRefreshScope { route_id: String },
    #[error(
        "source-backed refresh completed with source failures but retained no usable source: {failed_routes}"
    )]
    NoUsableSourceRoutes {
        failed_routes: SourceBackedSourceFailures,
    },
}

/// The only write surface provider drivers receive. It exposes staging and
/// certification, but never generation commit.
pub struct SourceBackedGenerationSink<'writer> {
    pub(in super::super) writer: &'writer mut GenerationWriter,
    pub(in super::super) owners: &'writer mut HashMap<[u8; 32], SourceOwner>,
    pub(in super::super) complete_inventories: &'writer mut Vec<CompleteInventoryOwner>,
    pub(in super::super) applied_removals: &'writer mut Vec<SourceBackedCertifiedRemoval>,
    pub(in super::super) route_index: usize,
    pub(in super::super) leaf_worker_budget: usize,
    pub(in super::super) record_progress: Option<
        &'writer mut dyn FnMut(
            SourceBackedRecordProgressDelta,
        ) -> SourceBackedCoordinatorResult<()>,
    >,
    pub(in super::super) current_source_progress: Option<
        &'writer mut dyn FnMut(SourceBackedCurrentSourceProgress) -> SourceBackedRouteResult<()>,
    >,
}

#[derive(Clone)]
pub(in super::super) struct SourceOwner {
    pub(in super::super) route_index: usize,
    pub(in super::super) source: SourceKey,
    pub(in super::super) present: bool,
    pub(in super::super) revalidation: Option<SourceBackedRouteRevalidation>,
}

#[derive(Clone)]
pub(in super::super) enum SourceBackedRouteRevalidation {
    Source(CertifiedSource),
    Deletion(CertifiedSourceDeletion),
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
        self.current_source_progress
            .as_mut()
            .map_or(Ok(()), |report| report(progress))
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
        self.claim_present(&source)?;
        self.writer.begin_source(source)?;
        Ok(())
    }

    pub fn begin_source_append(
        &mut self,
        source: SourceKey,
    ) -> SourceBackedCoordinatorResult<&CertifiedSource> {
        self.claim_present(&source)?;
        Ok(self.writer.begin_source_append(source)?)
    }

    pub fn add_core_record(&mut self, record: CoreRecord) -> SourceBackedCoordinatorResult<()> {
        self.writer.add_core_record(record)?;
        if let Some(report_progress) = self.record_progress.as_mut() {
            report_progress(SourceBackedRecordProgressDelta {
                accepted_records: 1,
                completed_bytes: 0,
            })?;
        }
        Ok(())
    }

    pub fn report_completed_bytes(&mut self, bytes: u64) -> SourceBackedCoordinatorResult<()> {
        if let Some(report_progress) = self.record_progress.as_mut() {
            report_progress(SourceBackedRecordProgressDelta {
                accepted_records: 0,
                completed_bytes: bytes,
            })?;
        }
        Ok(())
    }

    pub fn certify_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        let source = certificate.observation().source().clone();
        self.writer.certify_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn certify_source_append(
        &mut self,
        append: CertifiedSourceAppend,
    ) -> SourceBackedCoordinatorResult<()> {
        let certificate = append.current().clone();
        let source = certificate.observation().source().clone();
        self.writer.certify_source_append(append)?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
        Ok(())
    }

    pub fn retain_source(
        &mut self,
        certificate: CertifiedSource,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim_present(certificate.observation().source())?;
        let source = certificate.observation().source().clone();
        self.writer.retain_source(certificate.clone())?;
        self.record_revalidation(&source, SourceBackedRouteRevalidation::Source(certificate))?;
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
        self.claim_absent(deletion.source())?;
        self.writer
            .delete_source(deletion.clone(), inventory.clone())?;
        self.record_revalidation(
            deletion.source(),
            SourceBackedRouteRevalidation::Deletion(deletion.clone()),
        )?;
        self.applied_removals.push(SourceBackedCertifiedRemoval {
            deletion,
            inventory,
        });
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

    pub(in super::super) fn claim_present(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim(source, true)
    }

    pub(in super::super) fn claim_absent(
        &mut self,
        source: &SourceKey,
    ) -> SourceBackedCoordinatorResult<()> {
        self.claim(source, false)
    }

    fn claim(&mut self, source: &SourceKey, present: bool) -> SourceBackedCoordinatorResult<()> {
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
            Some(owner) if owner.present != present => {
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
                        present,
                        revalidation: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn record_revalidation(
        &mut self,
        source: &SourceKey,
        revalidation: SourceBackedRouteRevalidation,
    ) -> SourceBackedCoordinatorResult<()> {
        let owner = self
            .owners
            .get_mut(&source.identity().digest())
            .filter(|owner| {
                owner.route_index == self.route_index
                    && owner.source.exact_descriptor_eq(source)
                    && owner.revalidation.is_none()
            })
            .ok_or(IndexError::WriterInvariant(
                "source certification lost its route-local owner",
            ))?;
        owner.revalidation = Some(revalidation);
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
    pub(in super::super) certified_missing_paths: Vec<PathBuf>,
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
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
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
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
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
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
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
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: Some(driver),
            certified_missing_paths: Vec::new(),
        })
    }

    pub fn certified_missing(
        source: ProviderSource,
        selector_authority: SourceBackedSelectorAuthority,
    ) -> SourceBackedCoordinatorResult<Self> {
        let known = validate_executable_route(
            &source,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let route_identity = source_backed_route_identity(
            &source,
            known.certified_source_format,
            SourceBackedRouteSelection::Automatic,
            selector_authority,
        )?;
        let path = source.path.clone();
        Ok(Self {
            metadata: SourceBackedRouteMetadata {
                source,
                certified_source_format: known.certified_source_format,
                selection: Some(SourceBackedRouteSelection::Automatic),
                selector_authority,
                unsupported_reason: None,
                route_identity: Some(route_identity),
                watch_target_kind: known.watch_target_kind,
            },
            driver: None,
            certified_missing_paths: vec![path],
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
                route_identity: None,
                watch_target_kind: SourceBackedWatchTargetKind::Path,
            },
            driver: None,
            certified_missing_paths: Vec::new(),
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
        if let Some(identity) = route.metadata.route_identity.as_ref() {
            if let Some(existing) = self
                .routes
                .iter_mut()
                .find(|existing| existing.metadata.route_identity.as_ref() == Some(identity))
            {
                if existing.driver.is_some() {
                    return;
                }
                if route.driver.is_some() {
                    *existing = route;
                    return;
                }
                existing
                    .certified_missing_paths
                    .extend(route.certified_missing_paths);
                existing.certified_missing_paths.sort();
                existing.certified_missing_paths.dedup();
                return;
            }
        }
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
            .filter(|route| route.certified_missing_paths.is_empty())
            .count()
    }
}

fn source_backed_route_identity(
    source: &ProviderSource,
    certified_source_format: &str,
    selection: SourceBackedRouteSelection,
    selector_authority: SourceBackedSelectorAuthority,
) -> SourceBackedCoordinatorResult<SourceRouteIdentity> {
    let mut digest = Sha256::new();
    digest.update(b"ctx.source-route-identity-v1\0");
    digest.update(source.provider.as_str().as_bytes());
    digest.update([0]);
    digest.update(certified_source_format.as_bytes());
    digest.update([0]);
    digest.update(match selection {
        SourceBackedRouteSelection::Automatic => b"automatic".as_slice(),
        SourceBackedRouteSelection::ExplicitManual => b"explicit".as_slice(),
    });
    digest.update([0]);
    digest.update(match selector_authority {
        SourceBackedSelectorAuthority::DiscoveredWinner => b"discovered-winner".as_slice(),
        SourceBackedSelectorAuthority::ExplicitPath => b"explicit-path".as_slice(),
        SourceBackedSelectorAuthority::CatalogLineage => b"catalog-lineage".as_slice(),
        SourceBackedSelectorAuthority::ExactCwd => b"exact-cwd".as_slice(),
        SourceBackedSelectorAuthority::NamedSurface => b"named-surface".as_slice(),
        SourceBackedSelectorAuthority::SelectedWithRetainedExplicit => {
            b"selected-with-retained-explicit".as_slice()
        }
    });
    if selection == SourceBackedRouteSelection::ExplicitManual {
        let path = source.path.as_os_str().as_encoded_bytes();
        digest.update((path.len() as u64).to_be_bytes());
        digest.update(path);
    }
    SourceRouteIdentity::from_sha256(format!("{:x}", digest.finalize())).map_err(Into::into)
}

#[cfg(test)]
mod failure_tests {
    use super::*;

    #[test]
    fn source_failure_diagnostics_bound_count_selector_detail_and_display() {
        let failures = SourceBackedSourceFailures::from_failures((0_u8..70).map(|index| {
            SourceBackedFailedRoute::new(
                SourceRouteIdentity::from_sha256(format!("{index:02x}").repeat(32)).unwrap(),
                format!("{:02x}", index.saturating_add(1)).repeat(32),
                CaptureProvider::Codex,
                SourceBackedSourceFailureClass::Unavailable,
                false,
                "é".repeat(MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES),
                "δ".repeat(MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES),
            )
        }));

        assert_eq!(
            failures.failures().len(),
            MAX_RECORDED_SOURCE_BACKED_FAILURES
        );
        assert_eq!(failures.omitted(), 6);
        assert_eq!(failures.total(), 70);
        assert!(failures.failures().iter().all(|failure| {
            failure.source_selector.len() <= MAX_SOURCE_BACKED_FAILURE_SELECTOR_BYTES
                && failure.detail.len() <= MAX_SOURCE_BACKED_FAILURE_DETAIL_BYTES
                && failure
                    .source_selector
                    .is_char_boundary(failure.source_selector.len())
                && failure.detail.is_char_boundary(failure.detail.len())
        }));
        let displayed = failures.to_string();
        assert_eq!(displayed.matches("source-backed scan failed").count(), 3);
        assert!(displayed.contains("67 additional source failure(s) omitted"));
    }
}
