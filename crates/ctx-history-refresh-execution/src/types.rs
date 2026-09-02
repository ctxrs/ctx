use super::*;
use ctx_history_capture::{
    SourceBackedCurrentSourceProgress as CaptureSourceBackedCurrentSourceProgress,
    SourceBackedReconciliationDemand, SourceBackedRefreshScope,
};

/// Maximum exact filesystem members admitted for one route before the route
/// conservatively falls back to exhaustive reconciliation.
pub const SOURCE_BACKED_REFRESH_MEMBER_LIMIT: usize = 256;

/// Provider-neutral physical work admitted for one selected route.
#[derive(Debug, Clone, Eq, PartialEq, Default)]
pub enum SourceBackedRefreshWorkset {
    #[default]
    Exhaustive,
    Members(BTreeSet<PathBuf>),
}

impl SourceBackedRefreshWorkset {
    pub fn members(members: impl IntoIterator<Item = PathBuf>) -> Self {
        let members = members.into_iter().collect::<BTreeSet<_>>();
        if members.is_empty() || members.len() > SOURCE_BACKED_REFRESH_MEMBER_LIMIT {
            Self::Exhaustive
        } else {
            Self::Members(members)
        }
    }

    pub fn merge(&mut self, other: Self) {
        match (self, other) {
            (Self::Exhaustive, _) => {}
            (current @ Self::Members(_), Self::Exhaustive) => *current = Self::Exhaustive,
            (current @ Self::Members(_), Self::Members(additional)) => {
                let Self::Members(members) = current else {
                    return;
                };
                members.extend(additional);
                if members.len() > SOURCE_BACKED_REFRESH_MEMBER_LIMIT {
                    *current = Self::Exhaustive;
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SourceBackedCurrentSourceProgressStage {
    SourceFamilyCopy,
    OnlineBackup,
    LogicalFingerprint,
    LogicalScan,
    Parsing,
    IndexWriting,
}

impl SourceBackedCurrentSourceProgressStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SourceFamilyCopy => "source_family_copy",
            Self::OnlineBackup => "online_backup",
            Self::LogicalFingerprint => "logical_fingerprint",
            Self::LogicalScan => "logical_scan",
            Self::Parsing => "parsing",
            Self::IndexWriting => "index_writing",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "source_family_copy" => Some(Self::SourceFamilyCopy),
            "online_backup" => Some(Self::OnlineBackup),
            "logical_fingerprint" => Some(Self::LogicalFingerprint),
            "logical_scan" => Some(Self::LogicalScan),
            "parsing" => Some(Self::Parsing),
            "index_writing" => Some(Self::IndexWriting),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SourceBackedCurrentSourceProgress {
    pub stage: SourceBackedCurrentSourceProgressStage,
    pub snapshot_pages_completed: Option<u64>,
    pub snapshot_pages_total: Option<u64>,
    pub snapshot_bytes_completed: Option<u64>,
    pub snapshot_bytes_total: Option<u64>,
    pub logical_rows_scanned: Option<u64>,
    pub logical_certified_bytes: Option<u64>,
}

impl SourceBackedCurrentSourceProgress {
    pub fn to_json(self) -> Value {
        let mut value = json!({
            "stage": self.stage.as_str(),
            "snapshot_pages_completed": self.snapshot_pages_completed,
            "snapshot_pages_total": self.snapshot_pages_total,
            "snapshot_bytes_completed": self.snapshot_bytes_completed,
            "snapshot_bytes_total": self.snapshot_bytes_total,
            "logical_rows_scanned": self.logical_rows_scanned,
            "logical_certified_bytes": self.logical_certified_bytes,
        });
        if let Value::Object(fields) = &mut value {
            fields.retain(|_, value| !value.is_null());
        }
        value
    }

    #[doc(hidden)]
    pub fn from_json(value: &Value) -> Result<Self> {
        let fields = value.as_object().ok_or_else(|| {
            anyhow!("daemon source refresh current-source progress is not an object")
        })?;
        let stage = fields
            .get("stage")
            .and_then(Value::as_str)
            .and_then(SourceBackedCurrentSourceProgressStage::parse)
            .ok_or_else(|| {
                anyhow!("daemon source refresh current-source progress has an invalid stage")
            })?;
        Ok(Self {
            stage,
            snapshot_pages_completed: optional_progress_u64(fields, "snapshot_pages_completed")?,
            snapshot_pages_total: optional_progress_u64(fields, "snapshot_pages_total")?,
            snapshot_bytes_completed: optional_progress_u64(fields, "snapshot_bytes_completed")?,
            snapshot_bytes_total: optional_progress_u64(fields, "snapshot_bytes_total")?,
            logical_rows_scanned: optional_progress_u64(fields, "logical_rows_scanned")?,
            logical_certified_bytes: optional_progress_u64(fields, "logical_certified_bytes")?,
        })
    }

    pub(crate) fn from_capture(progress: CaptureSourceBackedCurrentSourceProgress) -> Self {
        Self {
            stage: match progress.stage {
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::SourceFamilyCopy => {
                    SourceBackedCurrentSourceProgressStage::SourceFamilyCopy
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::OnlineBackup => {
                    SourceBackedCurrentSourceProgressStage::OnlineBackup
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::LogicalFingerprint => {
                    SourceBackedCurrentSourceProgressStage::LogicalFingerprint
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::LogicalScan => {
                    SourceBackedCurrentSourceProgressStage::LogicalScan
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::Parsing => {
                    SourceBackedCurrentSourceProgressStage::Parsing
                }
                ctx_history_capture::SourceBackedCurrentSourceProgressStage::IndexWriting => {
                    SourceBackedCurrentSourceProgressStage::IndexWriting
                }
            },
            snapshot_pages_completed: progress.snapshot_pages_completed,
            snapshot_pages_total: progress.snapshot_pages_total,
            snapshot_bytes_completed: progress.snapshot_bytes_completed,
            snapshot_bytes_total: progress.snapshot_bytes_total,
            logical_rows_scanned: progress.logical_rows_scanned,
            logical_certified_bytes: progress.logical_certified_bytes,
        }
    }
}

fn optional_progress_u64(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<u64>> {
    fields
        .get(field)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| anyhow!("daemon source refresh progress {field} is invalid"))
        })
        .transpose()
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshOperation {
    Refresh,
    Import,
}

impl RefreshOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Import => "import",
        }
    }

    #[doc(hidden)]
    pub fn from_request_json(request: &Value) -> Result<Self> {
        request
            .get("operation")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("daemon source refresh request operation is missing"))
            .and_then(str::parse)
    }
}

impl std::str::FromStr for RefreshOperation {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "refresh" => Ok(Self::Refresh),
            "import" => Ok(Self::Import),
            operation => Err(anyhow!("invalid source refresh operation `{operation}`")),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct SourceBackedRefreshTimings {
    pub discovery_us: u64,
    pub scan_stage_us: u64,
    pub commit_us: u64,
}

impl SourceBackedRefreshTimings {
    #[doc(hidden)]
    pub fn to_json(self) -> Value {
        json!({
            "discovery": self.discovery_us,
            "scan_stage": self.scan_stage_us,
            "commit": self.commit_us,
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SourceBackedZeroSourceAuthorityKind {
    CompleteEmptyInventory,
    ConfirmedDeletion,
}

impl SourceBackedZeroSourceAuthorityKind {
    #[doc(hidden)]
    pub const fn compact_code(self) -> char {
        match self {
            Self::CompleteEmptyInventory => 'e',
            Self::ConfirmedDeletion => 'd',
        }
    }

    #[doc(hidden)]
    pub fn from_compact_code(value: char) -> Result<Self> {
        match value {
            'e' => Ok(Self::CompleteEmptyInventory),
            'd' => Ok(Self::ConfirmedDeletion),
            _ => bail!("Core zero-source authority has an unknown disposition"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SourceBackedZeroSourceAuthority {
    pub generation_id: String,
    pub route_identity: SourceRouteIdentity,
    pub kind: SourceBackedZeroSourceAuthorityKind,
}

#[derive(Clone)]
pub struct SourceBackedRefreshPublication {
    pub generation_id: String,
    pub published_explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub unsupported_routes: usize,
    pub certified_source_count: usize,
    pub certified_source_bytes: u64,
    pub current: SourceBackedRefreshCurrent,
    pub timings: SourceBackedRefreshTimings,
    pub route_results: Vec<SourceBackedRefreshRouteResult>,
    pub zero_source_authority: Vec<SourceBackedZeroSourceAuthority>,
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    pub verified_index: Option<Arc<VerifiedIndex>>,
}

impl fmt::Debug for SourceBackedRefreshPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceBackedRefreshPublication")
            .field("generation_id", &self.generation_id)
            .field("route_results", &self.route_results)
            .field("has_verified_index", &self.verified_index.is_some())
            .finish_non_exhaustive()
    }
}

pub struct PublishedSourceBackedState {
    pub verified_index: Option<VerifiedIndex>,
    pub explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub catalog_route_bindings: Vec<ExplicitSourceCatalogRouteBinding>,
    pub route_controls: BTreeMap<SourceRouteIdentity, Vec<u8>>,
}

pub trait PublishedSourceBackedStatePort: Send + Sync {
    fn open_published_state(&self, data_root: &Path) -> Result<PublishedSourceBackedState>;
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct SourceBackedExactScanProgress {
    pub total_bytes: u64,
    pub completed_bytes: u64,
}

#[doc(hidden)]
pub struct SourceBackedRefreshProgressUpdate {
    pub phase: String,
    pub completed_sources: usize,
    pub total_sources: usize,
    pub total_sources_known: bool,
    pub current_source: Option<String>,
    pub completed_records: Option<u64>,
    pub completed_bytes: Option<u64>,
    pub providers: Vec<String>,
    pub processed_sessions: u64,
    pub processed_messages: u64,
    pub processed_tool_calls: u64,
    pub processed_bytes: u64,
    pub elapsed_millis: Option<u64>,
    pub current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    pub exact_scan_progress: Option<SourceBackedExactScanProgress>,
}

#[derive(Clone)]
pub struct SourceBackedRefreshExecution<'a> {
    pub data_root: &'a Path,
    pub index_root: &'a Path,
    pub request_id: &'a str,
    pub operation: RefreshOperation,
    pub reconciliation_demand: SourceBackedReconciliationDemand,
    pub explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
    admitted_refresh: AdmittedRefresh,
    pub discovery_context: &'a DiscoveryContext,
    #[doc(hidden)]
    pub published_state: &'a dyn PublishedSourceBackedStatePort,
    #[doc(hidden)]
    pub attempt_history_progress: ctx_history_capture_model::SharedAttemptHistoryProgress,
    report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
}

impl<'a> SourceBackedRefreshExecution<'a> {
    #[doc(hidden)]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        data_root: &'a Path,
        index_root: &'a Path,
        request_id: &'a str,
        operation: RefreshOperation,
        explicit_source_catalog: Option<&'a ExplicitSourceCatalogAuthority>,
        admitted_refresh: AdmittedRefresh,
        discovery_context: &'a DiscoveryContext,
        published_state: &'a dyn PublishedSourceBackedStatePort,
        report_progress: &'a dyn Fn(SourceBackedRefreshProgressUpdate) -> Result<()>,
    ) -> Self {
        let reconciliation_demand = match operation {
            RefreshOperation::Refresh => SourceBackedReconciliationDemand::Incremental,
            RefreshOperation::Import => SourceBackedReconciliationDemand::Exhaustive,
        };
        Self {
            data_root,
            index_root,
            request_id,
            operation,
            reconciliation_demand,
            explicit_source_catalog,
            admitted_refresh,
            discovery_context,
            published_state,
            attempt_history_progress: Default::default(),
            report_progress,
        }
    }

    pub fn with_reconciliation_demand(mut self, demand: SourceBackedReconciliationDemand) -> Self {
        self.reconciliation_demand = demand;
        self
    }

    #[doc(hidden)]
    pub fn with_attempt_history_progress(
        mut self,
        progress: ctx_history_capture_model::SharedAttemptHistoryProgress,
    ) -> Self {
        self.attempt_history_progress = progress;
        self
    }

    pub fn admitted_refresh(&self) -> &AdmittedRefresh {
        &self.admitted_refresh
    }

    pub(crate) fn admitted_refresh_mut(&mut self) -> &mut AdmittedRefresh {
        &mut self.admitted_refresh
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_detailed_progress_with_total_state(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        total_sources_known: bool,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        self.report_history_progress_with_total_state(
            phase,
            completed_sources,
            total_sources,
            total_sources_known,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
            Vec::new(),
            0,
            0,
            0,
            0,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn report_history_progress_with_total_state(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        total_sources_known: bool,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
        providers: Vec<String>,
        processed_sessions: u64,
        processed_messages: u64,
        processed_tool_calls: u64,
        processed_bytes: u64,
        elapsed_millis: Option<u64>,
        exact_scan_progress: Option<SourceBackedExactScanProgress>,
    ) -> Result<()> {
        (self.report_progress)(SourceBackedRefreshProgressUpdate {
            phase: phase.to_owned(),
            completed_sources,
            total_sources,
            total_sources_known,
            current_source,
            completed_records,
            completed_bytes,
            providers,
            processed_sessions,
            processed_messages,
            processed_tool_calls,
            processed_bytes,
            elapsed_millis,
            current_source_progress,
            exact_scan_progress,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn report_detailed_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
        current_source_progress: Option<SourceBackedCurrentSourceProgress>,
    ) -> Result<()> {
        self.report_detailed_progress_with_total_state(
            phase,
            completed_sources,
            total_sources,
            true,
            current_source,
            completed_records,
            completed_bytes,
            current_source_progress,
        )
    }

    pub fn report_progress(
        &self,
        phase: &str,
        completed_sources: usize,
        total_sources: usize,
        current_source: Option<String>,
        completed_records: Option<u64>,
        completed_bytes: Option<u64>,
    ) -> Result<()> {
        self.report_detailed_progress(
            phase,
            completed_sources,
            total_sources,
            current_source,
            completed_records,
            completed_bytes,
            None,
        )
    }
}

/// Provider-neutral, request-local discovery authority for exact execution.
///
/// This value is intentionally transient. Durable jobs retain the logical
/// selector and exact physical scope, then recovery reconstructs and verifies
/// this authority before execution resumes.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct SourceBackedAdmittedDiscovery {
    report: DiscoveryReport,
    discovery_duration: StdDuration,
    watch_catalog: SourceBackedWatchCatalog,
    automatic_provider_discovery: Option<bool>,
    configured_provider_roots: Option<Vec<ctx_history_capture::ProviderRootDefinition>>,
}

/// Immutable physical authority produced by refresh admission.
///
/// Execution can inspect these exact routes and the discovery snapshot that
/// certified them, but it cannot discover or widen the request.
#[derive(Debug, Clone)]
pub struct AdmittedRefresh {
    coverage: AdmittedRefreshCoverage,
    exact_routes: BTreeSet<SourceRouteIdentity>,
    discovery: SourceBackedAdmittedDiscovery,
    route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum AdmittedRefreshCoverage {
    CompleteCatalog,
    SelectedRoutes,
}

impl AdmittedRefresh {
    pub(crate) fn new(
        coverage: AdmittedRefreshCoverage,
        exact_routes: BTreeSet<SourceRouteIdentity>,
        discovery: SourceBackedAdmittedDiscovery,
    ) -> Result<Self> {
        if coverage == AdmittedRefreshCoverage::SelectedRoutes && exact_routes.is_empty() {
            bail!("selected admitted refresh must contain at least one exact route");
        }
        Ok(Self {
            coverage,
            exact_routes,
            discovery,
            route_worksets: BTreeMap::new(),
        })
    }

    pub const fn coverage(&self) -> AdmittedRefreshCoverage {
        self.coverage
    }

    /// Binds an exact physical request directly to one immutable watcher
    /// catalog snapshot. The caller must supply route-local discovery inputs
    /// reconstructed from that same catalog; no provider discovery or scope
    /// inference is performed here.
    pub fn from_exact_catalog_authority(
        exact_routes: BTreeSet<SourceRouteIdentity>,
        discovery_duration: StdDuration,
        watch_catalog: SourceBackedWatchCatalog,
    ) -> Result<Self> {
        let catalog_routes = watch_catalog.route_ids().cloned().collect::<BTreeSet<_>>();
        let missing = exact_routes
            .difference(&catalog_routes)
            .cloned()
            .collect::<BTreeSet<_>>();
        if !missing.is_empty() {
            bail!("exact admitted refresh routes are absent from catalog authority: {missing:?}");
        }
        let report = watch_catalog
            .route_admission_report(&exact_routes)
            .ok_or_else(|| anyhow!("exact admitted refresh has no route-local catalog report"))?;
        Self::new(
            AdmittedRefreshCoverage::SelectedRoutes,
            exact_routes,
            SourceBackedAdmittedDiscovery::new(report, discovery_duration, watch_catalog),
        )
    }

    pub fn exact_routes(&self) -> &BTreeSet<SourceRouteIdentity> {
        &self.exact_routes
    }

    pub fn discovery(&self) -> &SourceBackedAdmittedDiscovery {
        &self.discovery
    }

    pub fn publication_scope(&self) -> SourceBackedRefreshScope {
        match self.coverage {
            AdmittedRefreshCoverage::CompleteCatalog => SourceBackedRefreshScope::All,
            AdmittedRefreshCoverage::SelectedRoutes => {
                SourceBackedRefreshScope::Exact(self.exact_routes.clone())
            }
        }
    }

    pub fn route_worksets(&self) -> &BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset> {
        &self.route_worksets
    }

    pub fn narrow_to(mut self, exact_routes: BTreeSet<SourceRouteIdentity>) -> Result<Self> {
        if !exact_routes.is_subset(&self.exact_routes) {
            bail!("admitted refresh cannot widen beyond its certified exact routes");
        }
        self.coverage = AdmittedRefreshCoverage::SelectedRoutes;
        if exact_routes.is_empty() {
            bail!("selected admitted refresh must contain at least one exact route");
        }
        self.exact_routes = exact_routes;
        Ok(self)
    }

    pub fn with_execution_facts(
        mut self,
        route_worksets: BTreeMap<SourceRouteIdentity, SourceBackedRefreshWorkset>,
    ) -> Result<Self> {
        if route_worksets
            .keys()
            .any(|route| !self.exact_routes.contains(route))
        {
            bail!("source refresh workset references a route outside physical admission");
        }
        self.route_worksets = route_worksets;
        Ok(self)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_configured_provider_roots_for_test(
        mut self,
        roots: Vec<ctx_history_capture::ProviderRootDefinition>,
    ) -> Self {
        self.discovery = self.discovery.with_configured_provider_roots(roots);
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn with_automatic_provider_discovery_for_test(mut self, enabled: bool) -> Self {
        self.discovery = self.discovery.with_automatic_provider_discovery(enabled);
        self
    }

    pub(crate) fn promote_worksets_to_exhaustive(&mut self) {
        self.route_worksets
            .values_mut()
            .for_each(|workset| *workset = SourceBackedRefreshWorkset::Exhaustive);
    }

    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn for_test(
        coverage: AdmittedRefreshCoverage,
        exact_routes: BTreeSet<SourceRouteIdentity>,
        discovery: SourceBackedAdmittedDiscovery,
    ) -> Result<Self> {
        Self::new(coverage, exact_routes, discovery)
    }
}

impl SourceBackedAdmittedDiscovery {
    pub fn new(
        report: DiscoveryReport,
        discovery_duration: StdDuration,
        watch_catalog: SourceBackedWatchCatalog,
    ) -> Self {
        Self {
            report,
            discovery_duration,
            watch_catalog,
            automatic_provider_discovery: None,
            configured_provider_roots: None,
        }
    }

    pub fn with_configured_provider_roots(
        mut self,
        roots: Vec<ctx_history_capture::ProviderRootDefinition>,
    ) -> Self {
        self.configured_provider_roots = Some(roots);
        self
    }

    pub fn with_automatic_provider_discovery(mut self, enabled: bool) -> Self {
        self.automatic_provider_discovery = Some(enabled);
        self
    }

    pub fn report(&self) -> &DiscoveryReport {
        &self.report
    }

    pub fn discovery_duration(&self) -> StdDuration {
        self.discovery_duration
    }

    pub fn watch_catalog(&self) -> &SourceBackedWatchCatalog {
        &self.watch_catalog
    }

    pub fn configured_provider_roots(
        &self,
    ) -> Option<&[ctx_history_capture::ProviderRootDefinition]> {
        self.configured_provider_roots.as_deref()
    }

    pub const fn automatic_provider_discovery(&self) -> Option<bool> {
        self.automatic_provider_discovery
    }
}

pub fn nonzero_duration_micros(duration: StdDuration) -> u64 {
    u64::try_from(duration.as_micros())
        .unwrap_or(u64::MAX)
        .max(1)
}
