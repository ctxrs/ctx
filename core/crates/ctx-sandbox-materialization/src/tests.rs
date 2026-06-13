use super::*;
use ctx_sandbox_container_runtime::sandbox_cli_env_test_lock;
use ctx_sandbox_contract::CTX_CONTAINER_WORKSPACE_ROOT;
use std::fs;
use uuid::Uuid;

struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &std::path::Path) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.prev.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[tokio::test]
async fn ensure_workspace_root_from_host_copy_fails_when_host_workspace_is_missing() {
    let _env_lock = sandbox_cli_env_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli.log");
    let cli_path = temp.path().join("fake-sandbox-cli.sh");
    fs::write(
        &cli_path,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> '{log_path}'\ncmd=\"$1\"\nshift\nif [ \"$cmd\" = \"exec\" ]; then\n  requested_user=\"\"\n  while [ \"$#\" -gt 0 ]; do\n    case \"$1\" in\n      --interactive)\n        shift\n        ;;\n      --user)\n        requested_user=\"$2\"\n        shift 2\n        ;;\n      --workdir)\n        workdir=\"$2\"\n        shift 2\n        ;;\n      *)\n        break\n        ;;\n    esac\n  done\n  container_id=\"$1\"\n  shift\n  command=\"$1\"\n  shift\n  case \"$command\" in\n    sh)\n      if [ \"$1\" = \"-lc\" ] && printf '%s' \"$2\" | grep -q 'git rev-parse'; then\n        exit 1\n      fi\n      exit 0\n      ;;\n    id)\n      if [ \"$1\" = \"-u\" ]; then\n        printf '502\\n'\n        exit 0\n      fi\n      if [ \"$1\" = \"-g\" ]; then\n        printf '20\\n'\n        exit 0\n      fi\n      echo \"unexpected id args: $*\" >&2\n      exit 1\n      ;;\n    chown)\n      if [ \"$requested_user\" != \"0\" ]; then\n        echo \"expected root chown\" >&2\n        exit 1\n      fi\n      exit 0\n      ;;\n    mkdir)\n      exit 0\n      ;;\n    *)\n      echo \"unexpected exec command: $command\" >&2\n      exit 1\n      ;;\n  esac\nfi\nif [ \"$cmd\" = \"cp\" ]; then\n  echo \"unexpected container cp\" >&2\n  exit 1\nfi\necho \"unexpected sandbox cli command: $cmd\" >&2\nexit 1\n",
            log_path = log_path.display(),
        ),
    )
    .expect("write fake sandbox cli");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(&cli_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli_path, perms).expect("chmod fake sandbox cli");
    }

    let _cli = EnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", &cli_path);

    let workspace = Workspace {
        id: WorkspaceId(uuid::Uuid::new_v4()),
        name: "missing-root".to_string(),
        root_path: temp
            .path()
            .join("missing-workspace")
            .to_string_lossy()
            .to_string(),
        created_at: chrono::Utc::now(),
        vcs_kind: Some(ctx_core::models::VcsKind::Git),
    };

    let err = ensure_workspace_root_from_host_copy(
        temp.path(),
        &ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer,
        &workspace,
    )
    .await
    .expect_err("missing host workspace should fail");
    assert!(format!("{err:#}").contains("host workspace root is unavailable"));

    let log = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(!log.contains("chmod 0777"));
    assert!(!log.contains("find \"$1\" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +"));
    assert!(!log.contains("id -u"));
    assert!(!log.contains("id -g"));
    assert!(!log.contains("exec --interactive --user 0"));
    assert!(!log.contains("chown 502:20 /ctx/ws"));
    assert!(!log.contains(" cp "));
}

#[tokio::test]
async fn ensure_worktree_from_host_copy_preflights_against_workspace_volume_root() {
    let _env_lock = sandbox_cli_env_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli.log");
    let cli_path = temp.path().join("fake-sandbox-cli.sh");
    let workspace_id = WorkspaceId(Uuid::new_v4());
    let worktree_id = WorktreeId(Uuid::new_v4());
    let container_id = workspace_container_name(workspace_id);
    fs::write(
        &cli_path,
        format!(
            "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$*\" >> '{log_path}'\ncmd=\"$1\"\nshift\nif [ \"$cmd\" != \"exec\" ]; then\n  echo \"unexpected sandbox cli command: $cmd\" >&2\n  exit 1\nfi\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    --interactive)\n      shift\n      ;;\n    --user)\n      shift 2\n      ;;\n    --workdir)\n      shift 2\n      ;;\n    *)\n      break\n      ;;\n  esac\ndone\nif [ \"$1\" != \"{container_id}\" ]; then\n  echo \"unexpected container: $1\" >&2\n  exit 1\nfi\nshift\ncommand=\"$1\"\nshift\ncase \"$command\" in\n  df)\n    printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n'\n    printf 'overlay 10485760 1024 7340032 1%% /ctx/ws\\n'\n    exit 0\n    ;;\n  id)\n    if [ \"$1\" = \"-u\" ]; then printf '502\\n'; exit 0; fi\n    if [ \"$1\" = \"-g\" ]; then printf '20\\n'; exit 0; fi\n    echo \"unexpected id flag: $1\" >&2\n    exit 1\n    ;;\n  mkdir)\n    exit 0\n    ;;\n  chown)\n    exit 0\n    ;;\n  tar)\n    cat >/dev/null\n    exit 0\n    ;;\n  git)\n    exit 0\n    ;;\n  sh)\n    if [ \"$1\" = \"-lc\" ] && printf '%s' \"$2\" | grep -q 'git rev-parse'; then\n      printf 'true\\n'\n      exit 0\n    fi\n    exit 0\n    ;;\n  *)\n    echo \"unexpected exec command: $command\" >&2\n    exit 1\n    ;;\nesac\n",
            log_path = log_path.display(),
            container_id = container_id,
        ),
    )
    .expect("write fake sandbox cli");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(&cli_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli_path, perms).expect("chmod fake sandbox cli");
    }

    let src = temp.path().join("src");
    fs::create_dir_all(src.join(".git")).expect("create git dir");
    fs::write(src.join("README.md"), "hello\n").expect("write readme");
    fs::write(src.join(".git").join("HEAD"), "ref: refs/heads/main\n").expect("write git head");

    let _cli = EnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", &cli_path);
    let expected_container_id = container_id.clone();
    let _storage_override = set_test_preflight_storage_samples_override(std::sync::Arc::new(
        move |data_root,
              mode,
              observed_container_id,
              _estimated_copy_bytes,
              destination_probe_root,
              operation,
              required_bytes| {
            assert!(matches!(
                mode,
                ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer
            ));
            assert_eq!(observed_container_id, expected_container_id);
            assert_eq!(
                operation,
                StorageAdmissionOperation::DiskIsolatedWorktreeMaterialization
            );
            assert_eq!(
                destination_probe_root,
                Path::new(CTX_CONTAINER_WORKSPACE_ROOT)
            );
            let total_bytes = required_bytes.saturating_add(2 * 1024 * 1024 * 1024);
            Ok((
                ctx_storage_admission::StorageAdmissionSample {
                    label: "CTX data root".to_string(),
                    path: data_root.to_string_lossy().to_string(),
                    mount_point: "/".to_string(),
                    free_bytes: required_bytes.saturating_add(1024),
                    total_bytes,
                },
                ctx_storage_admission::StorageAdmissionSample {
                    label: "sandbox workspace volume".to_string(),
                    path: destination_probe_root.to_string_lossy().to_string(),
                    mount_point: CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
                    free_bytes: required_bytes.saturating_add(1024),
                    total_bytes,
                },
            ))
        },
    ));

    let dest_root = ensure_worktree_from_host_copy(
        temp.path(),
        &ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer,
        workspace_id,
        worktree_id,
        &src,
        "deadbeef",
        "ctx/test-preflight-root",
    )
    .await
    .expect("materialize worktree");
    assert_eq!(dest_root, container_worktree_root(worktree_id));

    let log = fs::read_to_string(&log_path).expect("read sandbox cli log");
    assert!(
        !log.contains(&format!(
            "exec --interactive {container_id} df -Pk -- /ctx/ws/worktrees"
        )),
        "preflight should not probe the not-yet-created worktree parent: {log}"
    );
    assert!(
        log.contains(&format!(
            "exec --user 0 {container_id} mkdir -p -- {}",
            Path::new(CTX_CONTAINER_WORKSPACE_ROOT).display()
        )),
        "worktree materialization should prime the shared workspace root before creating a live worktree: {log}"
    );
    assert!(
        log.contains(&format!(
            "exec --user 0 {container_id} chown 502:20 {}",
            Path::new(CTX_CONTAINER_WORKSPACE_ROOT).display()
        )),
        "worktree materialization should hand the shared workspace root back to the sandbox exec user: {log}"
    );
    assert!(
        log.contains(&format!(
            "exec --user 0 {container_id} mkdir -p -- {}",
            container_worktree_root(worktree_id).display()
        )),
        "worktree creation should run mkdir as root so /ctx/ws itself need not be user-writable: {log}"
    );
    assert!(
        log.contains(&format!("exec --interactive {container_id} id -u")),
        "worktree creation should resolve the sandbox exec uid before chowning the new root: {log}"
    );
    assert!(
        log.contains(&format!("exec --interactive {container_id} id -g")),
        "worktree creation should resolve the sandbox exec gid before chowning the new root: {log}"
    );
    assert!(
        log.contains(&format!(
            "exec --user 0 {container_id} chown 502:20 {}",
            container_worktree_root(worktree_id).display()
        )),
        "worktree creation should hand the new root back to the sandbox exec user: {log}"
    );
}

/// Regression test: ensure_worktree_from_host_copy must NOT mutate the host linked
/// worktree's .git file into a standalone directory before (or during) the copy.
/// The copy staging is handled entirely via a temporary directory; the host stays
/// unchanged.
#[cfg(unix)]
#[tokio::test]
async fn ensure_worktree_from_host_copy_does_not_mutate_linked_host_worktree_git_dir() {
    let _env_lock = sandbox_cli_env_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli.log");
    let cli_path = temp.path().join("fake-sandbox-cli.sh");
    let workspace_id = WorkspaceId(Uuid::new_v4());
    let worktree_id = WorktreeId(Uuid::new_v4());
    let container_id = workspace_container_name(workspace_id);

    let (_repo_root, worktree_root) = copy::test_support::seed_linked_git_worktree_fixture(
        temp.path(),
        "worktree",
        "ctx/test-no-mutate",
    );

    let dotgit = worktree_root.join(".git");
    assert!(
        dotgit.is_file(),
        "linked git worktree .git must be a file pointer before materialization"
    );

    // Minimal fake sandbox CLI: handles mkdir, tar (drain stdin), git, and sh.
    fs::write(
        &cli_path,
        format!(
            concat!(
                "#!/bin/sh\nset -eu\n",
                "printf '%s\\n' \"$*\" >> '{log_path}'\n",
                "cmd=\"$1\"; shift\n",
                "[ \"$cmd\" = \"exec\" ] || {{ echo \"unexpected cmd: $cmd\" >&2; exit 1; }}\n",
                "while [ \"$#\" -gt 0 ]; do\n",
                "  case \"$1\" in\n",
                "    --interactive) shift ;;\n",
                "    --user) shift 2 ;;\n",
                "    --workdir) shift 2 ;;\n",
                "    *) break ;;\n",
                "  esac\n",
                "done\n",
                "[ \"$1\" = \"{container_id}\" ] || {{ echo \"unexpected container: $1\" >&2; exit 1; }}\n",
                "shift; command=\"$1\"; shift\n",
                "case \"$command\" in\n",
                "  id)\n",
                "    if [ \"$1\" = \"-u\" ]; then printf '502\\n'; exit 0; fi\n",
                "    if [ \"$1\" = \"-g\" ]; then printf '20\\n'; exit 0; fi\n",
                "    echo \"unexpected id flag: $1\" >&2; exit 1 ;;\n",
                "  mkdir) exit 0 ;;\n",
                "  chown) exit 0 ;;\n",
                "  tar) cat >/dev/null; exit 0 ;;\n",
                "  git) exit 0 ;;\n",
                "  sh)\n",
                "    if [ \"$1\" = \"-lc\" ] && printf '%s' \"$2\" | grep -q 'git rev-parse'; then\n",
                "      printf 'true\\n'; exit 0\n",
                "    fi\n",
                "    exit 0 ;;\n",
                "  *) echo \"unexpected exec: $command\" >&2; exit 1 ;;\n",
                "esac\n"
            ),
            log_path = log_path.display(),
            container_id = container_id,
        ),
    )
    .expect("write fake sandbox cli");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(&cli_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli_path, perms).expect("chmod fake sandbox cli");
    }

    let _cli = EnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", &cli_path);
    let _storage_override = set_test_preflight_storage_samples_override(std::sync::Arc::new(
        move |data_root, _mode, _cid, required_bytes, dest_probe, _op, _req| {
            let total = required_bytes.saturating_add(2 * 1024 * 1024 * 1024);
            Ok((
                ctx_storage_admission::StorageAdmissionSample {
                    label: "CTX data root".to_string(),
                    path: data_root.to_string_lossy().to_string(),
                    mount_point: "/".to_string(),
                    free_bytes: total,
                    total_bytes: total,
                },
                ctx_storage_admission::StorageAdmissionSample {
                    label: "sandbox workspace volume".to_string(),
                    path: dest_probe.to_string_lossy().to_string(),
                    mount_point: CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
                    free_bytes: total,
                    total_bytes: total,
                },
            ))
        },
    ));

    ensure_worktree_from_host_copy(
        temp.path(),
        &ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer,
        workspace_id,
        worktree_id,
        &worktree_root,
        "deadbeef",
        "ctx/test-no-mutate",
    )
    .await
    .expect("ensure_worktree_from_host_copy");

    // KEY invariant: the host linked worktree .git must still be a *file* pointer —
    // it must NOT have been converted to a directory during the copy path.
    assert!(
        dotgit.is_file(),
        "ensure_worktree_from_host_copy must not convert the host linked worktree \
         .git file to a standalone directory"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ensure_workspace_root_from_host_copy_avoids_interactive_root_exec_for_wrapper() {
    let _env_lock = sandbox_cli_env_test_lock().lock().await;
    let temp = tempfile::tempdir().expect("tempdir");
    let log_path = temp.path().join("sandbox-cli.log");
    let marker_path = temp.path().join("workspace-root-seeded");
    let cli_path = temp.path().join("fake-sandbox-cli.sh");
    let workspace = Workspace {
        id: WorkspaceId(Uuid::new_v4()),
        name: "wrapper-root-exec".to_string(),
        root_path: temp.path().join("workspace").to_string_lossy().to_string(),
        created_at: chrono::Utc::now(),
        vcs_kind: Some(ctx_core::models::VcsKind::Git),
    };
    let container_id = workspace_container_name(workspace.id);
    let workspace_root = PathBuf::from(&workspace.root_path);
    fs::create_dir_all(workspace_root.join(".git")).expect("create git dir");
    fs::write(
        workspace_root.join(".git").join("HEAD"),
        "ref: refs/heads/main\n",
    )
    .expect("write git head");
    fs::write(workspace_root.join("README.md"), "hello\n").expect("write readme");

    fs::write(
        &cli_path,
        format!(
            concat!(
                "#!/bin/sh\nset -eu\n",
                "printf '%s\\n' \"$*\" >> '{log_path}'\n",
                "cmd=\"$1\"; shift\n",
                "[ \"$cmd\" = \"exec\" ] || {{ echo \"unexpected cmd: $cmd\" >&2; exit 1; }}\n",
                "interactive=0\n",
                "requested_user=''\n",
                "while [ \"$#\" -gt 0 ]; do\n",
                "  case \"$1\" in\n",
                "    --interactive) interactive=1; shift ;;\n",
                "    --user) requested_user=\"$2\"; shift 2 ;;\n",
                "    --workdir) shift 2 ;;\n",
                "    *) break ;;\n",
                "  esac\n",
                "done\n",
                "[ \"$1\" = \"{container_id}\" ] || {{ echo \"unexpected container: $1\" >&2; exit 1; }}\n",
                "shift\n",
                "if [ \"$requested_user\" = \"0\" ] && [ \"$interactive\" -eq 1 ]; then\n",
                "  echo 'wrapper rejected interactive root exec' >&2\n",
                "  exit 64\n",
                "fi\n",
                "command=\"$1\"; shift\n",
                "case \"$command\" in\n",
                "  df)\n",
                "    printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\\n'\n",
                "    printf 'overlay 10485760 1024 7340032 1%% /ctx/ws\\n'\n",
                "    exit 0\n",
                "    ;;\n",
                "  id)\n",
                "    if [ \"$1\" = \"-u\" ]; then printf '502\\n'; exit 0; fi\n",
                "    if [ \"$1\" = \"-g\" ]; then printf '20\\n'; exit 0; fi\n",
                "    echo \"unexpected id flag: $1\" >&2\n",
                "    exit 1\n",
                "    ;;\n",
                "  chown)\n",
                "    exit 0\n",
                "    ;;\n",
                "  tar)\n",
                "    cat >/dev/null\n",
                "    : > '{marker_path}'\n",
                "    exit 0\n",
                "    ;;\n",
                "  sh)\n",
                "    if [ \"$1\" = \"-lc\" ] && printf '%s' \"$2\" | grep -q 'git rev-parse'; then\n",
                "      if [ -f '{marker_path}' ]; then\n",
                "        printf 'true\\n'\n",
                "        exit 0\n",
                "      fi\n",
                "      exit 1\n",
                "    fi\n",
                "    exit 0\n",
                "    ;;\n",
                "  *)\n",
                "    echo \"unexpected exec command: $command\" >&2\n",
                "    exit 1\n",
                "    ;;\n",
                "esac\n"
            ),
            log_path = log_path.display(),
            marker_path = marker_path.display(),
            container_id = container_id,
        ),
    )
    .expect("write fake sandbox cli");
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut perms = fs::metadata(&cli_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&cli_path, perms).expect("chmod fake sandbox cli");
    }

    let _cli = EnvGuard::set("CTX_HARNESS_SANDBOX_CLI_PATH", &cli_path);
    let expected_container_id = container_id.clone();
    let _storage_override = set_test_preflight_storage_samples_override(std::sync::Arc::new(
        move |data_root,
              mode,
              observed_container_id,
              _estimated_copy_bytes,
              destination_probe_root,
              operation,
              required_bytes| {
            assert!(matches!(
                mode,
                ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer
            ));
            assert_eq!(observed_container_id, expected_container_id);
            assert_eq!(
                operation,
                StorageAdmissionOperation::DiskIsolatedWorkspaceMaterialization
            );
            assert_eq!(
                destination_probe_root,
                Path::new(CTX_CONTAINER_WORKSPACE_ROOT)
            );
            let total_bytes = required_bytes.saturating_add(2 * 1024 * 1024 * 1024);
            Ok((
                ctx_storage_admission::StorageAdmissionSample {
                    label: "CTX data root".to_string(),
                    path: data_root.to_string_lossy().to_string(),
                    mount_point: "/".to_string(),
                    free_bytes: required_bytes.saturating_add(1024),
                    total_bytes,
                },
                ctx_storage_admission::StorageAdmissionSample {
                    label: "sandbox workspace volume".to_string(),
                    path: destination_probe_root.to_string_lossy().to_string(),
                    mount_point: CTX_CONTAINER_WORKSPACE_ROOT.to_string(),
                    free_bytes: required_bytes.saturating_add(1024),
                    total_bytes,
                },
            ))
        },
    ));

    let dest_root = ensure_workspace_root_from_host_copy(
        temp.path(),
        &ctx_sandbox_container_runtime::SandboxCommandMode::NativeContainer,
        &workspace,
    )
    .await
    .expect("materialize workspace root");
    assert_eq!(dest_root, PathBuf::from(CTX_CONTAINER_WORKSPACE_ROOT));

    let log = fs::read_to_string(&log_path).expect("read sandbox cli log");
    assert!(
        !log.contains(&format!("exec --interactive --user 0 {container_id}")),
        "workspace root materialization should not rely on interactive root exec under the managed wrapper: {log}"
    );
    assert!(
        log.contains(&format!("exec --user 0 {container_id} sh -lc")),
        "workspace root materialization should still use root-owned prep commands: {log}"
    );
    assert!(
        log.contains(&format!("exec --interactive --workdir /ctx/ws {container_id} tar -xf -")),
        "workspace root materialization should still stream the staged tarball interactively: {log}"
    );
}
