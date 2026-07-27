use anyhow::{bail, Context, Result};
use url::Url;

use super::{
    commercial_api::CommercialApiConfig,
    credential_vault::CredentialVaultNamespace,
    lifecycle::lifecycle_manifest::{release_trust, ReleaseChannel, ReleaseTrust},
    workos_device::WorkOsConfig,
};

const WORKOS_CLIENT_ID: &str = "client_01KQE1NZAXDDBNTV5HVB4N5CVC";
const WORKOS_API_ORIGIN: &str = "https://api.workos.com/";
const STAGING_COMMERCIAL_API_ORIGIN: &str =
    "https://ctx-local-pro-commercial-staging.fancy-sea-92df.workers.dev/";
const PRODUCTION_COMMERCIAL_API_ORIGIN: Option<&str> = None;
const CHANNEL_ENV: &str = "CTX_PRO_CHANNEL";
const STAGING_REFERRALS_AVAILABLE: bool = false;
const PRODUCTION_REFERRALS_AVAILABLE: bool = false;
const STAGING_ENTITLEMENT_ISSUER: &str = "https://commercial.staging.ctx.rs";
const STAGING_ENTITLEMENT_KEY_IDS: &[&str] = &["staging-2026-07-v1", "staging-2026-07-v2"];
const PRODUCTION_ENTITLEMENT_ISSUER: Option<&str> = None;
const PRODUCTION_ENTITLEMENT_KEY_IDS: &[&str] = &[];

#[derive(Debug, Clone, Copy)]
pub(super) struct EntitlementTrust {
    issuer: &'static str,
    key_ids: &'static [&'static str],
}

impl EntitlementTrust {
    pub(super) fn validate_identity(self, issuer: &str, key_id: &str) -> Result<()> {
        if issuer != self.issuer || !self.key_ids.contains(&key_id) {
            bail!("invalid_response: entitlement does not match the selected commercial channel");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub(super) struct CommercialConfig {
    pub(super) workos: WorkOsConfig,
    pub(super) api: CommercialApiConfig,
    pub(super) release_trust: ReleaseTrust,
    pub(super) entitlement_trust: EntitlementTrust,
    pub(super) vault_namespace: CredentialVaultNamespace,
}

impl CommercialConfig {
    pub(super) fn production() -> Result<Self> {
        Self::for_channel(selected_channel()?)
    }

    pub(super) fn ensure_referrals_available() -> Result<()> {
        if !referrals_available_for_channel(selected_channel()?) {
            bail!(
                "referral_unavailable: ctx referrals are disabled until the reviewed commercial and payout rollout is qualified"
            );
        }
        Ok(())
    }

    pub(super) fn referrals_available() -> bool {
        Self::ensure_referrals_available().is_ok()
    }

    fn for_channel(channel: ReleaseChannel) -> Result<Self> {
        match channel {
            ReleaseChannel::Stable => {
                let (Some(api_origin), Some(entitlement_issuer)) = (
                    PRODUCTION_COMMERCIAL_API_ORIGIN,
                    PRODUCTION_ENTITLEMENT_ISSUER,
                ) else {
                    bail!(
                        "commercial_unavailable: ctx Pro stable channel is not configured; production API, release key, and entitlement key are unavailable"
                    );
                };
                if PRODUCTION_ENTITLEMENT_KEY_IDS.is_empty() {
                    bail!(
                        "commercial_unavailable: ctx Pro stable channel is not configured; production API, release key, and entitlement key are unavailable"
                    );
                }
                Ok(Self {
                    workos: WorkOsConfig {
                        api_origin: parse_origin(WORKOS_API_ORIGIN, "WorkOS")?,
                        client_id: WORKOS_CLIENT_ID.to_owned(),
                    },
                    api: CommercialApiConfig {
                        origin: parse_origin(api_origin, "commercial API")?,
                    },
                    release_trust: release_trust(channel)?,
                    entitlement_trust: EntitlementTrust {
                        issuer: entitlement_issuer,
                        key_ids: PRODUCTION_ENTITLEMENT_KEY_IDS,
                    },
                    vault_namespace: CredentialVaultNamespace::Production,
                })
            }
            ReleaseChannel::Staging => Ok(Self {
                workos: WorkOsConfig {
                    api_origin: parse_origin(WORKOS_API_ORIGIN, "WorkOS")?,
                    client_id: WORKOS_CLIENT_ID.to_owned(),
                },
                api: CommercialApiConfig {
                    origin: parse_origin(STAGING_COMMERCIAL_API_ORIGIN, "commercial API")?,
                },
                release_trust: release_trust(channel)?,
                entitlement_trust: EntitlementTrust {
                    issuer: STAGING_ENTITLEMENT_ISSUER,
                    key_ids: STAGING_ENTITLEMENT_KEY_IDS,
                },
                vault_namespace: CredentialVaultNamespace::Staging,
            }),
        }
    }
}

fn referrals_available_for_channel(channel: ReleaseChannel) -> bool {
    match channel {
        ReleaseChannel::Stable => PRODUCTION_REFERRALS_AVAILABLE,
        ReleaseChannel::Staging => STAGING_REFERRALS_AVAILABLE,
    }
}

fn selected_channel() -> Result<ReleaseChannel> {
    let channel = match std::env::var(CHANNEL_ENV) {
        Ok(value) if value == "staging" => ReleaseChannel::Staging,
        Ok(value) if value == "stable" => ReleaseChannel::Stable,
        Ok(_) => bail!("invalid_request: {CHANNEL_ENV} must be stable or staging"),
        Err(std::env::VarError::NotPresent) => ReleaseChannel::Stable,
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("invalid_request: {CHANNEL_ENV} must be valid UTF-8")
        }
    };
    Ok(channel)
}

fn parse_origin(value: &str, label: &str) -> Result<Url> {
    Url::parse(value).with_context(|| format!("invalid_request: {label} origin is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_are_https_origins() {
        for value in [WORKOS_API_ORIGIN, STAGING_COMMERCIAL_API_ORIGIN] {
            let origin = Url::parse(value).unwrap();
            assert_eq!(origin.scheme(), "https");
            assert_eq!(origin.path(), "/");
            assert!(origin.host_str().is_some());
        }
        assert!(WORKOS_CLIENT_ID.starts_with("client_"));
    }

    #[test]
    fn stable_fails_closed_until_production_trust_is_configured() {
        let error = CommercialConfig::for_channel(ReleaseChannel::Stable).unwrap_err();
        assert_eq!(
            error.to_string(),
            "commercial_unavailable: ctx Pro stable channel is not configured; production API, release key, and entitlement key are unavailable"
        );
    }

    #[test]
    fn staging_registry_binds_api_release_and_entitlement_identities() {
        let config = CommercialConfig::for_channel(ReleaseChannel::Staging).unwrap();
        assert_eq!(config.api.origin.as_str(), STAGING_COMMERCIAL_API_ORIGIN);
        assert_eq!(config.release_trust.channel, ReleaseChannel::Staging);
        config
            .entitlement_trust
            .validate_identity(STAGING_ENTITLEMENT_ISSUER, "staging-2026-07-v2")
            .unwrap();
        assert!(config
            .entitlement_trust
            .validate_identity("https://commercial.ctx.rs", "production-2026-07-v1")
            .is_err());
    }

    #[test]
    fn referrals_are_reviewed_and_default_disabled_on_every_channel() {
        assert!(!referrals_available_for_channel(ReleaseChannel::Staging));
        assert!(!referrals_available_for_channel(ReleaseChannel::Stable));
        assert!(!CommercialConfig::referrals_available());
        assert!(CommercialConfig::ensure_referrals_available()
            .unwrap_err()
            .to_string()
            .starts_with("referral_unavailable:"));
    }
}
