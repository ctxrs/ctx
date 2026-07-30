use anyhow::{bail, Context, Result};
use std::ffi::OsString;
use url::Url;

use super::{
    commercial_api::{CloudflareAccessCredentials, CommercialApiConfig},
    commercial_production_record,
    credential_vault::CredentialVaultNamespace,
    lifecycle::lifecycle_manifest::{release_trust, ReleaseChannel, ReleaseTrust},
    workos_device::WorkOsConfig,
};

const STAGING_WORKOS_CLIENT_ID: &str = "client_01KQE1NZAXDDBNTV5HVB4N5CVC";
const WORKOS_API_ORIGIN: &str = "https://api.workos.com/";
const STAGING_COMMERCIAL_API_ORIGIN: &str = "https://pro-staging.ctx.rs/";
const PRODUCTION_COMMERCIAL_API_ORIGIN: &str = "https://pro.ctx.rs/";
const CHANNEL_ENV: &str = "CTX_PRO_CHANNEL";
const STAGING_ACCESS_CLIENT_ID_ENV: &str = "CTX_PRO_STAGING_ACCESS_CLIENT_ID";
const STAGING_ACCESS_CLIENT_SECRET_ENV: &str = "CTX_PRO_STAGING_ACCESS_CLIENT_SECRET";
const STAGING_ENTITLEMENT_ISSUER: &str = "https://pro-staging.ctx.rs";
const STAGING_ENTITLEMENT_KEY_IDS: &[&str] = &["staging-2026-07-v3"];
const PRODUCTION_ENTITLEMENT_ISSUER: &str = "https://pro.ctx.rs";
pub(super) const TEST_CONTROL_MANIFEST_ENV: &str = "CTX_PRO_TEST_CONTROL_MANIFEST";
#[cfg(ctx_pro_test_helper)]
const TEST_CONTROL_ENTITLEMENT_ISSUER: &str = "https://pro-test.ctx.invalid";
#[cfg(ctx_pro_test_helper)]
const TEST_CONTROL_ENTITLEMENT_KEY_ID: &str = "ctx-pro-test-control-v1";

#[derive(Debug, Clone, Copy)]
struct CommercialChannelRecord {
    api_origin: &'static str,
    workos_client_id: &'static str,
    entitlement_issuer: &'static str,
    entitlement_key_ids: &'static [&'static str],
    vault_namespace: CredentialVaultNamespace,
}

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

    #[cfg(ctx_pro_test_helper)]
    pub(super) fn validate_test_control_identity(issuer: &str, key_id: &str) -> Result<()> {
        EntitlementTrust {
            issuer: TEST_CONTROL_ENTITLEMENT_ISSUER,
            key_ids: &[TEST_CONTROL_ENTITLEMENT_KEY_ID],
        }
        .validate_identity(issuer, key_id)
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

    fn for_channel(channel: ReleaseChannel) -> Result<Self> {
        let record = channel_record(channel);
        if record.entitlement_key_ids.is_empty() {
            bail!("commercial_unavailable: the selected ctx Pro channel has no entitlement trust");
        }
        let access = match channel {
            ReleaseChannel::Stable => None,
            ReleaseChannel::Staging => staging_access_credentials()?,
        };
        Ok(Self {
            workos: WorkOsConfig {
                api_origin: parse_origin(WORKOS_API_ORIGIN, "WorkOS")?,
                client_id: record.workos_client_id.to_owned(),
            },
            api: CommercialApiConfig::new(
                parse_origin(record.api_origin, "commercial API")?,
                access,
            )?,
            release_trust: release_trust(channel)?,
            entitlement_trust: EntitlementTrust {
                issuer: record.entitlement_issuer,
                key_ids: record.entitlement_key_ids,
            },
            vault_namespace: record.vault_namespace,
        })
    }
}

fn channel_record(channel: ReleaseChannel) -> CommercialChannelRecord {
    match channel {
        ReleaseChannel::Stable => CommercialChannelRecord {
            api_origin: PRODUCTION_COMMERCIAL_API_ORIGIN,
            workos_client_id: commercial_production_record::WORKOS_CLIENT_ID,
            entitlement_issuer: PRODUCTION_ENTITLEMENT_ISSUER,
            entitlement_key_ids: commercial_production_record::ENTITLEMENT_KEY_IDS,
            vault_namespace: CredentialVaultNamespace::Production,
        },
        ReleaseChannel::Staging => CommercialChannelRecord {
            api_origin: STAGING_COMMERCIAL_API_ORIGIN,
            workos_client_id: STAGING_WORKOS_CLIENT_ID,
            entitlement_issuer: STAGING_ENTITLEMENT_ISSUER,
            entitlement_key_ids: STAGING_ENTITLEMENT_KEY_IDS,
            vault_namespace: CredentialVaultNamespace::Staging,
        },
    }
}

pub(super) fn selected_channel() -> Result<ReleaseChannel> {
    selected_channel_from_value(std::env::var_os(CHANNEL_ENV))
}

pub(super) fn reject_test_control_outside_test_host() -> Result<()> {
    #[cfg(not(ctx_pro_test_helper))]
    if std::env::var_os(TEST_CONTROL_MANIFEST_ENV).is_some() {
        bail!("invalid_request: {TEST_CONTROL_MANIFEST_ENV} is accepted only by ctx_pro_test_host");
    }
    Ok(())
}

#[cfg(ctx_pro_test_helper)]
pub(super) fn test_control_release_trust() -> Result<ReleaseTrust> {
    release_trust(ReleaseChannel::Stable)
}

fn selected_channel_from_value(value: Option<OsString>) -> Result<ReleaseChannel> {
    match value {
        Some(value) if value == "staging" => Ok(ReleaseChannel::Staging),
        Some(value) if value == "stable" => Ok(ReleaseChannel::Stable),
        Some(value) if value.to_str().is_some() => {
            bail!("invalid_request: {CHANNEL_ENV} must be stable or staging")
        }
        Some(_) => bail!("invalid_request: {CHANNEL_ENV} must be valid UTF-8"),
        None => Ok(ReleaseChannel::Stable),
    }
}

fn staging_access_credentials() -> Result<Option<CloudflareAccessCredentials>> {
    staging_access_credentials_from_values(
        std::env::var(STAGING_ACCESS_CLIENT_ID_ENV),
        std::env::var(STAGING_ACCESS_CLIENT_SECRET_ENV),
    )
}

fn staging_access_credentials_from_values(
    client_id: std::result::Result<String, std::env::VarError>,
    client_secret: std::result::Result<String, std::env::VarError>,
) -> Result<Option<CloudflareAccessCredentials>> {
    match (client_id, client_secret) {
        (Err(std::env::VarError::NotPresent), Err(std::env::VarError::NotPresent)) => Ok(None),
        (Ok(client_id), Ok(client_secret)) => {
            CloudflareAccessCredentials::new(client_id, client_secret).map(Some)
        }
        (Err(std::env::VarError::NotUnicode(_)), _)
        | (_, Err(std::env::VarError::NotUnicode(_))) => bail!(
            "invalid_request: staging Cloudflare Access credentials must be valid UTF-8"
        ),
        _ => bail!(
            "invalid_request: {STAGING_ACCESS_CLIENT_ID_ENV} and {STAGING_ACCESS_CLIENT_SECRET_ENV} must be set together"
        ),
    }
}

fn parse_origin(value: &str, label: &str) -> Result<Url> {
    Url::parse(value).with_context(|| format!("invalid_request: {label} origin is invalid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_channel_records_have_separate_https_origins_and_issuers() {
        let stable = channel_record(ReleaseChannel::Stable);
        let staging = channel_record(ReleaseChannel::Staging);
        for value in [
            WORKOS_API_ORIGIN,
            stable.api_origin,
            staging.api_origin,
            stable.entitlement_issuer,
            staging.entitlement_issuer,
        ] {
            let origin = Url::parse(value).unwrap();
            assert_eq!(origin.scheme(), "https");
            assert!(origin.host_str().is_some());
        }
        assert_eq!(stable.api_origin, "https://pro.ctx.rs/");
        assert_eq!(stable.entitlement_issuer, "https://pro.ctx.rs");
        assert_eq!(staging.api_origin, "https://pro-staging.ctx.rs/");
        assert_eq!(staging.entitlement_issuer, "https://pro-staging.ctx.rs");
        assert_ne!(stable.api_origin, staging.api_origin);
        assert_ne!(stable.entitlement_issuer, staging.entitlement_issuer);
        assert!(STAGING_WORKOS_CLIENT_ID.starts_with("client_"));
        assert_ne!(stable.workos_client_id, staging.workos_client_id);
    }

    #[test]
    fn unset_channel_defaults_stable_and_explicit_staging_is_supported() {
        assert_eq!(
            selected_channel_from_value(None).unwrap(),
            ReleaseChannel::Stable
        );
        assert_eq!(
            selected_channel_from_value(Some(OsString::from("stable"))).unwrap(),
            ReleaseChannel::Stable
        );
        assert_eq!(
            selected_channel_from_value(Some(OsString::from("staging"))).unwrap(),
            ReleaseChannel::Staging
        );
        assert!(selected_channel_from_value(Some(OsString::from("production"))).is_err());
    }

    #[test]
    fn stable_registry_binds_production_api_workos_release_and_entitlement_identities() {
        let config = CommercialConfig::for_channel(ReleaseChannel::Stable).unwrap();
        assert_eq!(
            config.api.origin().as_str(),
            PRODUCTION_COMMERCIAL_API_ORIGIN
        );
        assert_eq!(config.workos.client_id, "client_01KQE1P04WPEN66E12QCBWHQ8V");
        assert_eq!(config.release_trust.channel, ReleaseChannel::Stable);
        config
            .entitlement_trust
            .validate_identity(PRODUCTION_ENTITLEMENT_ISSUER, "production-2026-07-v1")
            .unwrap();
        assert!(config
            .entitlement_trust
            .validate_identity(STAGING_ENTITLEMENT_ISSUER, "staging-2026-07-v3")
            .is_err());
    }

    #[test]
    fn staging_registry_binds_api_release_and_entitlement_identities() {
        let config = CommercialConfig::for_channel(ReleaseChannel::Staging).unwrap();
        assert_eq!(config.api.origin().as_str(), STAGING_COMMERCIAL_API_ORIGIN);
        assert_eq!(config.release_trust.channel, ReleaseChannel::Staging);
        config
            .entitlement_trust
            .validate_identity(STAGING_ENTITLEMENT_ISSUER, "staging-2026-07-v3")
            .unwrap();
        assert!(config
            .entitlement_trust
            .validate_identity(PRODUCTION_ENTITLEMENT_ISSUER, "production-2026-07-v1")
            .is_err());
    }

    #[test]
    fn entitlement_identity_is_bound_in_both_channel_directions() {
        let production_fixture = EntitlementTrust {
            issuer: PRODUCTION_ENTITLEMENT_ISSUER,
            key_ids: &["production-fixture-v1"],
        };
        let staging = EntitlementTrust {
            issuer: STAGING_ENTITLEMENT_ISSUER,
            key_ids: STAGING_ENTITLEMENT_KEY_IDS,
        };
        assert!(production_fixture
            .validate_identity(STAGING_ENTITLEMENT_ISSUER, STAGING_ENTITLEMENT_KEY_IDS[0])
            .is_err());
        assert!(staging
            .validate_identity(PRODUCTION_ENTITLEMENT_ISSUER, "production-fixture-v1")
            .is_err());
    }

    #[test]
    fn staging_access_credentials_are_an_optional_complete_pair() {
        assert!(staging_access_credentials_from_values(
            Err(std::env::VarError::NotPresent),
            Err(std::env::VarError::NotPresent)
        )
        .unwrap()
        .is_none());

        let pair = staging_access_credentials_from_values(
            Ok("access-client-id".to_owned()),
            Ok("access-client-secret".to_owned()),
        )
        .unwrap()
        .unwrap();
        let debug = format!("{pair:?}");
        assert!(!debug.contains("access-client-id"));
        assert!(!debug.contains("access-client-secret"));

        for partial in [
            staging_access_credentials_from_values(
                Ok("access-client-id".to_owned()),
                Err(std::env::VarError::NotPresent),
            ),
            staging_access_credentials_from_values(
                Err(std::env::VarError::NotPresent),
                Ok("access-client-secret".to_owned()),
            ),
        ] {
            assert!(partial
                .unwrap_err()
                .to_string()
                .contains("must be set together"));
        }
    }
}
