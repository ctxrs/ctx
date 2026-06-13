use std::path::Path;

use anyhow::{anyhow, bail, Result};
use ctx_core::ids::SandboxInstanceId;
use ctx_core::models::SandboxSubstrate;
use serde::Serialize;

use crate::{
    probe_helper, shared_vm_is_launch_ready, AvfLinuxSharedVmLifecycleState,
    AvfLinuxSharedVmStartOutcome, AvfLinuxSharedVmState, AvfLinuxSharedVmStopOutcome,
    ContainerExecutionSettings, HarnessSetupObserver, SharedVmLifecycleOrchestrator,
    SubstrateShutdownOutcome, SubstrateShutdownReason, SubstrateStartupOutcome,
    SubstrateStartupReason, SubstrateStartupSelection, UbuntuSandboxSubstrate,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SubstrateLifecycleRecord {
    pub substrate: SandboxSubstrate,
    pub startup_selection: Option<SubstrateStartupSelection>,
    pub startup_outcome: Option<SubstrateStartupOutcome>,
    pub startup_reason: Option<SubstrateStartupReason>,
    pub shutdown_outcome: Option<SubstrateShutdownOutcome>,
    pub shutdown_reason: Option<SubstrateShutdownReason>,
    pub restore_attempted: bool,
    pub restore_error_present: bool,
    pub save_error_present: bool,
    pub saved_state_written_on_shutdown: bool,
    pub simulated: bool,
}

pub struct SharedSubstrateLifecycleManager<'a> {
    data_root: &'a Path,
}

impl<'a> SharedSubstrateLifecycleManager<'a> {
    pub fn new(data_root: &'a Path) -> Self {
        Self { data_root }
    }

    pub fn read_shared_runtime_lifecycle(
        &self,
        settings: &ContainerExecutionSettings,
    ) -> Result<SubstrateLifecycleRecord> {
        let substrate = UbuntuSandboxSubstrate::from_runtime_kind(settings.runtime.clone());
        substrate.ensure_enabled()?;
        if !substrate.is_shared_vm_backed() {
            bail!(
                "shared substrate lifecycle manager only supports the shared VM container runtime"
            );
        }

        let sandbox_instance_id = SandboxInstanceId(uuid::Uuid::nil());
        let orchestrator = SharedVmLifecycleOrchestrator::new(self.data_root);
        let state = orchestrator.workspace_runtime_state(sandbox_instance_id)?;
        current_lifecycle_record_from_state(substrate.substrate, &state)
    }

    pub async fn ensure_shared_runtime_ready(
        &self,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<SubstrateLifecycleRecord> {
        let substrate = UbuntuSandboxSubstrate::from_runtime_kind(settings.runtime.clone());
        substrate.ensure_enabled()?;
        if !substrate.is_shared_vm_backed() {
            bail!(
                "shared substrate lifecycle manager only supports the shared VM container runtime"
            );
        }

        let sandbox_instance_id = SandboxInstanceId(uuid::Uuid::nil());
        let orchestrator = SharedVmLifecycleOrchestrator::new(self.data_root);
        let state = orchestrator.workspace_runtime_state(sandbox_instance_id)?;
        let current = current_lifecycle_record_from_state(substrate.substrate, &state)?;
        let startup_selection = current
            .startup_selection
            .ok_or_else(|| anyhow!("shared VM substrate startup selection is unavailable"))?;
        if matches!(startup_selection, SubstrateStartupSelection::Reuse) {
            return Ok(current);
        }

        let started = orchestrator
            .ensure_shared_runtime_ready(settings, observer)
            .await?;
        Ok(build_lifecycle_record(
            substrate.substrate,
            Some(startup_selection),
            Some(startup_outcome_from_state(&started)?),
            &started,
        ))
    }

    pub async fn ensure_workspace_runtime_ready(
        &self,
        sandbox_instance_id: SandboxInstanceId,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<SubstrateLifecycleRecord> {
        self.ensure_shared_vm_runtime_ready(sandbox_instance_id, settings, observer)
            .await
    }

    pub async fn save_or_stop_shared_runtime(
        &self,
        settings: &ContainerExecutionSettings,
    ) -> Result<SubstrateLifecycleRecord> {
        let substrate = UbuntuSandboxSubstrate::from_runtime_kind(settings.runtime.clone());
        substrate.ensure_enabled()?;
        if !substrate.is_shared_vm_backed() {
            bail!(
                "shared substrate lifecycle manager only supports the shared VM container runtime"
            );
        }

        let sandbox_instance_id = SandboxInstanceId(uuid::Uuid::nil());
        let orchestrator = SharedVmLifecycleOrchestrator::new(self.data_root);
        let state = orchestrator.workspace_runtime_state(sandbox_instance_id)?;
        if matches!(
            state.state,
            AvfLinuxSharedVmLifecycleState::Missing | AvfLinuxSharedVmLifecycleState::Stopped
        ) {
            return Ok(build_lifecycle_record(
                substrate.substrate,
                None,
                None,
                &state,
            ));
        }

        let stopped = orchestrator.save_or_stop_shared_runtime()?;
        Ok(build_lifecycle_record(
            substrate.substrate,
            None,
            None,
            &stopped,
        ))
    }

    async fn ensure_shared_vm_runtime_ready(
        &self,
        sandbox_instance_id: SandboxInstanceId,
        settings: &ContainerExecutionSettings,
        observer: Option<&dyn HarnessSetupObserver>,
    ) -> Result<SubstrateLifecycleRecord> {
        let substrate = UbuntuSandboxSubstrate::from_runtime_kind(settings.runtime.clone());
        substrate.ensure_enabled()?;
        if !substrate.is_shared_vm_backed() {
            bail!(
                "shared substrate lifecycle manager only supports the shared VM container runtime"
            );
        }

        let orchestrator = SharedVmLifecycleOrchestrator::new(self.data_root);
        let state = orchestrator.workspace_runtime_state(sandbox_instance_id)?;
        let current = current_lifecycle_record_from_state(substrate.substrate, &state)?;
        let startup_selection = current
            .startup_selection
            .ok_or_else(|| anyhow!("shared VM substrate startup selection is unavailable"))?;
        if matches!(startup_selection, SubstrateStartupSelection::Reuse) {
            return Ok(current);
        }

        let started = orchestrator
            .ensure_workspace_runtime_ready(sandbox_instance_id, settings, observer)
            .await?;
        Ok(build_lifecycle_record(
            substrate.substrate,
            Some(startup_selection),
            Some(startup_outcome_from_state(&started)?),
            &started,
        ))
    }
}

fn current_lifecycle_record_from_state(
    substrate: SandboxSubstrate,
    state: &AvfLinuxSharedVmState,
) -> Result<SubstrateLifecycleRecord> {
    let startup_selection = startup_selection_from_state(state)?;
    let startup_outcome = if matches!(startup_selection, SubstrateStartupSelection::Reuse)
        && shared_vm_is_launch_ready(state)
    {
        Some(SubstrateStartupOutcome::Reuse)
    } else {
        map_startup_outcome(state.last_start_outcome)
    };

    Ok(build_lifecycle_record(
        substrate,
        Some(startup_selection),
        startup_outcome,
        state,
    ))
}

fn startup_selection_from_state(
    state: &AvfLinuxSharedVmState,
) -> Result<SubstrateStartupSelection> {
    let restore_supported = if state.saved_state_exists {
        shared_vm_restore_supported()?
    } else {
        false
    };
    Ok(startup_selection_from_state_with_restore_support(
        state,
        restore_supported,
    ))
}

fn startup_selection_from_state_with_restore_support(
    state: &AvfLinuxSharedVmState,
    restore_supported: bool,
) -> SubstrateStartupSelection {
    if shared_vm_is_launch_ready(state) {
        return SubstrateStartupSelection::Reuse;
    }
    if state.saved_state_exists && restore_supported {
        return SubstrateStartupSelection::Restore;
    }
    SubstrateStartupSelection::ColdBoot
}

fn shared_vm_restore_supported() -> Result<bool> {
    Ok(probe_helper()?.save_restore_supported)
}

fn startup_outcome_from_state(state: &AvfLinuxSharedVmState) -> Result<SubstrateStartupOutcome> {
    map_startup_outcome(state.last_start_outcome).ok_or_else(|| {
        anyhow!(
            "shared VM substrate reached {:?} without reporting a startup outcome",
            state.state
        )
    })
}

fn map_startup_outcome(
    outcome: Option<AvfLinuxSharedVmStartOutcome>,
) -> Option<SubstrateStartupOutcome> {
    match outcome? {
        AvfLinuxSharedVmStartOutcome::AlreadyRunning => Some(SubstrateStartupOutcome::Reuse),
        AvfLinuxSharedVmStartOutcome::Restored => Some(SubstrateStartupOutcome::Restore),
        AvfLinuxSharedVmStartOutcome::ColdBoot
        | AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure => {
            Some(SubstrateStartupOutcome::ColdBoot)
        }
    }
}

fn map_shutdown_outcome(
    outcome: Option<AvfLinuxSharedVmStopOutcome>,
) -> Option<SubstrateShutdownOutcome> {
    match outcome? {
        AvfLinuxSharedVmStopOutcome::SavedStateWritten => Some(SubstrateShutdownOutcome::Saved),
        AvfLinuxSharedVmStopOutcome::ColdStop
        | AvfLinuxSharedVmStopOutcome::ColdStopSaveUnsupported => {
            Some(SubstrateShutdownOutcome::ColdStop)
        }
        AvfLinuxSharedVmStopOutcome::ColdStopAfterSaveFailure => {
            Some(SubstrateShutdownOutcome::ColdStopAfterSaveFailure)
        }
    }
}

fn map_startup_reason(
    outcome: Option<AvfLinuxSharedVmStartOutcome>,
) -> Option<SubstrateStartupReason> {
    match outcome? {
        AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure => {
            Some(SubstrateStartupReason::RestoreFailed)
        }
        AvfLinuxSharedVmStartOutcome::AlreadyRunning
        | AvfLinuxSharedVmStartOutcome::Restored
        | AvfLinuxSharedVmStartOutcome::ColdBoot => None,
    }
}

fn map_shutdown_reason(
    outcome: Option<AvfLinuxSharedVmStopOutcome>,
) -> Option<SubstrateShutdownReason> {
    match outcome? {
        AvfLinuxSharedVmStopOutcome::ColdStopSaveUnsupported => {
            Some(SubstrateShutdownReason::SaveUnsupported)
        }
        AvfLinuxSharedVmStopOutcome::ColdStopAfterSaveFailure => {
            Some(SubstrateShutdownReason::SaveFailed)
        }
        AvfLinuxSharedVmStopOutcome::SavedStateWritten | AvfLinuxSharedVmStopOutcome::ColdStop => {
            None
        }
    }
}

fn restore_attempted(state: &AvfLinuxSharedVmState) -> bool {
    matches!(
        state.last_start_outcome,
        Some(
            AvfLinuxSharedVmStartOutcome::Restored
                | AvfLinuxSharedVmStartOutcome::ColdBootAfterRestoreFailure
        )
    )
}

fn saved_state_written_on_shutdown(state: &AvfLinuxSharedVmState) -> bool {
    matches!(
        state.last_stop_outcome,
        Some(AvfLinuxSharedVmStopOutcome::SavedStateWritten)
    )
}

fn build_lifecycle_record(
    substrate: SandboxSubstrate,
    startup_selection: Option<SubstrateStartupSelection>,
    startup_outcome: Option<SubstrateStartupOutcome>,
    state: &AvfLinuxSharedVmState,
) -> SubstrateLifecycleRecord {
    SubstrateLifecycleRecord {
        substrate,
        startup_selection,
        startup_outcome,
        startup_reason: map_startup_reason(state.last_start_outcome),
        shutdown_outcome: map_shutdown_outcome(state.last_stop_outcome),
        shutdown_reason: map_shutdown_reason(state.last_stop_outcome),
        restore_attempted: restore_attempted(state),
        restore_error_present: state.last_restore_error.is_some(),
        save_error_present: state.last_save_error.is_some(),
        saved_state_written_on_shutdown: saved_state_written_on_shutdown(state),
        simulated: state.simulated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AvfLinuxSharedVmTransitionStatus;

    fn sample_state(
        state: AvfLinuxSharedVmLifecycleState,
        saved_state_exists: bool,
    ) -> AvfLinuxSharedVmState {
        let vm_root = std::path::PathBuf::from("/tmp/ctx-shared-vm");
        let logs_root = vm_root.join("logs");
        let state_path = vm_root.join("shared-vm-state.json");
        AvfLinuxSharedVmState {
            protocol_version: 1,
            protocol_schema: "ctx.avf_linux_helper.v1".to_string(),
            state,
            vm_root,
            logs_root,
            state_path,
            log_path: None,
            saved_state_path: None,
            saved_state_exists,
            runtime_root: None,
            rootfs_image: None,
            kernel_path: None,
            initrd_path: None,
            runtime_version: None,
            runtime_shape_digest: None,
            updated_at: None,
            last_started_at: None,
            last_saved_at: None,
            last_stopped_at: None,
            transition_status: matches!(state, AvfLinuxSharedVmLifecycleState::Running)
                .then_some(AvfLinuxSharedVmTransitionStatus::Ready),
            last_start_outcome: None,
            last_stop_outcome: None,
            last_restore_error: None,
            last_save_error: None,
            simulated: true,
            notes: Vec::new(),
        }
    }

    #[test]
    fn startup_selection_prefers_reuse_for_running_vm() {
        let state = sample_state(AvfLinuxSharedVmLifecycleState::Running, true);
        assert_eq!(
            startup_selection_from_state_with_restore_support(&state, true),
            SubstrateStartupSelection::Reuse
        );
    }

    #[test]
    fn startup_selection_prefers_restore_when_saved_state_is_supported() {
        let state = sample_state(AvfLinuxSharedVmLifecycleState::Stopped, true);
        assert_eq!(
            startup_selection_from_state_with_restore_support(&state, true),
            SubstrateStartupSelection::Restore
        );
    }

    #[test]
    fn startup_selection_falls_back_to_cold_boot_without_restore_support() {
        let state = sample_state(AvfLinuxSharedVmLifecycleState::Stopped, true);
        assert_eq!(
            startup_selection_from_state_with_restore_support(&state, false),
            SubstrateStartupSelection::ColdBoot
        );

        let missing_state = sample_state(AvfLinuxSharedVmLifecycleState::Missing, false);
        assert_eq!(
            startup_selection_from_state_with_restore_support(&missing_state, true),
            SubstrateStartupSelection::ColdBoot
        );
    }
}
