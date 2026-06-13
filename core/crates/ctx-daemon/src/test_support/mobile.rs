use ctx_core::ids::{ConnectionProfileId, MobileDeviceId};
use ctx_core::models::{MobileConnectionProfile, MobileDeviceRegistration};
use ctx_store::store::{MobileAccessConfig, MobileDeviceUpsert};
use sha2::Digest;

use super::TestMobileAccessForTest;

impl TestMobileAccessForTest<'_> {
    fn token_hash(token: &str) -> String {
        let mut hasher = sha2::Sha256::new();
        hasher.update(token.as_bytes());
        hex::encode(hasher.finalize())
    }

    fn token_prefix(token: &str) -> String {
        token.chars().take(8).collect()
    }

    async fn create_profile_with_token_hash(
        &self,
        label: &str,
        base_url: &str,
        token_hash: String,
        token_prefix: String,
        scopes: &[&str],
    ) -> anyhow::Result<MobileConnectionProfile> {
        self.state
            .global_store()
            .create_mobile_connection_profile(
                label.to_string(),
                base_url.to_string(),
                token_hash,
                token_prefix,
                scopes.iter().map(|scope| (*scope).to_string()).collect(),
            )
            .await
    }

    pub async fn seed_mobile_api_profile_for_test(
        &self,
        token: &str,
        scopes: &[&str],
    ) -> anyhow::Result<MobileConnectionProfile> {
        self.create_profile_with_token_hash(
            "mobile",
            "https://example.com",
            Self::token_hash(token),
            Self::token_prefix(token),
            scopes,
        )
        .await
    }

    pub async fn seed_empty_managed_mobile_access_profile_for_test(
        &self,
    ) -> anyhow::Result<MobileConnectionProfile> {
        self.create_profile_with_token_hash(
            "Managed Mobile Access",
            "https://legacy.example.com",
            "legacy-token-hash".to_string(),
            "legacy-m".to_string(),
            &[],
        )
        .await
    }

    pub async fn mobile_profile_for_test(
        &self,
        profile_id: ConnectionProfileId,
    ) -> anyhow::Result<Option<MobileConnectionProfile>> {
        self.state
            .global_store()
            .get_mobile_connection_profile(profile_id)
            .await
    }

    pub async fn seed_default_mobile_access_config_for_test(
        &self,
        profile_id: ConnectionProfileId,
        enabled: bool,
        daemon_public_key: String,
        daemon_private_key: String,
    ) -> anyhow::Result<MobileAccessConfig> {
        self.seed_mobile_access_config_for_test(
            profile_id,
            "tunnel-1",
            "https://example.com",
            "https://relay.example.com",
            "secret",
            daemon_public_key,
            daemon_private_key,
            enabled,
        )
        .await
    }

    pub async fn seed_legacy_mobile_access_config_for_test(
        &self,
        profile_id: ConnectionProfileId,
        enabled: bool,
        daemon_public_key: String,
        daemon_private_key: String,
    ) -> anyhow::Result<MobileAccessConfig> {
        self.seed_mobile_access_config_for_test(
            profile_id,
            "legacy-tunnel",
            "https://legacy.example.com",
            "https://legacy-relay.example.com",
            "legacy-secret",
            daemon_public_key,
            daemon_private_key,
            enabled,
        )
        .await
    }

    async fn seed_mobile_access_config_for_test(
        &self,
        profile_id: ConnectionProfileId,
        tunnel_id: &str,
        public_base_url: &str,
        relay_base_url: &str,
        tunnel_secret: &str,
        daemon_public_key: String,
        daemon_private_key: String,
        enabled: bool,
    ) -> anyhow::Result<MobileAccessConfig> {
        self.state
            .global_store()
            .upsert_mobile_access_config(MobileAccessConfig {
                id: "default".to_string(),
                profile_id,
                tunnel_id: tunnel_id.to_string(),
                public_base_url: public_base_url.to_string(),
                relay_base_url: relay_base_url.to_string(),
                tunnel_secret: tunnel_secret.to_string(),
                daemon_public_key,
                daemon_private_key,
                enabled,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            })
            .await
    }

    pub async fn mobile_access_config_for_test(
        &self,
    ) -> anyhow::Result<Option<MobileAccessConfig>> {
        self.state.global_store().get_mobile_access_config().await
    }

    pub async fn seed_mobile_device_for_test(
        &self,
        device_id: MobileDeviceId,
        profile_id: ConnectionProfileId,
        public_key: String,
        device_label: &str,
    ) -> anyhow::Result<MobileDeviceRegistration> {
        self.state
            .global_store()
            .upsert_mobile_device(
                device_id,
                profile_id,
                MobileDeviceUpsert {
                    device_label: Some(device_label.to_string()),
                    platform: Some("ios".to_string()),
                    push_token: None,
                    push_provider: None,
                    public_key: Some(public_key),
                    app_version: Some("1.0.0".to_string()),
                },
            )
            .await
    }

    pub async fn mobile_device_for_test(
        &self,
        device_id: MobileDeviceId,
    ) -> anyhow::Result<Option<MobileDeviceRegistration>> {
        self.state.global_store().get_mobile_device(device_id).await
    }

    pub async fn seed_mobile_pairing_token_for_test(
        &self,
        id: &str,
        token: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> anyhow::Result<String> {
        let token_hash = Self::token_hash(token);
        self.state
            .global_store()
            .insert_mobile_pairing_token(id, &token_hash, expires_at)
            .await?;
        Ok(token_hash)
    }

    pub async fn consume_mobile_pairing_token_hash_for_test(
        &self,
        token_hash: &str,
    ) -> anyhow::Result<bool> {
        self.state
            .global_store()
            .consume_mobile_pairing_token(token_hash)
            .await
    }
}
