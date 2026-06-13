use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde::Serialize;
use tokio::process::Command;

use ctx_avf_linux_runtime::{
    runtime_available as avf_linux_runtime_available, runtime_state as avf_linux_runtime_state,
    runtime_target_label as avf_linux_runtime_target_label, SharedSubstrateLifecycleManager,
    SharedVmLifecycleOrchestrator, SubstrateLifecycleRecord,
};
use ctx_harness_setup::{
    observe_log, observe_phase, HarnessSetupLogLevel, HarnessSetupObserver, HarnessSetupPhase,
};
use ctx_linux_sandbox_runtime::linux_sandbox_runtime_status;
use ctx_sandbox_container_runtime::{
    command_output_message, command_output_with_timeout,
    container_image_present as runtime_container_image_present,
    container_image_status as runtime_container_image_status, native_container_runtime_available,
    prefetch_container_image as runtime_prefetch_container_image,
    prefetch_container_image_with_observer as runtime_prefetch_container_image_with_observer,
    prefetch_container_startup_artifacts_with_observer as runtime_prefetch_container_startup_artifacts_with_observer,
    resolve_container_image as resolve_configured_container_image, sandbox_cli_invocation,
    sandbox_container_command as runtime_sandbox_container_command,
    sandbox_engine_ready as runtime_sandbox_engine_ready, ContainerImageStatus, SandboxCommandMode,
    CTX_HARNESS_SANDBOX_CLI_PATH_ENV,
};
use ctx_sandbox_contract::{
    ContainerExecutionSettings, ContainerRuntimeKind, UbuntuSandboxSubstrate,
};
use ctx_workspace_container::sandbox_machine_required;

pub mod container_builder;
#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxCommandBackend {
    NativeContainer,
    SharedVmContainer,
}

pub const CTX_HARNESS_RUNTIME_KIND_ENV: &str = "CTX_HARNESS_RUNTIME_KIND";
pub const CTX_HARNESS_LINUX_SANDBOX_ENV: &str = "CTX_HARNESS_LINUX_SANDBOX";
pub const CTX_AVF_HOST_DATA_ROOT_ENV: &str = "CTX_AVF_HOST_DATA_ROOT";
pub const CTX_AVF_WORKSPACE_ID_ENV: &str = "CTX_AVF_WORKSPACE_ID";
pub const CTX_AVF_WORKTREE_ID_ENV: &str = "CTX_AVF_WORKTREE_ID";
pub const CTX_AVF_HOST_WORKTREE_ROOT_ENV: &str = "CTX_AVF_HOST_WORKTREE_ROOT";

const CTX_SANDBOX_MACHINE_PREFIX: &str = "ctx";
const SANDBOX_MACHINE_START_TIMEOUT: Duration = Duration::from_secs(180);
const SANDBOX_MACHINE_READY_TIMEOUT: Duration = Duration::from_secs(2 * 60);

#[derive(Debug, Clone, Serialize)]
pub struct HarnessRuntimeStats {
    pub container_count: usize,
    pub container_allowlist_entries: usize,
    pub container_external_mounts: usize,
    pub container_egress_guards: usize,
}

#[derive(Debug, Clone)]
pub enum HarnessRuntimeKind {
    Host,
    NativeContainer { name: String },
    SharedVmContainer,
}

impl HarnessRuntimeKind {
    pub fn is_linux_sandbox(&self) -> bool {
        !matches!(self, Self::Host)
    }
}

#[derive(Debug, Clone)]
pub struct HarnessExecutionPlan {
    pub runtime: HarnessRuntimeKind,
    pub env_overrides: HashMap<String, String>,
}

impl HarnessExecutionPlan {
    pub fn is_linux_sandbox(&self) -> bool {
        self.runtime.is_linux_sandbox()
            || self
                .env_overrides
                .get(CTX_HARNESS_LINUX_SANDBOX_ENV)
                .is_some_and(|value| value == "1")
    }

    pub fn runtime_data_root(&self) -> Option<&Path> {
        self.env_overrides.get("CTX_DATA_ROOT").map(Path::new)
    }
}

pub fn sandbox_machine_name(data_root: &Path) -> String {
    use sha2::Digest;

    let mut hasher = sha2::Sha256::new();
    hasher.update(data_root.to_string_lossy().as_bytes());
    let digest = hasher.finalize();
    let hash = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{CTX_SANDBOX_MACHINE_PREFIX}-{hash}")
}

fn explicit_sandbox_cli_override_path() -> Option<PathBuf> {
    let raw = std::env::var(CTX_HARNESS_SANDBOX_CLI_PATH_ENV).ok()?;
    let path = PathBuf::from(raw.trim());
    if path.exists() {
        Some(path)
    } else {
        None
    }
}

pub fn selected_sandbox_command_mode(data_root: &Path) -> Result<SandboxCommandMode> {
    if explicit_sandbox_cli_override_path().is_some() {
        return Ok(SandboxCommandMode::NativeContainer);
    }
    #[cfg(target_os = "macos")]
    if avf_linux_runtime_available() {
        return Ok(SandboxCommandMode::SharedVm {
            helper_path: ctx_avf_linux_runtime::helper_path()?,
        });
    }
    if native_container_runtime_available(data_root) {
        return Ok(SandboxCommandMode::NativeContainer);
    }
    anyhow::bail!("sandbox container CLI unavailable");
}

pub fn selected_sandbox_command_backend(data_root: &Path) -> Result<SandboxCommandBackend> {
    match selected_sandbox_command_mode(data_root)? {
        SandboxCommandMode::NativeContainer => Ok(SandboxCommandBackend::NativeContainer),
        SandboxCommandMode::SharedVm { .. } => Ok(SandboxCommandBackend::SharedVmContainer),
    }
}

pub fn container_runtime_available(data_root: &Path) -> bool {
    selected_sandbox_command_mode(data_root).is_ok()
}

pub fn sandbox_container_command(data_root: &Path) -> Result<Command> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_sandbox_container_command(data_root, &mode)
}

pub async fn sandbox_engine_ready(data_root: &Path) -> Result<bool> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_sandbox_engine_ready(data_root, &mode).await
}

pub fn resolve_container_image(settings: &ContainerExecutionSettings) -> String {
    resolve_configured_container_image(settings.image.as_deref())
}

pub async fn container_image_present(data_root: &Path, image: &str) -> Result<bool> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_container_image_present(data_root, &mode, image).await
}

pub async fn container_image_status(data_root: &Path, image: &str) -> Result<ContainerImageStatus> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_container_image_status(data_root, &mode, image).await
}

pub async fn prefetch_container_startup_artifacts_with_observer(
    data_root: &Path,
    image: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_prefetch_container_startup_artifacts_with_observer(data_root, &mode, image, observer)
        .await
}

pub async fn prefetch_container_image_with_observer(
    data_root: &Path,
    image: &str,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_prefetch_container_image_with_observer(data_root, &mode, image, observer).await
}

pub async fn prefetch_container_image(data_root: &Path, image: &str) -> Result<()> {
    let mode = selected_sandbox_command_mode(data_root)?;
    runtime_prefetch_container_image(data_root, &mode, image).await
}

pub fn local_runtime_available(data_root: &Path, runtime: &ContainerRuntimeKind) -> bool {
    match runtime {
        ContainerRuntimeKind::NativeContainer => container_runtime_available(data_root),
        ContainerRuntimeKind::SharedVmContainer => avf_linux_runtime_available(),
    }
}

pub fn runtime_prewarm_target(settings: &ContainerExecutionSettings) -> String {
    match settings.runtime {
        ContainerRuntimeKind::NativeContainer => resolve_container_image(settings),
        ContainerRuntimeKind::SharedVmContainer => avf_linux_runtime_target_label(),
    }
}

pub async fn selected_runtime_state(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
) -> Result<(bool, bool)> {
    match settings.runtime {
        ContainerRuntimeKind::NativeContainer => {
            let mode = SandboxCommandMode::NativeContainer;
            let machine_ready = normalize_container_engine_ready_for_runtime(
                runtime_sandbox_engine_ready(data_root, &mode).await,
            )?;
            let image_present = if machine_ready {
                runtime_container_image_present(
                    data_root,
                    &mode,
                    &resolve_container_image(settings),
                )
                .await?
            } else {
                false
            };
            Ok((machine_ready, image_present))
        }
        ContainerRuntimeKind::SharedVmContainer => avf_linux_runtime_state(data_root),
    }
}

pub async fn prewarm_selected_runtime_with_observer(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    match settings.runtime {
        ContainerRuntimeKind::NativeContainer => {
            let image = resolve_container_image(settings);
            let mode = SandboxCommandMode::NativeContainer;
            let machine_ready = normalize_container_engine_ready_for_runtime(
                runtime_sandbox_engine_ready(data_root, &mode).await,
            )?;
            if machine_ready {
                runtime_prefetch_container_image_with_observer(data_root, &mode, &image, observer)
                    .await
            } else {
                runtime_prefetch_container_startup_artifacts_with_observer(
                    data_root, &mode, &image, observer,
                )
                .await
            }
        }
        ContainerRuntimeKind::SharedVmContainer => {
            SharedVmLifecycleOrchestrator::new(data_root)
                .prefetch_runtime(settings, observer)
                .await
        }
    }
}

pub async fn prewarm_selected_runtime_for_launch_with_observer(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    match settings.runtime {
        ContainerRuntimeKind::NativeContainer => {
            ensure_native_container_runtime_launch_ready_with_observer(
                data_root, settings, observer,
            )
            .await
        }
        ContainerRuntimeKind::SharedVmContainer => SharedVmLifecycleOrchestrator::new(data_root)
            .ensure_shared_runtime_ready(settings, observer)
            .await
            .map(|_| ()),
    }
}

pub fn selected_shared_substrate_lifecycle(
    data_root: &Path,
) -> Result<Option<SubstrateLifecycleRecord>> {
    if !matches!(
        selected_sandbox_command_backend(data_root),
        Ok(SandboxCommandBackend::SharedVmContainer)
    ) {
        return Ok(None);
    }

    let settings = ContainerExecutionSettings {
        runtime: ContainerRuntimeKind::SharedVmContainer,
        ..ContainerExecutionSettings::default()
    };
    SharedSubstrateLifecycleManager::new(data_root)
        .read_shared_runtime_lifecycle(&settings)
        .map(Some)
}

pub async fn selected_runtime_launch_ready(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
) -> Result<bool> {
    let (vm_ready, image_ready) =
        selected_runtime_launch_readiness_state(data_root, settings).await?;
    Ok(vm_ready && image_ready)
}

pub async fn selected_runtime_launch_readiness_state(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
) -> Result<(bool, bool)> {
    let substrate = UbuntuSandboxSubstrate::from_runtime_kind(settings.runtime.clone());
    match substrate.substrate {
        ctx_core::models::SandboxSubstrate::NativeContainer => {
            selected_runtime_state(data_root, settings).await
        }
        ctx_core::models::SandboxSubstrate::SharedVmContainer => {
            SharedVmLifecycleOrchestrator::new(data_root)
                .launch_readiness_state(settings)
                .await
        }
    }
}

pub fn launch_ready_gap_message(
    runtime_kind: ContainerRuntimeKind,
    runtime_target: &str,
    vm_ready: bool,
    image_ready: bool,
) -> String {
    UbuntuSandboxSubstrate::from_runtime_kind(runtime_kind).launch_ready_gap_message(
        runtime_target,
        vm_ready,
        image_ready,
    )
}

pub fn launch_ready_detail_message(runtime_kind: &ContainerRuntimeKind) -> &'static str {
    UbuntuSandboxSubstrate::from_runtime_kind(runtime_kind.clone()).launch_ready_detail_message()
}

pub fn runtime_prewarm_ready_message(
    runtime_kind: &ContainerRuntimeKind,
    launch_ready: bool,
) -> &'static str {
    UbuntuSandboxSubstrate::from_runtime_kind(runtime_kind.clone())
        .runtime_prewarm_ready_message(launch_ready)
}

pub fn workspace_launch_ready_message(runtime_kind: &ContainerRuntimeKind) -> &'static str {
    UbuntuSandboxSubstrate::from_runtime_kind(runtime_kind.clone()).workspace_launch_ready_message()
}

pub async fn ensure_builder_backend_launch_ready_with_observer(
    data_root: &Path,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    match selected_sandbox_command_backend(data_root)? {
        SandboxCommandBackend::NativeContainer => {
            let settings = ContainerExecutionSettings {
                runtime: ContainerRuntimeKind::NativeContainer,
                ..ContainerExecutionSettings::default()
            };
            ensure_native_container_runtime_engine_ready_with_observer(
                data_root, &settings, observer,
            )
            .await
        }
        SandboxCommandBackend::SharedVmContainer => {
            let settings = ContainerExecutionSettings {
                runtime: ContainerRuntimeKind::SharedVmContainer,
                ..ContainerExecutionSettings::default()
            };
            SharedSubstrateLifecycleManager::new(data_root)
                .ensure_shared_runtime_ready(&settings, observer)
                .await
                .map(|_| ())
        }
    }
}

fn normalize_container_engine_ready_for_runtime(result: Result<bool>) -> Result<bool> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => {
            let lowered = err.to_string().to_ascii_lowercase();
            if lowered.contains("sandbox container cli unavailable")
                || lowered.contains("native sandbox container runtime is unavailable")
            {
                return Ok(false);
            }
            Err(err)
        }
    }
}

async fn ensure_native_container_runtime_engine_ready_with_observer(
    data_root: &Path,
    _settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    observe_phase(
        observer,
        HarnessSetupPhase::MachineCheck,
        "checking container runtime",
    );
    if sandbox_engine_ready(data_root).await.unwrap_or(false) {
        observe_log(
            observer,
            HarnessSetupPhase::MachineCheck,
            HarnessSetupLogLevel::Info,
            "local sandbox runtime is already reachable",
        );
        return Ok(());
    }

    if sandbox_machine_required() {
        let machine_name = sandbox_machine_name(data_root);
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "starting local sandbox runtime",
        );
        let mut start = sandbox_container_command(data_root)?;
        start.arg("machine").arg("start").arg(&machine_name);
        let output = command_output_with_timeout(start, SANDBOX_MACHINE_START_TIMEOUT).await?;
        if !output.status.success() {
            let combined = command_output_message(&output);
            let combined_lc = combined.to_ascii_lowercase();
            if !(combined_lc.contains("already running")
                || combined_lc.contains("already starting")
                || combined_lc.contains("already started"))
            {
                if combined_lc.contains("not found")
                    || combined_lc.contains("does not exist")
                    || combined_lc.contains("no machine")
                {
                    anyhow::bail!(
                        "native sandbox container runtime machine '{machine_name}' is not initialized"
                    );
                }
                if combined.is_empty() {
                    anyhow::bail!(
                        "sandbox machine start failed with non-zero exit {}",
                        output.status
                    );
                }
                anyhow::bail!("sandbox machine start failed: {combined}");
            }
            observe_log(
                observer,
                HarnessSetupPhase::MachineStartOrInit,
                HarnessSetupLogLevel::Warn,
                "sandbox machine start reported an already-running state; waiting for runtime readiness",
            );
        }
        observe_phase(
            observer,
            HarnessSetupPhase::MachineStartOrInit,
            "waiting for local sandbox runtime readiness",
        );
        let deadline = Instant::now() + sandbox_machine_ready_timeout();
        loop {
            if sandbox_engine_ready(data_root).await.unwrap_or(false) {
                observe_log(
                    observer,
                    HarnessSetupPhase::MachineStartOrInit,
                    HarnessSetupLogLevel::Info,
                    "local sandbox runtime is ready",
                );
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!(
                    "native sandbox container runtime did not become reachable after starting machine '{machine_name}'"
                );
            }
            tokio::time::sleep(sandbox_machine_ready_poll_interval()).await;
        }
    }

    if sandbox_cli_invocation(data_root).is_err() {
        let bootstrap = linux_sandbox_runtime_status(data_root).await?;
        anyhow::bail!("{}", bootstrap.message);
    }

    let bootstrap = linux_sandbox_runtime_status(data_root).await?;
    anyhow::bail!("{}", bootstrap.message)
}

async fn ensure_native_container_runtime_launch_ready_with_observer(
    data_root: &Path,
    settings: &ContainerExecutionSettings,
    observer: Option<&dyn HarnessSetupObserver>,
) -> Result<()> {
    ensure_native_container_runtime_engine_ready_with_observer(data_root, settings, observer)
        .await?;
    let image = resolve_container_image(settings);
    prefetch_container_image_with_observer(data_root, &image, observer).await
}

fn sandbox_machine_ready_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_millis(300)
    } else {
        SANDBOX_MACHINE_READY_TIMEOUT
    }
}

fn sandbox_machine_ready_poll_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(25)
    } else {
        Duration::from_secs(1)
    }
}
