use std::{
    collections::BTreeSet,
    io::Read,
    path::Path,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use ctx_pro_host_protocol::{
    read_frame, write_frame, BlameResult, BlameTarget, Capability, EntitlementAccessState,
    HelloRequest, HelperEnvelope, HelperMessage, HostMessage, StatusRequest, StatusResult,
    PROTOCOL_FINGERPRINT, PROTOCOL_VERSION,
};
use serde::Serialize;
use uuid::Uuid;

use crate::analytics::{
    pro_helper_connection_outcome, ProHelperConnectionOutcomeV1, ProMaterializationModeV1,
    ProMaterializationTelemetryV1,
};

#[path = "client_support.rs"]
mod support;
use super::authorization::{
    AuthorizationProvider, EntitlementSchedule, StoredAuthorizationProvider,
};
use super::helper_command;
use super::verified_executable::VerifiedHelperExecutable;
pub(crate) use support::{default_helper_path, git_executable};
use support::{helper_executable, helper_path};

#[path = "client_errors.rs"]
mod errors;
use errors::protocol_error;
pub(super) use errors::typed_blame_diagnostic;
pub(crate) use errors::{
    blame_diagnostic, stable_error_code, stable_error_diagnostic, RESOURCE_NOT_FOUND_DIAGNOSTIC,
};

#[path = "client_output.rs"]
mod core_materialization;
pub(crate) use core_materialization::sync_core_materialization;

/// Verifies that this installation has a selectable Pro helper before callers
/// perform expensive Core index admission work. The eventual connection still
/// performs the complete executable verification and protocol handshake.
pub(crate) fn preflight_core_materialization(data_root: &Path) -> Result<()> {
    helper_path(data_root).map(drop)
}

pub(crate) fn selected_helper_artifact_sha256(data_root: &Path) -> Result<Option<String>> {
    match helper_executable(data_root) {
        Ok(helper) => Ok(Some(helper.artifact_sha256().to_owned())),
        Err(error) if stable_error_code(&error) == Some("pro_not_installed") => Ok(None),
        Err(error) => Err(error),
    }
}

#[path = "client_status.rs"]
mod client_status;
#[cfg(test)]
pub(crate) use client_status::status_with_helper_resolver;
pub(crate) use client_status::{
    smoke_helper_at_path, status, status_for_core, HelperSmoke, ProSetupRepairability, ProStatus,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(3);
const BLAME_TIMEOUT: Duration = Duration::from_secs(30);
const BATCH_TIMEOUT: Duration = Duration::from_secs(60);
const STDERR_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct MaterializeReport {
    pub(crate) schema_version: u32,
    pub(crate) payload_type: &'static str,
    pub(crate) core_generation_id: String,
    pub(crate) source_count: u64,
    pub(crate) batches: u64,
    pub(crate) observations: u64,
    pub(crate) replayed_batches: u64,
}

mod materialization;
mod operations;
mod transport;

pub(crate) use materialization::materialize;
use materialization::*;
pub(crate) use operations::blame;
#[cfg(test)]
use operations::*;
pub(crate) use transport::ProClient;

impl ProClient {
    pub(super) fn helper_artifact_sha256(&self) -> Result<&str> {
        self._execution_guard
            .as_ref()
            .map(VerifiedHelperExecutable::artifact_sha256)
            .ok_or_else(|| anyhow!("invalid_response: Pro helper execution identity is missing"))
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
