use ctx_core::models::{AttachmentMode, AttachmentUpdatePolicy, WorkspaceAttachmentKind};
use serde::Deserialize;

use super::WorkspaceRouteError;

#[derive(Debug, Deserialize)]
pub struct SyncWorkspaceAttachmentsRouteRequest {
    #[serde(default)]
    refresh: Option<bool>,
}

impl SyncWorkspaceAttachmentsRouteRequest {
    pub fn refresh(&self) -> bool {
        self.refresh.unwrap_or(false)
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkspaceAttachmentRouteRequest {
    kind: WorkspaceAttachmentKind,
    name: String,
    source: String,
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    subpath: Option<String>,
    #[serde(default)]
    mount_relpath: Option<String>,
    #[serde(default)]
    mode: Option<AttachmentMode>,
    #[serde(default)]
    update_policy: Option<AttachmentUpdatePolicy>,
}

impl CreateWorkspaceAttachmentRouteRequest {
    pub fn into_spec(self) -> Result<WorkspaceAttachmentCreateRouteSpec, WorkspaceRouteError> {
        if self.name.trim().is_empty() || self.source.trim().is_empty() {
            return Err(WorkspaceRouteError::bad_request(
                "name and source are required",
            ));
        }
        Ok(WorkspaceAttachmentCreateRouteSpec {
            kind: self.kind,
            name: self.name,
            source: self.source,
            revision: self.revision,
            subpath: self.subpath,
            mount_relpath: self.mount_relpath,
            mode: self.mode,
            update_policy: self.update_policy,
        })
    }
}

#[derive(Debug)]
pub struct WorkspaceAttachmentCreateRouteSpec {
    pub kind: WorkspaceAttachmentKind,
    pub name: String,
    pub source: String,
    pub revision: Option<String>,
    pub subpath: Option<String>,
    pub mount_relpath: Option<String>,
    pub mode: Option<AttachmentMode>,
    pub update_policy: Option<AttachmentUpdatePolicy>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteWorkspaceAttachmentRouteRequest {
    kind: WorkspaceAttachmentKind,
    name: String,
}

impl DeleteWorkspaceAttachmentRouteRequest {
    pub fn into_spec(self) -> Result<WorkspaceAttachmentDeleteRouteSpec, WorkspaceRouteError> {
        if self.name.trim().is_empty() {
            return Err(WorkspaceRouteError::bad_request("name is required"));
        }
        Ok(WorkspaceAttachmentDeleteRouteSpec {
            kind: self.kind,
            name: self.name,
        })
    }
}

#[derive(Debug)]
pub struct WorkspaceAttachmentDeleteRouteSpec {
    pub kind: WorkspaceAttachmentKind,
    pub name: String,
}
