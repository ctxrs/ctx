use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::time::Duration;

use ctx_sandbox_container_runtime::{command_output_message, command_output_with_timeout};
use tokio::process::Command;

const TERMINAL_CONTAINER_CWD_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalLaunchErrorKind {
    BadRequest,
    NotFound,
    Internal,
}

#[derive(Debug, Eq, PartialEq)]
pub struct TerminalLaunchError {
    kind: TerminalLaunchErrorKind,
    message: String,
}

impl TerminalLaunchError {
    pub fn bad_request(error: impl Into<String>) -> Self {
        Self::new(TerminalLaunchErrorKind::BadRequest, error)
    }

    pub fn not_found(error: impl Into<String>) -> Self {
        Self::new(TerminalLaunchErrorKind::NotFound, error)
    }

    pub fn internal(error: impl Into<String>) -> Self {
        Self::new(TerminalLaunchErrorKind::Internal, error)
    }

    pub fn kind(&self) -> TerminalLaunchErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: TerminalLaunchErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

pub fn default_terminal_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

pub fn resolve_container_terminal_cwd(
    live_workspace_root: &FsPath,
    live_worktree_root: Option<&FsPath>,
    host_workspace_root: &FsPath,
    host_worktree_root: Option<&FsPath>,
    requested_cwd: Option<&FsPath>,
) -> Result<PathBuf, TerminalLaunchError> {
    let live_root = if host_worktree_root.is_some() {
        live_worktree_root.ok_or_else(|| {
            TerminalLaunchError::internal("sandbox terminal requires a resolved live worktree root")
        })?
    } else {
        live_workspace_root
    };
    let host_root = host_worktree_root.unwrap_or(host_workspace_root);

    let Some(requested) = requested_cwd else {
        return Ok(live_root.to_path_buf());
    };

    let requested_str = requested.to_string_lossy().to_string();
    if requested.is_relative() {
        return resolve_path_lexical_within_root(live_root, &requested_str).map_err(|_| {
            TerminalLaunchError::bad_request(
                "cwd must be within the container worktree/workspace root",
            )
        });
    }

    if let Ok(cwd) = resolve_path_lexical_within_root(live_root, &requested_str) {
        return Ok(cwd);
    }

    if let Ok(host_cwd) = resolve_path_lexical_within_root(host_root, &requested_str) {
        let relative = host_cwd.strip_prefix(host_root).map_err(|_| {
            TerminalLaunchError::bad_request(
                "cwd must be within the container worktree/workspace root",
            )
        })?;
        return Ok(live_root.join(relative));
    }

    Err(TerminalLaunchError::bad_request(
        "cwd must be within the container worktree/workspace root",
    ))
}

pub async fn resolve_host_terminal_cwd(
    bound_root: &FsPath,
    requested_cwd: Option<&FsPath>,
) -> Result<PathBuf, TerminalLaunchError> {
    let candidate = requested_cwd
        .map(|requested| {
            if requested.is_relative() {
                bound_root.join(requested)
            } else {
                requested.to_path_buf()
            }
        })
        .unwrap_or_else(|| bound_root.to_path_buf());
    let cwd = tokio::fs::canonicalize(&candidate)
        .await
        .map_err(|_| TerminalLaunchError::bad_request("cwd does not exist"))?;
    if !cwd.starts_with(bound_root) {
        return Err(TerminalLaunchError::bad_request(
            "cwd must be within the terminal root",
        ));
    }
    Ok(cwd)
}

pub async fn resolve_terminal_host_root(
    path: &FsPath,
    container_mode: bool,
    unavailable_error: &'static str,
) -> Result<PathBuf, TerminalLaunchError> {
    if container_mode {
        return Ok(path.to_path_buf());
    }

    tokio::fs::canonicalize(path)
        .await
        .map_err(|_| TerminalLaunchError::bad_request(unavailable_error))
}

pub fn validate_canonical_container_terminal_cwd(
    live_root: &FsPath,
    canonical: &FsPath,
) -> Result<PathBuf, TerminalLaunchError> {
    if !canonical.is_absolute() {
        return Err(TerminalLaunchError::internal(
            "sandbox terminal cwd validation returned a relative path",
        ));
    }
    if !canonical.starts_with(live_root) {
        return Err(TerminalLaunchError::bad_request(
            "cwd must be within the container worktree/workspace root",
        ));
    }
    Ok(canonical.to_path_buf())
}

pub async fn canonicalize_container_terminal_cwd(
    mut cmd: Command,
    container_name: &str,
    cwd: &FsPath,
    live_root: &FsPath,
) -> Result<PathBuf, TerminalLaunchError> {
    cmd.arg("exec")
        .arg("--user")
        .arg("0")
        .arg(container_name)
        .arg("realpath")
        .arg("-e")
        .arg("--")
        .arg(cwd);
    let output = command_output_with_timeout(cmd, TERMINAL_CONTAINER_CWD_TIMEOUT)
        .await
        .map_err(|e| {
            TerminalLaunchError::internal(format!("failed to validate sandbox terminal cwd: {e}"))
        })?;
    if !output.status.success() {
        let detail = command_output_message(&output);
        if detail.is_empty() {
            return Err(TerminalLaunchError::bad_request("cwd does not exist"));
        }
        return Err(TerminalLaunchError::bad_request(format!(
            "cwd does not exist: {detail}"
        )));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        TerminalLaunchError::internal("sandbox terminal cwd validation returned invalid UTF-8")
    })?;
    let canonical = stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            TerminalLaunchError::internal("sandbox terminal cwd validation returned no path")
        })?;
    validate_canonical_container_terminal_cwd(live_root, &canonical)
}

pub fn container_terminal_env() -> HashMap<String, String> {
    HashMap::from([
        (
            "HOME".to_string(),
            ctx_workspace_container::CONTAINER_TERMINAL_HOME.to_string(),
        ),
        (
            "USER".to_string(),
            ctx_workspace_container::CONTAINER_TERMINAL_USER.to_string(),
        ),
        (
            "LOGNAME".to_string(),
            ctx_workspace_container::CONTAINER_TERMINAL_USER.to_string(),
        ),
    ])
}

fn resolve_path_lexical_within_root(root: &FsPath, path: &str) -> anyhow::Result<PathBuf> {
    let candidate = if PathBuf::from(path).is_absolute() {
        PathBuf::from(path)
    } else {
        root.join(path)
    };
    let mut is_abs = false;
    let mut parts: Vec<std::ffi::OsString> = Vec::new();
    for comp in candidate.components() {
        use std::path::Component;
        match comp {
            Component::Prefix(_) => anyhow::bail!("unsupported path prefix"),
            Component::RootDir => {
                is_abs = true;
                parts.clear();
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if parts.is_empty() {
                    continue;
                }
                parts.pop();
            }
            Component::Normal(seg) => parts.push(seg.to_os_string()),
        }
    }
    let mut normalized = PathBuf::new();
    if is_abs {
        normalized.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for part in &parts {
        normalized.push(part);
    }
    if !normalized.starts_with(root) {
        anyhow::bail!("path outside root");
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_launch_error(
        error: &TerminalLaunchError,
        kind: TerminalLaunchErrorKind,
        message: &str,
    ) {
        assert_eq!(error.kind(), kind);
        assert_eq!(error.message(), message);
    }

    #[test]
    fn resolve_container_terminal_cwd_maps_host_subdir_into_managed_container_worktree() {
        let host_workspace_root = PathBuf::from("/host/ws");
        let host_worktree_root = PathBuf::from("/host/managed/wt");
        let live_worktree_root = PathBuf::from("/ctx/ws/worktrees/wt");
        let requested = host_worktree_root.join("src/bin");

        let cwd = resolve_container_terminal_cwd(
            &PathBuf::from("/ctx/ws"),
            Some(&live_worktree_root),
            &host_workspace_root,
            Some(&host_worktree_root),
            Some(&requested),
        )
        .unwrap();

        assert_eq!(cwd, live_worktree_root.join("src/bin"));
    }

    #[test]
    fn resolve_container_terminal_cwd_rejects_workspace_root_for_bound_worktree() {
        let err = resolve_container_terminal_cwd(
            &PathBuf::from("/ctx/ws"),
            Some(&PathBuf::from("/ctx/ws/worktrees/wt")),
            &PathBuf::from("/host/ws"),
            Some(&PathBuf::from("/host/ws/worktrees/wt")),
            Some(&PathBuf::from("/host/ws/subdir")),
        )
        .expect_err("bound worktree terminal must not map workspace root");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the container worktree/workspace root",
        );
    }

    #[test]
    fn resolve_container_terminal_cwd_maps_plain_workspace_terminal_paths_without_worktree_root() {
        let cwd = resolve_container_terminal_cwd(
            &PathBuf::from("/ctx/ws"),
            None,
            &PathBuf::from("/host/ws"),
            None,
            Some(&PathBuf::from("/host/ws/subdir")),
        )
        .unwrap();

        assert_eq!(cwd, PathBuf::from("/ctx/ws/subdir"));
    }

    #[test]
    fn resolve_container_terminal_cwd_maps_relative_paths_within_live_root() {
        let live_worktree_root = PathBuf::from("/ctx/ws/worktrees/wt");

        let cwd = resolve_container_terminal_cwd(
            &PathBuf::from("/ctx/ws"),
            Some(&live_worktree_root),
            &PathBuf::from("/host/ws"),
            Some(&PathBuf::from("/host/ws/worktrees/wt")),
            Some(&PathBuf::from("src/bin")),
        )
        .unwrap();

        assert_eq!(cwd, live_worktree_root.join("src/bin"));
    }

    #[test]
    fn resolve_container_terminal_cwd_rejects_live_sibling_worktree_for_bound_worktree() {
        let err = resolve_container_terminal_cwd(
            &PathBuf::from("/ctx/ws"),
            Some(&PathBuf::from("/ctx/ws/worktrees/wt")),
            &PathBuf::from("/host/ws"),
            Some(&PathBuf::from("/host/ws/worktrees/wt")),
            Some(&PathBuf::from("/ctx/ws/worktrees/sibling/src")),
        )
        .expect_err("bound worktree terminal must reject sibling live roots");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the container worktree/workspace root",
        );
    }

    #[test]
    fn resolve_container_terminal_cwd_rejects_relative_parent_escape() {
        let err = resolve_container_terminal_cwd(
            &PathBuf::from("/ctx/ws"),
            Some(&PathBuf::from("/ctx/ws/worktrees/wt")),
            &PathBuf::from("/host/ws"),
            Some(&PathBuf::from("/host/ws/worktrees/wt")),
            Some(&PathBuf::from("../../escape")),
        )
        .expect_err("relative cwd escape should be rejected");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the container worktree/workspace root",
        );
    }

    #[test]
    fn validate_canonical_container_terminal_cwd_rejects_symlink_to_workspace_root() {
        let live_root = PathBuf::from("/ctx/ws/worktrees/wt");

        let err = validate_canonical_container_terminal_cwd(&live_root, &PathBuf::from("/ctx/ws"))
            .expect_err("canonicalized symlink target outside bound worktree must reject");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the container worktree/workspace root",
        );
    }

    #[test]
    fn validate_canonical_container_terminal_cwd_rejects_symlink_to_sibling_worktree() {
        let live_root = PathBuf::from("/ctx/ws/worktrees/wt");

        let err = validate_canonical_container_terminal_cwd(
            &live_root,
            &PathBuf::from("/ctx/ws/worktrees/sibling/src"),
        )
        .expect_err("canonicalized symlink target in sibling worktree must reject");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the container worktree/workspace root",
        );
    }

    #[test]
    fn validate_canonical_container_terminal_cwd_accepts_canonical_child() {
        let live_root = PathBuf::from("/ctx/ws/worktrees/wt");

        let cwd = validate_canonical_container_terminal_cwd(
            &live_root,
            &PathBuf::from("/ctx/ws/worktrees/wt/src"),
        )
        .expect("canonicalized path inside bound worktree should be accepted");

        assert_eq!(cwd, PathBuf::from("/ctx/ws/worktrees/wt/src"));
    }

    #[tokio::test]
    async fn resolve_host_terminal_cwd_rejects_workspace_root_for_bound_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        let worktree_root = workspace_root.join("worktrees").join("wt");
        let workspace_subdir = workspace_root.join("src");
        tokio::fs::create_dir_all(&worktree_root)
            .await
            .expect("create worktree root");
        tokio::fs::create_dir_all(&workspace_subdir)
            .await
            .expect("create workspace subdir");
        let bound_root = tokio::fs::canonicalize(&worktree_root)
            .await
            .expect("canonical worktree root");

        let err = resolve_host_terminal_cwd(&bound_root, Some(&workspace_subdir))
            .await
            .expect_err("host worktree terminal must reject workspace root");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the terminal root",
        );
    }

    #[tokio::test]
    async fn resolve_host_terminal_cwd_rejects_sibling_worktree_for_bound_worktree() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace_root = temp.path().join("workspace");
        let worktree_root = workspace_root.join("worktrees").join("wt");
        let sibling_root = workspace_root.join("worktrees").join("sibling");
        tokio::fs::create_dir_all(&worktree_root)
            .await
            .expect("create worktree root");
        tokio::fs::create_dir_all(&sibling_root)
            .await
            .expect("create sibling root");
        let bound_root = tokio::fs::canonicalize(&worktree_root)
            .await
            .expect("canonical worktree root");

        let err = resolve_host_terminal_cwd(&bound_root, Some(&sibling_root))
            .await
            .expect_err("host worktree terminal must reject sibling worktree");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "cwd must be within the terminal root",
        );
    }

    #[tokio::test]
    async fn resolve_host_terminal_cwd_maps_relative_paths_inside_bound_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let worktree_root = temp.path().join("workspace").join("worktrees").join("wt");
        let src_root = worktree_root.join("src");
        tokio::fs::create_dir_all(&src_root)
            .await
            .expect("create src root");
        let bound_root = tokio::fs::canonicalize(&worktree_root)
            .await
            .expect("canonical worktree root");
        let expected = tokio::fs::canonicalize(&src_root)
            .await
            .expect("canonical src root");

        let cwd = resolve_host_terminal_cwd(&bound_root, Some(&PathBuf::from("src")))
            .await
            .expect("relative cwd inside bound root should be allowed");

        assert_eq!(cwd, expected);
    }

    #[tokio::test]
    async fn resolve_terminal_host_root_preserves_missing_path_for_container_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing-root");

        let resolved = resolve_terminal_host_root(&missing, true, "workspace root is unavailable")
            .await
            .expect("sandbox mode should not require host path materialization");

        assert_eq!(resolved, missing);
    }

    #[tokio::test]
    async fn resolve_terminal_host_root_requires_existing_path_for_host_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing = temp.path().join("missing-root");

        let err = resolve_terminal_host_root(&missing, false, "workspace root is unavailable")
            .await
            .expect_err("host terminals should still require a materialized host path");

        assert_launch_error(
            &err,
            TerminalLaunchErrorKind::BadRequest,
            "workspace root is unavailable",
        );
    }

    #[test]
    fn container_terminal_env_sets_ctx_user_identity() {
        let env = container_terminal_env();
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(ctx_workspace_container::CONTAINER_TERMINAL_HOME)
        );
        assert_eq!(
            env.get("USER").map(String::as_str),
            Some(ctx_workspace_container::CONTAINER_TERMINAL_USER)
        );
        assert_eq!(
            env.get("LOGNAME").map(String::as_str),
            Some(ctx_workspace_container::CONTAINER_TERMINAL_USER)
        );
    }
}
