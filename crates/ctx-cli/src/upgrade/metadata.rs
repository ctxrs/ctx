use std::{collections::BTreeMap, env};

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::config::AppConfig;

use super::{env_flag, sha256_hex};

const DEFAULT_METADATA_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBigKCAYEAyBPNIx3H/NwWlN9CPHY5kOEe9kQEshOJEMpv3Atq086H1FWqliTm
3BCWiO4s/89wNMn11Pla2JetCWNiWsbxm3BIxCd1o6cq8y9ur6Zk1RGOQBLQgqhF
m5BpcTTavhtlc3FdV2KSm2UU1IEJAiFXJyMlbgmf3tXfO8Cji/3mG11rWCXfnEzX
Jmig5/WWA21ZgsafPJGH9ow7FsLok5G1kvOeVDXcv0gzmxWH+2O40kCGWo7BK7P/
2DPD2GbXc81Mf6S7vWi7CeFiBeGH8EGZ6MgBM0UnAFEqtx/WvY47O+LHzFrGlJTp
ss3xlxsSQOTmXDJdOzmQVi04GkbOtBEl+dIyYsxZGusLBMGDqkZekO4Z5LvqA8zH
t4JAElZCs8SGTlV70MSlnyZb5/rkKx9kMvb7YjuYbY6vnN5Pp3P7gMhOKehP+62U
80cgyj1m6Sk5bByrs54ne2mM+cwNXXgKp5UntmkefDcfKP7MmISy93U/kg3fWojE
/a+X6TNV/k5fAgMBAAE=
-----END RSA PUBLIC KEY-----"#;
const RELEASE_BASE_PREFIX: &str = "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/";

#[derive(Debug, Clone)]
pub(super) struct ReleaseMetadata {
    pub(super) version: String,
    pub(super) base_url: String,
    pub(super) artifact: String,
    pub(super) sha256: String,
    pub(super) source_commit: Option<String>,
    pub(super) published_at: Option<String>,
    pub(super) self_upgrade_allowed: bool,
    pub(super) auto_upgrade_allowed: bool,
    pub(super) store_schema_version: Option<String>,
}

pub(super) fn metadata_url(config: &AppConfig, channel: &str) -> String {
    if let Ok(url) = env::var("CTX_RELEASE_METADATA_URL") {
        if !url.trim().is_empty() {
            return url;
        }
    }
    format!(
        "{}/releases/{channel}/ctx-release-metadata.env",
        config.upgrade.functions_base.trim_end_matches('/')
    )
}

pub(super) fn metadata_signature_url(metadata_url: &str) -> String {
    env::var("CTX_RELEASE_METADATA_SIGNATURE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("{metadata_url}.sig"))
}

pub(super) fn parse_release_metadata(
    bytes: &[u8],
    platform: &str,
    expected_channel: &str,
) -> Result<ReleaseMetadata> {
    let text = std::str::from_utf8(bytes).context("release metadata is not UTF-8")?;
    let metadata = parse_metadata_map(text)?;
    let value = |key: &str| metadata_value(&metadata, key);
    let schema = value("CTX_RELEASE_SCHEMA_VERSION")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_SCHEMA_VERSION"))?;
    if schema != "1" {
        return Err(anyhow!("unsupported release metadata schema: {schema}"));
    }
    let channel = value("CTX_RELEASE_CHANNEL").unwrap_or_else(|| expected_channel.to_owned());
    if channel != expected_channel {
        return Err(anyhow!(
            "metadata channel {channel} does not match requested channel {expected_channel}"
        ));
    }
    let version = value("CTX_RELEASE_VERSION")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_VERSION"))?;
    let base_url = value("CTX_RELEASE_BASE_URL")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_BASE_URL"))?;
    let platform_key = platform.replace('-', "_");
    let artifact = value(&format!("CTX_RELEASE_ARTIFACT_{platform_key}"))
        .ok_or_else(|| anyhow!("metadata missing artifact for {platform}"))?;
    let sha256 = value(&format!("CTX_RELEASE_SHA256_{platform_key}"))
        .ok_or_else(|| anyhow!("metadata missing checksum for {platform}"))?;
    validate_sha256(&sha256)?;
    Ok(ReleaseMetadata {
        version,
        base_url,
        artifact,
        sha256,
        source_commit: value("CTX_RELEASE_SOURCE_COMMIT"),
        published_at: value("CTX_RELEASE_PUBLISHED_AT"),
        self_upgrade_allowed: metadata_bool(&metadata, "CTX_RELEASE_SELF_UPGRADE_ALLOWED", false)?,
        auto_upgrade_allowed: metadata_bool(&metadata, "CTX_RELEASE_AUTO_UPGRADE_ALLOWED", false)?,
        store_schema_version: value("CTX_RELEASE_STORE_SCHEMA_VERSION"),
    })
}

fn parse_metadata_map(text: &str) -> Result<BTreeMap<String, String>> {
    let mut metadata = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if metadata
            .insert(key.to_owned(), value.trim_end_matches('\r').to_owned())
            .is_some()
        {
            return Err(anyhow!("metadata contains duplicate key {key}"));
        }
    }
    Ok(metadata)
}

fn metadata_value(metadata: &BTreeMap<String, String>, key: &str) -> Option<String> {
    metadata.get(key).cloned()
}

fn metadata_bool(metadata: &BTreeMap<String, String>, key: &str, default: bool) -> Result<bool> {
    let Some(value) = metadata_value(metadata, key) else {
        return Ok(default);
    };
    match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" => Ok(true),
        "0" | "false" | "no" => Ok(false),
        _ => Err(anyhow!("metadata {key} must be a boolean")),
    }
}

pub(super) fn verify_metadata_signature(metadata: &[u8], signature: &[u8]) -> Result<()> {
    if cfg!(debug_assertions) && env_flag("CTX_RELEASE_SKIP_SIGNATURE_VERIFY_FOR_TESTS") {
        return Ok(());
    }
    let der = public_key_der()?;
    let signature_text = std::str::from_utf8(signature)
        .context("metadata signature is not UTF-8 base64")?
        .trim();
    let signature_bytes = BASE64
        .decode(signature_text)
        .context("metadata signature is not base64")?;
    let key =
        ring::signature::UnparsedPublicKey::new(&ring::signature::RSA_PKCS1_2048_8192_SHA256, der);
    key.verify(metadata, &signature_bytes)
        .map_err(|_| anyhow!("metadata signature verification failed"))
}

fn public_key_der() -> Result<Vec<u8>> {
    let pem = env::var("CTX_RELEASE_METADATA_PUBLIC_KEY_PEM")
        .unwrap_or_else(|_| DEFAULT_METADATA_PUBLIC_KEY_PEM.to_owned());
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    BASE64
        .decode(body)
        .context("decode release metadata public key")
}

pub(super) fn validate_artifact_url(base_url: &str, artifact: &str) -> Result<()> {
    if !base_url.starts_with("https://") && !base_url.starts_with("file://") {
        return Err(anyhow!("metadata base URL must be HTTPS"));
    }
    if !base_url.starts_with(RELEASE_BASE_PREFIX)
        && !base_url.starts_with("file://")
        && !env_flag("CTX_ALLOW_CUSTOM_RELEASE_BASE_URL")
    {
        return Err(anyhow!(
            "metadata base URL must be under {RELEASE_BASE_PREFIX}"
        ));
    }
    if artifact.contains('/') || artifact.contains('\\') || artifact.contains("..") {
        return Err(anyhow!("unsafe artifact name: {artifact}"));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("checksum is not a SHA-256 hex digest"));
    }
    if value == "0000000000000000000000000000000000000000000000000000000000000000" {
        return Err(anyhow!("checksum is a placeholder"));
    }
    Ok(())
}

pub(super) fn verify_artifact_sha(bytes: &[u8], expected: &str) -> Result<()> {
    let actual = sha256_hex(bytes);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(anyhow!(
            "artifact checksum mismatch: expected {expected}, got {actual}"
        ));
    }
    Ok(())
}
