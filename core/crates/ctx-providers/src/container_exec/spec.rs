use std::collections::HashMap;
use std::path::PathBuf;

pub(super) const CTX_HARNESS_RUNTIME_KIND_ENV: &str = "CTX_HARNESS_RUNTIME_KIND";
pub(super) const CTX_HARNESS_CONTAINER_ID_ENV: &str = "CTX_HARNESS_CONTAINER_ID";
pub(super) const CTX_HARNESS_CONTAINER_USER_ENV: &str = "CTX_HARNESS_CONTAINER_USER";
pub(super) const CTX_HARNESS_HOST_WORKTREE_ROOT_ENV: &str = "CTX_HARNESS_HOST_WORKTREE_ROOT";
pub(super) const CTX_HARNESS_GUEST_WORKTREE_ROOT_ENV: &str = "CTX_HARNESS_GUEST_WORKTREE_ROOT";
pub(super) const CTX_HARNESS_GUEST_WORKSPACE_ROOT_ENV: &str = "CTX_HARNESS_GUEST_WORKSPACE_ROOT";
pub(super) const CTX_HARNESS_SANDBOX_CLI_PATH_ENV: &str = "CTX_HARNESS_SANDBOX_CLI_PATH";
pub(super) const CTX_AVF_LINUX_HELPER_PATH_ENV: &str = "CTX_AVF_LINUX_HELPER_PATH";
pub(super) const CTX_AVF_HOST_DATA_ROOT_ENV: &str = "CTX_AVF_HOST_DATA_ROOT";
pub(super) const CTX_AVF_WORKSPACE_ID_ENV: &str = "CTX_AVF_WORKSPACE_ID";
pub(super) const CTX_AVF_WORKTREE_ID_ENV: &str = "CTX_AVF_WORKTREE_ID";
pub(super) const CTX_AVF_HOST_WORKTREE_ROOT_ENV: &str = "CTX_AVF_HOST_WORKTREE_ROOT";
pub(super) const CTX_AVF_GUEST_WORKTREE_ROOT_ENV: &str = "CTX_AVF_GUEST_WORKTREE_ROOT";
pub(super) const CTX_AVF_REAL_GUEST_EXEC_ENV: &str = "CTX_AVF_REAL_GUEST_EXEC";

#[derive(Debug, Clone)]
pub enum ContainerExecSpec {
    NativeContainer {
        container_id: String,
        user: Option<String>,
        sandbox_cli_path: Option<String>,
        host_worktree_root: Option<PathBuf>,
        guest_worktree_root: Option<PathBuf>,
        guest_workspace_root: Option<PathBuf>,
    },
    SharedVmContainer {
        helper_path: String,
        data_root: PathBuf,
        real_guest_exec: bool,
        workspace_id: String,
        worktree_id: String,
        host_worktree_root: PathBuf,
        guest_worktree_root: PathBuf,
        guest_workspace_root: PathBuf,
        user: Option<String>,
    },
}

pub fn container_exec_spec(env: &HashMap<String, String>) -> Option<ContainerExecSpec> {
    if env.get(CTX_HARNESS_RUNTIME_KIND_ENV).map(String::as_str) == Some("shared_vm_container") {
        return Some(ContainerExecSpec::SharedVmContainer {
            helper_path: env.get(CTX_AVF_LINUX_HELPER_PATH_ENV)?.to_string(),
            data_root: PathBuf::from(env.get(CTX_AVF_HOST_DATA_ROOT_ENV)?),
            real_guest_exec: env
                .get(CTX_AVF_REAL_GUEST_EXEC_ENV)
                .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes")),
            workspace_id: env.get(CTX_AVF_WORKSPACE_ID_ENV)?.to_string(),
            worktree_id: env.get(CTX_AVF_WORKTREE_ID_ENV)?.to_string(),
            host_worktree_root: PathBuf::from(env.get(CTX_AVF_HOST_WORKTREE_ROOT_ENV)?),
            guest_worktree_root: PathBuf::from(env.get(CTX_AVF_GUEST_WORKTREE_ROOT_ENV)?),
            guest_workspace_root: PathBuf::from(env.get(CTX_HARNESS_GUEST_WORKSPACE_ROOT_ENV)?),
            user: env.get(CTX_HARNESS_CONTAINER_USER_ENV).cloned(),
        });
    }

    if env
        .get(CTX_HARNESS_CONTAINER_ID_ENV)
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Some(ContainerExecSpec::NativeContainer {
            container_id: env.get(CTX_HARNESS_CONTAINER_ID_ENV)?.to_string(),
            user: env.get(CTX_HARNESS_CONTAINER_USER_ENV).cloned(),
            sandbox_cli_path: env.get(CTX_HARNESS_SANDBOX_CLI_PATH_ENV).cloned(),
            host_worktree_root: env
                .get(CTX_HARNESS_HOST_WORKTREE_ROOT_ENV)
                .map(PathBuf::from),
            guest_worktree_root: env
                .get(CTX_HARNESS_GUEST_WORKTREE_ROOT_ENV)
                .map(PathBuf::from),
            guest_workspace_root: env
                .get(CTX_HARNESS_GUEST_WORKSPACE_ROOT_ENV)
                .map(PathBuf::from),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_exec_spec_detects_shared_vm_env_contract() {
        let mut env = HashMap::new();
        env.insert(
            CTX_HARNESS_RUNTIME_KIND_ENV.to_string(),
            "shared_vm_container".to_string(),
        );
        env.insert(
            CTX_AVF_LINUX_HELPER_PATH_ENV.to_string(),
            "/tmp/ctx-avf-linux-helper".to_string(),
        );
        env.insert(
            CTX_AVF_HOST_DATA_ROOT_ENV.to_string(),
            "/tmp/ctx-data-root".to_string(),
        );
        env.insert(CTX_AVF_REAL_GUEST_EXEC_ENV.to_string(), "1".to_string());
        env.insert(CTX_AVF_WORKSPACE_ID_ENV.to_string(), "ws-123".to_string());
        env.insert(CTX_AVF_WORKTREE_ID_ENV.to_string(), "wt-456".to_string());
        env.insert(
            CTX_AVF_HOST_WORKTREE_ROOT_ENV.to_string(),
            "/home/fixture/code/repo".to_string(),
        );
        env.insert(
            CTX_AVF_GUEST_WORKTREE_ROOT_ENV.to_string(),
            "/ctx/ws/worktrees/wt-456".to_string(),
        );
        env.insert(
            CTX_HARNESS_GUEST_WORKSPACE_ROOT_ENV.to_string(),
            "/ctx/ws".to_string(),
        );

        let spec = container_exec_spec(&env).expect("AVF exec spec");
        match spec {
            ContainerExecSpec::SharedVmContainer {
                helper_path,
                data_root,
                real_guest_exec,
                workspace_id,
                worktree_id,
                host_worktree_root,
                guest_worktree_root,
                guest_workspace_root,
                user,
            } => {
                assert_eq!(helper_path, "/tmp/ctx-avf-linux-helper");
                assert_eq!(data_root, PathBuf::from("/tmp/ctx-data-root"));
                assert!(real_guest_exec);
                assert_eq!(workspace_id, "ws-123");
                assert_eq!(worktree_id, "wt-456");
                assert_eq!(host_worktree_root, PathBuf::from("/home/fixture/code/repo"));
                assert_eq!(
                    guest_worktree_root,
                    PathBuf::from("/ctx/ws/worktrees/wt-456")
                );
                assert_eq!(guest_workspace_root, PathBuf::from("/ctx/ws"));
                assert_eq!(user, None);
            }
            other => panic!("expected shared VM sandbox spec, got {other:?}"),
        }
    }

    #[test]
    fn container_exec_spec_prefers_shared_vm_runtime_kind_over_container_id() {
        let mut env = HashMap::new();
        env.insert(
            CTX_HARNESS_RUNTIME_KIND_ENV.to_string(),
            "shared_vm_container".to_string(),
        );
        env.insert(
            CTX_HARNESS_CONTAINER_ID_ENV.to_string(),
            "ctx-harness-ws-123".to_string(),
        );
        env.insert(
            CTX_AVF_LINUX_HELPER_PATH_ENV.to_string(),
            "/tmp/ctx-avf-linux-helper".to_string(),
        );
        env.insert(
            CTX_AVF_HOST_DATA_ROOT_ENV.to_string(),
            "/tmp/ctx-data-root".to_string(),
        );
        env.insert(CTX_AVF_REAL_GUEST_EXEC_ENV.to_string(), "1".to_string());
        env.insert(CTX_AVF_WORKSPACE_ID_ENV.to_string(), "ws-123".to_string());
        env.insert(CTX_AVF_WORKTREE_ID_ENV.to_string(), "wt-456".to_string());
        env.insert(
            CTX_AVF_HOST_WORKTREE_ROOT_ENV.to_string(),
            "/home/fixture/code/repo".to_string(),
        );
        env.insert(
            CTX_AVF_GUEST_WORKTREE_ROOT_ENV.to_string(),
            "/ctx/ws/worktrees/wt-456".to_string(),
        );
        env.insert(
            CTX_HARNESS_GUEST_WORKSPACE_ROOT_ENV.to_string(),
            "/ctx/ws".to_string(),
        );

        let spec = container_exec_spec(&env).expect("AVF exec spec");
        assert!(
            matches!(spec, ContainerExecSpec::SharedVmContainer { .. }),
            "expected shared VM runtime spec, got {spec:?}"
        );
    }
}
