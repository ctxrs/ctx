use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ring::signature::{UnparsedPublicKey, RSA_PKCS1_2048_8192_SHA256};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub(crate) const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub(crate) const MAX_SIGNATURE_BYTES: u64 = 16 * 1024;
pub(crate) const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
pub(super) const MAX_INSTALL_MARKER_BYTES: u64 = 128 * 1024;
pub(super) const PRO_RELEASE_STABLE_KEY_ID: &str = "ctx-pro-release-stable-2026-07-27";
pub(super) const PRO_RELEASE_STABLE_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBigKCAYEAq2vmUvoGcm0bAJhCdjzqzLF9SAALDA33KQOHWI3JeKFjxHTLs3hP
88b3WfUfWgd/Bj6pVWpAI/S6MnrT5IQ8VMxPMe9VMM97F4TMN+ZMwo9y4sxefGwJ
+GI/7SJP3hnRLMV2xme9RRMERuaAEL1ComPdqKwcAMzvZSpAHnDrWnjLrhBFyahl
2n8JoxvNr4sNHGJBdK0voDBgHmgoJvL23zrRDoo+yA7M7F0gQJc0hwxXUeku7rxb
hlPU7WPZGwDbaNEzoVJBinlXoLuFT3cR7ImwnfPOARSa7q7KZaIoaeljqM3d6lTa
abVHhCI+EJy1XX4ydQxFbqccMzhsz5g6Wim8q7pKliKT97uwV3r80f3DpjBUiG3e
+6QpMkxZqVaIgK85Za1stYKPfy9wOZyKkXthHeRbhKjozuogyK8cp03TjY6K6pw2
VNd6soFZl6R0F8V4tNR5CXwlMjgFogl6t2sKIGHUhHC7y1U01lYlRJNqmvCClD2N
uvdK7q5ndg1XAgMBAAE=
-----END RSA PUBLIC KEY-----"#;
pub(super) const PRO_RELEASE_STAGING_KEY_ID: &str = "ctx-pro-release-staging-2026-07-21";
pub(super) const PRO_RELEASE_STAGING_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBigKCAYEAzhEEzfwq/hJtK8l0G+K722eEUy9vNyTFvRLK2rIOkTBDwZQs1tgp
7/dnshIKbk6MYkp2/QqfDJGECdIeIzXZ3M+5RtGXCHxmSJwzGgQ/1AUsFixEaKYP
YlqQeohft939GJPtdyK+CPmqp6+5B2xeI8i0hen66x/76k73a258ZndFJnUKHPTo
eCvWtv4i5TRNgtdQbuYW9rpwlQOihKaDzl5nSsrOet+/dp9ISjP/DYog7KXdQdOB
oZwuvIgViqbbgXV4fU5DOczf6hG2aykxM5cDWWShLG3k82bvJhtrmEfD+zC0GINx
a1j/hA17NW96si55r2bK1Y0hT8eoASjLX+XwM9QfW7fkYH6Ox0iQ6YuEcSyI8oov
hJArDEAIbcp0NPyqcm5+p7Kk/RQWwaTJJ0Ak2W51u8BLFMTSSmPpDIUTG9svVETt
Crct4wyMI76peaDsehiJoHcX+c6ldIKXgLWkADThS//JWYsxXB4qmlqczRxp90FT
h6pox43/CszJAgMBAAE=
-----END RSA PUBLIC KEY-----"#;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ReleaseChannel {
    Stable,
    Staging,
}

impl ReleaseChannel {
    pub(crate) const fn wire_name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Staging => "staging",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ReleaseTrust {
    pub(crate) channel: ReleaseChannel,
    pub(crate) key_id: &'static str,
    pub(crate) public_key_pem: &'static str,
}

pub(crate) fn release_trust(channel: ReleaseChannel) -> Result<ReleaseTrust> {
    match channel {
        ReleaseChannel::Staging => Ok(ReleaseTrust {
            channel,
            key_id: PRO_RELEASE_STAGING_KEY_ID,
            public_key_pem: PRO_RELEASE_STAGING_PUBLIC_KEY_PEM,
        }),
        ReleaseChannel::Stable => Ok(ReleaseTrust {
            channel,
            key_id: PRO_RELEASE_STABLE_KEY_ID,
            public_key_pem: PRO_RELEASE_STABLE_PUBLIC_KEY_PEM,
        }),
    }
}
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProManifest {
    pub(crate) schema_version: u32,
    pub(crate) product: String,
    pub(crate) channel: String,
    pub(crate) version: String,
    pub(crate) source_commit: String,
    pub(crate) public_source_commit: String,
    pub(crate) private_source_commit: String,
    pub(crate) build_identity: String,
    pub(crate) protocol_min: u16,
    pub(crate) protocol_max: u16,
    pub(crate) protocol_fingerprint: String,
    pub(crate) target: String,
    pub(crate) architecture: String,
    pub(crate) artifact_object: String,
    pub(crate) artifact_size: u64,
    pub(crate) artifact_sha256: String,
    pub(crate) public_artifact_sha256: String,
    pub(crate) public_package_sha256: String,
    pub(crate) private_package_sha256: String,
    pub(crate) runtime_evidence_sha256: String,
    pub(crate) runtime_run_id: String,
    pub(crate) release_key_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProInstallMarker {
    schema_version: u32,
    manifest_base64: String,
    signature_base64: String,
}

impl ProInstallMarker {
    pub(super) fn new(manifest_bytes: &[u8], signature: &[u8]) -> Result<Self> {
        let signature = std::str::from_utf8(signature)
            .context("invalid_response: manifest signature is not UTF-8 base64")?;
        Ok(Self {
            schema_version: 1,
            manifest_base64: BASE64.encode(manifest_bytes),
            signature_base64: signature.trim().to_owned(),
        })
    }

    pub(super) fn signed_manifest(&self, public_key_pem: &str) -> Result<ProManifest> {
        if self.schema_version != 1
            || self.manifest_base64.len() as u64 > MAX_INSTALL_MARKER_BYTES
            || self.signature_base64.len() as u64 > MAX_SIGNATURE_BYTES
        {
            bail!("invalid_response: installed Pro marker is outside allowed bounds");
        }
        let manifest_bytes = BASE64
            .decode(&self.manifest_base64)
            .context("invalid_response: installed Pro marker manifest is not base64")?;
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
            bail!("invalid_response: installed Pro marker manifest exceeds maximum size");
        }
        verified_manifest(
            &manifest_bytes,
            self.signature_base64.as_bytes(),
            public_key_pem,
        )
    }
}

pub(crate) fn verified_manifest(
    manifest_bytes: &[u8],
    signature: &[u8],
    public_key_pem: &str,
) -> Result<ProManifest> {
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("invalid_response: signed Pro manifest exceeds maximum size");
    }
    verify_signature_with_key(manifest_bytes, signature, public_key_pem)?;
    let manifest: ProManifest = serde_json::from_slice(manifest_bytes)
        .context("invalid_response: parse signed Pro manifest")?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub(crate) fn verified_manifest_for_trust(
    manifest_bytes: &[u8],
    signature: &[u8],
    trust: ReleaseTrust,
) -> Result<ProManifest> {
    if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
        bail!("invalid_response: signed Pro manifest exceeds maximum size");
    }
    #[derive(Deserialize)]
    struct KeySelector {
        schema_version: u32,
        channel: String,
        release_key_id: String,
    }
    let selector: KeySelector = serde_json::from_slice(manifest_bytes)
        .context("invalid_response: parse signed Pro manifest key selector")?;
    if selector.schema_version != 1
        || selector.channel != trust.channel.wire_name()
        || selector.release_key_id != trust.key_id
    {
        bail!("invalid_response: signed manifest does not match the selected release channel");
    }
    verify_signature_with_key(manifest_bytes, signature, trust.public_key_pem)?;
    let manifest: ProManifest = serde_json::from_slice(manifest_bytes)
        .context("invalid_response: parse signed Pro manifest")?;
    validate_manifest(&manifest)?;
    validate_manifest_release_trust(&manifest, trust)?;
    Ok(manifest)
}

pub(crate) fn validate_manifest_release_trust(
    manifest: &ProManifest,
    trust: ReleaseTrust,
) -> Result<()> {
    if manifest.schema_version != 1
        || manifest.channel != trust.channel.wire_name()
        || manifest.release_key_id != trust.key_id
    {
        bail!("invalid_response: signed manifest does not match the selected release channel");
    }
    Ok(())
}

pub(super) fn validate_manifest(manifest: &ProManifest) -> Result<()> {
    if manifest.schema_version != 1 || manifest.product != "ctx-pro" {
        bail!("invalid_response: signed manifest is not a supported ctx-pro manifest");
    }
    if manifest.protocol_min != ctx_pro_host_protocol::PROTOCOL_VERSION
        || manifest.protocol_max != ctx_pro_host_protocol::PROTOCOL_VERSION
    {
        bail!("protocol_mismatch: signed artifact is not compatible with this ctx host");
    }
    if manifest.protocol_fingerprint != ctx_pro_host_protocol::PROTOCOL_FINGERPRINT {
        bail!(
            "protocol_mismatch: signed artifact protocol fingerprint does not match this ctx host"
        );
    }
    if manifest.target != platform_target() {
        bail!(
            "invalid_response: signed artifact target {} does not match {}",
            manifest.target,
            platform_target()
        );
    }
    if manifest.artifact_size == 0 || manifest.artifact_size > MAX_ARTIFACT_BYTES {
        bail!("invalid_response: signed artifact size is outside allowed bounds");
    }
    if parse_release_version(&manifest.version).is_err()
        || !is_lower_hex(&manifest.source_commit, 40)
        || !is_lower_hex(&manifest.public_source_commit, 40)
        || !is_lower_hex(&manifest.private_source_commit, 40)
        || manifest.source_commit != manifest.private_source_commit
        || !is_lower_hex(&manifest.artifact_sha256, 64)
        || !is_lower_hex(&manifest.public_artifact_sha256, 64)
        || !is_lower_hex(&manifest.public_package_sha256, 64)
        || !is_lower_hex(&manifest.private_package_sha256, 64)
        || !is_lower_hex(&manifest.runtime_evidence_sha256, 64)
        || !is_canonical_uuid(&manifest.runtime_run_id)
    {
        bail!("invalid_response: signed manifest contains invalid identity fields");
    }
    if manifest.architecture != std::env::consts::ARCH
        || !is_lower_hex(&manifest.build_identity, 64)
        || manifest.release_key_id.is_empty()
        || manifest.release_key_id.len() > 128
        || !manifest
            .release_key_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("invalid_response: signed manifest contains invalid release identity");
    }
    let executable = if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    };
    let expected_object = format!(
        "pro/artifacts/{channel}/{}/{}/{executable}",
        manifest.version,
        manifest.target,
        channel = manifest.channel,
    );
    if manifest.artifact_object != expected_object {
        bail!("invalid_response: signed manifest contains invalid artifact object");
    }
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_canonical_uuid(value: &str) -> bool {
    match Uuid::parse_str(value) {
        Ok(parsed) => parsed.to_string() == value,
        Err(_) => false,
    }
}

pub(super) fn verify_signature_with_key(
    manifest: &[u8],
    signature: &[u8],
    public_key_pem: &str,
) -> Result<()> {
    let text = std::str::from_utf8(signature)
        .context("invalid_response: manifest signature is not UTF-8 base64")?;
    let signature = BASE64
        .decode(text.trim())
        .context("invalid_response: manifest signature is not base64")?;
    let body: String = public_key_pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    let der = BASE64
        .decode(body)
        .context("invalid_response: decode embedded Pro signing key")?;
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, der)
        .verify(manifest, &signature)
        .map_err(|_| anyhow!("invalid_response: Pro manifest signature verification failed"))
}

pub(crate) fn parse_release_version(version: &str) -> Result<(u64, u64, u64)> {
    if version.is_empty() || version.len() > 64 {
        bail!("invalid_response: signed manifest contains invalid identity fields");
    }
    let mut parts = version.split('.');
    let mut parse_part = || -> Result<u64> {
        let part = parts
            .next()
            .ok_or_else(|| anyhow!("invalid_response: signed manifest version is invalid"))?;
        if part.is_empty()
            || !part.bytes().all(|byte| byte.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            bail!("invalid_response: signed manifest version is invalid");
        }
        part.parse::<u64>()
            .context("invalid_response: signed manifest version is invalid")
    };
    let parsed = (parse_part()?, parse_part()?, parse_part()?);
    if parts.next().is_some() {
        bail!("invalid_response: signed manifest version is invalid");
    }
    Ok(parsed)
}

pub(crate) fn platform_target() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu".to_owned(),
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu".to_owned(),
        ("macos", "x86_64") => "x86_64-apple-darwin".to_owned(),
        ("macos", "aarch64") => "aarch64-apple-darwin".to_owned(),
        ("windows", "x86_64") => "x86_64-pc-windows-msvc".to_owned(),
        ("windows", "aarch64") => "aarch64-pc-windows-msvc".to_owned(),
        ("freebsd", "x86_64") => "x86_64-unknown-freebsd".to_owned(),
        ("freebsd", "aarch64") => "aarch64-unknown-freebsd".to_owned(),
        (os, arch) => format!("{arch}-unknown-{os}"),
    }
}

#[cfg(test)]
mod release_tests {
    use super::*;

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReleaseVersionContract {
        schema_version: u32,
        kind: String,
        grammar: String,
        component_max: String,
        valid: Vec<String>,
        invalid: Vec<String>,
        ordering: Vec<ReleaseVersionOrdering>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ReleaseVersionOrdering {
        left: String,
        right: String,
        result: i8,
    }

    fn release_version_contract() -> ReleaseVersionContract {
        serde_json::from_str(include_str!(
            "../../../../contracts/release-version-v1.json"
        ))
        .expect("release-version contract must be valid JSON")
    }

    #[test]
    fn release_registry_has_distinct_channel_bound_public_keys() {
        let stable = release_trust(ReleaseChannel::Stable).unwrap();
        let staging = release_trust(ReleaseChannel::Staging).unwrap();
        assert_eq!(stable.channel, ReleaseChannel::Stable);
        assert_eq!(stable.key_id, "ctx-pro-release-stable-2026-07-27");
        assert_eq!(staging.channel, ReleaseChannel::Staging);
        assert_ne!(stable.key_id, staging.key_id);
        assert_ne!(stable.public_key_pem, staging.public_key_pem);
    }

    #[test]
    fn release_selector_rejects_cross_channel_material_before_signature_validation() {
        let stable_selector = serde_json::json!({
            "schema_version": 1,
            "channel": "stable",
            "release_key_id": PRO_RELEASE_STABLE_KEY_ID,
        });
        let staging_selector = serde_json::json!({
            "schema_version": 1,
            "channel": "staging",
            "release_key_id": PRO_RELEASE_STAGING_KEY_ID,
        });
        for (selector, trust) in [
            (
                serde_json::to_vec(&stable_selector).unwrap(),
                release_trust(ReleaseChannel::Staging).unwrap(),
            ),
            (
                serde_json::to_vec(&staging_selector).unwrap(),
                release_trust(ReleaseChannel::Stable).unwrap(),
            ),
        ] {
            let error = verified_manifest_for_trust(&selector, b"not-a-signature", trust)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                "invalid_response: signed manifest does not match the selected release channel"
            );
        }
    }

    #[test]
    fn release_version_parser_matches_the_public_adversarial_contract() {
        let contract = release_version_contract();
        assert_eq!(contract.schema_version, 1);
        assert_eq!(contract.kind, "ctx-release-version-contract");
        assert_eq!(
            contract.grammar,
            "MAJOR.MINOR.PATCH; each component is canonical ASCII decimal in 0..18446744073709551615"
        );
        assert_eq!(contract.component_max.parse::<u64>().unwrap(), u64::MAX);
        for version in contract.valid {
            assert!(
                parse_release_version(&version).is_ok(),
                "valid release version was rejected: {version:?}"
            );
        }
        for version in contract.invalid {
            assert!(
                parse_release_version(&version).is_err(),
                "invalid release version was accepted: {version:?}"
            );
        }
        for vector in contract.ordering {
            let left = parse_release_version(&vector.left).unwrap();
            let right = parse_release_version(&vector.right).unwrap();
            let actual = match left.cmp(&right) {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            };
            assert_eq!(
                actual, vector.result,
                "release-version ordering drift for {} and {}",
                vector.left, vector.right
            );
        }
    }
}
