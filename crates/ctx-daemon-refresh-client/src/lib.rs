//! Process-neutral client state machine for daemon-owned source refresh.

use std::{
    fmt,
    path::Path,
    time::{Duration as StdDuration, Instant as StdInstant},
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_history_capture::CaptureError;
use ctx_history_refresh::{
    explicit_catalog_request_is_accounted_for, optional_generation,
    published_refresh_receipt_for_index, ExplicitSourceCatalogAuthority, RefreshIntent,
    RefreshOutcomeClass, RefreshRequest, RefreshRequestState, RefreshRequestTrigger,
    RefreshSelection, RefreshStatus, RefreshStatusKind, RefreshTerminalOutcome,
    SourceBackedPublicationMetadata, SourceBackedRefreshReceipt,
};
use serde_json::{json, Value};
use uuid::Uuid;

#[cfg(test)]
use ctx_history_refresh::{
    RefreshLogicalPhase, SourceBackedRefreshCurrent, SourceBackedRefreshRouteResult,
};

mod client;
mod observation_recovery;
mod refresh_mode;
mod request;
mod types;

use request::SourceBackedRefreshRequest;

pub use client::{
    coordinate_import_source_backed_refresh_with_progress,
    coordinate_setup_source_backed_refresh_with_progress, coordinate_source_backed_refresh,
    coordinate_source_backed_refresh_with_progress,
};
pub(crate) use observation_recovery::retained_request_unobservable;
pub use refresh_mode::SourceBackedRefreshMode;
pub use types::{
    SourceBackedRefreshDaemonUnavailable, SourceBackedRefreshObservation,
    SourceBackedRefreshPendingPublication, SourceBackedRefreshTerminalError,
};

const SOURCE_REFRESH_REQUEST_OP: &str = "source_refresh_request";
const SOURCE_REFRESH_STATUS_OP: &str = "source_refresh_status";
const SOURCE_REFRESH_UNKNOWN_REQUEST_STATE: &str = "request_unknown";
const SOURCE_REFRESH_UNKNOWN_REQUEST_ERROR_CODE: &str = "source_refresh_request_unknown";
const SOURCE_REFRESH_POLL_INTERVAL: StdDuration = StdDuration::from_millis(50);
const SOURCE_REFRESH_IPC_TIMEOUT: StdDuration = StdDuration::from_secs(2);
const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRefreshDaemonAvailability {
    Available,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceRefreshDaemonDemand {
    Background,
    ExplicitWait,
}

/// The sole lower-boundary seam for daemon availability, IPC, and generation
/// retention. Implementations own process startup and transport details.
pub trait SourceRefreshClientHost: Sync {
    fn ensure_available(
        &self,
        data_root: &Path,
        trigger: RefreshRequestTrigger,
        demand: SourceRefreshDaemonDemand,
    ) -> Result<SourceRefreshDaemonAvailability>;

    fn checkpoint(&self) -> Result<()> {
        Ok(())
    }

    fn interrupted(&self, _error: &anyhow::Error) -> bool {
        false
    }

    fn pause(&self, duration: StdDuration) -> Result<()> {
        self.checkpoint()?;
        std::thread::sleep(duration);
        self.checkpoint()
    }

    fn source_refresh_request(
        &self,
        data_root: &Path,
        request: Value,
        timeout: StdDuration,
        max_response_bytes: u64,
    ) -> Result<Option<Value>>;

    fn pin_published_generation(
        &self,
        data_root: &Path,
    ) -> Result<Option<PinnedSourceBackedGeneration>>;

    fn pin_active_verified_generation(
        &self,
        data_root: &Path,
    ) -> Result<PinnedSourceBackedGeneration>;

    fn pin_retained_generation(
        &self,
        data_root: &Path,
        generation_id: &str,
    ) -> Result<PinnedSourceBackedGeneration>;
}

#[derive(Debug)]
pub struct SourceRefreshTransportUnavailable {
    request_may_have_been_submitted: bool,
}

impl SourceRefreshTransportUnavailable {
    pub fn new(request_may_have_been_submitted: bool) -> Self {
        Self {
            request_may_have_been_submitted,
        }
    }

    pub fn request_may_have_been_submitted(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<Self>()
            .is_some_and(|unavailable| unavailable.request_may_have_been_submitted)
    }
}

impl fmt::Display for SourceRefreshTransportUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("daemon source refresh service is unavailable")
    }
}

impl std::error::Error for SourceRefreshTransportUnavailable {}

pub struct PinnedSourceBackedGeneration(ctx_history_refresh::PinnedSourceBackedGeneration);

impl PinnedSourceBackedGeneration {
    #[doc(hidden)]
    pub fn from_refresh_pin(pin: ctx_history_refresh::PinnedSourceBackedGeneration) -> Self {
        Self(pin)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn from_index(index: ctx_history_index::VerifiedIndex) -> Self {
        Self(ctx_history_refresh::PinnedSourceBackedGeneration::from_index(index))
    }

    pub fn generation_id(&self) -> &str {
        self.0.generation_id()
    }

    pub fn semantic_eligible_event_count(&self) -> Result<u64> {
        self.0.semantic_eligible_event_count()
    }

    pub fn verified_index(&self) -> &ctx_history_index::VerifiedIndex {
        self.0.verified_index()
    }

    pub fn into_index(self) -> ctx_history_index::VerifiedIndex {
        self.0.into_index()
    }
}

pub(crate) fn published_refresh_receipt(
    response: &Value,
    pin: &PinnedSourceBackedGeneration,
) -> Result<SourceBackedRefreshReceipt> {
    ctx_history_refresh::published_refresh_receipt(response, &pin.0)
}

pub(crate) fn compact_json(mut value: Value) -> Value {
    prune_null_json(&mut value);
    value
}

fn prune_null_json(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, nested| {
                prune_null_json(nested);
                !nested.is_null()
            });
        }
        Value::Array(items) => {
            for item in items {
                prune_null_json(item);
            }
        }
        _ => {}
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    #[derive(Clone)]
    pub struct RefreshClientTestPolicy {
        pub intent: ctx_history_refresh::RefreshIntent,
        pub trigger: ctx_history_refresh::RefreshRequestTrigger,
        pub allow_daemon_autostart: bool,
    }

    impl RefreshClientTestPolicy {
        pub fn import(
            selection: ctx_history_refresh::RefreshSelection,
            allow_daemon_autostart: bool,
        ) -> Self {
            Self {
                intent: ctx_history_refresh::RefreshIntent::SelectedImport(selection),
                trigger: ctx_history_refresh::RefreshRequestTrigger::Import,
                allow_daemon_autostart,
            }
        }
    }

    pub use crate::{
        PinnedSourceBackedGeneration, SourceRefreshClientHost, SourceRefreshDaemonAvailability,
        SourceRefreshDaemonDemand, SourceRefreshTransportUnavailable,
    };

    pub use crate::observation_recovery::SourceRefreshObservationRecoveryFailed;
    pub use crate::observation_recovery::DISCONNECT_POLICY as SOURCE_REFRESH_DISCONNECT_POLICY;

    pub use crate::client::SourceRefreshAdmissionRecoveryFailed;

    pub const AMBIGUOUS_ADMISSION_RECOVERY_ATTEMPT_LIMIT: usize = 3;
    pub const SOURCE_REFRESH_REQUEST_OP: &str = crate::SOURCE_REFRESH_REQUEST_OP;
    pub const SOURCE_REFRESH_RESPONSE_MAX_BYTES: u64 = crate::SOURCE_REFRESH_RESPONSE_MAX_BYTES;
    pub const SOURCE_REFRESH_STATUS_OP: &str = crate::SOURCE_REFRESH_STATUS_OP;

    pub fn recover_wait_refresh_request(
        host: &dyn crate::SourceRefreshClientHost,
        data_root: &std::path::Path,
        request_id: &str,
        trigger: ctx_history_refresh::RefreshRequestTrigger,
        allow_daemon_autostart: bool,
    ) -> anyhow::Result<String> {
        crate::client::recover_wait_refresh_request(
            host,
            data_root,
            request_id,
            trigger,
            allow_daemon_autostart,
        )
    }

    pub fn source_refresh_request_is_unknown(
        response: &serde_json::Value,
        expected_request_id: &str,
    ) -> anyhow::Result<bool> {
        crate::client::source_refresh_request_is_unknown(response, expected_request_id)
    }

    pub fn validate_source_refresh_status_response_authority(
        response: &serde_json::Value,
        expected_request_id: &str,
    ) -> anyhow::Result<()> {
        crate::client::validate_source_refresh_status_response_authority(
            response,
            expected_request_id,
        )
    }

    pub fn wait_for_published_generation(
        host: &dyn crate::SourceRefreshClientHost,
        data_root: &std::path::Path,
        request_id: String,
        mode: crate::SourceBackedRefreshMode,
        operation: ctx_history_refresh::RefreshOperation,
        expected_catalog: Option<&ctx_history_refresh::ExplicitSourceCatalogAuthority>,
        allow_daemon_autostart: bool,
    ) -> anyhow::Result<crate::SourceBackedRefreshObservation> {
        crate::client::wait_for_published_generation(
            host,
            data_root,
            request_id,
            mode,
            operation,
            expected_catalog,
            allow_daemon_autostart,
        )
    }

    pub fn coordinate_source_backed_refresh_with_policy(
        host: &dyn crate::SourceRefreshClientHost,
        data_root: &std::path::Path,
        mode: crate::SourceBackedRefreshMode,
        policy: RefreshClientTestPolicy,
    ) -> anyhow::Result<crate::SourceBackedRefreshObservation> {
        crate::client::coordinate_source_backed_refresh_with_test_policy(
            host, data_root, mode, policy,
        )
    }

    pub fn enqueue_equivalent_wait_refresh_request(
        host: &dyn crate::SourceRefreshClientHost,
        data_root: &std::path::Path,
        request_id: &str,
        intent: ctx_history_refresh::RefreshIntent,
        trigger: ctx_history_refresh::RefreshRequestTrigger,
    ) -> anyhow::Result<String> {
        crate::client::enqueue_equivalent_wait_refresh_request(
            host, data_root, request_id, intent, trigger,
        )
    }

    pub fn wait_authority_request_json(
        mode: crate::SourceBackedRefreshMode,
        request: &ctx_history_refresh::RefreshRequest,
    ) -> anyhow::Result<serde_json::Value> {
        crate::client::test_wait_authority_request_json(mode, request)
    }
}
