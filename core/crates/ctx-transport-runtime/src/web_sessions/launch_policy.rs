use anyhow::Context;
use ctx_core::models::ExecutionEnvironment;
use url::Url;

pub fn validate_web_session_url(raw: &str) -> anyhow::Result<()> {
    let parsed = Url::parse(raw).context("url must be an absolute URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        anyhow::bail!("url must use http:// or https://");
    }
    if parsed.host_str().is_none() {
        anyhow::bail!("url must include host");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebSessionLaunchPolicyErrorKind {
    BadRequest,
    Forbidden,
}

#[derive(Debug, Eq, PartialEq)]
pub struct WebSessionLaunchPolicyError {
    kind: WebSessionLaunchPolicyErrorKind,
    message: String,
}

impl WebSessionLaunchPolicyError {
    pub fn kind(&self) -> WebSessionLaunchPolicyErrorKind {
        self.kind
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for WebSessionLaunchPolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WebSessionLaunchPolicyError {}

fn policy_error(
    kind: WebSessionLaunchPolicyErrorKind,
    message: impl Into<String>,
) -> WebSessionLaunchPolicyError {
    WebSessionLaunchPolicyError {
        kind,
        message: message.into(),
    }
}

pub fn validate_web_session_launch_scope(
    has_session_scope: bool,
    has_worktree_scope: bool,
) -> Result<(), WebSessionLaunchPolicyError> {
    if has_session_scope || has_worktree_scope {
        return Ok(());
    }
    Err(policy_error(
        WebSessionLaunchPolicyErrorKind::BadRequest,
        "web session launches must be scoped to a session_id or worktree_id",
    ))
}

pub fn validate_web_session_host_session(
    execution_environment: ExecutionEnvironment,
) -> Result<(), WebSessionLaunchPolicyError> {
    if matches!(execution_environment, ExecutionEnvironment::Sandbox) {
        return Err(policy_error(
            WebSessionLaunchPolicyErrorKind::Forbidden,
            "web sessions currently run on the host and are disabled for sandbox sessions until web sessions run inside the sandbox",
        ));
    }
    Ok(())
}

pub fn validate_web_session_host_worktree(
    has_sandbox_binding: bool,
    workspace_execution_mode_is_sandbox: bool,
) -> Result<(), WebSessionLaunchPolicyError> {
    if has_sandbox_binding {
        return Err(policy_error(
            WebSessionLaunchPolicyErrorKind::Forbidden,
            "web sessions currently run on the host and are disabled for sandbox worktrees until web sessions run inside the sandbox",
        ));
    }
    if workspace_execution_mode_is_sandbox {
        return Err(policy_error(
            WebSessionLaunchPolicyErrorKind::Forbidden,
            "web sessions currently run on the host and are disabled for sandbox workspaces until web sessions run inside the sandbox",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use ctx_core::models::ExecutionEnvironment;

    use super::{
        validate_web_session_host_session, validate_web_session_host_worktree,
        validate_web_session_launch_scope, validate_web_session_url,
        WebSessionLaunchPolicyErrorKind,
    };

    #[test]
    fn validate_web_session_url_accepts_http_and_https() {
        validate_web_session_url("http://127.0.0.1:3000").expect("http URL");
        validate_web_session_url("https://example.com/path").expect("https URL");
    }

    #[test]
    fn validate_web_session_url_rejects_relative_url() {
        let error = validate_web_session_url("/workbench").expect_err("relative URL");
        assert!(format!("{error:#}").contains("absolute URL"));
    }

    #[test]
    fn validate_web_session_url_rejects_non_http_scheme() {
        let error = validate_web_session_url("file:///tmp/index.html").expect_err("file URL");
        assert!(format!("{error:#}").contains("http:// or https://"));
    }

    #[test]
    fn validate_web_session_launch_scope_requires_session_or_worktree() {
        let error =
            validate_web_session_launch_scope(false, false).expect_err("missing scope rejected");
        assert_eq!(error.kind(), WebSessionLaunchPolicyErrorKind::BadRequest);
        assert!(error.message().contains("session_id or worktree_id"));

        validate_web_session_launch_scope(true, false).expect("session scope");
        validate_web_session_launch_scope(false, true).expect("worktree scope");
    }

    #[test]
    fn validate_web_session_host_session_rejects_sandbox_sessions() {
        let error = validate_web_session_host_session(ExecutionEnvironment::Sandbox)
            .expect_err("sandbox session rejected");
        assert_eq!(error.kind(), WebSessionLaunchPolicyErrorKind::Forbidden);
        assert!(error.message().contains("sandbox sessions"));

        validate_web_session_host_session(ExecutionEnvironment::Host).expect("host session");
    }

    #[test]
    fn validate_web_session_host_worktree_rejects_sandbox_contexts() {
        let binding_error =
            validate_web_session_host_worktree(true, false).expect_err("sandbox binding rejected");
        assert_eq!(
            binding_error.kind(),
            WebSessionLaunchPolicyErrorKind::Forbidden
        );
        assert!(binding_error.message().contains("sandbox worktrees"));

        let workspace_error = validate_web_session_host_worktree(false, true)
            .expect_err("sandbox workspace rejected");
        assert_eq!(
            workspace_error.kind(),
            WebSessionLaunchPolicyErrorKind::Forbidden
        );
        assert!(workspace_error.message().contains("sandbox workspaces"));

        validate_web_session_host_worktree(false, false).expect("host worktree");
    }
}
