//! Neutral Protocol V4 launch and bounded byte transport for an installed companion.
//!
//! Core supplies only the installed companion executable and a typed operation. The
//! bridge clears ambient environment authority and launches the operation
//! directly. CLI and maintenance perform a bounded Protocol V4 handshake;
//! MCP's versioned invocation is its own protocol gate.
//!
//! The detached signed-envelope API remains available solely to distribution
//! and installation callers. Launch does not invoke it or consume pair identity.

mod environment;
mod error;
mod identity;
mod limits;
mod process;
mod protocol;
mod request;
mod verifier;

use std::{
    fs, io,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

pub use environment::{CompanionEnvironment, EnvironmentKey};
pub use error::BridgeError;
pub use identity::Sha256Digest;
pub use limits::{
    BridgeLimits, LimitConfiguration, MAX_ADMISSION_WAIT, MAX_ARGUMENTS, MAX_CAPTURED_WALL_TIME,
    MAX_CONCURRENT_PROCESSES, MAX_CONTROL_BYTES, MAX_ENVIRONMENT_ENTRIES, MAX_INPUT_BYTES,
    MAX_STDERR_BYTES, MAX_STDOUT_BYTES,
};
pub use process::{ExitClass, TerminationReason};
pub use protocol::{
    InstalledCompanion, ProtocolVersion, CORE_COMPANION_PROTOCOL_VERSION, CORE_PRO_PROTOCOL_VERSION,
};
pub use request::{CancellationToken, CliRequest, MaintenanceRequest, McpRequest};
pub use verifier::{
    verify_signed_managed_pair_envelope, ManagedPairExpectations, ReleaseChannel,
    SignedManagedPairComponentIdentity, SignedManagedPairIdentity, SignedManagedPairTarget,
    MANAGED_PAIR_ENVELOPE_FILENAME, MANAGED_PAIR_STATE_FILENAME,
};

use process::ProcessExit;
use protocol::parse_handshake_receipt;
use request::ProcessRequest;

const HANDSHAKE_STDOUT_BYTES: usize = 256;
const HANDSHAKE_STDERR_BYTES: usize = 4 * 1024;
const HANDSHAKE_WALL_TIME: Duration = Duration::from_secs(5);
const MAINTENANCE_WALL_TIME: Duration = Duration::from_secs(6 * 60 * 60);
const MAINTENANCE_RECEIPT: &[u8] = b"{\"accepted\":true,\"schema_version\":1}\n";

pub struct McpResponse {
    response_frame: Vec<u8>,
    outcome: Option<std::sync::mpsc::SyncSender<McpFinishOutcome>>,
    lifecycle_owner: Option<std::thread::JoinHandle<Result<(), BridgeError>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpFinishOutcome {
    WrittenAndFlushed,
    OutputFailed,
}

impl McpFinishOutcome {
    pub(crate) const fn receipt_frame(self) -> &'static [u8] {
        match self {
            Self::WrittenAndFlushed => protocol::MCP_WRITTEN_AND_FLUSHED_RECEIPT,
            Self::OutputFailed => protocol::MCP_OUTPUT_FAILED_RECEIPT,
        }
    }
}

#[derive(Debug)]
pub struct CliResponse {
    exit: ExitClass,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MaintenanceResponse {
    accepted: bool,
}

impl CliResponse {
    pub const fn exit_class(&self) -> ExitClass {
        self.exit
    }
}

impl McpResponse {
    pub fn response_frame(&self) -> &[u8] {
        &self.response_frame
    }

    pub fn stdout(&self) -> &[u8] {
        self.response_frame()
    }

    pub fn finish(mut self, outcome: McpFinishOutcome) -> Result<(), BridgeError> {
        let outcome_sent = self
            .outcome
            .take()
            .ok_or(BridgeError::WorkerFailed)?
            .send(outcome)
            .is_ok();
        let result = self
            .lifecycle_owner
            .take()
            .ok_or(BridgeError::WorkerFailed)?
            .join()
            .map_err(|_| BridgeError::WorkerFailed)?;
        if outcome_sent {
            result
        } else {
            result.and(Err(BridgeError::WorkerFailed))
        }
    }
}

impl Drop for McpResponse {
    fn drop(&mut self) {
        // Disconnect means the final client outcome is unknown. The owner
        // terminates and reaps without sending either terminal receipt. A Drop
        // implementation cannot surface teardown results; explicit `finish`
        // returns every finalization error to Core.
        self.outcome.take();
        if let Some(owner) = self.lifecycle_owner.take() {
            let _ = owner.join();
        }
    }
}

impl std::fmt::Debug for McpResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResponse")
            .field("response_frame", &self.response_frame)
            .finish_non_exhaustive()
    }
}

impl MaintenanceResponse {
    pub const fn accepted(self) -> bool {
        self.accepted
    }
}

pub struct CompanionBridge {
    limits: LimitConfiguration,
    gate: Arc<ConcurrencyGate>,
}

impl CompanionBridge {
    pub fn new(limits: BridgeLimits) -> Self {
        let limits = limits.configuration();
        Self {
            gate: Arc::new(ConcurrencyGate::new(limits.concurrent_processes)),
            limits,
        }
    }

    pub fn launch_mcp(
        &self,
        companion: &InstalledCompanion,
        request: McpRequest,
        cancellation: &CancellationToken,
    ) -> Result<McpResponse, BridgeError> {
        request.validate(self.limits)?;
        let request = request.into_process();
        request.validate(self.limits)?;
        let permit = self.prepare_mcp_launch(companion, cancellation)?;
        let output = process::launch_mcp(
            companion,
            request,
            cancellation.clone(),
            self.limits,
            permit,
        )?;
        Ok(McpResponse {
            response_frame: output.response_frame,
            outcome: Some(output.outcome),
            lifecycle_owner: Some(output.lifecycle_owner),
        })
    }

    pub fn launch_maintenance(
        &self,
        companion: &InstalledCompanion,
        request: MaintenanceRequest,
        cancellation: &CancellationToken,
    ) -> Result<MaintenanceResponse, BridgeError> {
        let request = request.into_process();
        request.validate(self.limits)?;
        let _permit = self.prepare_launch(companion, cancellation)?;
        // Maintenance is owned by the persistent Core daemon and can
        // legitimately materialize a large local corpus. Keep the same
        // process-tree containment and byte limits, but give it a dedicated
        // product-operation deadline instead of the short captured-RPC limit.
        let output = process::run_maintenance(
            companion,
            request,
            cancellation,
            self.limits,
            MAINTENANCE_WALL_TIME,
        )?;
        if output.exit != ExitClass::Success
            || output.stdout_truncated
            || output.stderr_truncated
            || output.stdout != MAINTENANCE_RECEIPT
            || !output.stderr.is_empty()
        {
            return Err(BridgeError::InvalidProtocolResponse("maintenance"));
        }
        Ok(MaintenanceResponse { accepted: true })
    }

    /// Launches a Protocol V4 CLI operation with the caller's existing standard
    /// streams inherited directly. Once admitted, the child runs until it exits
    /// or cancellation terminates its process tree.
    pub fn launch_cli(
        &self,
        companion: &InstalledCompanion,
        request: CliRequest,
        cancellation: &CancellationToken,
    ) -> Result<CliResponse, BridgeError> {
        let request = request.into_process();
        request.validate(self.limits)?;
        let _permit = self.prepare_launch(companion, cancellation)?;
        let ProcessExit { exit } = process::run_streaming(companion, request, cancellation)?;
        Ok(CliResponse { exit })
    }

    fn prepare_launch(
        &self,
        companion: &InstalledCompanion,
        cancellation: &CancellationToken,
    ) -> Result<ConcurrencyPermit, BridgeError> {
        validate_executable(companion)?;
        let permit = self
            .gate
            .acquire(cancellation, self.limits.admission_wait)?;
        if cancellation.is_cancelled() {
            return Err(BridgeError::CancelledBeforeSpawn);
        }
        self.handshake(companion, cancellation)?;
        if cancellation.is_cancelled() {
            return Err(BridgeError::CancelledBeforeSpawn);
        }
        Ok(permit)
    }

    fn prepare_mcp_launch(
        &self,
        companion: &InstalledCompanion,
        cancellation: &CancellationToken,
    ) -> Result<ConcurrencyPermit, BridgeError> {
        validate_executable(companion)?;
        let permit = self
            .gate
            .acquire(cancellation, self.limits.admission_wait)?;
        if cancellation.is_cancelled() {
            return Err(BridgeError::CancelledBeforeSpawn);
        }
        // MCP V4's versioned invocation is its protocol gate. CLI and
        // maintenance retain their separate captured handshake.
        Ok(permit)
    }

    fn handshake(
        &self,
        companion: &InstalledCompanion,
        cancellation: &CancellationToken,
    ) -> Result<(), BridgeError> {
        let request = ProcessRequest::handshake();
        let limits = handshake_limits(self.limits);
        request.validate(limits)?;
        let output = process::run_captured(companion, request, cancellation, limits)?;
        if output.exit == ExitClass::Terminated(TerminationReason::Cancelled) {
            return Err(BridgeError::CancelledBeforeSpawn);
        }
        if output.exit != ExitClass::Success
            || output.stdout_truncated
            || output.stderr_truncated
            || output.stdout.is_empty()
            || !output.stderr.is_empty()
        {
            return Err(BridgeError::HandshakeFailed {
                exit: output.exit,
                stderr: output.stderr,
                stderr_truncated: output.stderr_truncated,
            });
        }
        let observed = parse_handshake_receipt(&output.stdout)
            .ok_or(BridgeError::InvalidProtocolResponse("handshake"))?;
        if observed != CORE_COMPANION_PROTOCOL_VERSION {
            return Err(BridgeError::ProtocolMismatch {
                expected: CORE_COMPANION_PROTOCOL_VERSION,
                observed,
            });
        }
        Ok(())
    }
}

impl Default for CompanionBridge {
    fn default() -> Self {
        Self::new(BridgeLimits::default())
    }
}

fn handshake_limits(mut limits: LimitConfiguration) -> LimitConfiguration {
    limits.input_bytes = 1;
    limits.stdout_bytes = limits.stdout_bytes.min(HANDSHAKE_STDOUT_BYTES);
    limits.stderr_bytes = limits.stderr_bytes.min(HANDSHAKE_STDERR_BYTES);
    limits.captured_wall_time = limits.captured_wall_time.min(HANDSHAKE_WALL_TIME);
    limits
}

fn validate_executable(companion: &InstalledCompanion) -> Result<(), BridgeError> {
    let executable = companion.executable();
    if !executable.is_absolute() {
        return Err(BridgeError::InvalidExecutablePath);
    }
    let metadata = match fs::metadata(executable) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(BridgeError::MissingExecutable {
                path: executable.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(BridgeError::ExecutableMetadata {
                path: executable.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() {
        return Err(BridgeError::ExecutableNotFile {
            path: executable.to_path_buf(),
        });
    }
    Ok(())
}

struct ConcurrencyGate {
    maximum: usize,
    active: Mutex<usize>,
    changed: Condvar,
}

impl ConcurrencyGate {
    const fn new(maximum: usize) -> Self {
        Self {
            maximum,
            active: Mutex::new(0),
            changed: Condvar::new(),
        }
    }

    fn acquire(
        self: &Arc<Self>,
        cancellation: &CancellationToken,
        timeout: Duration,
    ) -> Result<ConcurrencyPermit, BridgeError> {
        let started = Instant::now();
        let mut active = self.active.lock().map_err(|_| BridgeError::WorkerFailed)?;
        while *active >= self.maximum {
            if cancellation.is_cancelled() {
                return Err(BridgeError::CancelledBeforeSpawn);
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(BridgeError::QueueTimeout);
            }
            let wait = Duration::from_millis(10).min(remaining);
            let (next, _) = self
                .changed
                .wait_timeout(active, wait)
                .map_err(|_| BridgeError::WorkerFailed)?;
            active = next;
        }
        *active += 1;
        Ok(ConcurrencyPermit {
            gate: Arc::clone(self),
        })
    }
}

struct ConcurrencyPermit {
    gate: Arc<ConcurrencyGate>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        if let Ok(mut active) = self.gate.active.lock() {
            *active = active.saturating_sub(1);
            self.gate.changed.notify_one();
        }
    }
}

#[cfg(test)]
mod tests;
