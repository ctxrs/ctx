use super::*;

/// The caller's requested Core operation, independent of its process or wire transport.
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

    pub(crate) fn from_request_json(request: &Value) -> Result<Self> {
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
    SourceChanged => "source_changed",
    MalformedSource => "malformed_source",
    UnsupportedSchema => "unsupported_schema",
    SourceFailures => "source_failures",
    LogicalSourceFailures => "logical_source_failures",
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

/// Process-neutral facts required to submit one logical refresh request.
#[derive(Debug, Clone)]
pub struct RefreshSubmission {
    pub(crate) request_id: String,
    pub(crate) operation: RefreshOperation,
    pub(crate) explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
    pub(crate) refresh_scope: SourceBackedRefreshScope,
    pub(crate) fresh_after_admitted_snapshot: bool,
    pub(crate) maintenance_wake: bool,
}

impl RefreshSubmission {
    pub fn new(
        request_id: String,
        operation: RefreshOperation,
        explicit_source_catalog: Option<ExplicitSourceCatalogAuthority>,
        refresh_scope: SourceBackedRefreshScope,
        fresh_after_admitted_snapshot: bool,
        maintenance_wake: bool,
    ) -> Self {
        Self {
            request_id,
            operation,
            explicit_source_catalog,
            refresh_scope,
            fresh_after_admitted_snapshot,
            maintenance_wake,
        }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
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
    let code = required_outcome_string(fields, "code")?.parse()?;
    let class = required_outcome_string(fields, "class")?.parse()?;
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
        || (!affected_routes.is_empty() && retryable != !retryable_routes.is_empty())
    {
        bail!("source refresh structured outcome has inconsistent route dispositions");
    }
    let physical_attempt_id = required_outcome_string(fields, "physical_attempt_id")?.to_owned();
    let retry_advice = match optional_outcome_string(fields, "retry_advice")? {
        Some(value) => Some(value.parse()?),
        None => None,
    };
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
