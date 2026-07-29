use std::{path::Path, time::Duration};

use serde_json::{Map, Value};

use super::{count_bucket, CountBucket, OperationCompletedV1, Outcome, PublicEventV1};
use crate::config::AppConfig;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProHostOperationV1 {
    Lifecycle(ProLifecycleTelemetryV1),
    Materialize(ProMaterializationTelemetryV1),
    Status(ProStatusTelemetryV1),
    Blame(ProBlameTelemetryV1),
}

impl ProHostOperationV1 {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Lifecycle(_) => "lifecycle",
            Self::Materialize(_) => "materialize",
            Self::Status(_) => "status",
            Self::Blame(_) => "blame",
        }
    }

    pub(crate) fn insert_properties(&self, properties: &mut Map<String, Value>) {
        match self {
            Self::Lifecycle(value) => value.insert_properties(properties),
            Self::Materialize(value) => value.insert_properties(properties),
            Self::Status(value) => value.insert_properties(properties),
            Self::Blame(value) => value.insert_properties(properties),
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProLifecycleOperationV1 {
    Setup,
    Manage,
    Status,
    Uninstall,
}

impl ProLifecycleOperationV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Manage => "manage",
            Self::Status => "status",
            Self::Uninstall => "uninstall",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProAccessStateV1 {
    Trial,
    Active,
    CancelingPaid,
    OfflineGrace,
    Locked,
}

impl ProAccessStateV1 {
    pub(crate) fn from_safe_name(value: &str) -> Option<Self> {
        match value {
            "trial" => Some(Self::Trial),
            "active" => Some(Self::Active),
            "canceling_paid" => Some(Self::CancelingPaid),
            "offline_grace" => Some(Self::OfflineGrace),
            "locked" => Some(Self::Locked),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Trial => "trial",
            Self::Active => "active",
            Self::CancelingPaid => "canceling_paid",
            Self::OfflineGrace => "offline_grace",
            Self::Locked => "locked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProHelperConnectionOutcomeV1 {
    NotAttempted,
    Connected,
    NotInstalled,
    AuthorizationFailed,
    ProtocolMismatch,
    TimedOut,
    Crashed,
    Unavailable,
}

impl ProHelperConnectionOutcomeV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Connected => "connected",
            Self::NotInstalled => "not_installed",
            Self::AuthorizationFailed => "authorization_failed",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::TimedOut => "timed_out",
            Self::Crashed => "crashed",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProReconcileOutcomeV1 {
    NotAttempted,
    Missing,
    Current,
    Installed,
    Updated,
    Failed,
}

impl ProReconcileOutcomeV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Missing => "missing",
            Self::Current => "current",
            Self::Installed => "installed",
            Self::Updated => "updated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProMaterializationModeV1 {
    NoOp,
    Full,
    Incremental,
}

impl ProMaterializationModeV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::NoOp => "no_op",
            Self::Full => "full",
            Self::Incremental => "incremental",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProCommitOutcomeV1 {
    NotCommitted,
    NoOp,
    Committed,
    Replayed,
    Mixed,
}

impl ProCommitOutcomeV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::NotCommitted => "not_committed",
            Self::NoOp => "no_op",
            Self::Committed => "committed",
            Self::Replayed => "replayed",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProFreshnessV1 {
    Current,
    Unknown,
}

impl ProFreshnessV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProMaterializationResultV1 {
    Completed,
    Failed,
}

impl ProMaterializationResultV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProBlameTargetV1 {
    File,
    Commit,
    PullRequest,
}

impl ProBlameTargetV1 {
    pub(crate) const fn from_protocol(target: &ctx_pro_host_protocol::BlameTarget) -> Self {
        match target {
            ctx_pro_host_protocol::BlameTarget::File { .. } => Self::File,
            ctx_pro_host_protocol::BlameTarget::Commit { .. } => Self::Commit,
            ctx_pro_host_protocol::BlameTarget::PullRequest { .. } => Self::PullRequest,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Commit => "commit",
            Self::PullRequest => "pull_request",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProSurfaceV1 {
    Cli,
    Mcp,
}

impl ProSurfaceV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProFailureBucketV1 {
    Commercial,
    Installation,
    Authorization,
    KeyStore,
    Protocol,
    Source,
    Repository,
    Stale,
    Ambiguous,
    InvalidRequest,
    InvalidResponse,
    Cancelled,
    HelperCrashed,
    HelperTimeout,
    Output,
    Other,
}

impl ProFailureBucketV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Commercial => "commercial",
            Self::Installation => "installation",
            Self::Authorization => "authorization",
            Self::KeyStore => "key_store",
            Self::Protocol => "protocol",
            Self::Source => "source",
            Self::Repository => "repository",
            Self::Stale => "stale",
            Self::Ambiguous => "ambiguous",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidResponse => "invalid_response",
            Self::Cancelled => "cancelled",
            Self::HelperCrashed => "helper_crashed",
            Self::HelperTimeout => "helper_timeout",
            Self::Output => "output",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProUninstallDataDispositionV1 {
    Delete,
    Preserve,
}

impl ProUninstallDataDispositionV1 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Preserve => "preserve",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProMaterializationTelemetryV1 {
    pub(crate) mode: Option<ProMaterializationModeV1>,
    pub(crate) commit: ProCommitOutcomeV1,
    pub(crate) freshness: ProFreshnessV1,
    pub(crate) result: ProMaterializationResultV1,
    pub(crate) batches: Option<CountBucket>,
    pub(crate) input_records: Option<CountBucket>,
    pub(crate) output_records: Option<CountBucket>,
    pub(crate) lag: Option<CountBucket>,
    pub(crate) helper_connection: ProHelperConnectionOutcomeV1,
    pub(crate) failure: Option<ProFailureBucketV1>,
}

impl ProMaterializationTelemetryV1 {
    pub(crate) fn started() -> Self {
        Self {
            mode: None,
            commit: ProCommitOutcomeV1::NotCommitted,
            freshness: ProFreshnessV1::Unknown,
            result: ProMaterializationResultV1::Failed,
            batches: None,
            input_records: None,
            output_records: None,
            lag: None,
            helper_connection: ProHelperConnectionOutcomeV1::NotAttempted,
            failure: None,
        }
    }

    pub(crate) fn complete(
        &mut self,
        batches: u64,
        input_records: u64,
        output_records: u64,
        replayed_batches: u64,
        lag: u64,
    ) {
        if input_records == 0 && output_records == 0 {
            if self.mode == Some(ProMaterializationModeV1::Incremental) {
                self.mode = Some(ProMaterializationModeV1::NoOp);
            }
            self.commit = ProCommitOutcomeV1::NoOp;
        } else {
            self.commit = match replayed_batches {
                0 => ProCommitOutcomeV1::Committed,
                replayed if replayed == batches => ProCommitOutcomeV1::Replayed,
                _ => ProCommitOutcomeV1::Mixed,
            };
        }
        self.freshness = ProFreshnessV1::Current;
        self.result = ProMaterializationResultV1::Completed;
        self.batches = Some(count_bucket(batches));
        self.input_records = Some(count_bucket(input_records));
        self.output_records = Some(count_bucket(output_records));
        self.lag = Some(count_bucket(lag));
        self.failure = None;
    }

    pub(crate) fn fail(&mut self, code: Option<&str>) {
        self.result = ProMaterializationResultV1::Failed;
        self.failure = Some(pro_failure_bucket(code));
    }

    fn insert_properties(&self, properties: &mut Map<String, Value>) {
        insert_optional_str(
            properties,
            "materialization_mode",
            self.mode.map(ProMaterializationModeV1::as_str),
        );
        insert_str(properties, "materialization_commit", self.commit.as_str());
        insert_str(
            properties,
            "materialization_freshness",
            self.freshness.as_str(),
        );
        insert_str(properties, "materialization_result", self.result.as_str());
        insert_optional_count(
            properties,
            "materialization_batch_count_bucket",
            self.batches,
        );
        insert_optional_count(
            properties,
            "materialization_input_count_bucket",
            self.input_records,
        );
        insert_optional_count(
            properties,
            "materialization_output_count_bucket",
            self.output_records,
        );
        insert_optional_count(properties, "materialization_lag_bucket", self.lag);
        insert_str(
            properties,
            "helper_connection_outcome",
            self.helper_connection.as_str(),
        );
        insert_optional_str(
            properties,
            "materialization_failure_bucket",
            self.failure.map(ProFailureBucketV1::as_str),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProLifecycleTelemetryV1 {
    pub(crate) operation: ProLifecycleOperationV1,
    pub(crate) access_state: Option<ProAccessStateV1>,
    pub(crate) helper_connection: ProHelperConnectionOutcomeV1,
    pub(crate) reconcile: ProReconcileOutcomeV1,
    pub(crate) uninstall_data: Option<ProUninstallDataDispositionV1>,
    pub(crate) materialization: Option<ProMaterializationTelemetryV1>,
    pub(crate) failure: Option<ProFailureBucketV1>,
}

impl ProLifecycleTelemetryV1 {
    pub(crate) fn new(operation: ProLifecycleOperationV1) -> Self {
        Self {
            operation,
            access_state: None,
            helper_connection: ProHelperConnectionOutcomeV1::NotAttempted,
            reconcile: ProReconcileOutcomeV1::NotAttempted,
            uninstall_data: None,
            materialization: None,
            failure: None,
        }
    }

    pub(crate) fn fail(&mut self, code: Option<&str>) {
        self.failure = Some(pro_failure_bucket(code));
    }

    fn insert_properties(&self, properties: &mut Map<String, Value>) {
        insert_str(properties, "lifecycle_operation", self.operation.as_str());
        insert_optional_str(
            properties,
            "access_state",
            self.access_state.map(ProAccessStateV1::as_str),
        );
        insert_str(
            properties,
            "helper_connection_outcome",
            self.helper_connection.as_str(),
        );
        insert_str(properties, "reconcile_outcome", self.reconcile.as_str());
        insert_optional_str(
            properties,
            "uninstall_data_disposition",
            self.uninstall_data
                .map(ProUninstallDataDispositionV1::as_str),
        );
        insert_optional_str(
            properties,
            "lifecycle_failure_bucket",
            self.failure.map(ProFailureBucketV1::as_str),
        );
        if let Some(materialization) = self.materialization {
            materialization.insert_properties(properties);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProStatusTelemetryV1 {
    pub(crate) surface: ProSurfaceV1,
    pub(crate) access_state: Option<ProAccessStateV1>,
    pub(crate) helper_connection: ProHelperConnectionOutcomeV1,
    pub(crate) failure: Option<ProFailureBucketV1>,
}

impl ProStatusTelemetryV1 {
    pub(crate) fn new(surface: ProSurfaceV1) -> Self {
        Self {
            surface,
            access_state: None,
            helper_connection: ProHelperConnectionOutcomeV1::NotAttempted,
            failure: None,
        }
    }

    pub(crate) fn fail(&mut self, code: Option<&str>) {
        self.failure = Some(pro_failure_bucket(code));
    }

    fn insert_properties(&self, properties: &mut Map<String, Value>) {
        insert_str(properties, "status_surface", self.surface.as_str());
        insert_optional_str(
            properties,
            "access_state",
            self.access_state.map(ProAccessStateV1::as_str),
        );
        insert_str(
            properties,
            "helper_connection_outcome",
            self.helper_connection.as_str(),
        );
        insert_optional_str(
            properties,
            "status_failure_bucket",
            self.failure.map(ProFailureBucketV1::as_str),
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProBlameTelemetryV1 {
    pub(crate) target: Option<ProBlameTargetV1>,
    pub(crate) surface: ProSurfaceV1,
    pub(crate) result_count: Option<CountBucket>,
    pub(crate) has_more: Option<bool>,
    pub(crate) failure: Option<ProFailureBucketV1>,
}

impl ProBlameTelemetryV1 {
    pub(crate) fn new(target: Option<ProBlameTargetV1>, surface: ProSurfaceV1) -> Self {
        Self {
            target,
            surface,
            result_count: None,
            has_more: None,
            failure: None,
        }
    }

    pub(crate) fn complete(&mut self, result_count: usize, has_more: bool) {
        self.result_count = Some(count_bucket(result_count as u64));
        self.has_more = Some(has_more);
        self.failure = None;
    }

    pub(crate) fn fail(&mut self, code: Option<&str>) {
        self.failure = Some(pro_failure_bucket(code));
    }

    fn insert_properties(&self, properties: &mut Map<String, Value>) {
        insert_optional_str(
            properties,
            "blame_target_kind",
            self.target.map(ProBlameTargetV1::as_str),
        );
        insert_str(properties, "blame_surface", self.surface.as_str());
        insert_optional_count(properties, "blame_result_count_bucket", self.result_count);
        insert_optional_bool(properties, "blame_has_more", self.has_more);
        insert_optional_str(
            properties,
            "blame_failure_bucket",
            self.failure.map(ProFailureBucketV1::as_str),
        );
    }
}

pub(crate) fn pro_failure_bucket(code: Option<&str>) -> ProFailureBucketV1 {
    match code {
        Some("commercial_unavailable") => ProFailureBucketV1::Commercial,
        Some("pro_not_installed" | "helper_upgrade_required") => ProFailureBucketV1::Installation,
        Some("entitlement_expired") => ProFailureBucketV1::Authorization,
        Some("key_store_unavailable" | "key_store_locked") => ProFailureBucketV1::KeyStore,
        Some("protocol_mismatch" | "corrupt_graph") => ProFailureBucketV1::Protocol,
        Some("source_unavailable") => ProFailureBucketV1::Source,
        Some("repository_unavailable") => ProFailureBucketV1::Repository,
        Some("stale_fact" | "stale_snapshot") => ProFailureBucketV1::Stale,
        Some("line_out_of_range") => ProFailureBucketV1::InvalidRequest,
        Some("ambiguous") => ProFailureBucketV1::Ambiguous,
        Some("invalid_request") => ProFailureBucketV1::InvalidRequest,
        Some("invalid_response") => ProFailureBucketV1::InvalidResponse,
        Some("cancelled") => ProFailureBucketV1::Cancelled,
        Some("helper_crashed") => ProFailureBucketV1::HelperCrashed,
        Some("helper_timeout") => ProFailureBucketV1::HelperTimeout,
        Some("not_materialized" | "needs_rebuild" | "partial" | "needs_resume") => {
            ProFailureBucketV1::Source
        }
        Some(_) | None => ProFailureBucketV1::Other,
    }
}

pub(crate) fn pro_helper_connection_outcome(code: Option<&str>) -> ProHelperConnectionOutcomeV1 {
    match code {
        Some("pro_not_installed") => ProHelperConnectionOutcomeV1::NotInstalled,
        Some("entitlement_expired" | "key_store_unavailable" | "key_store_locked") => {
            ProHelperConnectionOutcomeV1::AuthorizationFailed
        }
        Some("helper_upgrade_required" | "protocol_mismatch" | "invalid_response") => {
            ProHelperConnectionOutcomeV1::ProtocolMismatch
        }
        Some("helper_timeout") => ProHelperConnectionOutcomeV1::TimedOut,
        Some("helper_crashed") => ProHelperConnectionOutcomeV1::Crashed,
        Some(_) | None => ProHelperConnectionOutcomeV1::Unavailable,
    }
}

pub(crate) fn send_pro_operation(
    data_root: &Path,
    operation: ProHostOperationV1,
    outcome: Outcome,
    duration: Duration,
) {
    let Some(config) = delivery_config(data_root) else {
        return;
    };
    let event = pro_operation_event(operation, outcome, duration);
    #[cfg(test)]
    {
        let _ = (config, event);
    }
    #[cfg(not(test))]
    {
        super::send_batch(data_root, &config, &[event]);
    }
}

pub(crate) fn pro_operation_event(
    operation: ProHostOperationV1,
    outcome: Outcome,
    duration: Duration,
) -> PublicEventV1 {
    PublicEventV1::OperationCompleted(OperationCompletedV1::for_pro_host(
        operation, outcome, duration,
    ))
}

fn delivery_config(data_root: &Path) -> Option<AppConfig> {
    AppConfig::load(data_root)
        .ok()
        .filter(|config| config.analytics.enabled)
}

fn insert_str(properties: &mut Map<String, Value>, key: &'static str, value: &'static str) {
    properties.insert(key.to_owned(), Value::String(value.to_owned()));
}

fn insert_optional_str(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<&'static str>,
) {
    if let Some(value) = value {
        insert_str(properties, key, value);
    }
}

fn insert_optional_bool(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<bool>,
) {
    if let Some(value) = value {
        properties.insert(key.to_owned(), Value::Bool(value));
    }
}

fn insert_optional_count(
    properties: &mut Map<String, Value>,
    key: &'static str,
    value: Option<CountBucket>,
) {
    insert_optional_str(properties, key, value.map(CountBucket::as_str));
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn blame_properties_are_closed_bucketed_and_content_free() {
        let mut blame =
            ProBlameTelemetryV1::new(Some(ProBlameTargetV1::PullRequest), ProSurfaceV1::Mcp);
        blame.complete(3, true);

        let mut properties = Map::new();
        ProHostOperationV1::Blame(blame).insert_properties(&mut properties);
        assert_eq!(properties["blame_target_kind"], "pull_request");
        assert_eq!(properties["blame_surface"], "mcp");
        assert_eq!(properties["blame_result_count_bucket"], "2-5");
        assert_eq!(properties["blame_has_more"], true);
        assert!(properties
            .values()
            .all(|value| value.is_string() || value.is_boolean()));
        assert_eq!(properties.len(), 4);
        for forbidden in [
            "selector",
            "repository",
            "path",
            "session",
            "checkpoint",
            "frontier",
            "sequence",
            "deadline",
            "token",
            "url",
            "error",
        ] {
            assert!(!properties.keys().any(|key| key.contains(forbidden)));
        }
    }

    #[test]
    fn status_properties_are_separate_from_blame() {
        let mut status = ProStatusTelemetryV1::new(ProSurfaceV1::Mcp);
        status.access_state = Some(ProAccessStateV1::Trial);
        status.helper_connection = ProHelperConnectionOutcomeV1::Connected;

        let mut properties = Map::new();
        ProHostOperationV1::Status(status).insert_properties(&mut properties);
        assert_eq!(properties["status_surface"], "mcp");
        assert_eq!(properties["access_state"], "trial");
        assert_eq!(properties["helper_connection_outcome"], "connected");
        assert!(!properties.keys().any(|key| key.starts_with("blame_")));
    }

    #[test]
    fn failures_collapse_to_closed_buckets_without_raw_details() {
        assert_eq!(
            pro_failure_bucket(Some("entitlement_expired")),
            ProFailureBucketV1::Authorization
        );
        assert_eq!(
            pro_failure_bucket(Some("unknown_private_detail")),
            ProFailureBucketV1::Other
        );
        assert_eq!(
            pro_helper_connection_outcome(Some("helper_timeout")),
            ProHelperConnectionOutcomeV1::TimedOut
        );
        assert_eq!(
            pro_helper_connection_outcome(None),
            ProHelperConnectionOutcomeV1::Unavailable
        );
    }

    #[test]
    fn materialization_outcomes_distinguish_noop_replay_and_mixed_commits() {
        let mut no_op = ProMaterializationTelemetryV1::started();
        no_op.mode = Some(ProMaterializationModeV1::Incremental);
        no_op.complete(1, 0, 0, 1, 0);
        assert_eq!(no_op.mode, Some(ProMaterializationModeV1::NoOp));
        assert_eq!(no_op.commit, ProCommitOutcomeV1::NoOp);

        let mut replayed = ProMaterializationTelemetryV1::started();
        replayed.complete(2, 5, 5, 2, 5);
        assert_eq!(replayed.commit, ProCommitOutcomeV1::Replayed);

        let mut mixed = ProMaterializationTelemetryV1::started();
        mixed.complete(2, 5, 5, 1, 5);
        assert_eq!(mixed.commit, ProCommitOutcomeV1::Mixed);
    }

    #[test]
    fn effective_config_opt_out_stops_before_sender_identity_or_network_work() {
        let root = tempfile::tempdir().unwrap();
        fs::write(
            root.path().join(crate::config::CONFIG_FILE),
            "[analytics]\nenabled = false\n",
        )
        .unwrap();
        assert!(delivery_config(root.path()).is_none());
    }
}
