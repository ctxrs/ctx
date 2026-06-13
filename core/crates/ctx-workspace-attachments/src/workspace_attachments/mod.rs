use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use ctx_core::ids::{WorkspaceAttachmentId, WorkspaceId};
use ctx_core::models::{
    AttachmentMode, AttachmentUpdatePolicy, Workspace, WorkspaceAttachment,
    WorkspaceAttachmentKind, WorkspaceAttachmentStatus,
};
use serde::{Deserialize, Serialize};

mod doc_mirror;
mod materialized_install;
mod materialized_paths;
mod reference_repo;

use doc_mirror::{
    materialize_doc_mirror, validate_doc_mirror_source, validate_doc_mirror_source_value,
};

pub use materialized_paths::{
    materialized_path_for_attachment, materialized_root_for_attachment,
    remove_materialized_root_if_exists, revision_key, sanitize_attachment_subpath,
    sanitize_mount_relpath, validate_materialized_path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentConfig {
    pub kind: WorkspaceAttachmentKind,
    pub name: String,
    pub source: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub subpath: Option<String>,
    #[serde(default)]
    pub mount_relpath: Option<String>,
    #[serde(default)]
    pub mode: Option<AttachmentMode>,
    #[serde(default)]
    pub update_policy: Option<AttachmentUpdatePolicy>,
}

#[derive(Debug, Clone)]
pub struct MaterializationResult {
    pub path: std::path::PathBuf,
    pub materialized_id: String,
}

#[derive(Debug, Clone, Copy)]
pub struct AttachmentSyncPlan {
    pub id: WorkspaceAttachmentId,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct WorkspaceAttachmentSyncResult {
    pub attachments: Vec<WorkspaceAttachment>,
    pub plans: Vec<AttachmentSyncPlan>,
}

#[async_trait]
pub trait WorkspaceAttachmentsHost: Send + Sync + 'static {
    fn data_root(&self) -> &std::path::Path;

    async fn list_workspace_attachments(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<WorkspaceAttachment>>;

    async fn get_workspace_attachment(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
    ) -> Result<Option<WorkspaceAttachment>>;

    async fn upsert_workspace_attachment(&self, attachment: &WorkspaceAttachment) -> Result<()>;

    async fn update_workspace_attachment_status(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
        status: WorkspaceAttachmentStatus,
        last_sync_at: Option<DateTime<Utc>>,
        error_message: Option<String>,
        updated_at: DateTime<Utc>,
    ) -> Result<()>;

    async fn delete_workspace_attachment_record(
        &self,
        workspace_id: WorkspaceId,
        attachment_id: WorkspaceAttachmentId,
    ) -> Result<()>;

    async fn attachment_became_ready(
        &self,
        workspace: &Workspace,
        attachment: &WorkspaceAttachment,
    ) -> Result<()>;

    async fn cleanup_removed_attachment(&self, attachment: &WorkspaceAttachment) -> Result<()>;
}

pub async fn sync_workspace_attachments<H>(
    host: &H,
    workspace: &Workspace,
    refresh: bool,
) -> Result<WorkspaceAttachmentSyncResult>
where
    H: WorkspaceAttachmentsHost,
{
    let existing = host.list_workspace_attachments(workspace.id).await?;

    let mut attachments = Vec::with_capacity(existing.len());
    let mut plans = Vec::new();
    for mut attachment in existing {
        let now = Utc::now();
        let should_refresh = refresh || attachment.update_policy != AttachmentUpdatePolicy::Manual;
        let materialized_exists =
            materialized_path_for_attachment(host.data_root(), &attachment).exists();
        let should_materialize = should_refresh || !materialized_exists;
        if should_materialize && attachment.status != WorkspaceAttachmentStatus::Syncing {
            match validate_attachment_source_before_materialization(workspace, &attachment) {
                Ok(()) => {
                    attachment.status = WorkspaceAttachmentStatus::Pending;
                    attachment.error_message = None;
                    attachment.updated_at = now;
                    plans.push(AttachmentSyncPlan {
                        id: attachment.id,
                        refresh: should_refresh,
                    });
                }
                Err(err) => {
                    attachment.status = WorkspaceAttachmentStatus::Error;
                    attachment.error_message = Some(err.to_string());
                    attachment.updated_at = now;
                }
            }
        } else if !should_materialize && attachment.status != WorkspaceAttachmentStatus::Ready {
            attachment.status = WorkspaceAttachmentStatus::Ready;
            attachment.error_message = None;
            if attachment.last_sync_at.is_none() {
                attachment.last_sync_at = Some(now);
            }
            attachment.updated_at = now;
        }
        host.upsert_workspace_attachment(&attachment).await?;
        attachments.push(attachment);
    }

    Ok(WorkspaceAttachmentSyncResult { attachments, plans })
}

fn validate_attachment_source_before_materialization(
    workspace: &Workspace,
    attachment: &WorkspaceAttachment,
) -> Result<()> {
    match attachment.kind {
        WorkspaceAttachmentKind::DocMirror => validate_doc_mirror_source(workspace, attachment),
        WorkspaceAttachmentKind::ReferenceRepo => Ok(()),
    }
}

pub async fn upsert_workspace_attachment<H>(
    host: &H,
    workspace_id: WorkspaceId,
    cfg: AttachmentConfig,
) -> Result<WorkspaceAttachment>
where
    H: WorkspaceAttachmentsHost,
{
    let existing =
        find_workspace_attachment(host, workspace_id, cfg.kind.clone(), &cfg.name).await?;
    let attachment = normalize_attachment_config(workspace_id, cfg, existing)?;
    host.upsert_workspace_attachment(&attachment).await?;
    Ok(attachment)
}

pub async fn find_workspace_attachment<H>(
    host: &H,
    workspace_id: WorkspaceId,
    kind: WorkspaceAttachmentKind,
    name: &str,
) -> Result<Option<WorkspaceAttachment>>
where
    H: WorkspaceAttachmentsHost,
{
    let existing = host.list_workspace_attachments(workspace_id).await?;
    Ok(existing
        .into_iter()
        .find(|attachment| attachment.kind == kind && attachment.name.trim() == name.trim()))
}

pub async fn delete_workspace_attachment<H>(
    host: &H,
    attachment: &WorkspaceAttachment,
) -> Result<()>
where
    H: WorkspaceAttachmentsHost,
{
    host.cleanup_removed_attachment(attachment).await?;
    host.delete_workspace_attachment_record(attachment.workspace_id, attachment.id)
        .await
}

pub async fn run_attachment_materialization<H>(
    host: &H,
    workspace: &Workspace,
    attachment_id: WorkspaceAttachmentId,
    refresh: bool,
) -> Result<()>
where
    H: WorkspaceAttachmentsHost,
{
    let Some(attachment) = host
        .get_workspace_attachment(workspace.id, attachment_id)
        .await?
    else {
        return Ok(());
    };

    let now = Utc::now();
    host.update_workspace_attachment_status(
        workspace.id,
        attachment_id,
        WorkspaceAttachmentStatus::Syncing,
        None,
        None,
        now,
    )
    .await?;

    match materialize_attachment(host.data_root(), workspace, &attachment, refresh).await {
        Ok(_) => {
            let now = Utc::now();
            host.update_workspace_attachment_status(
                workspace.id,
                attachment_id,
                WorkspaceAttachmentStatus::Ready,
                Some(now),
                None,
                now,
            )
            .await?;
            let _ = host.attachment_became_ready(workspace, &attachment).await;
            Ok(())
        }
        Err(err) => {
            let now = Utc::now();
            host.update_workspace_attachment_status(
                workspace.id,
                attachment_id,
                WorkspaceAttachmentStatus::Error,
                None,
                Some(err.to_string()),
                now,
            )
            .await?;
            Err(err)
        }
    }
}

pub async fn materialize_attachment(
    data_root: &std::path::Path,
    workspace: &Workspace,
    attachment: &WorkspaceAttachment,
    refresh: bool,
) -> Result<MaterializationResult> {
    match attachment.kind {
        WorkspaceAttachmentKind::ReferenceRepo => {
            reference_repo::materialize_reference_repo(data_root, attachment, refresh).await
        }
        WorkspaceAttachmentKind::DocMirror => {
            materialize_doc_mirror(data_root, workspace, attachment, refresh).await
        }
    }
}

fn normalize_attachment_config(
    workspace_id: WorkspaceId,
    cfg: AttachmentConfig,
    existing: Option<WorkspaceAttachment>,
) -> Result<WorkspaceAttachment> {
    let name = cfg.name.trim().to_string();
    let source = cfg.source.trim().to_string();
    if source.is_empty() {
        anyhow::bail!("source must not be empty");
    }

    match &cfg.kind {
        WorkspaceAttachmentKind::ReferenceRepo => {
            reference_repo::validate_reference_repo_source(&source)?
        }
        WorkspaceAttachmentKind::DocMirror => {
            validate_doc_mirror_source_value(&source)?;
            if cfg.mode == Some(AttachmentMode::Rw) {
                anyhow::bail!("doc_mirror attachments are read-only; mode=rw is not supported");
            }
        }
    }

    let now = Utc::now();
    let (id, created_at, status, last_sync_at, error_message) = match existing {
        Some(existing) => (
            existing.id,
            existing.created_at,
            existing.status,
            existing.last_sync_at,
            existing.error_message,
        ),
        None => (
            WorkspaceAttachmentId::new(),
            now,
            WorkspaceAttachmentStatus::Pending,
            None,
            None,
        ),
    };

    let mount_relpath = cfg
        .mount_relpath
        .clone()
        .unwrap_or_else(|| materialized_paths::default_mount_relpath(&cfg.kind, &name));
    let mount_relpath = sanitize_mount_relpath(&mount_relpath)?
        .to_string_lossy()
        .to_string();
    let subpath = cfg
        .subpath
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    if let Some(value) = subpath.as_deref() {
        sanitize_attachment_subpath(value)?;
    }

    Ok(WorkspaceAttachment {
        id,
        workspace_id,
        kind: cfg.kind,
        name,
        source,
        revision: cfg
            .revision
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
        subpath,
        mount_relpath,
        mode: cfg.mode.unwrap_or(AttachmentMode::Ro),
        update_policy: cfg.update_policy.unwrap_or(AttachmentUpdatePolicy::Manual),
        status,
        last_sync_at,
        error_message,
        created_at,
        updated_at: now,
    })
}

#[cfg(test)]
mod tests;
