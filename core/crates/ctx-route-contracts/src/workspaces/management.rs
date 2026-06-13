use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub root_path: String,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkspacePrimaryBranchRequest {
    pub primary_branch: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspacePrimaryBranchSnapshot {
    pub primary_branch: String,
}

#[derive(Debug, Serialize)]
pub struct WorkspaceConfigUpdateResult {
    pub ok: bool,
}
