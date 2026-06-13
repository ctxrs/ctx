use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use ctx_core::ids::WorkspaceId;
use ctx_core::models::{Workspace, Worktree};
use ctx_harness_setup::{observe_log, observe_phase, HarnessSetupObserver, HarnessSetupPhase};
use ctx_sandbox_container_runtime::{
    command_output_message, command_output_with_timeout, container_exists, container_image_present,
    container_running, ensure_container_image_available, ensure_workspace_volume,
    force_reload_default_container_image, is_default_container_image, resolve_container_image,
    sandbox_container_command, SandboxCommandMode,
};
use ctx_sandbox_contract::{
    ContainerExecutionSettings, ContainerMountMode, ContainerNetworkMode, ContainerRuntimeKind,
};
use serde::Serialize;
use tokio::process::Command;
use tokio::sync::Mutex;

mod allowlist;
mod container;
mod lifecycle;
mod network_policy_transition;

pub use container::{
    bind_mount, build_mounts, container_data_root, container_user, daemon_port_from_url,
    rewrite_daemon_url_for_avf_guest, rewrite_daemon_url_for_container, sandbox_machine_required,
    should_mount_bundle_dir_in_container, should_use_keep_id_userns, workspace_container_hostname,
    MountPlan, AVF_GUEST_HOST_GATEWAY, CONTAINER_TERMINAL_HOME, CONTAINER_TERMINAL_USER,
};
pub use network_policy_transition::{
    apply_container_network_policy, transparent_proxy_policy, AppliedContainerNetworkPolicy,
};

const SANDBOX_OP_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct WorkspaceContainer {
    pub name: String,
    pub mount_mode: ContainerMountMode,
    pub network_mode: ContainerNetworkMode,
    pub allowlist: Vec<String>,
    pub external_mounts: HashSet<String>,
    pub egress_guard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachedContainerAction {
    Reuse,
    Reconfigure,
    Recreate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceContainerReadiness {
    MachineReady,
    RuntimeReady,
}

pub struct EnsureWorkspaceContainerRequest<'a> {
    pub workspace: &'a Workspace,
    pub worktree: Option<&'a Worktree>,
    pub settings: &'a ContainerExecutionSettings,
    pub daemon_host: &'a str,
    pub daemon_port: u16,
    pub observer: Option<&'a dyn HarnessSetupObserver>,
    pub readiness: WorkspaceContainerReadiness,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceContainerStatus {
    pub name: String,
    pub running: bool,
    pub known: bool,
    pub mount_mode: Option<ContainerMountMode>,
    pub network_mode: Option<ContainerNetworkMode>,
    pub allowlist: Vec<String>,
    pub egress_guard: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WorkspaceContainerStats {
    pub container_count: usize,
    pub container_allowlist_entries: usize,
    pub container_external_mounts: usize,
    pub container_egress_guards: usize,
}

pub struct WorkspaceContainerOwner {
    data_root: PathBuf,
    containers: Mutex<HashMap<WorkspaceId, WorkspaceContainer>>,
}

pub fn workspace_container_name(workspace_id: WorkspaceId) -> String {
    format!("ctx-harness-{}", workspace_id.0)
}

pub async fn list_running_workspace_container_names(
    data_root: &std::path::Path,
    mode: &SandboxCommandMode,
) -> Result<Vec<String>> {
    let mut cmd = sandbox_container_command(data_root, mode)?;
    cmd.arg("container")
        .arg("ls")
        .arg("--format")
        .arg("{{.Names}}");
    let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
    if !output.status.success() {
        let combined = command_output_message(&output);
        if combined.is_empty() {
            anyhow::bail!(
                "container list failed while probing running workspace containers (status: {})",
                output.status
            );
        }
        anyhow::bail!(
            "container list failed while probing running workspace containers: {combined}"
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.starts_with("ctx-harness-"))
        .map(ToOwned::to_owned)
        .collect())
}

pub fn cached_container_action(
    cached: &WorkspaceContainer,
    settings: &ContainerExecutionSettings,
    external_mounts: &HashSet<String>,
) -> CachedContainerAction {
    if cached.mount_mode != settings.mount_mode || cached.external_mounts != *external_mounts {
        return CachedContainerAction::Recreate;
    }
    if cached.network_mode != settings.network_mode || cached.allowlist != settings.allowlist {
        return CachedContainerAction::Reconfigure;
    }
    CachedContainerAction::Reuse
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SandboxContainerLaunchNetworking {
    network: Option<&'static str>,
    add_host: &'static str,
}

fn sandbox_container_launch_networking(
    settings: &ContainerExecutionSettings,
) -> SandboxContainerLaunchNetworking {
    SandboxContainerLaunchNetworking {
        network: if matches!(settings.runtime, ContainerRuntimeKind::SharedVmContainer) {
            None
        } else {
            Some("slirp4netns:allow_host_loopback=true")
        },
        add_host: "host.containers.internal:host-gateway",
    }
}

fn append_sandbox_container_launch_network_args(
    cmd: &mut Command,
    settings: &ContainerExecutionSettings,
) {
    let networking = sandbox_container_launch_networking(settings);
    if let Some(network) = networking.network {
        cmd.arg("--network").arg(network);
    }
    cmd.arg("--cap-add").arg("NET_ADMIN");
    cmd.arg("--add-host").arg(networking.add_host);
}

impl WorkspaceContainerOwner {
    pub fn new(data_root: PathBuf) -> Self {
        Self {
            data_root,
            containers: Mutex::new(HashMap::new()),
        }
    }

    pub async fn stats(&self) -> WorkspaceContainerStats {
        let containers = self.containers.lock().await;
        let mut stats = WorkspaceContainerStats {
            container_count: containers.len(),
            ..WorkspaceContainerStats::default()
        };
        for container in containers.values() {
            stats.container_allowlist_entries += container.allowlist.len();
            stats.container_external_mounts += container.external_mounts.len();
            if container.egress_guard {
                stats.container_egress_guards += 1;
            }
        }
        stats
    }

    pub async fn put_cached_container_for_test(
        &self,
        workspace_id: WorkspaceId,
        container: WorkspaceContainer,
    ) {
        self.containers.lock().await.insert(workspace_id, container);
    }

    pub async fn workspace_container_exists(
        &self,
        mode: &SandboxCommandMode,
        workspace_id: WorkspaceId,
    ) -> Result<bool> {
        container_exists(
            &self.data_root,
            mode,
            &workspace_container_name(workspace_id),
        )
        .await
    }

    pub async fn container_status(
        &self,
        mode: &SandboxCommandMode,
        workspace_id: WorkspaceId,
    ) -> Result<Option<WorkspaceContainerStatus>> {
        let name = workspace_container_name(workspace_id);
        let present = container_exists(&self.data_root, mode, &name).await?;
        if !present {
            return Ok(None);
        }
        let running = container_running(&self.data_root, mode, &name)
            .await?
            .unwrap_or(false);
        let cached = {
            let containers = self.containers.lock().await;
            containers.get(&workspace_id).cloned()
        };
        let (known, mount_mode, network_mode, allowlist, egress_guard) =
            if let Some(container) = cached {
                (
                    true,
                    Some(container.mount_mode),
                    Some(container.network_mode),
                    container.allowlist,
                    Some(container.egress_guard),
                )
            } else {
                (false, None, None, Vec::new(), None)
            };
        Ok(Some(WorkspaceContainerStatus {
            name,
            running,
            known,
            mount_mode,
            network_mode,
            allowlist,
            egress_guard,
        }))
    }

    pub async fn running_workspace_container_names(
        &self,
        mode: &SandboxCommandMode,
    ) -> Result<Vec<String>> {
        list_running_workspace_container_names(&self.data_root, mode).await
    }

    pub async fn stop_container(
        &self,
        mode: &SandboxCommandMode,
        workspace_id: WorkspaceId,
    ) -> Result<bool> {
        let name = workspace_container_name(workspace_id);
        let present = container_exists(&self.data_root, mode, &name).await?;
        if !present {
            self.containers.lock().await.remove(&workspace_id);
            return Ok(false);
        }

        self.containers.lock().await.remove(&workspace_id);
        let mut cmd = sandbox_container_command(&self.data_root, mode)?;
        cmd.arg("rm").arg("-f").arg(&name);
        let output = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
        if output.status.success() {
            return Ok(true);
        }
        let combined = command_output_message(&output);
        if combined.is_empty() {
            anyhow::bail!("container rm failed for {name} (status: {})", output.status);
        }
        anyhow::bail!("container rm failed for {name}: {combined}");
    }

    pub async fn remove_workspace_volume(
        &self,
        mode: &SandboxCommandMode,
        workspace_id: WorkspaceId,
    ) -> Result<bool> {
        let name = format!("ctx-ws-{}", workspace_id.0);
        let mut inspect = sandbox_container_command(&self.data_root, mode)?;
        inspect.arg("volume").arg("inspect").arg(&name);
        let out = command_output_with_timeout(inspect, SANDBOX_OP_TIMEOUT).await?;
        if !out.status.success() {
            return Ok(false);
        }

        let mut cmd = sandbox_container_command(&self.data_root, mode)?;
        cmd.arg("volume").arg("rm").arg("-f").arg(&name);
        let out = command_output_with_timeout(cmd, SANDBOX_OP_TIMEOUT).await?;
        if out.status.success() {
            return Ok(true);
        }
        let combined = command_output_message(&out);
        if combined.is_empty() {
            anyhow::bail!(
                "container volume rm failed for {name} (status: {})",
                out.status
            );
        }
        anyhow::bail!("container volume rm failed for {name}: {combined}");
    }
}

#[cfg(test)]
mod tests;
