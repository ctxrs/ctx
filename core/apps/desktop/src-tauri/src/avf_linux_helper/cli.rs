use super::*;

pub(super) fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("probe") if args.next().is_none() => write_json(&build_probe()?),
        Some("prepare-runtime-layout") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            ensure_no_extra_args(args)?;
            write_json(&prepare_runtime_layout(&data_root)?)
        }
        Some("shared-vm-state") | Some("workspace-vm-state") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            ensure_no_extra_args(args)?;
            write_json(&shared_vm_state(&data_root)?)
        }
        Some("start-shared-vm") | Some("start-workspace-vm") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            let runtime_root = required_path_arg(args.next(), "runtime_root")?;
            let rootfs_image = required_path_arg(args.next(), "rootfs_image")?;
            let kernel_path = required_path_arg(args.next(), "kernel_path")?;
            let initrd_path = required_path_arg(args.next(), "initrd_path")?;
            let runtime_version = required_string_arg(args.next(), "runtime_version")?;
            ensure_no_extra_args(args)?;
            write_json(&start_shared_vm(
                &data_root,
                &runtime_root,
                &rootfs_image,
                &kernel_path,
                &initrd_path,
                runtime_version,
            )?)
        }
        Some("stop-shared-vm") | Some("stop-workspace-vm") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            ensure_no_extra_args(args)?;
            write_json(&stop_shared_vm(&data_root)?)
        }
        Some("prepare-guest-worktree") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            let workspace_id = required_string_arg(args.next(), "workspace_id")?;
            let worktree_id = required_string_arg(args.next(), "worktree_id")?;
            let host_workspace_root = required_path_arg(args.next(), "host_workspace_root")?;
            let base_commit_sha = required_string_arg(args.next(), "base_commit_sha")?;
            let branch_name = required_string_arg(args.next(), "branch_name")?;
            ensure_no_extra_args(args)?;
            write_json(&prepare_guest_worktree(
                &data_root,
                &workspace_id,
                &worktree_id,
                &host_workspace_root,
                &base_commit_sha,
                &branch_name,
            )?)
        }
        Some("serve-shared-vm") | Some("serve-workspace-vm") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            ensure_no_extra_args(args)?;
            serve_shared_vm(&data_root)
        }
        Some("serve-guest-agent") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            ensure_no_extra_args(args)?;
            serve_guest_agent(&data_root)
        }
        Some("run-shared-vm") | Some("run-workspace-vm") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            ensure_no_extra_args(args)?;
            run_shared_vm(&data_root)
        }
        Some("watch-shared-vm-memory") | Some("watch-workspace-vm-memory") => {
            let data_root = required_path_arg(args.next(), "data_root")?;
            let owner_pid = required_u32_arg(args.next(), "owner_pid")?;
            ensure_no_extra_args(args)?;
            run_shared_vm_memory_watchdog(&data_root, owner_pid)
        }
        Some("guest-exec") => {
            let mut data_root = None;
            let mut workspace_id = None;
            let mut worktree_id = None;
            let mut cwd = None;
            let mut command = None;
            let mut guest_env = Vec::new();
            let mut user = None;
            let mut pty = false;
            let mut passthrough_args = Vec::new();
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--data-root" => data_root = args.next().map(PathBuf::from),
                    "--workspace-id" => workspace_id = args.next(),
                    "--worktree-id" => worktree_id = args.next(),
                    "--cwd" => cwd = args.next().map(PathBuf::from),
                    "--command" => command = args.next(),
                    "--env" => guest_env.push(required_string_arg(args.next(), "env")?),
                    "--user" => user = args.next(),
                    "--pty" => pty = true,
                    "--" => {
                        passthrough_args.extend(args);
                        break;
                    }
                    other => bail!("unknown guest-exec argument `{other}`"),
                }
            }
            let exit_code = guest_exec(
                &required_path_arg(
                    data_root.map(|path| path.to_string_lossy().to_string()),
                    "data_root",
                )?,
                &required_string_arg(workspace_id, "workspace_id")?,
                &required_string_arg(worktree_id, "worktree_id")?,
                &required_path_arg(cwd.map(|path| path.to_string_lossy().to_string()), "cwd")?,
                &required_string_arg(command, "command")?,
                &guest_env,
                user.as_deref(),
                pty,
                &passthrough_args,
            )?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        Some("shared-vm-exec") => {
            let mut data_root = None;
            let mut cwd = None;
            let mut command = None;
            let mut guest_env = Vec::new();
            let mut user = None;
            let mut pty = false;
            let mut passthrough_args = Vec::new();
            while let Some(flag) = args.next() {
                match flag.as_str() {
                    "--data-root" => data_root = args.next().map(PathBuf::from),
                    "--cwd" => cwd = args.next().map(PathBuf::from),
                    "--command" => command = args.next(),
                    "--env" => guest_env.push(required_string_arg(args.next(), "env")?),
                    "--user" => user = args.next(),
                    "--pty" => pty = true,
                    "--" => {
                        passthrough_args.extend(args);
                        break;
                    }
                    other => bail!("unknown shared-vm-exec argument `{other}`"),
                }
            }
            let exit_code = shared_vm_exec(
                &required_path_arg(
                    data_root.map(|path| path.to_string_lossy().to_string()),
                    "data_root",
                )?,
                &required_path_arg(cwd.map(|path| path.to_string_lossy().to_string()), "cwd")?,
                &required_string_arg(command, "command")?,
                &guest_env,
                user.as_deref(),
                pty,
                &passthrough_args,
            )?;
            if exit_code != 0 {
                std::process::exit(exit_code);
            }
            Ok(())
        }
        Some(other) => bail!("unsupported ctx-avf-linux-helper command: {other}"),
        None => bail!(
            "usage: ctx-avf-linux-helper <probe|prepare-runtime-layout|workspace-vm-state|start-workspace-vm|stop-workspace-vm|prepare-guest-worktree|serve-workspace-vm|serve-guest-agent|run-workspace-vm|watch-workspace-vm-memory|guest-exec|shared-vm-exec> ..."
        ),
    }
}

fn write_json<T>(value: &T) -> Result<()>
where
    T: Serialize,
{
    serde_json::to_writer(std::io::stdout(), value).context("writing AVF Linux helper response")?;
    println!();
    Ok(())
}

fn required_path_arg(value: Option<String>, name: &str) -> Result<PathBuf> {
    let raw = required_string_arg(value, name)?;
    Ok(PathBuf::from(raw))
}

fn required_string_arg(value: Option<String>, name: &str) -> Result<String> {
    let Some(value) = value else {
        bail!("missing required argument `{name}`");
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        bail!("argument `{name}` is empty");
    }
    Ok(trimmed.to_string())
}

fn required_u32_arg(value: Option<String>, name: &str) -> Result<u32> {
    let raw = required_string_arg(value, name)?;
    raw.parse::<u32>()
        .with_context(|| format!("parsing argument `{name}` as a u32"))
}

fn ensure_no_extra_args(mut args: impl Iterator<Item = String>) -> Result<()> {
    if let Some(extra) = args.next() {
        bail!("unexpected extra argument: {extra}");
    }
    Ok(())
}
