use super::*;

pub use ctx_history_refresh_execution::RefreshOperation;
pub(crate) type SourceBackedRefreshOperation = RefreshOperation;

macro_rules! string_enum {
    ($name:ident, $label:literal, { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, Eq, PartialEq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = anyhow::Error;

            fn from_str(value: &str) -> Result<Self> {
                match value {
                    $($wire => Ok(Self::$variant)),+,
                    _ => bail!(concat!("source refresh structured outcome has unknown ", $label, " `{value}`")),
                }
            }
        }
    };
}

string_enum!(RefreshOutcomeCode, "code", {
    Completed => "completed",
    CompletedWithRejections => "completed_with_rejections",
    CompletedWithSourceFailures => "completed_with_source_failures",
    CompletedWithRejectionsAndSourceFailures => "completed_with_rejections_and_source_failures",
    SourceUnavailable => "source_unavailable",
    ExplicitSourcePathMissing => "explicit_source_path_missing",
    SourceChanged => "source_changed",
    MalformedSource => "malformed_source",
    UnsupportedSchema => "unsupported_schema",
    SourceFailures => "source_failures",
    LogicalSourceFailures => "logical_source_failures",
    SourceUnclaimed => "source_unclaimed",
    SourceRefreshFailed => "source_refresh_failed",
    SourceRefreshInternal => "source_refresh_internal",
    ResourceUnavailable => "resource_unavailable",
    IndexIncompatible => "index_incompatible",
    IndexCorruption => "index_corruption",
    SourceRefreshAdmissionFailed => "source_refresh_admission_failed",
    AllProviderTerminalCoverageUnavailable => "all_provider_terminal_coverage_unavailable",
});

impl RefreshOutcomeCode {
    pub const fn is_failure(self) -> bool {
        !matches!(
            self,
            Self::Completed
                | Self::CompletedWithRejections
                | Self::CompletedWithSourceFailures
                | Self::CompletedWithRejectionsAndSourceFailures
        )
    }

    /// Returns the bounded observability classification owned by the Core
    /// terminal outcome. Successful outcomes do not carry failure facts.
    pub const fn terminal_failure_classification(
        self,
    ) -> Option<(RefreshTerminalFailureScope, RefreshTerminalFailureType)> {
        match self {
            Self::UnsupportedSchema => Some((
                RefreshTerminalFailureScope::Source,
                RefreshTerminalFailureType::UnsupportedSchema,
            )),
            Self::MalformedSource => Some((
                RefreshTerminalFailureScope::Source,
                RefreshTerminalFailureType::MalformedSource,
            )),
            Self::SourceUnavailable
            | Self::ExplicitSourcePathMissing
            | Self::SourceChanged
            | Self::SourceFailures
            | Self::LogicalSourceFailures
            | Self::SourceUnclaimed => Some((
                RefreshTerminalFailureScope::Source,
                RefreshTerminalFailureType::Unknown,
            )),
            Self::SourceRefreshFailed
            | Self::SourceRefreshInternal
            | Self::ResourceUnavailable
            | Self::IndexIncompatible
            | Self::IndexCorruption
            | Self::SourceRefreshAdmissionFailed
            | Self::AllProviderTerminalCoverageUnavailable => Some((
                RefreshTerminalFailureScope::System,
                RefreshTerminalFailureType::System,
            )),
            Self::Completed
            | Self::CompletedWithRejections
            | Self::CompletedWithSourceFailures
            | Self::CompletedWithRejectionsAndSourceFailures => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshTerminalFailureScope {
    Source,
    System,
    Unknown,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshTerminalFailureType {
    UnsupportedSchema,
    MalformedSource,
    System,
    Unknown,
}

string_enum!(RefreshOutcomeClass, "class", {
    Completed => "completed",
    CompletedWithRetryableFailures => "completed_with_retryable_failures",
    CompletedWithDiagnostics => "completed_with_diagnostics",
    Unavailable => "unavailable",
    SourceChanged => "source_changed",
    Unreadable => "unreadable",
    Incompatible => "incompatible",
    Mixed => "mixed",
    Internal => "internal",
    ResourceUnavailable => "resource_unavailable",
    Corruption => "corruption",
    ControlPlane => "control_plane",
    Coverage => "coverage",
});

string_enum!(RefreshRetryAdvice, "retry advice", {
    RetryAffectedRoutes => "retry_affected_routes",
    RetryRetryableRoutesAndInspectBlocked => "retry_retryable_routes_and_inspect_blocked",
    RetryRequest => "retry_request",
    RetryAdmission => "retry_admission",
    RetryFinalization => "retry_finalization",
    InspectSources => "inspect_sources",
    UpgradeOrReconfigure => "upgrade_or_reconfigure",
    RebuildIndex => "rebuild_index",
});

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshRequestState {
    AdmissionPending,
    Queued,
    Running,
    Published,
    Failed,
}

impl RefreshRequestState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Failed)
    }
}

impl std::str::FromStr for RefreshRequestState {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "admission_pending" => Ok(Self::AdmissionPending),
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "published" => Ok(Self::Published),
            "failed" => Ok(Self::Failed),
            _ => bail!("source refresh response has unknown typed state `{value}`"),
        }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshLogicalPhase {
    Waiting,
    Attached,
    CoverageCheck,
    ExactSuccessor,
    Direct,
    Terminal,
}

impl std::str::FromStr for RefreshLogicalPhase {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "waiting" => Ok(Self::Waiting),
            "attached" => Ok(Self::Attached),
            "coverage_check" => Ok(Self::CoverageCheck),
            "exact_successor" => Ok(Self::ExactSuccessor),
            "direct" => Ok(Self::Direct),
            "terminal" => Ok(Self::Terminal),
            _ => bail!("source refresh response has invalid logical phase"),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RefreshTerminalOutcome {
    pub code: RefreshOutcomeCode,
    pub class: RefreshOutcomeClass,
    pub retryable: bool,
    pub affected_routes: BTreeSet<SourceRouteIdentity>,
    pub retryable_routes: BTreeSet<SourceRouteIdentity>,
    pub blocked_routes: BTreeSet<SourceRouteIdentity>,
    pub physical_attempt_id: String,
    pub retained_generation: Option<String>,
    pub published_generation: Option<String>,
    pub retry_advice: Option<RefreshRetryAdvice>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RefreshLogicalStatus {
    pub request_state: RefreshRequestState,
    pub logical_phase: RefreshLogicalPhase,
    pub physical_attempt_id: String,
    pub physical_attempt_state: RefreshRequestState,
    pub progress_owner_request_id: String,
    pub progress_owner_attempt_state: RefreshRequestState,
    pub structured_outcome: Option<RefreshTerminalOutcome>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RefreshMaintenanceWakeStatus {
    pub request_id: String,
    pub previous_generation: Option<String>,
    pub published_generation: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RefreshStatusKind {
    Legacy { request_state: RefreshRequestState },
    BackgroundMaintenanceWake(RefreshMaintenanceWakeStatus),
    Logical(RefreshLogicalStatus),
}

impl RefreshStatusKind {
    pub const fn request_state(&self) -> RefreshRequestState {
        match self {
            Self::Legacy { request_state } => *request_state,
            Self::BackgroundMaintenanceWake(_) => RefreshRequestState::Queued,
            Self::Logical(status) => status.request_state,
        }
    }

    pub fn terminal_outcome(&self) -> Option<&RefreshTerminalOutcome> {
        match self {
            Self::Logical(status) => status.structured_outcome.as_ref(),
            Self::Legacy { .. } | Self::BackgroundMaintenanceWake(_) => None,
        }
    }

    pub fn into_terminal_outcome(self) -> Option<RefreshTerminalOutcome> {
        match self {
            Self::Logical(status) => status.structured_outcome,
            Self::Legacy { .. } | Self::BackgroundMaintenanceWake(_) => None,
        }
    }
}

/// Closed command trigger attached to an explicitly submitted refresh.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum RefreshRequestTrigger {
    Setup,
    Search,
    Import,
}

impl RefreshRequestTrigger {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Search => "search",
            Self::Import => "import",
        }
    }
}

impl std::str::FromStr for RefreshRequestTrigger {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "setup" => Ok(Self::Setup),
            "search" => Ok(Self::Search),
            "import" => Ok(Self::Import),
            _ => bail!("source refresh request has unknown trigger `{value}`"),
        }
    }
}

/// The one logical source selection accepted by Core refresh admission.
/// Physical route identities are produced only by the admission resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshSelection {
    All,
    Provider(CaptureProvider),
    ExactSource(ExplicitSourceCatalogAuthority),
}

impl RefreshSelection {
    pub fn to_json(&self) -> Value {
        match self {
            Self::All => json!({ "kind": "all" }),
            Self::Provider(provider) => json!({
                "kind": "provider",
                "provider": provider.as_str(),
            }),
            Self::ExactSource(authority) => json!({
                "kind": "exact_source",
                "authority": authority.to_json(),
            }),
        }
    }

    pub fn from_json(value: &Value) -> Result<Self> {
        let fields = value
            .as_object()
            .ok_or_else(|| anyhow!("source refresh selection is not an object"))?;
        match fields.get("kind").and_then(Value::as_str) {
            Some("all") if fields.len() == 1 => Ok(Self::All),
            Some("provider") if fields.len() == 2 => {
                let provider = fields
                    .get("provider")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("provider source refresh selection has no provider"))?
                    .parse()
                    .context("parse provider source refresh selection")?;
                if provider == CaptureProvider::Unknown {
                    bail!("provider source refresh selection has an unknown provider");
                }
                Ok(Self::Provider(provider))
            }
            Some("exact_source") if fields.len() == 2 => fields
                .get("authority")
                .ok_or_else(|| anyhow!("exact-source refresh selection has no authority"))
                .and_then(ExplicitSourceCatalogAuthority::from_json)
                .map(Self::ExactSource),
            Some(kind) => bail!("source refresh selection `{kind}` is malformed"),
            None => bail!("source refresh selection kind is missing"),
        }
    }

    pub fn explicit_source_authority(&self) -> Option<&ExplicitSourceCatalogAuthority> {
        match self {
            Self::ExactSource(authority) => Some(authority),
            Self::All | Self::Provider(_) => None,
        }
    }
}

/// Canonical logical intent for one Core refresh request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshIntent {
    AutomaticMaintenance,
    SelectedImport(RefreshSelection),
}

impl RefreshIntent {
    pub fn to_json(&self) -> Value {
        match self {
            Self::AutomaticMaintenance => json!({ "kind": "automatic_maintenance" }),
            Self::SelectedImport(selection) => json!({
                "kind": "selected_import",
                "selection": selection.to_json(),
            }),
        }
    }

    pub fn from_json(value: &Value) -> Result<Self> {
        let fields = value
            .as_object()
            .ok_or_else(|| anyhow!("source refresh intent is not an object"))?;
        match fields.get("kind").and_then(Value::as_str) {
            Some("automatic_maintenance") if fields.len() == 1 => Ok(Self::AutomaticMaintenance),
            Some("selected_import") if fields.len() == 2 => fields
                .get("selection")
                .ok_or_else(|| anyhow!("selected import has no source selection"))
                .and_then(RefreshSelection::from_json)
                .map(Self::SelectedImport),
            Some(kind) => bail!("source refresh intent `{kind}` is malformed"),
            None => bail!("source refresh intent kind is missing"),
        }
    }

    pub const fn operation(&self) -> RefreshOperation {
        match self {
            Self::AutomaticMaintenance => RefreshOperation::Refresh,
            Self::SelectedImport(_) => RefreshOperation::Import,
        }
    }

    pub const fn reconciliation_demand(&self) -> SourceBackedReconciliationDemand {
        match self {
            Self::AutomaticMaintenance => SourceBackedReconciliationDemand::Incremental,
            Self::SelectedImport(_) => SourceBackedReconciliationDemand::Exhaustive,
        }
    }

    pub fn selection(&self) -> Option<&RefreshSelection> {
        match self {
            Self::AutomaticMaintenance => None,
            Self::SelectedImport(selection) => Some(selection),
        }
    }

    pub const fn is_selected_import(&self) -> bool {
        matches!(self, Self::SelectedImport(_))
    }

    pub fn explicit_source_authority(&self) -> Option<&ExplicitSourceCatalogAuthority> {
        self.selection()
            .and_then(RefreshSelection::explicit_source_authority)
    }
}

/// Process-neutral logical request accepted by the refresh engine.
///
/// Admission is solely responsible for turning `intent` into certified exact
/// routes. Callers cannot provide a physical scope or an execution fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshRequest {
    pub(crate) request_id: String,
    pub(crate) intent: RefreshIntent,
    pub(crate) trigger: RefreshRequestTrigger,
}

impl RefreshRequest {
    pub fn new(request_id: String, intent: RefreshIntent, trigger: RefreshRequestTrigger) -> Self {
        Self {
            request_id,
            intent,
            trigger,
        }
    }

    pub fn automatic(request_id: String, trigger: RefreshRequestTrigger) -> Self {
        Self::new(request_id, RefreshIntent::AutomaticMaintenance, trigger)
    }

    pub fn selected_import(request_id: String, selection: RefreshSelection) -> Self {
        Self::new(
            request_id,
            RefreshIntent::SelectedImport(selection),
            RefreshRequestTrigger::Import,
        )
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    pub fn intent(&self) -> &RefreshIntent {
        &self.intent
    }

    pub const fn trigger(&self) -> RefreshRequestTrigger {
        self.trigger
    }

    #[doc(hidden)]
    pub fn with_trigger(mut self, trigger: RefreshRequestTrigger) -> Self {
        self.trigger = trigger;
        self
    }
}

/// Typed engine status. Schema-v1 rendering remains owned by the process
/// adapter so transport code never reaches into coordinator state.
#[derive(Debug, Clone, PartialEq)]
pub struct RefreshStatus {
    schema_v1_fields: Value,
}

impl RefreshStatus {
    pub(crate) fn from_schema_v1_fields(fields: Value) -> Self {
        Self {
            schema_v1_fields: fields,
        }
    }

    /// Parses one schema-v1 status snapshot through the engine-owned logical
    /// status and nested progress validators without changing its wire shape.
    pub fn parse_schema_v1(fields: Value) -> Result<Self> {
        let status = Self::from_schema_v1_fields(fields);
        status.kind()?;
        status.progress()?;
        status.whole_run_stage()?;
        status.estimated_remaining_millis()?;
        status.total_sources_known()?;
        Ok(status)
    }

    #[doc(hidden)]
    pub fn schema_v1_fields(&self) -> &Value {
        &self.schema_v1_fields
    }

    pub fn request_id(&self) -> Option<&str> {
        self.schema_v1_fields
            .get("request_id")
            .and_then(Value::as_str)
    }

    pub fn kind(&self) -> Result<RefreshStatusKind> {
        Self::classify_schema_v1(self.schema_v1_fields())
    }

    pub fn progress(&self) -> Result<SourceBackedRefreshProgress> {
        SourceBackedRefreshProgress::from_status_json(self.schema_v1_fields())
    }

    pub fn whole_run_stage(&self) -> Result<SourceBackedRefreshStage> {
        let derived = self.progress()?.whole_run_stage();
        let progress = self
            .schema_v1_fields
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("daemon source refresh status has no progress object"))?;
        match progress.get("whole_run_stage") {
            None => Ok(derived),
            Some(Value::String(stage)) => match stage.as_str() {
                "preparing" => Ok(SourceBackedRefreshStage::Preparing),
                "reading" => Ok(SourceBackedRefreshStage::Reading),
                "merging" => Ok(SourceBackedRefreshStage::Merging),
                "syncing" => Ok(SourceBackedRefreshStage::Syncing),
                "physical_verification" => Ok(SourceBackedRefreshStage::PhysicalVerification),
                "logical_verification" => Ok(SourceBackedRefreshStage::LogicalVerification),
                "activation" => Ok(SourceBackedRefreshStage::Activation),
                "complete" => Ok(SourceBackedRefreshStage::Complete),
                "failed" => Ok(SourceBackedRefreshStage::Failed),
                _ => bail!("daemon source refresh progress has an invalid whole_run_stage"),
            },
            Some(_) => bail!("daemon source refresh progress has an invalid whole_run_stage"),
        }
    }

    pub fn estimated_remaining_millis(&self) -> Result<Option<u64>> {
        let progress = self
            .schema_v1_fields
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("daemon source refresh status has no progress object"))?;
        match progress.get("estimated_remaining_millis") {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value.as_u64().map(Some).ok_or_else(|| {
                anyhow!("daemon source refresh progress has an invalid estimated_remaining_millis")
            }),
        }
    }

    pub fn total_sources_known(&self) -> Result<bool> {
        let progress = self
            .schema_v1_fields
            .get("progress")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("daemon source refresh status has no progress object"))?;
        match progress.get("total_sources_known") {
            Some(Value::Bool(known)) => Ok(*known),
            None => Ok(progress
                .get("total_sources")
                .and_then(Value::as_u64)
                .is_some_and(|total| total != 0)),
            Some(_) => bail!("daemon source refresh progress has an invalid total_sources_known"),
        }
    }

    pub fn classify_schema_v1(fields: &Value) -> Result<RefreshStatusKind> {
        let request_state = required_status_string(fields, "request_state")?.parse()?;
        if fields.get("maintenance_wake").is_some() {
            return parse_maintenance_wake(fields, request_state);
        }
        let Some(logical_phase) = fields.get("logical_phase") else {
            if TYPED_STATUS_FIELDS
                .iter()
                .any(|field| fields.get(*field).is_some())
            {
                bail!("source refresh response has a partial typed logical status");
            }
            return Ok(RefreshStatusKind::Legacy { request_state });
        };
        let logical_phase = logical_phase
            .as_str()
            .ok_or_else(|| anyhow!("source refresh response has invalid logical phase"))?
            .parse()?;
        let request_id = required_status_string(fields, "request_id")?;
        if required_status_string(fields, "logical_request_id")? != request_id {
            bail!("source refresh logical request authority does not match its request ID");
        }
        let physical_attempt_id = required_status_string(fields, "physical_attempt_id")?.to_owned();
        let physical_attempt_state =
            required_status_string(fields, "physical_attempt_state")?.parse()?;
        let progress_owner_request_id =
            required_status_string(fields, "progress_owner_request_id")?.to_owned();
        let progress_owner_attempt_state =
            required_status_string(fields, "progress_owner_attempt_state")?.parse()?;
        if (logical_phase == RefreshLogicalPhase::Terminal) != request_state.is_terminal() {
            bail!("source refresh logical phase disagrees with its request state");
        }
        let structured_outcome = fields
            .get("structured_outcome")
            .map(parse_terminal_outcome)
            .transpose()?;
        if request_state.is_terminal() != structured_outcome.is_some() {
            bail!("source refresh terminal state has no structured outcome");
        }
        if structured_outcome
            .as_ref()
            .is_some_and(|outcome| outcome.physical_attempt_id != physical_attempt_id)
        {
            bail!("source refresh outcome names a different physical attempt");
        }
        Ok(RefreshStatusKind::Logical(RefreshLogicalStatus {
            request_state,
            logical_phase,
            physical_attempt_id,
            physical_attempt_state,
            progress_owner_request_id,
            progress_owner_attempt_state,
            structured_outcome,
        }))
    }
}

const TYPED_STATUS_FIELDS: &[&str] = &[
    "logical_request_id",
    "physical_attempt_id",
    "physical_attempt_state",
    "progress_owner_request_id",
    "progress_owner_attempt_state",
    "structured_outcome",
    "maintenance_wake",
];

fn parse_maintenance_wake(
    fields: &Value,
    request_state: RefreshRequestState,
) -> Result<RefreshStatusKind> {
    if fields.get("maintenance_wake").and_then(Value::as_bool) != Some(true)
        || request_state != RefreshRequestState::Queued
        || fields.get("logical_phase").and_then(Value::as_str) != Some("waiting")
        || fields
            .get("progress")
            .and_then(|progress| progress.get("phase"))
            .and_then(Value::as_str)
            != Some("maintenance_wake")
        || [
            "physical_attempt_id",
            "physical_attempt_state",
            "progress_owner_request_id",
            "progress_owner_attempt_state",
            "structured_outcome",
        ]
        .iter()
        .any(|field| fields.get(*field).is_some())
    {
        bail!("source refresh response has invalid background maintenance wake status");
    }
    let request_id = required_status_string(fields, "request_id")?.to_owned();
    if required_status_string(fields, "logical_request_id")? != request_id {
        bail!("source refresh maintenance wake authority does not match its request ID");
    }
    let previous_generation = optional_status_string(fields, "previous_generation")?;
    let published_generation = optional_status_string(fields, "published_generation")?;
    if previous_generation != published_generation {
        bail!("source refresh maintenance wake generation authority is inconsistent");
    }
    Ok(RefreshStatusKind::BackgroundMaintenanceWake(
        RefreshMaintenanceWakeStatus {
            request_id,
            previous_generation,
            published_generation,
        },
    ))
}

fn parse_terminal_outcome(value: &Value) -> Result<RefreshTerminalOutcome> {
    let fields = value
        .as_object()
        .ok_or_else(|| anyhow!("source refresh structured outcome is not an object"))?;
    let code: RefreshOutcomeCode = required_outcome_string(fields, "code")?.parse()?;
    let class: RefreshOutcomeClass = required_outcome_string(fields, "class")?.parse()?;
    let retryable = fields
        .get("retryable")
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow!("source refresh structured outcome has invalid retryability"))?;
    let affected_routes = outcome_routes(fields, "affected_routes")?;
    let retryable_routes = outcome_routes(fields, "retryable_routes")?;
    let blocked_routes = outcome_routes(fields, "blocked_routes")?;
    if !retryable_routes.is_disjoint(&blocked_routes)
        || !retryable_routes.is_subset(&affected_routes)
        || !blocked_routes.is_subset(&affected_routes)
        || (code.is_failure()
            && retryable_routes
                .union(&blocked_routes)
                .ne(affected_routes.iter()))
        || (!affected_routes.is_empty() && retryable == retryable_routes.is_empty())
    {
        bail!("source refresh structured outcome has inconsistent route dispositions");
    }
    let physical_attempt_id = required_outcome_string(fields, "physical_attempt_id")?.to_owned();
    let retry_advice = match optional_outcome_string(fields, "retry_advice")? {
        Some(value) => Some(value.parse()?),
        None => None,
    };
    if code == RefreshOutcomeCode::SourceUnclaimed
        && (class != RefreshOutcomeClass::Coverage
            || blocked_routes.is_empty()
            || retry_advice
                != Some(if retryable {
                    RefreshRetryAdvice::RetryRetryableRoutesAndInspectBlocked
                } else {
                    RefreshRetryAdvice::InspectSources
                }))
    {
        bail!("source refresh source-unclaimed outcome is inconsistent");
    }
    Ok(RefreshTerminalOutcome {
        code,
        class,
        retryable,
        affected_routes,
        retryable_routes,
        blocked_routes,
        physical_attempt_id,
        retained_generation: optional_outcome_string(fields, "retained_generation")?,
        published_generation: optional_outcome_string(fields, "published_generation")?,
        retry_advice,
        detail: optional_outcome_string(fields, "detail")?,
    })
}

fn required_status_string<'a>(value: &'a Value, field: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("source refresh response has invalid `{field}`"))
}

fn optional_status_string(value: &Value, field: &str) -> Result<Option<String>> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("source refresh response has invalid `{field}`"),
    }
}

fn required_outcome_string<'a>(
    fields: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str> {
    fields
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("source refresh structured outcome has invalid `{field}`"))
}

fn optional_outcome_string(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>> {
    match fields.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        _ => bail!("source refresh structured outcome has invalid `{field}`"),
    }
}

fn outcome_routes(
    fields: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<BTreeSet<SourceRouteIdentity>> {
    let values = fields
        .get(field)
        .and_then(Value::as_array)
        .filter(|routes| routes.len() <= SOURCE_REFRESH_TERMINAL_ROUTE_LIMIT)
        .ok_or_else(|| anyhow!("source refresh structured outcome has invalid `{field}`"))?;
    let routes = values
        .iter()
        .map(|route| {
            route
                .as_str()
                .ok_or_else(|| anyhow!("source refresh outcome route is not a string"))
                .and_then(|route| {
                    SourceRouteIdentity::from_sha256(route.to_owned()).map_err(Into::into)
                })
        })
        .collect::<Result<BTreeSet<_>>>()?;
    if routes.len() != values.len() {
        bail!("source refresh structured outcome has duplicate `{field}` routes");
    }
    Ok(routes)
}

#[cfg(any(test, feature = "test-support"))]
impl std::ops::Index<&str> for RefreshStatus {
    type Output = Value;

    fn index(&self, field: &str) -> &Self::Output {
        &self.schema_v1_fields[field]
    }
}

/// Admission hold released only after the hosting process attempts to write
/// the typed acknowledgement to its transport.
#[derive(Debug)]
pub struct AdmissionResponseBarrier {
    request_id: Option<String>,
}

impl AdmissionResponseBarrier {
    pub(crate) fn new(request_id: String) -> Self {
        Self {
            request_id: Some(request_id),
        }
    }

    pub fn release(mut self, engine: &crate::RefreshEngine) {
        if let Some(request_id) = self.request_id.take() {
            engine.release_admission_response(&request_id);
        }
    }
}

/// Result of one durable submission reservation.
#[derive(Debug)]
pub struct RefreshAdmission {
    status: RefreshStatus,
    response_barrier: Option<AdmissionResponseBarrier>,
}

impl RefreshAdmission {
    pub(crate) fn new(
        status: RefreshStatus,
        response_barrier: Option<AdmissionResponseBarrier>,
    ) -> Self {
        Self {
            status,
            response_barrier,
        }
    }

    pub fn status(&self) -> &RefreshStatus {
        &self.status
    }

    pub fn into_parts(self) -> (RefreshStatus, Option<AdmissionResponseBarrier>) {
        (self.status, self.response_barrier)
    }
}
