use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(in crate::api) struct CreateMobileConnectionProfileReq {
    pub(in crate::api) label: String,
    pub(in crate::api) base_url: String,
    pub(in crate::api) scopes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(in crate::api) struct CreateMobileConnectionProfileResp {
    pub(in crate::api) profile: MobileConnectionProfile,
    pub(in crate::api) token: String,
    pub(in crate::api) qr_payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(in crate::api) struct RegisterMobileDeviceReq {
    pub(in crate::api) device_id: String,
    #[serde(default)]
    pub(in crate::api) device_label: Option<String>,
    #[serde(default)]
    pub(in crate::api) platform: Option<String>,
    #[serde(default)]
    pub(in crate::api) push_token: Option<String>,
    #[serde(default)]
    pub(in crate::api) push_provider: Option<String>,
    #[serde(default)]
    pub(in crate::api) public_key: Option<String>,
    #[serde(default)]
    pub(in crate::api) app_version: Option<String>,
}
