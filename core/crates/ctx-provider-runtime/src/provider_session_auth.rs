use std::collections::HashMap;
use std::path::PathBuf;

use ctx_providers::adapters::ProviderRunHooks;
use ctx_providers::events::NormalizedEvent;
use tokio::sync::mpsc;

use crate::ProviderRuntime;

#[derive(Debug)]
pub enum ProviderSessionAuthenticationError {
    AdapterUnavailable,
    Authenticate(anyhow::Error),
}

pub struct ProviderSessionAuthenticationRequest {
    pub session_key: String,
    pub workdir: PathBuf,
    pub env: HashMap<String, String>,
    pub method_id: Option<String>,
    pub event_sink: mpsc::Sender<NormalizedEvent>,
    pub hooks: ProviderRunHooks,
}

impl ProviderRuntime {
    pub async fn authenticate_provider_session(
        &self,
        provider_id: &str,
        request: ProviderSessionAuthenticationRequest,
    ) -> Result<(), ProviderSessionAuthenticationError> {
        let Some(adapter) = self.provider_adapter(provider_id).await else {
            return Err(ProviderSessionAuthenticationError::AdapterUnavailable);
        };
        adapter
            .authenticate_session(
                request.session_key,
                request.workdir,
                request.env,
                request.method_id,
                request.event_sink,
                request.hooks,
            )
            .await
            .map_err(ProviderSessionAuthenticationError::Authenticate)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use anyhow::Result;
    use async_trait::async_trait;
    use ctx_providers::adapters::{
        ProviderAdapter, ProviderHealth, ProviderProcessInfo, ProviderStatus, ProviderUsability,
        RunHandle, TurnInput,
    };
    use std::sync::Arc;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct AuthCall {
        session_key: String,
        workdir: PathBuf,
        env_value: Option<String>,
        method_id: Option<String>,
    }

    #[derive(Default)]
    struct AuthRecordingAdapter {
        calls: StdMutex<Vec<AuthCall>>,
        error: StdMutex<Option<String>>,
    }

    impl AuthRecordingAdapter {
        fn calls(&self) -> Vec<AuthCall> {
            self.calls
                .lock()
                .expect("auth recording adapter call lock")
                .clone()
        }

        fn set_error(&self, error: &str) {
            *self
                .error
                .lock()
                .expect("auth recording adapter error lock") = Some(error.to_string());
        }
    }

    #[async_trait]
    impl ProviderAdapter for AuthRecordingAdapter {
        async fn inspect(&self) -> Result<ProviderStatus> {
            Ok(ProviderStatus {
                provider_id: "auth-recording".into(),
                installed: true,
                detected_path: None,
                version: Some("test".into()),
                capabilities: None,
                health: ProviderHealth::Ok,
                diagnostics: Vec::new(),
                details: HashMap::new(),
                usability: ProviderUsability::default(),
            })
        }

        async fn run(
            &self,
            _input: TurnInput,
            _workdir: PathBuf,
            _env: HashMap<String, String>,
            _event_sink: mpsc::Sender<NormalizedEvent>,
            _hooks: ProviderRunHooks,
        ) -> Result<RunHandle> {
            anyhow::bail!("not used in test");
        }

        async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
            Ok(())
        }

        async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
            Vec::new()
        }

        async fn authenticate_session(
            &self,
            session_key: String,
            workdir: PathBuf,
            env: HashMap<String, String>,
            method_id: Option<String>,
            _event_sink: mpsc::Sender<NormalizedEvent>,
            _hooks: ProviderRunHooks,
        ) -> Result<()> {
            self.calls
                .lock()
                .expect("auth recording adapter call lock")
                .push(AuthCall {
                    session_key,
                    workdir,
                    env_value: env.get("AUTH_ENV").cloned(),
                    method_id,
                });
            if let Some(error) = self
                .error
                .lock()
                .expect("auth recording adapter error lock")
                .clone()
            {
                anyhow::bail!("{error}");
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn authenticate_provider_session_invokes_matching_adapter() {
        let adapter = Arc::new(AuthRecordingAdapter::default());
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("gemini".into(), adapter.clone());
        let runtime = ProviderRuntime::new(providers);
        let (event_tx, _event_rx) = mpsc::channel(1);
        let mut env = HashMap::new();
        env.insert("AUTH_ENV".to_string(), "present".to_string());
        let workdir = PathBuf::from("/tmp/auth-workdir");

        runtime
            .authenticate_provider_session(
                "gemini",
                ProviderSessionAuthenticationRequest {
                    session_key: "session-key".to_string(),
                    workdir: workdir.clone(),
                    env,
                    method_id: Some("oauth".to_string()),
                    event_sink: event_tx,
                    hooks: ProviderRunHooks::default(),
                },
            )
            .await
            .expect("authenticate provider session");

        assert_eq!(
            adapter.calls(),
            vec![AuthCall {
                session_key: "session-key".to_string(),
                workdir,
                env_value: Some("present".to_string()),
                method_id: Some("oauth".to_string()),
            }]
        );
    }

    #[tokio::test]
    async fn authenticate_provider_session_reports_missing_adapter() {
        let runtime = ProviderRuntime::new(HashMap::new());
        let (event_tx, _event_rx) = mpsc::channel(1);

        let err = runtime
            .authenticate_provider_session(
                "missing",
                ProviderSessionAuthenticationRequest {
                    session_key: "session-key".to_string(),
                    workdir: PathBuf::from("/tmp/auth-workdir"),
                    env: HashMap::new(),
                    method_id: None,
                    event_sink: event_tx,
                    hooks: ProviderRunHooks::default(),
                },
            )
            .await
            .expect_err("missing adapter should fail");

        assert!(matches!(
            err,
            ProviderSessionAuthenticationError::AdapterUnavailable
        ));
    }

    #[tokio::test]
    async fn authenticate_provider_session_preserves_adapter_error() {
        let adapter = Arc::new(AuthRecordingAdapter::default());
        adapter.set_error("auth failed");
        let mut providers: HashMap<String, Arc<dyn ProviderAdapter>> = HashMap::new();
        providers.insert("gemini".into(), adapter);
        let runtime = ProviderRuntime::new(providers);
        let (event_tx, _event_rx) = mpsc::channel(1);

        let err = runtime
            .authenticate_provider_session(
                "gemini",
                ProviderSessionAuthenticationRequest {
                    session_key: "session-key".to_string(),
                    workdir: PathBuf::from("/tmp/auth-workdir"),
                    env: HashMap::new(),
                    method_id: None,
                    event_sink: event_tx,
                    hooks: ProviderRunHooks::default(),
                },
            )
            .await
            .expect_err("adapter error should fail");

        match err {
            ProviderSessionAuthenticationError::Authenticate(err) => {
                assert_eq!(err.to_string(), "auth failed");
            }
            ProviderSessionAuthenticationError::AdapterUnavailable => {
                panic!("expected adapter authenticate error")
            }
        }
    }
}
