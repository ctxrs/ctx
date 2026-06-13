use super::*;

pub(super) fn map_state_response(
    persisted: Option<&PersistedSharedVmState>,
    data_root: &Path,
    vm_root: PathBuf,
    logs_root: PathBuf,
    state_path: PathBuf,
    log_path: PathBuf,
) -> AvfLinuxSharedVmStateResponse {
    let state = persisted
        .map(|state| state.state)
        .unwrap_or(AvfLinuxSharedVmLifecycleState::Missing);
    let effective_transition_status = persisted.and_then(|state| {
        if !state.simulated
            && matches!(state.state, AvfLinuxSharedVmLifecycleState::Running)
            && matches!(
                state.transition_status,
                Some(AvfLinuxSharedVmTransitionStatus::Ready)
            )
            && !shared_vm_owner_guest_probe_ready(data_root)
        {
            Some(AvfLinuxSharedVmTransitionStatus::Scaffolded)
        } else {
            state.transition_status
        }
    });
    let effective_notes = persisted
        .map(|state| {
            let mut notes = state.notes.clone();
            if !state.simulated
                && matches!(state.state, AvfLinuxSharedVmLifecycleState::Running)
                && matches!(
                    state.transition_status,
                    Some(AvfLinuxSharedVmTransitionStatus::Ready)
                )
                && !shared_vm_owner_guest_probe_ready(data_root)
            {
                notes.push(
                    "real shared AVF Linux VM is still waiting for guest-control readiness; launch-ready marker is absent"
                        .to_string(),
                );
            }
            notes
        })
        .unwrap_or_else(|| vec!["shared VM state has not been initialized yet".to_string()]);
    let saved_state_path = shared_vm_saved_state_path(data_root);
    AvfLinuxSharedVmStateResponse {
        protocol_version: HELPER_PROTOCOL_VERSION,
        protocol_schema: HELPER_PROTOCOL_SCHEMA,
        state,
        vm_root,
        logs_root,
        state_path,
        log_path: Some(log_path),
        saved_state_path: Some(saved_state_path.clone()),
        saved_state_exists: saved_state_path.exists(),
        runtime_root: persisted.and_then(|state| state.runtime_root.clone()),
        rootfs_image: persisted.and_then(|state| state.rootfs_image.clone()),
        kernel_path: persisted.and_then(|state| state.kernel_path.clone()),
        initrd_path: persisted.and_then(|state| state.initrd_path.clone()),
        runtime_version: persisted.and_then(|state| state.runtime_version.clone()),
        runtime_shape_digest: persisted.and_then(|state| state.runtime_shape_digest.clone()),
        writable_surface_contract_digest: persisted
            .and_then(|state| state.writable_surface_contract_digest.clone()),
        updated_at: persisted.and_then(|state| state.updated_at.clone()),
        last_started_at: persisted.and_then(|state| state.last_started_at.clone()),
        last_saved_at: persisted.and_then(|state| state.last_saved_at.clone()),
        last_stopped_at: persisted.and_then(|state| state.last_stopped_at.clone()),
        transition_status: effective_transition_status,
        last_start_outcome: persisted.and_then(|state| state.last_start_outcome),
        last_stop_outcome: persisted.and_then(|state| state.last_stop_outcome),
        last_restore_error: persisted.and_then(|state| state.last_restore_error.clone()),
        last_save_error: persisted.and_then(|state| state.last_save_error.clone()),
        relay_pid: persisted.and_then(|state| state.relay_pid),
        guest_agent_pid: persisted.and_then(|state| state.guest_agent_pid),
        simulated: persisted.map(|state| state.simulated).unwrap_or(true),
        notes: effective_notes,
    }
}

pub(super) fn default_stopped_state() -> PersistedSharedVmState {
    PersistedSharedVmState {
        state: AvfLinuxSharedVmLifecycleState::Stopped,
        guest_identity: supported_guest_identity(),
        runtime_root: None,
        rootfs_image: None,
        kernel_path: None,
        initrd_path: None,
        runtime_version: None,
        runtime_shape_digest: None,
        writable_surface_contract_digest: None,
        updated_at: Some(now_timestamp_string()),
        last_started_at: None,
        last_saved_at: None,
        last_stopped_at: None,
        transition_status: None,
        last_start_outcome: None,
        last_stop_outcome: None,
        last_restore_error: None,
        last_save_error: None,
        relay_pid: None,
        guest_agent_pid: None,
        simulated: true,
        notes: vec![
            "shared VM lifecycle scaffold is present, but actual AVF guest boot is not implemented yet".to_string(),
        ],
    }
}

pub(super) fn load_state(path: &Path) -> Result<Option<PersistedSharedVmState>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: PersistedSharedVmState =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    ensure_supported_guest_identity(parsed.guest_identity)
        .with_context(|| format!("validating {}", path.display()))?;
    Ok(Some(parsed))
}

pub(super) fn persist_state(path: &Path, state: &PersistedSharedVmState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(state).context("serializing shared VM state")?;
    fs::write(path, raw).with_context(|| format!("writing {}", path.display()))
}

pub(super) fn load_guest_worktree_state(
    path: &Path,
) -> Result<Option<PersistedGuestWorktreeState>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let parsed: PersistedGuestWorktreeState =
        serde_json::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    ensure_supported_guest_identity(parsed.guest_identity)
        .with_context(|| format!("validating {}", path.display()))?;
    Ok(Some(parsed))
}

pub(super) fn persist_guest_worktree_state(
    path: &Path,
    state: &PersistedGuestWorktreeState,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let raw = serde_json::to_vec_pretty(state).context("serializing guest worktree state")?;
    fs::write(path, raw).with_context(|| format!("writing {}", path.display()))
}

pub(super) fn map_guest_worktree_response(
    workspace_id: &str,
    worktree_id: &str,
    guest_root: PathBuf,
    guest_user: String,
    host_shadow_root: PathBuf,
    metadata_path: PathBuf,
    status: AvfLinuxGuestWorktreeStatus,
    simulated: bool,
    notes: Vec<String>,
) -> AvfLinuxGuestWorktreeResponse {
    AvfLinuxGuestWorktreeResponse {
        protocol_version: HELPER_PROTOCOL_VERSION,
        protocol_schema: HELPER_PROTOCOL_SCHEMA,
        workspace_id: workspace_id.to_string(),
        worktree_id: worktree_id.to_string(),
        guest_root,
        guest_user,
        host_shadow_root,
        metadata_path,
        status,
        simulated,
        notes,
    }
}
