use std::collections::BTreeMap;

#[cfg(ctx_release_qualification)]
use std::env;

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use url::Url;

use crate::config::AppConfig;

use super::version::parse_semver;

mod semantic;
pub(in crate::upgrade) use semantic::{
    SelectedSemanticAsset, SelectedSemanticProvisioning, SemanticAccelerator,
    SemanticAssetMetadata, SemanticFileMetadata,
};

const RELEASE_METADATA_BASE_URL: &str = "https://cli.ctx.rs/functions/v1";
const RELEASE_METADATA_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
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
#[cfg(test)]
const RELEASE_METADATA_PUBLIC_KEY_DER_SHA256: &str =
    "f989f75ad5eb476db0606125746fa524edb0a02ea5a6dde3bc83a5ea4fa93a4c";
const ONNXRUNTIME_METADATA_PREFIX: &str = "CTX_RELEASE_ONNXRUNTIME_";
const SUPPORTED_PLATFORM_KEYS: [&str; 6] = [
    "linux_x64",
    "linux_aarch64",
    "macos_arm64",
    "macos_x64",
    "windows_x64",
    "freebsd_x64",
];

#[derive(Debug, Clone)]
pub(super) struct OnnxRuntimeMetadata {
    pub(super) version: String,
    pub(super) artifact: String,
    pub(super) sha256: String,
}

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
    pub(super) onnxruntime: Option<OnnxRuntimeMetadata>,
    pub(super) semantic: Option<semantic::SemanticReleaseMetadata>,
}

pub(super) fn metadata_url(_config: &AppConfig, channel: &str) -> String {
    #[cfg(ctx_release_qualification)]
    if let Some(url) = qualification_env("CTX_RELEASE_METADATA_URL") {
        return url;
    }
    format!("{RELEASE_METADATA_BASE_URL}/releases/{channel}/ctx-release-metadata.env")
}

pub(super) fn metadata_signature_url(metadata_url: &str) -> String {
    #[cfg(ctx_release_qualification)]
    if let Some(url) = qualification_env("CTX_RELEASE_METADATA_SIGNATURE_URL") {
        return url;
    }
    format!("{metadata_url}.sig")
}

pub(super) fn parse_release_metadata(
    bytes: &[u8],
    platform: &str,
    expected_channel: &str,
    semantic_enabled: bool,
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
    parse_semver(&version).with_context(|| {
        format!("metadata CTX_RELEASE_VERSION {version:?} must be valid SemVer")
    })?;
    let base_url = value("CTX_RELEASE_BASE_URL")
        .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_BASE_URL"))?;
    let platform_key = platform.replace('-', "_");
    let artifact = value(&format!("CTX_RELEASE_ARTIFACT_{platform_key}"))
        .ok_or_else(|| anyhow!("metadata missing artifact for {platform}"))?;
    let sha256 = value(&format!("CTX_RELEASE_SHA256_{platform_key}"))
        .ok_or_else(|| anyhow!("metadata missing checksum for {platform}"))?;
    validate_sha256(&sha256)?;
    let onnxruntime = if metadata
        .keys()
        .any(|key| key.starts_with(ONNXRUNTIME_METADATA_PREFIX))
    {
        let version = value("CTX_RELEASE_ONNXRUNTIME_VERSION")
            .ok_or_else(|| anyhow!("metadata missing CTX_RELEASE_ONNXRUNTIME_VERSION"))?;
        validate_onnxruntime_version(&version)?;
        for key in SUPPORTED_PLATFORM_KEYS {
            let artifact_key = format!("CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{key}");
            let checksum_key = format!("CTX_RELEASE_ONNXRUNTIME_SHA256_{key}");
            let artifact =
                value(&artifact_key).ok_or_else(|| anyhow!("metadata missing {artifact_key}"))?;
            let checksum =
                value(&checksum_key).ok_or_else(|| anyhow!("metadata missing {checksum_key}"))?;
            validate_artifact_name(&artifact)?;
            validate_sha256(&checksum)?;
        }
        let artifact = value(&format!("CTX_RELEASE_ONNXRUNTIME_ARTIFACT_{platform_key}"))
            .ok_or_else(|| anyhow!("metadata missing ONNX Runtime artifact for {platform}"))?;
        let sha256 = value(&format!("CTX_RELEASE_ONNXRUNTIME_SHA256_{platform_key}"))
            .ok_or_else(|| anyhow!("metadata missing ONNX Runtime checksum for {platform}"))?;
        Some(OnnxRuntimeMetadata {
            version,
            artifact,
            sha256,
        })
    } else {
        None
    };
    // Semantic metadata has no effect while the feature remains opt-in. When
    // enabled, the signed canonical catalog is validated before any contained
    // URL suffix, archive hash, or file hash can become acquisition authority.
    let semantic = if semantic_enabled {
        semantic::parse_semantic_metadata(&metadata)?
    } else {
        None
    };
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
        onnxruntime,
        semantic,
    })
}

fn parse_metadata_map(text: &str) -> Result<BTreeMap<String, String>> {
    let mut metadata = BTreeMap::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim_start().starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(anyhow!("metadata contains malformed line: {line:?}"));
        };
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(anyhow!("metadata contains invalid key: {key:?}"));
        }
        if metadata.insert(key.to_owned(), value.to_owned()).is_some() {
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
    #[cfg(ctx_release_qualification)]
    let pem = qualification_env("CTX_RELEASE_METADATA_PUBLIC_KEY_PEM")
        .unwrap_or_else(|| RELEASE_METADATA_PUBLIC_KEY_PEM.to_owned());
    #[cfg(not(ctx_release_qualification))]
    let pem = RELEASE_METADATA_PUBLIC_KEY_PEM;
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
    #[cfg(ctx_release_qualification)]
    if qualification_artifact_base(base_url) {
        return validate_artifact_name(artifact);
    }
    if !is_production_artifact_base(base_url) {
        return Err(anyhow!("metadata base URL must be HTTPS"));
    }
    validate_artifact_name(artifact)
}

fn is_production_artifact_base(base_url: &str) -> bool {
    let Ok(url) = Url::parse(base_url) else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str() == Some("cli.ctx.rs")
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && base_url.starts_with(RELEASE_BASE_PREFIX)
        && url.as_str().starts_with(RELEASE_BASE_PREFIX)
}

#[cfg(ctx_release_qualification)]
fn qualification_artifact_base(base_url: &str) -> bool {
    let Ok(url) = Url::parse(base_url) else {
        return false;
    };
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    if url.scheme() == "file" {
        return true;
    }
    matches!(url.scheme(), "http" | "https")
        && url.host().is_some_and(|host| match host {
            url::Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(address) => address.is_loopback(),
            url::Host::Ipv6(address) => address.is_loopback(),
        })
}

#[cfg(ctx_release_qualification)]
fn qualification_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

pub(super) fn validate_artifact_name(artifact: &str) -> Result<()> {
    if artifact.is_empty()
        || artifact.contains('/')
        || artifact.contains('\\')
        || artifact.contains("..")
        || artifact.contains('\n')
        || artifact.contains('\r')
    {
        return Err(anyhow!("unsafe artifact name: {artifact}"));
    }
    Ok(())
}

fn validate_onnxruntime_version(version: &str) -> Result<()> {
    if version.len() > 32
        || version.trim() != version
        || version.split('.').count() != 3
        || version.split('.').any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| byte.is_ascii_digit())
                || (part.len() > 1 && part.starts_with('0'))
                || part.parse::<u32>().is_err()
        })
    {
        return Err(anyhow!(
            "ONNX Runtime version must be a safe MAJOR.MINOR.PATCH identifier"
        ));
    }
    Ok(())
}

pub(super) fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!("checksum is not a SHA-256 hex digest"));
    }
    if value == "0000000000000000000000000000000000000000000000000000000000000000" {
        return Err(anyhow!("checksum is a placeholder"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{
        env,
        ffi::OsString,
        sync::{Mutex, MutexGuard},
    };

    static RELEASE_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct ReleaseEnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl ReleaseEnvGuard {
        fn hostile() -> Self {
            let lock = RELEASE_ENV_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let values = [
                (
                    "CTX_RELEASE_METADATA_URL",
                    "file:///attacker/ctx-release-metadata.env",
                ),
                (
                    "CTX_RELEASE_METADATA_SIGNATURE_URL",
                    "custom://attacker/signature",
                ),
                ("CTX_RELEASE_METADATA_PUBLIC_KEY_PEM", "not-a-public-key"),
                ("CTX_RELEASE_BASE_URL", "https://attacker.invalid/artifacts"),
                ("CTX_RELEASE_SKIP_SIGNATURE_VERIFY_FOR_TESTS", "1"),
                ("CTX_ALLOW_CUSTOM_RELEASE_BASE_URL", "1"),
                (
                    "CTX_UPGRADE_FUNCTIONS_BASE",
                    "https://attacker.invalid/functions",
                ),
                ("CTX_FUNCTIONS_BASE", "file:///legacy-attacker"),
            ];
            let saved = values
                .iter()
                .map(|(key, value)| {
                    let previous = env::var_os(key);
                    env::set_var(key, value);
                    (*key, previous)
                })
                .collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for ReleaseEnvGuard {
        fn drop(&mut self) {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }

    fn minimal_release_metadata(version: &str) -> Vec<u8> {
        format!(
            "\
CTX_RELEASE_SCHEMA_VERSION=1
CTX_RELEASE_CHANNEL=stable
CTX_RELEASE_VERSION={version}
CTX_RELEASE_BASE_URL=https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/test
CTX_RELEASE_ARTIFACT_linux_x64=ctx-linux-x64
CTX_RELEASE_SHA256_linux_x64={}
",
            "1".repeat(64)
        )
        .into_bytes()
    }

    #[test]
    fn release_metadata_requires_an_exact_semver_version() {
        for version in [
            "v1.2.3",
            "1.2",
            "01.2.3",
            "1.2.3-01",
            "1.2.3+",
            "release-1.2.3",
            "1.2.3 ",
        ] {
            let error = parse_release_metadata(
                &minimal_release_metadata(version),
                "linux-x64",
                "stable",
                false,
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("CTX_RELEASE_VERSION")
                    && error.to_string().contains("valid SemVer"),
                "{version:?}: {error:#}"
            );
        }
    }

    #[test]
    fn release_metadata_accepts_prerelease_and_build_semver() {
        let metadata = parse_release_metadata(
            &minimal_release_metadata("1.2.3-rc.1+linux.7"),
            "linux-x64",
            "stable",
            false,
        )
        .unwrap();
        assert_eq!(metadata.version, "1.2.3-rc.1+linux.7");
    }

    #[test]
    fn disabled_semantic_search_does_not_parse_catalog_fields() {
        let mut bytes = minimal_release_metadata("1.2.3");
        bytes.extend_from_slice(b"CTX_RELEASE_SEMANTIC_ASSETS=not-trusted-or-parsed\n");

        let metadata = parse_release_metadata(&bytes, "linux-x64", "stable", false).unwrap();

        assert!(metadata.semantic.is_none());
    }

    #[cfg(not(ctx_release_qualification))]
    #[test]
    fn production_authority_ignores_mixed_ambient_substitution() {
        let _guard = ReleaseEnvGuard::hostile();
        let metadata = metadata_url(&AppConfig::default(), "stable");

        assert_eq!(
            metadata,
            "https://cli.ctx.rs/functions/v1/releases/stable/ctx-release-metadata.env"
        );
        assert_eq!(
            metadata_signature_url(&metadata),
            "https://cli.ctx.rs/functions/v1/releases/stable/ctx-release-metadata.env.sig"
        );
        let key = public_key_der().expect("decode embedded production release key");
        assert_eq!(
            format!("{:x}", Sha256::digest(key)),
            RELEASE_METADATA_PUBLIC_KEY_DER_SHA256
        );
    }

    #[cfg(not(ctx_release_qualification))]
    #[test]
    fn production_artifact_authority_rejects_local_custom_and_mixed_urls() {
        let _guard = ReleaseEnvGuard::hostile();
        for base_url in [
            "file:///tmp/ctx-release",
            "http://127.0.0.1:8080/releases",
            "https://attacker.invalid/releases",
            "custom://attacker/releases",
            "https://cli.ctx.rs.attacker.invalid/storage/v1/object/public/releases/artifacts/",
            "https://attacker@cli.ctx.rs/storage/v1/object/public/releases/artifacts/",
            "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/../redirected",
            "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/?redirect=attacker",
        ] {
            assert!(
                validate_artifact_url(base_url, "ctx-linux-x64").is_err(),
                "production accepted ambient/custom artifact base {base_url}"
            );
        }
        validate_artifact_url(
            "https://cli.ctx.rs/storage/v1/object/public/releases/artifacts/1.0.0",
            "ctx-linux-x64",
        )
        .expect("accept immutable production artifact authority");
    }
}
