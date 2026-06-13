use super::*;
pub use ctx_route_contracts::web_sessions::{
    WebSessionInfo, WebSessionRunRequest, WebSessionRunResponse, WebSessionStatus,
    WebSessionViewport,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebSessionCreateRequest {
    pub url: String,
    pub viewport: Option<WebSessionViewport>,
    pub fps: Option<u32>,
    pub work_dir: Option<PathBuf>,
    pub session_id: Option<String>,
    pub worktree_id: Option<String>,
    pub node_bin: PathBuf,
    pub worker_path: PathBuf,
    pub node_modules_path: PathBuf,
}
