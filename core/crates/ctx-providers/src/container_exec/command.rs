use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::Path;

use tokio::process::Command;

use super::path_map::{resolve_linux_sandbox_cwd, rewrite_container_env_value_for_linux};
use super::sandbox_env::{
    apply_sandbox_cli_env, sandbox_cli_env_for_data_root, shared_vm_sandbox_cli_guest_env,
};
use super::spec::{ContainerExecSpec, CTX_HARNESS_SANDBOX_CLI_PATH_ENV};

pub fn build_container_exec_command(
    spec: &ContainerExecSpec,
    workdir: &Path,
    env: &HashMap<String, String>,
    command: &str,
    args: &[String],
) -> Result<Command> {
    match spec {
        ContainerExecSpec::NativeContainer {
            container_id,
            user,
            sandbox_cli_path,
            host_worktree_root,
            guest_worktree_root,
            guest_workspace_root,
        } => {
            let guest_cwd = match (
                host_worktree_root.as_deref(),
                guest_worktree_root.as_deref(),
                guest_workspace_root.as_deref(),
            ) {
                (Some(host_root), Some(guest_root), Some(guest_workspace_root)) => {
                    resolve_linux_sandbox_cwd(workdir, host_root, guest_root, guest_workspace_root)?
                }
                _ => workdir.to_path_buf(),
            };
            let mut cmd = Command::new(sandbox_cli_path.as_deref().unwrap_or("nerdctl"));
            if let Some(root) = env
                .get("CTX_DATA_ROOT_HOST")
                .or_else(|| env.get("CTX_DATA_ROOT"))
            {
                apply_sandbox_cli_env(&mut cmd, root);
            }
            cmd.arg("exec").arg("--interactive");
            if let Some(user) = user.as_deref() {
                cmd.arg("--user").arg(user);
            }
            cmd.arg("--workdir").arg(&guest_cwd);
            for (key, value) in env {
                if should_skip_linux_exec_env_key(spec, key) {
                    continue;
                }
                let rewritten =
                    rewrite_container_env_value_for_linux(key, value).with_context(|| {
                        format!("rewriting container env {key} for linux execution")
                    })?;
                cmd.arg("--env").arg(format!("{key}={rewritten}"));
            }
            cmd.arg(container_id);
            cmd.arg(command);
            cmd.args(args);
            Ok(cmd)
        }
        ContainerExecSpec::SharedVmContainer {
            helper_path,
            data_root,
            real_guest_exec,
            workspace_id,
            worktree_id: _,
            host_worktree_root,
            guest_worktree_root,
            guest_workspace_root,
            user,
        } => {
            let guest_cwd = resolve_linux_sandbox_cwd(
                workdir,
                host_worktree_root,
                guest_worktree_root,
                guest_workspace_root,
            )?;
            let mut cmd = Command::new(helper_path);
            cmd.arg("shared-vm-exec")
                .arg("--data-root")
                .arg(data_root)
                .arg("--cwd")
                .arg("/")
                .arg("--command")
                .arg("nerdctl")
                .arg("--user")
                .arg("root");
            let sandbox_env = if *real_guest_exec {
                shared_vm_sandbox_cli_guest_env()
            } else {
                sandbox_cli_env_for_data_root(&data_root.to_string_lossy())
            };
            for (key, value) in sandbox_env {
                cmd.arg("--env").arg(format!("{key}={value}"));
            }
            cmd.arg("--");
            cmd.arg("exec").arg("--interactive");
            if let Some(user) = user.as_deref() {
                cmd.arg("--user").arg(user);
            }
            cmd.arg("--workdir").arg(&guest_cwd);
            for (key, value) in env {
                if should_skip_linux_exec_env_key(spec, key) {
                    continue;
                }
                let rewritten =
                    rewrite_container_env_value_for_linux(key, value).with_context(|| {
                        format!("rewriting container env {key} for linux execution")
                    })?;
                cmd.arg("--env").arg(format!("{key}={rewritten}"));
            }
            cmd.arg(format!("ctx-harness-{workspace_id}"));
            cmd.arg(command);
            cmd.args(args);
            Ok(cmd)
        }
    }
}

fn should_skip_linux_exec_env_key(spec: &ContainerExecSpec, key: &str) -> bool {
    match spec {
        ContainerExecSpec::NativeContainer { .. } => {
            key.starts_with("CTX_HARNESS_CONTAINER_") || key == CTX_HARNESS_SANDBOX_CLI_PATH_ENV
        }
        ContainerExecSpec::SharedVmContainer { .. } => key.starts_with("CTX_AVF_"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn build_container_exec_command_rewrites_bundled_path_envs_for_linux() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let host_provider_dir = tmp.path().join("bundles/providers/droid/macos/aarch64");
        let linux_provider_dir = tmp.path().join("bundles/providers/droid/linux/aarch64");
        fs::create_dir_all(&host_provider_dir).expect("mkdir host provider dir");
        fs::create_dir_all(&linux_provider_dir).expect("mkdir linux provider dir");
        fs::write(host_provider_dir.join("droid"), b"host").expect("write host droid");
        fs::write(linux_provider_dir.join("droid"), b"linux").expect("write linux droid");
        fs::write(linux_provider_dir.join("droid-acp"), b"linux").expect("write linux droid-acp");

        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::join_paths([host_provider_dir.as_path(), Path::new("/usr/bin")])
                .expect("join path")
                .to_string_lossy()
                .to_string(),
        );
        env.insert(
            "DROID_PATH".to_string(),
            host_provider_dir
                .join("droid")
                .to_string_lossy()
                .to_string(),
        );

        let spec = ContainerExecSpec::NativeContainer {
            container_id: "ctx-harness-1".to_string(),
            user: Some("1000:1000".to_string()),
            sandbox_cli_path: None,
            host_worktree_root: None,
            guest_worktree_root: None,
            guest_workspace_root: None,
        };

        let cmd = build_container_exec_command(
            &spec,
            Path::new("/workspace"),
            &env,
            linux_provider_dir
                .join("droid-acp")
                .to_string_lossy()
                .as_ref(),
            &[],
        )
        .expect("build command");

        let args = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        let rewritten_path = args
            .windows(2)
            .find_map(|window| {
                (window[0] == "--env" && window[1].starts_with("PATH="))
                    .then(|| window[1].trim_start_matches("PATH=").to_string())
            })
            .expect("PATH env");
        let path_parts =
            std::env::split_paths(OsStr::new(&rewritten_path)).collect::<Vec<PathBuf>>();
        assert!(
            path_parts.first() == Some(&linux_provider_dir),
            "missing rewritten PATH env in args: {args:?}"
        );
        assert_eq!(path_parts.get(1), Some(&PathBuf::from("/usr/bin")));
        assert!(
            args.windows(2).any(|window| {
                window[0] == "--env"
                    && window[1]
                        == format!("DROID_PATH={}", linux_provider_dir.join("droid").display())
            }),
            "missing rewritten DROID_PATH env in args: {args:?}"
        );
    }

    #[test]
    fn build_container_exec_command_errors_when_linux_bundled_path_for_env_is_missing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let host_provider_dir = tmp.path().join("bundles/providers/droid/macos/aarch64");
        fs::create_dir_all(&host_provider_dir).expect("mkdir host provider dir");

        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            host_provider_dir.to_string_lossy().to_string(),
        );

        let spec = ContainerExecSpec::NativeContainer {
            container_id: "ctx-harness-1".to_string(),
            user: None,
            sandbox_cli_path: None,
            host_worktree_root: None,
            guest_worktree_root: None,
            guest_workspace_root: None,
        };

        let err =
            build_container_exec_command(&spec, Path::new("/workspace"), &env, "/bin/true", &[])
                .expect_err("expected missing linux bundle path error");

        let err_text = format!("{err:#}");
        assert!(
            err_text.contains("missing linux bundled path"),
            "unexpected error: {err_text}"
        );
    }

    #[test]
    fn build_container_exec_command_maps_host_workdir_to_avf_guest_exec() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let host_provider_dir = tmp.path().join("bundles/providers/droid/macos/aarch64");
        let linux_provider_dir = tmp.path().join("bundles/providers/droid/linux/aarch64");
        fs::create_dir_all(&host_provider_dir).expect("mkdir host provider dir");
        fs::create_dir_all(&linux_provider_dir).expect("mkdir linux provider dir");
        fs::write(host_provider_dir.join("droid"), b"host").expect("write host droid");
        fs::write(linux_provider_dir.join("droid"), b"linux").expect("write linux droid");

        let host_worktree_root = tmp.path().join("repo");
        fs::create_dir_all(host_worktree_root.join("src")).expect("mkdir worktree");
        let helper_path = tmp.path().join("ctx-avf-linux-helper");
        fs::write(&helper_path, b"#!/bin/sh\nexit 0\n").expect("write helper");

        let mut env = HashMap::new();
        env.insert(
            "PATH".to_string(),
            std::env::join_paths([host_provider_dir.as_path(), Path::new("/usr/bin")])
                .expect("join path")
                .to_string_lossy()
                .to_string(),
        );
        env.insert(
            "DROID_PATH".to_string(),
            host_provider_dir
                .join("droid")
                .to_string_lossy()
                .to_string(),
        );

        let spec = ContainerExecSpec::SharedVmContainer {
            helper_path: helper_path.to_string_lossy().to_string(),
            data_root: tmp.path().join("ctx-data-root"),
            real_guest_exec: true,
            workspace_id: "ws-123".to_string(),
            worktree_id: "wt-456".to_string(),
            host_worktree_root: host_worktree_root.clone(),
            guest_worktree_root: PathBuf::from("/ctx/ws/worktrees/wt-456"),
            guest_workspace_root: PathBuf::from("/ctx/ws"),
            user: Some("ctx-ws-123".to_string()),
        };

        let cmd = build_container_exec_command(
            &spec,
            &host_worktree_root.join("src"),
            &env,
            "/usr/bin/env",
            &["--version".to_string()],
        )
        .expect("build AVF shared-vm container exec command");

        let args = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(args.first().map(String::as_str), Some("shared-vm-exec"));
        assert!(
            args.windows(2).any(|window| window[0] == "--data-root"
                && window[1] == tmp.path().join("ctx-data-root").to_string_lossy()),
            "missing --data-root in args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| { window[0] == "--command" && window[1] == "nerdctl" }),
            "missing shared-vm container CLI command in args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--cwd" && window[1] == "/"),
            "missing shared-vm cwd in args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--user" && window[1] == "ctx-ws-123"),
            "missing container user in args: {args:?}"
        );
        assert!(
            args.windows(2).any(|window| {
                window[0] == "--workdir" && window[1] == "/ctx/ws/worktrees/wt-456/src"
            }),
            "missing translated --workdir in args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window[0] == "--" && window[1] == "exec"),
            "missing container exec boundary in args: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|window| window[0] == "ctx-harness-ws-123" && window[1] == "/usr/bin/env"),
            "missing container name and command in args: {args:?}"
        );
        assert!(
            args.windows(2).any(|window| {
                if window[0] != "--env" || !window[1].starts_with("PATH=") {
                    return false;
                }
                let value = window[1].trim_start_matches("PATH=");
                let parts = std::env::split_paths(OsStr::new(value)).collect::<Vec<PathBuf>>();
                parts.first() == Some(&linux_provider_dir)
                    && parts.get(1) == Some(&PathBuf::from("/usr/bin"))
            }),
            "missing rewritten PATH env in args: {args:?}"
        );
        assert!(
            args.windows(2).any(|window| {
                window[0] == "--env"
                    && window[1]
                        == format!("DROID_PATH={}", linux_provider_dir.join("droid").display())
            }),
            "missing rewritten DROID_PATH env in args: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == "--version"),
            "missing passthrough argument in args: {args:?}"
        );
    }

    #[test]
    fn build_container_exec_command_keeps_host_scoped_env_on_simulated_shared_vm() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let helper_path = tmp.path().join("ctx-avf-linux-helper");
        fs::write(&helper_path, b"#!/bin/sh\nexit 0\n").expect("write helper");
        let host_worktree_root = tmp.path().join("repo");
        fs::create_dir_all(&host_worktree_root).expect("mkdir worktree");
        let data_root = tmp.path().join("ctx-data-root");

        let spec = ContainerExecSpec::SharedVmContainer {
            helper_path: helper_path.to_string_lossy().to_string(),
            data_root: data_root.clone(),
            real_guest_exec: false,
            workspace_id: "ws-123".to_string(),
            worktree_id: "wt-456".to_string(),
            host_worktree_root: host_worktree_root.clone(),
            guest_worktree_root: PathBuf::from("/ctx/ws/worktrees/wt-456"),
            guest_workspace_root: PathBuf::from("/ctx/ws"),
            user: Some("ctx-ws-123".to_string()),
        };

        let env = HashMap::new();
        let cmd =
            build_container_exec_command(&spec, &host_worktree_root, &env, "/usr/bin/env", &[])
                .expect("build AVF shared-vm container exec command");

        let args = cmd
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert!(
            args.windows(2).any(|window| {
                window[0] == "--env"
                    && window[1]
                        == format!("HOME={}", data_root.join("sandbox").join("home").display())
            }),
            "missing host-scoped HOME env in args: {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|window| window[0] == "--env" && window[1] == "HOME=/ctx/home/root"),
            "unexpected guest HOME env in args: {args:?}"
        );
    }
}
