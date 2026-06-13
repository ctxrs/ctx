use anyhow::Result;
use reqwest::Method;

use ctx_core::ids::WorkspaceId;
use ctx_core::models::WorkspaceAttachment;
use ctx_providers::adapters::ProviderStatus;

use crate::client::Client;
use crate::types::*;

impl Client {
    pub async fn list_providers(&self) -> Result<Vec<ProviderStatus>> {
        self.request_json(Method::GET, "/api/providers", None::<&()>)
            .await
    }

    pub async fn get_settings(&self) -> Result<PublicSettings> {
        self.request_json(Method::GET, "/api/settings", None::<&()>)
            .await
    }

    pub async fn update_settings(&self, req: &UpdateSettingsRequest) -> Result<PublicSettings> {
        self.request_json(Method::POST, "/api/settings", Some(req))
            .await
    }

    pub async fn list_workspace_attachments(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let path = format!("/api/workspaces/{}/attachments", workspace_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn sync_workspace_attachments(
        &self,
        workspace_id: WorkspaceId,
        refresh: bool,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let path = format!("/api/workspaces/{}/attachments/sync", workspace_id.0);
        let req = SyncWorkspaceAttachmentsRequest { refresh };
        self.request_json(Method::POST, &path, Some(&req)).await
    }

    pub async fn create_workspace_attachment(
        &self,
        workspace_id: WorkspaceId,
        req: &CreateWorkspaceAttachmentRequest,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let path = format!("/api/workspaces/{}/attachments", workspace_id.0);
        self.request_json(Method::POST, &path, Some(req)).await
    }

    pub async fn delete_workspace_attachment(
        &self,
        workspace_id: WorkspaceId,
        req: &DeleteWorkspaceAttachmentRequest,
    ) -> Result<Vec<WorkspaceAttachment>> {
        let path = format!("/api/workspaces/{}/attachments", workspace_id.0);
        self.request_json(Method::DELETE, &path, Some(req)).await
    }

    pub async fn get_resource_utilization(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<ResourceUtilizationSnapshot> {
        let path = format!("/api/resource_utilization?workspace_id={}", workspace_id.0);
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_mobile_access_status(&self) -> Result<MobileAccessStatus> {
        self.request_json(Method::GET, "/api/mobile/access/status", None::<&()>)
            .await
    }

    pub async fn enable_mobile_access(
        &self,
        supabase_token: &str,
    ) -> Result<EnableMobileAccessResponse> {
        let req = serde_json::json!({ "supabase_token": supabase_token });
        self.request_json(Method::POST, "/api/mobile/access/enable", Some(&req))
            .await
    }

    pub async fn disable_mobile_access(&self, supabase_token: &str) -> Result<()> {
        let req = serde_json::json!({ "supabase_token": supabase_token });
        self.request_empty(Method::POST, "/api/mobile/access/disable", Some(&req))
            .await
    }

    pub async fn get_provider_options(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
    ) -> Result<ProviderOptions> {
        let path = format!(
            "/api/workspaces/{}/providers/{}/options",
            workspace_id.0, provider_id
        );
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn authenticate_provider_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
        method_id: Option<&str>,
    ) -> Result<ProviderAuthCheck> {
        let path = format!(
            "/api/workspaces/{}/providers/{}/authenticate",
            workspace_id.0, provider_id
        );
        let req = AuthenticateProviderRequest {
            method_id: method_id.map(|value| value.to_string()),
        };
        self.request_json(Method::POST, &path, Some(&req)).await
    }

    pub async fn verify_provider_for_workspace(
        &self,
        workspace_id: WorkspaceId,
        provider_id: &str,
    ) -> Result<ProviderAuthCheck> {
        let path = format!(
            "/api/workspaces/{}/providers/{}/verify",
            workspace_id.0, provider_id
        );
        self.request_json(Method::POST, &path, None::<&()>).await
    }

    pub async fn install_provider(&self, provider_id: &str) -> Result<InstallStartResponse> {
        let path = format!("/api/providers/{provider_id}/install");
        self.request_json(Method::POST, &path, None::<&()>).await
    }

    pub async fn install_all_providers(&self) -> Result<Vec<InstallStartResponse>> {
        self.request_json(Method::POST, "/api/providers/install_all", None::<&()>)
            .await
    }

    pub async fn get_install(&self, install_id: &str) -> Result<InstallInfo> {
        let path = format!("/api/providers/install/{install_id}");
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn list_install_events(&self, install_id: &str) -> Result<Vec<InstallProgressEvent>> {
        let path = format!("/api/providers/install/{install_id}/events");
        self.request_json(Method::GET, &path, None::<&()>).await
    }

    pub async fn get_title_generation_local_status(&self) -> Result<TitleGenerationLocalStatus> {
        self.request_json(
            Method::GET,
            "/api/title_generation/local/status",
            None::<&()>,
        )
        .await
    }

    pub async fn install_title_generation_local(
        &self,
    ) -> Result<TitleGenerationLocalInstallResponse> {
        self.request_json(
            Method::POST,
            "/api/title_generation/local/install",
            None::<&()>,
        )
        .await
    }
}
