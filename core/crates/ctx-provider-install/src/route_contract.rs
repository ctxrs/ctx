use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::install_state::{InstallId, InstallInfo, InstallProgressEvent, InstallTarget};

pub type ProviderInstallInfo = InstallInfo;
pub type ProviderInstallProgressEvent = InstallProgressEvent;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInstallStartRouteResponse {
    provider_id: String,
    install_id: InstallId,
    target: InstallTarget,
}

impl ProviderInstallStartRouteResponse {
    pub fn new(provider_id: String, install_id: InstallId, target: InstallTarget) -> Self {
        Self {
            provider_id,
            install_id,
            target,
        }
    }

    pub fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub fn install_id(&self) -> InstallId {
        self.install_id
    }

    pub fn target(&self) -> InstallTarget {
        self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInstallJsonRouteErrorStatus {
    BadRequest,
    Forbidden,
}

#[derive(Debug, Clone)]
pub struct ProviderInstallJsonRouteError {
    status: ProviderInstallJsonRouteErrorStatus,
    body: Value,
}

impl ProviderInstallJsonRouteError {
    pub fn new(status: ProviderInstallJsonRouteErrorStatus, body: Value) -> Self {
        Self { status, body }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: ProviderInstallJsonRouteErrorStatus::BadRequest,
            body: serde_json::json!({
                "error": message.into(),
            }),
        }
    }

    pub fn start_failure(
        status: ProviderInstallJsonRouteErrorStatus,
        message: impl Into<String>,
        code: Option<String>,
    ) -> Self {
        Self {
            status,
            body: serde_json::json!({
                "error": message.into(),
                "code": code,
            }),
        }
    }

    pub fn status(&self) -> ProviderInstallJsonRouteErrorStatus {
        self.status
    }

    pub fn body(&self) -> &Value {
        &self.body
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInstallStatusesRouteRequest {
    install_ids: Vec<String>,
}

impl ProviderInstallStatusesRouteRequest {
    pub fn new(install_ids: Vec<String>) -> Self {
        Self { install_ids }
    }

    pub fn install_id_strings(&self) -> &[String] {
        &self.install_ids
    }

    pub fn into_raw_install_ids(self) -> Vec<String> {
        self.install_ids
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInstallStatusBatchItem {
    install_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<InstallInfo>,
}

impl ProviderInstallStatusBatchItem {
    pub fn new(install_id: String, info: Option<InstallInfo>) -> Self {
        Self { install_id, info }
    }

    pub fn install_id(&self) -> &str {
        &self.install_id
    }

    pub fn info(&self) -> Option<&InstallInfo> {
        self.info.as_ref()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderInstallStatusesRouteResponse {
    installs: Vec<ProviderInstallStatusBatchItem>,
}

impl ProviderInstallStatusesRouteResponse {
    pub fn new(installs: Vec<ProviderInstallStatusBatchItem>) -> Self {
        Self { installs }
    }

    pub fn installs(&self) -> &[ProviderInstallStatusBatchItem] {
        &self.installs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderInstallStatusOnlyRouteError {
    BadRequest,
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::install_state::{InstallStateKind, InstallTarget};
    use chrono::Utc;

    #[test]
    fn start_response_preserves_wire_shape() {
        let install_id = InstallId::nil();
        let response = ProviderInstallStartRouteResponse::new(
            "codex".to_string(),
            install_id,
            InstallTarget::Container,
        );
        let payload = serde_json::to_value(response).expect("serialize start response");

        assert_eq!(payload["provider_id"].as_str(), Some("codex"));
        assert_eq!(
            payload["install_id"].as_str(),
            Some(&*install_id.to_string())
        );
        assert_eq!(payload["target"].as_str(), Some("container"));
    }

    #[test]
    fn json_errors_preserve_status_and_body_shape() {
        let invalid = ProviderInstallJsonRouteError::bad_request("invalid install target");
        assert_eq!(
            invalid.status(),
            ProviderInstallJsonRouteErrorStatus::BadRequest
        );
        assert_eq!(
            invalid.body()["error"].as_str(),
            Some("invalid install target")
        );
        assert!(invalid.body().get("code").is_none());

        let disabled = ProviderInstallJsonRouteError::start_failure(
            ProviderInstallJsonRouteErrorStatus::Forbidden,
            "host provider installs are disabled by daemon policy",
            Some("install_target_disabled".to_string()),
        );
        assert_eq!(
            disabled.status(),
            ProviderInstallJsonRouteErrorStatus::Forbidden
        );
        assert_eq!(
            disabled.body()["error"].as_str(),
            Some("host provider installs are disabled by daemon policy")
        );
        assert_eq!(
            disabled.body()["code"].as_str(),
            Some("install_target_disabled")
        );
    }

    #[test]
    fn statuses_request_preserves_raw_install_id_strings() {
        let request: ProviderInstallStatusesRouteRequest = serde_json::from_value(
            serde_json::json!({ "install_ids": [" 00000000-0000-0000-0000-000000000000 "] }),
        )
        .expect("deserialize status request");

        assert_eq!(
            request.install_id_strings(),
            &[" 00000000-0000-0000-0000-000000000000 ".to_string()]
        );
        assert_eq!(
            request.into_raw_install_ids(),
            vec![" 00000000-0000-0000-0000-000000000000 ".to_string()]
        );
    }

    #[test]
    fn statuses_response_preserves_wire_shape_and_omits_missing_info() {
        let install_id = InstallId::nil();
        let info = InstallInfo {
            install_id,
            provider_id: "codex".to_string(),
            target: Some(InstallTarget::Container),
            state: InstallStateKind::Running,
            started_at: Utc::now(),
            finished_at: None,
            error: None,
            error_code: None,
            progress_pct: None,
            last_event: None,
        };
        let response = ProviderInstallStatusesRouteResponse::new(vec![
            ProviderInstallStatusBatchItem::new(install_id.to_string(), Some(info)),
            ProviderInstallStatusBatchItem::new(InstallId::from_u128(u128::MAX).to_string(), None),
        ]);
        let payload = serde_json::to_value(response).expect("serialize status response");
        let installs = payload["installs"].as_array().expect("installs array");

        assert_eq!(
            installs[0]["install_id"].as_str(),
            Some(&*install_id.to_string())
        );
        assert_eq!(installs[0]["info"]["provider_id"].as_str(), Some("codex"));
        assert_eq!(
            installs[1]["install_id"].as_str(),
            Some(&*InstallId::from_u128(u128::MAX).to_string())
        );
        assert!(installs[1].get("info").is_none());
    }
}
