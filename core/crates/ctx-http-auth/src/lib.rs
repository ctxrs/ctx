use ctx_core::ids::SessionId;
use http::{HeaderMap, Method};
use sha2::Digest;
use url::form_urlencoded;

pub mod daemon;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserCapabilityAuthScope {
    Blob {
        blob_id: String,
    },
    SessionArtifact {
        session_id: String,
        artifact_id: String,
    },
    WorkArtifact {
        workspace_id: String,
        work_id: String,
        artifact_id: String,
    },
}

impl BrowserCapabilityAuthScope {
    fn serialize(&self) -> String {
        match self {
            Self::Blob { blob_id } => format!("blob:{blob_id}"),
            Self::SessionArtifact {
                session_id,
                artifact_id,
            } => format!("session_artifact:{session_id}:{artifact_id}"),
            Self::WorkArtifact {
                workspace_id,
                work_id,
                artifact_id,
            } => format!("work_artifact:{workspace_id}:{work_id}:{artifact_id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserStreamAuthScope {
    WorkspaceActiveSnapshot { workspace_id: String },
    WorkspaceStream { workspace_id: String },
    WorkspaceVcs { workspace_id: String },
    ExecutionLaunch { job_id: String },
    DictationLivekit,
    ProviderInstall { install_id: String },
}

impl BrowserStreamAuthScope {
    fn serialize(&self) -> String {
        match self {
            Self::WorkspaceActiveSnapshot { workspace_id } => {
                format!("workspace_active_snapshot:{workspace_id}")
            }
            Self::WorkspaceStream { workspace_id } => format!("workspace_stream:{workspace_id}"),
            Self::WorkspaceVcs { workspace_id } => format!("workspace_vcs:{workspace_id}"),
            Self::ExecutionLaunch { job_id } => format!("execution_launch:{job_id}"),
            Self::DictationLivekit => "dictation_livekit".to_string(),
            Self::ProviderInstall { install_id } => format!("provider_install:{install_id}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopedMcpRoute {
    SessionSubagents { session_id: SessionId },
    SessionArtifacts { session_id: SessionId },
    MergeQueueSubmit,
}

pub fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    headers
        .get(http::header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
        || headers.contains_key("sec-websocket-key")
}

pub fn derive_browser_query_secret(auth_token: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"ctx-desktop-browser-query-secret|");
    hasher.update(auth_token.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn browser_query_secret_bearer_is_valid(
    path: &str,
    bearer_token: Option<&str>,
    auth_token: &str,
) -> bool {
    path.starts_with("/api/")
        && bearer_token.is_some_and(|value| value == derive_browser_query_secret(auth_token))
}

pub fn derive_browser_capability_token(
    auth_token: &str,
    scope: &BrowserCapabilityAuthScope,
    expires_at: i64,
) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"ctx-browser-capability|");
    hasher.update(scope.serialize().as_bytes());
    hasher.update(b"|");
    hasher.update(expires_at.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(auth_token.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn browser_capability_query_token_is_valid(
    method: &Method,
    path: &str,
    query: Option<&str>,
    auth_token: &str,
) -> bool {
    if method != Method::GET && method != Method::HEAD {
        return false;
    }
    let Some(scope) = browser_capability_scope(path) else {
        return false;
    };
    let Some(query_token) = query_param(query, "token") else {
        return false;
    };
    let Some(expires_at) = query_expires_at_within_window(
        query,
        BROWSER_CAPABILITY_TOKEN_TTL_SECS,
        0,
        BROWSER_CAPABILITY_TOKEN_MAX_FUTURE_SKEW_SECS,
    ) else {
        return false;
    };
    query_token == derive_browser_capability_token(auth_token, &scope, expires_at)
        || query_token
            == derive_browser_capability_token(
                &derive_browser_query_secret(auth_token),
                &scope,
                expires_at,
            )
}

pub fn derive_browser_stream_token(
    auth_token: &str,
    scope: &BrowserStreamAuthScope,
    expires_at: i64,
) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"ctx-browser-stream|");
    hasher.update(scope.serialize().as_bytes());
    hasher.update(b"|");
    hasher.update(expires_at.to_string().as_bytes());
    hasher.update(b"|");
    hasher.update(auth_token.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn browser_stream_query_token_is_valid(
    method: &Method,
    path: &str,
    query: Option<&str>,
    auth_token: &str,
) -> bool {
    if method != Method::GET {
        return false;
    }
    let Some(scope) = browser_stream_scope(path, query) else {
        return false;
    };
    let Some(query_token) = query_param(query, "token") else {
        return false;
    };
    let Some(expires_at) = query_expires_at_within_window(
        query,
        BROWSER_STREAM_TOKEN_TTL_SECS,
        BROWSER_STREAM_TOKEN_MAX_PAST_SKEW_SECS,
        BROWSER_STREAM_TOKEN_MAX_FUTURE_SKEW_SECS,
    ) else {
        return false;
    };
    query_token == derive_browser_stream_token(auth_token, &scope, expires_at)
        || query_token
            == derive_browser_stream_token(
                &derive_browser_query_secret(auth_token),
                &scope,
                expires_at,
            )
}

pub fn scoped_mcp_route(method: &Method, path: &str) -> Option<ScopedMcpRoute> {
    if method == Method::POST && path == "/api/merge-queue/entries" {
        return Some(ScopedMcpRoute::MergeQueueSubmit);
    }
    let subagent_route_suffixes = if method == Method::POST {
        &[
            "spawn_agent",
            "send_input",
            "archive_agent",
            "interrupt_agent",
            "get_agent",
            "wait_agent",
        ][..]
    } else if method == Method::GET {
        &["list_agents"][..]
    } else {
        &[][..]
    };
    if let Some(session_id) =
        parse_scoped_mcp_session_id(path, "/api/mcp/sessions/", subagent_route_suffixes)
    {
        return Some(ScopedMcpRoute::SessionSubagents { session_id });
    }
    if method == Method::POST {
        if let Some(session_id) =
            parse_scoped_mcp_session_id(path, "/api/sessions/", &["artifacts"])
        {
            return Some(ScopedMcpRoute::SessionArtifacts { session_id });
        }
    }
    None
}

const BROWSER_CAPABILITY_TOKEN_TTL_SECS: i64 = 60 * 60;
const BROWSER_CAPABILITY_TOKEN_MAX_FUTURE_SKEW_SECS: i64 = 60;
const BROWSER_STREAM_TOKEN_TTL_SECS: i64 = 5 * 60;
const BROWSER_STREAM_TOKEN_MAX_PAST_SKEW_SECS: i64 = 10 * 60;
const BROWSER_STREAM_TOKEN_MAX_FUTURE_SKEW_SECS: i64 = 10 * 60;

fn browser_capability_scope(path: &str) -> Option<BrowserCapabilityAuthScope> {
    if let Some(blob_id) = path.strip_prefix("/api/blobs/") {
        let blob_id = blob_id.trim();
        if blob_id.is_empty() || blob_id.contains('/') {
            return None;
        }
        return Some(BrowserCapabilityAuthScope::Blob {
            blob_id: blob_id.to_string(),
        });
    }

    if let Some(remainder) = path.strip_prefix("/api/workspaces/") {
        let (workspace_id, suffix) = remainder.split_once('/')?;
        let workspace_id = workspace_id.trim();
        if workspace_id.is_empty() {
            return None;
        }
        let suffix = suffix.strip_prefix("work/")?;
        let (work_id, suffix) = suffix.split_once('/')?;
        let work_id = work_id.trim();
        if work_id.is_empty() || work_id.contains('/') {
            return None;
        }
        let artifact_id = suffix.strip_prefix("artifacts/")?.trim();
        if artifact_id.is_empty() || artifact_id.contains('/') {
            return None;
        }
        return Some(BrowserCapabilityAuthScope::WorkArtifact {
            workspace_id: workspace_id.to_string(),
            work_id: work_id.to_string(),
            artifact_id: artifact_id.to_string(),
        });
    }

    let remainder = path.strip_prefix("/api/sessions/")?;
    let (session_id, suffix) = remainder.split_once('/')?;
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return None;
    }
    let artifact_suffix = suffix.strip_prefix("artifacts/")?;
    let artifact_id = artifact_suffix.trim();
    if artifact_id.is_empty() || artifact_id.contains('/') {
        return None;
    }
    Some(BrowserCapabilityAuthScope::SessionArtifact {
        session_id: session_id.to_string(),
        artifact_id: artifact_id.to_string(),
    })
}

fn browser_stream_scope(path: &str, query: Option<&str>) -> Option<BrowserStreamAuthScope> {
    if let Some(scope) = workspace_stream_scope(path) {
        return Some(scope);
    }
    if let Some(scope) = provider_install_stream_scope(path) {
        return Some(scope);
    }
    match path {
        "/api/execution/launch/stream" => {
            let job_id = query_param(query, "job_id")?;
            let job_id = job_id.trim();
            if job_id.is_empty() {
                return None;
            }
            Some(BrowserStreamAuthScope::ExecutionLaunch {
                job_id: job_id.to_string(),
            })
        }
        "/api/dictation/livekit/stream" => Some(BrowserStreamAuthScope::DictationLivekit),
        _ => None,
    }
}

fn workspace_stream_scope(path: &str) -> Option<BrowserStreamAuthScope> {
    let remainder = path.strip_prefix("/api/workspaces/")?;
    let (workspace_id, suffix) = remainder.split_once('/')?;
    let workspace_id = workspace_id.trim();
    if workspace_id.is_empty() {
        return None;
    }
    match suffix {
        "active_snapshot/stream" => Some(BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace_id.to_string(),
        }),
        "stream" => Some(BrowserStreamAuthScope::WorkspaceStream {
            workspace_id: workspace_id.to_string(),
        }),
        "vcs/stream" => Some(BrowserStreamAuthScope::WorkspaceVcs {
            workspace_id: workspace_id.to_string(),
        }),
        _ => None,
    }
}

fn provider_install_stream_scope(path: &str) -> Option<BrowserStreamAuthScope> {
    let remainder = path.strip_prefix("/api/providers/install/")?;
    let (install_id, suffix) = remainder.split_once('/')?;
    let install_id = install_id.trim();
    if install_id.is_empty() || suffix != "stream" {
        return None;
    }
    Some(BrowserStreamAuthScope::ProviderInstall {
        install_id: install_id.to_string(),
    })
}

fn parse_scoped_mcp_session_id(
    path: &str,
    prefix: &str,
    allowed_suffixes: &[&str],
) -> Option<SessionId> {
    let remainder = path.strip_prefix(prefix)?;
    let (raw_session_id, suffix) = remainder.split_once('/')?;
    if !allowed_suffixes.contains(&suffix) {
        return None;
    }
    uuid::Uuid::parse_str(raw_session_id).ok().map(SessionId)
}

fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    form_urlencoded::parse(query?.as_bytes())
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.into_owned())
}

fn query_expires_at_within_window(
    query: Option<&str>,
    ttl_secs: i64,
    max_past_skew_secs: i64,
    max_future_skew_secs: i64,
) -> Option<i64> {
    let expires_at = query_param(query, "expires_at").and_then(|value| value.parse().ok())?;
    let now = chrono::Utc::now().timestamp();
    if expires_at < now - max_past_skew_secs {
        return None;
    }
    if expires_at > now + ttl_secs + max_future_skew_secs {
        return None;
    }
    Some(expires_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_query_secret_is_domain_separated() {
        assert_ne!(
            derive_browser_query_secret("daemon-secret"),
            "daemon-secret"
        );
        assert_eq!(
            derive_browser_query_secret("daemon-secret"),
            derive_browser_query_secret("daemon-secret")
        );
    }

    #[test]
    fn browser_query_secret_authorizes_api_bearer_only() {
        let secret = derive_browser_query_secret("daemon-secret");

        assert!(browser_query_secret_bearer_is_valid(
            "/api/workspaces",
            Some(&secret),
            "daemon-secret"
        ));
        assert!(!browser_query_secret_bearer_is_valid(
            "/non-api",
            Some(&secret),
            "daemon-secret"
        ));
        assert!(!browser_query_secret_bearer_is_valid(
            "/api/workspaces",
            Some("wrong"),
            "daemon-secret"
        ));
    }

    #[test]
    fn browser_capability_token_accepts_matching_blob_scope() {
        let expires_at = chrono::Utc::now().timestamp() + 60;
        let scope = BrowserCapabilityAuthScope::Blob {
            blob_id: "blob-1".to_string(),
        };
        let token = derive_browser_capability_token("daemon-secret", &scope, expires_at);
        let query = format!("expires_at={expires_at}&token={token}");

        assert!(browser_capability_query_token_is_valid(
            &Method::GET,
            "/api/blobs/blob-1",
            Some(&query),
            "daemon-secret"
        ));
        assert!(!browser_capability_query_token_is_valid(
            &Method::GET,
            "/api/blobs/blob-2",
            Some(&query),
            "daemon-secret"
        ));
        assert!(!browser_capability_query_token_is_valid(
            &Method::POST,
            "/api/blobs/blob-1",
            Some(&query),
            "daemon-secret"
        ));
    }

    #[test]
    fn browser_capability_token_accepts_matching_work_artifact_scope() {
        let expires_at = chrono::Utc::now().timestamp() + 60;
        let scope = BrowserCapabilityAuthScope::WorkArtifact {
            workspace_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_string(),
            work_id: "wrk_1234567890".to_string(),
            artifact_id: "11111111-1111-4111-8111-111111111111".to_string(),
        };
        let token = derive_browser_capability_token("daemon-secret", &scope, expires_at);
        let query = format!("expires_at={expires_at}&token={token}");

        assert!(browser_capability_query_token_is_valid(
            &Method::GET,
            "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/11111111-1111-4111-8111-111111111111",
            Some(&query),
            "daemon-secret"
        ));
        assert!(!browser_capability_query_token_is_valid(
            &Method::GET,
            "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/22222222-2222-4222-8222-222222222222",
            Some(&query),
            "daemon-secret"
        ));
        assert!(!browser_capability_query_token_is_valid(
            &Method::POST,
            "/api/workspaces/aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa/work/wrk_1234567890/artifacts/11111111-1111-4111-8111-111111111111",
            Some(&query),
            "daemon-secret"
        ));
    }

    #[test]
    fn browser_stream_token_accepts_workspace_scope_and_rejects_bad_job_id() {
        let expires_at = chrono::Utc::now().timestamp() + 60;
        let scope = BrowserStreamAuthScope::WorkspaceStream {
            workspace_id: "workspace-1".to_string(),
        };
        let token = derive_browser_stream_token("daemon-secret", &scope, expires_at);
        let query = format!("expires_at={expires_at}&token={token}");

        assert!(browser_stream_query_token_is_valid(
            &Method::GET,
            "/api/workspaces/workspace-1/stream",
            Some(&query),
            "daemon-secret"
        ));
        assert!(!browser_stream_query_token_is_valid(
            &Method::GET,
            "/api/workspaces/workspace-2/stream",
            Some(&query),
            "daemon-secret"
        ));
        assert!(!browser_stream_query_token_is_valid(
            &Method::GET,
            "/api/execution/launch/stream",
            Some("job_id=&expires_at=1&token=bad"),
            "daemon-secret"
        ));
    }

    #[test]
    fn browser_stream_token_accepts_browser_query_secret_signer() {
        let expires_at = chrono::Utc::now().timestamp() + 60;
        let browser_secret = derive_browser_query_secret("daemon-secret");
        let scope = BrowserStreamAuthScope::DictationLivekit;
        let token = derive_browser_stream_token(&browser_secret, &scope, expires_at);
        let query = format!("expires_at={expires_at}&token={token}");

        assert!(browser_stream_query_token_is_valid(
            &Method::GET,
            "/api/dictation/livekit/stream",
            Some(&query),
            "daemon-secret"
        ));
    }

    #[test]
    fn scoped_mcp_route_classifies_allowed_routes_only() {
        let session_id = SessionId::new();
        for (method, suffix) in [
            (&Method::POST, "spawn_agent"),
            (&Method::POST, "send_input"),
            (&Method::POST, "archive_agent"),
            (&Method::POST, "interrupt_agent"),
            (&Method::POST, "get_agent"),
            (&Method::POST, "wait_agent"),
            (&Method::GET, "list_agents"),
        ] {
            assert_eq!(
                scoped_mcp_route(
                    method,
                    &format!("/api/mcp/sessions/{}/{suffix}", session_id.0)
                ),
                Some(ScopedMcpRoute::SessionSubagents { session_id }),
                "{method} {suffix} should be scoped"
            );
        }
        assert_eq!(
            scoped_mcp_route(
                &Method::POST,
                &format!("/api/sessions/{}/artifacts", session_id.0)
            ),
            Some(ScopedMcpRoute::SessionArtifacts { session_id })
        );
        assert_eq!(
            scoped_mcp_route(&Method::POST, "/api/merge-queue/entries"),
            Some(ScopedMcpRoute::MergeQueueSubmit)
        );
        assert_eq!(
            scoped_mcp_route(
                &Method::GET,
                &format!("/api/sessions/{}/artifacts", session_id.0)
            ),
            None
        );
    }

    #[test]
    fn scoped_mcp_route_rejects_subagent_route_drift() {
        let session_id = SessionId::new();
        for (wrong_method, suffix) in [
            (&Method::GET, "spawn_agent"),
            (&Method::GET, "send_input"),
            (&Method::GET, "archive_agent"),
            (&Method::GET, "interrupt_agent"),
            (&Method::GET, "get_agent"),
            (&Method::GET, "wait_agent"),
            (&Method::POST, "list_agents"),
        ] {
            assert_eq!(
                scoped_mcp_route(
                    wrong_method,
                    &format!("/api/mcp/sessions/{}/{suffix}", session_id.0)
                ),
                None,
                "{wrong_method} {suffix} should not be scoped"
            );
        }

        for path in [
            format!("/api/mcp/sessions/{}/unknown", session_id.0),
            format!("/api/mcp/sessions/{}/list_agents/extra", session_id.0),
            "/api/mcp/sessions/not-a-session/list_agents".to_string(),
            format!("/api/sessions/{}/list_agents", session_id.0),
        ] {
            assert_eq!(scoped_mcp_route(&Method::GET, &path), None, "{path}");
        }
    }

    #[test]
    fn websocket_upgrade_accepts_upgrade_or_sec_key_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(http::header::UPGRADE, "WebSocket".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));

        let mut headers = HeaderMap::new();
        headers.insert("sec-websocket-key", "abc".parse().unwrap());
        assert!(is_websocket_upgrade(&headers));
    }
}
