use super::*;

pub struct WebSessionHandle {
    pub(super) info: WebSessionInfo,
    pub(super) stream_tokens: Arc<Mutex<HashMap<String, WebSessionStreamToken>>>,
    pub(super) worker_auth_secret: String,
    pub(super) runtime: Arc<Mutex<WebSessionRuntime>>,
    pub(super) run_lock: Arc<Mutex<()>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSessionStreamTokenKind {
    View,
    Signal,
}

#[derive(Debug, Clone)]
pub(super) struct WebSessionStreamToken {
    kind: WebSessionStreamTokenKind,
    expires_at: DateTime<Utc>,
}

pub(super) struct WebSessionRuntime {
    pub(super) status: WebSessionStatus,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) last_activity: DateTime<Utc>,
    pub(super) viewers: u32,
    pub(super) worker_port: u16,
    pub(super) child: Option<Child>,
    pub(super) work_dir: Option<PathBuf>,
}

impl WebSessionHandle {
    pub async fn snapshot(&self) -> WebSessionInfo {
        let runtime = self.runtime.lock().await;
        WebSessionInfo {
            status: runtime.status.clone(),
            updated_at: runtime.updated_at,
            last_activity: runtime.last_activity,
            viewers: runtime.viewers,
            ..self.info.clone()
        }
    }

    pub async fn touch(&self) {
        let mut runtime = self.runtime.lock().await;
        runtime.last_activity = Utc::now();
        runtime.updated_at = runtime.last_activity;
    }

    pub async fn set_viewers(&self, viewers: u32) {
        let mut runtime = self.runtime.lock().await;
        runtime.viewers = viewers;
        runtime.updated_at = Utc::now();
    }

    pub async fn worker_port(&self) -> u16 {
        let runtime = self.runtime.lock().await;
        runtime.worker_port
    }

    pub async fn work_dir(&self) -> Option<PathBuf> {
        let runtime = self.runtime.lock().await;
        runtime.work_dir.clone()
    }

    async fn issue_stream_connect_path(
        &self,
        kind: WebSessionStreamTokenKind,
    ) -> (String, DateTime<Utc>) {
        let now = Utc::now();
        let expires_at = now + chrono::Duration::seconds(WEB_SESSION_STREAM_TOKEN_TTL_SECS);
        let mut tokens = self.stream_tokens.lock().await;
        tokens.retain(|_, access| access.expires_at > now);
        let token = Uuid::new_v4().to_string();
        tokens.insert(token.clone(), WebSessionStreamToken { kind, expires_at });
        let path = match kind {
            WebSessionStreamTokenKind::View => build_stream_connect_path(&self.info.id, &token),
            WebSessionStreamTokenKind::Signal => build_signal_connect_path(&self.info.id, &token),
        };
        (path, expires_at)
    }

    pub async fn issue_view_connect_path(&self) -> (String, DateTime<Utc>) {
        self.issue_stream_connect_path(WebSessionStreamTokenKind::View)
            .await
    }

    pub async fn issue_signal_connect_path(&self) -> (String, DateTime<Utc>) {
        self.issue_stream_connect_path(WebSessionStreamTokenKind::Signal)
            .await
    }

    async fn consume_stream_token(&self, token: &str, kind: WebSessionStreamTokenKind) -> bool {
        let now = Utc::now();
        let mut tokens = self.stream_tokens.lock().await;
        tokens.retain(|_, access| access.expires_at > now);
        let Some(access) = tokens.get(token).cloned() else {
            return false;
        };
        if access.expires_at <= now || access.kind != kind {
            return false;
        }
        tokens.remove(token);
        true
    }

    pub async fn consume_view_token(&self, token: &str) -> bool {
        self.consume_stream_token(token, WebSessionStreamTokenKind::View)
            .await
    }

    pub async fn consume_signal_token(&self, token: &str) -> bool {
        self.consume_stream_token(token, WebSessionStreamTokenKind::Signal)
            .await
    }

    pub fn worker_auth_secret(&self) -> &str {
        &self.worker_auth_secret
    }

    pub async fn close(&self) -> Result<()> {
        let mut runtime = self.runtime.lock().await;
        if let Some(mut child) = runtime.child.take() {
            let _ = child.kill().await;
        }
        runtime.status = WebSessionStatus::Closed;
        runtime.updated_at = Utc::now();
        Ok(())
    }
}
