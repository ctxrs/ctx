use super::*;

pub(crate) fn ensure_shared_vm_launch_ready_for_operation(
    data_root: &Path,
    shared_vm: &AvfLinuxSharedVmStateResponse,
    operation: &str,
) -> Result<()> {
    if !matches!(shared_vm.state, AvfLinuxSharedVmLifecycleState::Running) {
        bail!(
            "shared AVF Linux VM must be running before {operation} (state={:?})",
            shared_vm.state
        );
    }
    if !matches!(
        shared_vm.transition_status,
        Some(AvfLinuxSharedVmTransitionStatus::Ready)
    ) {
        bail!(
            "shared AVF Linux VM must be launch-ready before {operation} (state={:?}, transition_status={:?})",
            shared_vm.state,
            shared_vm.transition_status
        );
    }
    if !shared_vm.simulated && !shared_vm_owner_guest_probe_ready(data_root) {
        bail!("shared AVF Linux VM must publish the guest-control ready marker before {operation}");
    }
    Ok(())
}

pub(crate) fn guest_directory_exists(data_root: &Path, guest_path: &Path) -> Result<bool> {
    let result = run_guest_exec_capture(
        &shared_vm_control_socket_path(data_root),
        Path::new("/"),
        "/usr/bin/test",
        &[String::from("-d"), guest_path.display().to_string()],
        Some("root"),
        HashMap::new(),
        None,
    )
    .with_context(|| {
        format!(
            "checking whether guest path {} exists",
            guest_path.display()
        )
    })?;
    Ok(result.exit_code == 0)
}

pub(crate) fn guest_exec(
    data_root: &Path,
    workspace_id: &str,
    worktree_id: &str,
    cwd: &Path,
    command: &str,
    env: &[String],
    user: Option<&str>,
    pty: bool,
    args: &[String],
) -> Result<i32> {
    let shared_vm = shared_vm_state(data_root)?;
    ensure_shared_vm_launch_ready_for_operation(data_root, &shared_vm, "guest exec")?;

    let metadata_path = shared_vm_worktree_metadata_path(data_root, workspace_id, worktree_id);
    let Some(worktree) = load_guest_worktree_state(&metadata_path)? else {
        bail!(
            "guest worktree metadata is missing for workspace {} worktree {}",
            workspace_id,
            worktree_id
        );
    };
    if !cwd.starts_with(&worktree.guest_root) {
        bail!(
            "guest exec cwd {} must stay under guest worktree root {}",
            cwd.display(),
            worktree.guest_root.display()
        );
    }

    let control_socket = shared_vm_control_socket_path(data_root);
    let guest_env = parse_guest_exec_env(env)?;
    let guest_user = user
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            if worktree.guest_user.trim().is_empty() {
                Some(guest_workspace_user(workspace_id))
            } else {
                Some(worktree.guest_user.clone())
            }
        });
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();
    run_guest_exec_cli(
        &control_socket,
        cwd,
        command,
        args,
        guest_user.as_deref(),
        guest_env,
        pty,
        &mut stdout,
        &mut stderr,
    )
    .with_context(|| {
        format!(
            "running AVF Linux guest exec for workspace {} worktree {}",
            workspace_id, worktree_id
        )
    })
}

pub(crate) fn shared_vm_exec(
    data_root: &Path,
    cwd: &Path,
    command: &str,
    env: &[String],
    user: Option<&str>,
    pty: bool,
    args: &[String],
) -> Result<i32> {
    let shared_vm = shared_vm_state(data_root)?;
    ensure_shared_vm_launch_ready_for_operation(data_root, &shared_vm, "shared-vm-exec")?;

    let control_socket = shared_vm_control_socket_path(data_root);
    let guest_env = parse_guest_exec_env(env)?;
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let mut stdout = stdout.lock();
    let mut stderr = stderr.lock();
    run_guest_exec_cli(
        &control_socket,
        cwd,
        command,
        args,
        user,
        guest_env,
        pty,
        &mut stdout,
        &mut stderr,
    )
    .with_context(|| format!("running shared AVF Linux guest exec `{command}`"))
}

pub(crate) fn parse_guest_exec_env(env: &[String]) -> Result<HashMap<String, String>> {
    let mut parsed = HashMap::new();
    for entry in env {
        let Some((key, value)) = entry.split_once('=') else {
            bail!("guest exec env entry must be KEY=VALUE, got `{entry}`");
        };
        let key = key.trim();
        if key.is_empty() {
            bail!("guest exec env key must not be empty");
        }
        if key.starts_with("CTX_AVF_") {
            bail!("guest exec env key `{key}` is reserved for helper control state");
        }
        parsed.insert(key.to_string(), value.to_string());
    }
    Ok(parsed)
}

pub(crate) fn guest_workspace_user(workspace_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(workspace_id.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("{GUEST_WORKSPACE_USER_PREFIX}{}", &digest[..12])
}

pub(crate) fn guest_workspace_home(guest_user: &str) -> PathBuf {
    PathBuf::from(GUEST_WORKSPACE_HOMES_ROOT).join(guest_user)
}

pub(crate) fn ensure_guest_workspace_user(data_root: &Path, guest_user: &str) -> Result<()> {
    let control_socket = shared_vm_control_socket_path(data_root);
    ensure_guest_exec_success(
        "creating guest workspace account roots",
        run_guest_exec_capture(
            &control_socket,
            Path::new("/"),
            "/bin/mkdir",
            &[
                String::from("-p"),
                GUEST_WORKSPACE_HOMES_ROOT.to_string(),
                GUEST_WORKSPACE_CACHE_ROOT.to_string(),
                GUEST_WORKSPACE_TMP_ROOT.to_string(),
            ],
            None,
            HashMap::new(),
            None,
        )?,
    )?;

    let user_exists = run_guest_exec_capture(
        &control_socket,
        Path::new("/"),
        "/usr/bin/id",
        &[String::from("-u"), guest_user.to_string()],
        None,
        HashMap::new(),
        None,
    )?
    .exit_code
        == 0;
    if !user_exists {
        ensure_guest_exec_success(
            &format!("creating guest workspace user {guest_user}"),
            run_guest_exec_capture(
                &control_socket,
                Path::new("/"),
                "/usr/sbin/useradd",
                &[
                    String::from("--create-home"),
                    String::from("--home-dir"),
                    guest_workspace_home(guest_user).display().to_string(),
                    String::from("--shell"),
                    String::from("/bin/bash"),
                    guest_user.to_string(),
                ],
                None,
                HashMap::new(),
                None,
            )?,
        )?;
    }

    ensure_guest_exec_success(
        &format!("ensuring guest workspace directories for {guest_user}"),
        run_guest_exec_capture(
            &control_socket,
            Path::new("/"),
            "/usr/bin/install",
            &[
                String::from("-d"),
                String::from("-o"),
                guest_user.to_string(),
                String::from("-g"),
                guest_user.to_string(),
                String::from("-m"),
                String::from("700"),
                guest_workspace_home(guest_user).display().to_string(),
                PathBuf::from(GUEST_WORKSPACE_CACHE_ROOT)
                    .join(guest_user)
                    .display()
                    .to_string(),
                PathBuf::from(GUEST_WORKSPACE_TMP_ROOT)
                    .join(guest_user)
                    .display()
                    .to_string(),
            ],
            None,
            HashMap::new(),
            None,
        )?,
    )
}

pub(crate) fn finalize_guest_worktree_permissions(
    data_root: &Path,
    guest_root: &Path,
    guest_user: &str,
) -> Result<()> {
    let control_socket = shared_vm_control_socket_path(data_root);
    ensure_guest_exec_success(
        &format!(
            "setting guest worktree ownership on {}",
            guest_root.display()
        ),
        run_guest_exec_capture(
            &control_socket,
            Path::new("/"),
            "/bin/chown",
            &[
                String::from("-R"),
                format!("{guest_user}:{guest_user}"),
                guest_root.display().to_string(),
            ],
            None,
            HashMap::new(),
            None,
        )?,
    )?;
    ensure_guest_exec_success(
        &format!("restricting guest worktree root {}", guest_root.display()),
        run_guest_exec_capture(
            &control_socket,
            Path::new("/"),
            "/bin/chmod",
            &[String::from("700"), guest_root.display().to_string()],
            None,
            HashMap::new(),
            None,
        )?,
    )
}
