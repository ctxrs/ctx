use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone)]
pub enum AgentHistoryBackend {
    Local(LocalBackendConfig),
    Hosted(HostedBackendConfig),
}

#[derive(Debug, Clone)]
pub struct LocalBackendConfig {
    pub ctx_binary: PathBuf,
    pub data_root: Option<PathBuf>,
    pub timeout: Duration,
}

impl Default for LocalBackendConfig {
    fn default() -> Self {
        Self {
            ctx_binary: PathBuf::from("ctx"),
            data_root: None,
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostedBackendConfig {
    pub base_url: String,
    pub timeout: Duration,
}
