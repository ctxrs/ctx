use super::*;

pub struct MobileDeviceUpsert {
    pub device_label: Option<String>,
    pub platform: Option<String>,
    pub push_token: Option<String>,
    pub push_provider: Option<String>,
    pub public_key: Option<String>,
    pub app_version: Option<String>,
}

#[derive(Debug)]
pub struct MobileAccessConfig {
    pub id: String,
    pub profile_id: ConnectionProfileId,
    pub tunnel_id: String,
    pub public_base_url: String,
    pub relay_base_url: String,
    pub tunnel_secret: String,
    pub daemon_public_key: String,
    pub daemon_private_key: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct RuntimeSettingsDocument {
    pub id: String,
    pub schema_version: i64,
    pub settings_json: String,
    pub secret_ref: Option<String>,
    pub updated_at: DateTime<Utc>,
}

pub enum MobileDeviceSeqAdvance {
    Advanced,
    Stale { current: i64 },
    Missing,
}
