use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_history_core::platform_security::{
    restrict_private_directory, restrict_private_executable, restrict_private_file,
};
use ctx_pro_host_protocol::ProFilesystemLayout;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use super::{
    commercial_api::{commercial_http_error, COMMERCIAL_ACCEPT},
    request_identity::new_idempotency_key,
};

use super::lifecycle::lifecycle_manifest::{
    platform_target, verified_manifest_for_trust, ProManifest, ReleaseTrust, MAX_ARTIFACT_BYTES,
    MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES,
};
use super::lifecycle::ProInstallArgs;

const MAX_API_RESPONSE_BYTES: u64 = 96 * 1024;
const MAX_DOWNLOAD_AUTHORIZATION_BYTES: usize = 4096;
const MAX_DOWNLOAD_LIFETIME_SECONDS: u64 = 5 * 60;

pub(crate) struct CommercialArtifactAuth<'a> {
    pub(crate) api_base_url: &'a str,
    pub(crate) authorization: &'a str,
    pub(crate) release_trust: ReleaseTrust,
}

#[derive(Debug)]
pub(crate) struct VerifiedArtifactBundle {
    stage_dir: PathBuf,
    pub(crate) artifact: PathBuf,
    pub(crate) manifest: PathBuf,
    pub(crate) signature: PathBuf,
}

impl Drop for VerifiedArtifactBundle {
    fn drop(&mut self) {
        for path in [&self.artifact, &self.manifest, &self.signature] {
            let _ = fs::remove_file(path);
        }
        let _ = fs::remove_dir(&self.stage_dir);
    }
}

impl VerifiedArtifactBundle {
    pub(super) fn install_args(&self) -> ProInstallArgs {
        ProInstallArgs::new(
            self.artifact.clone(),
            self.manifest.clone(),
            self.signature.clone(),
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEnvelope {
    schema_version: u32,
    manifest_base64: String,
    signature_base64: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiSuccess<T> {
    api_version: String,
    request_id: String,
    data: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DownloadAuthorization {
    schema_version: u32,
    channel: String,
    version: String,
    target: String,
    build_identity: String,
    protocol_fingerprint: String,
    artifact_object: String,
    url: String,
    authorization: String,
    expires_at_unix: u64,
    manifest_sha256: String,
    artifact_size: u64,
    artifact_sha256: String,
}

pub(crate) fn fetch_latest(
    data_root: &Path,
    commercial_auth: CommercialArtifactAuth<'_>,
    current_version: Option<&str>,
) -> Result<VerifiedArtifactBundle> {
    validate_auth(&commercial_auth)?;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(15))
        .build();
    let manifest_url = api_url(commercial_auth.api_base_url, "/v1/artifacts/manifest")?;
    let envelope_bytes = post_json_bounded(
        &agent,
        &manifest_url,
        commercial_auth.authorization,
        &json!({
            "schema_version": 1,
            "channel": commercial_auth.release_trust.channel.wire_name(),
            "target": platform_target(),
            "current_version": current_version,
            "protocol_version": ctx_pro_host_protocol::PROTOCOL_VERSION,
            "protocol_fingerprint": ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        }),
    )?;
    let envelope: ApiSuccess<ManifestEnvelope> = serde_json::from_slice(&envelope_bytes)
        .context("invalid_response: parse artifact manifest response")?;
    validate_success_envelope(&envelope)?;
    let (manifest, manifest_bytes, signature_bytes) =
        verify_envelope(envelope.data, commercial_auth.release_trust)?;
    let manifest_digest = format!("{:x}", Sha256::digest(&manifest_bytes));

    let authorization_url = api_url(commercial_auth.api_base_url, "/v1/artifacts/download")?;
    let authorization_bytes = post_json_bounded(
        &agent,
        &authorization_url,
        commercial_auth.authorization,
        &json!({
            "schema_version": 1,
            "channel": commercial_auth.release_trust.channel.wire_name(),
            "version": manifest.version,
            "target": manifest.target,
            "manifest_sha256": manifest_digest,
            "protocol_version": ctx_pro_host_protocol::PROTOCOL_VERSION,
            "protocol_fingerprint": ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        }),
    )?;
    let authorization: ApiSuccess<DownloadAuthorization> =
        serde_json::from_slice(&authorization_bytes)
            .context("invalid_response: parse artifact download authorization")?;
    validate_success_envelope(&authorization)?;
    validate_download_authorization(
        &authorization.data,
        commercial_auth.api_base_url,
        &manifest,
        &manifest_digest,
    )?;

    let stage_dir = create_stage_directory(data_root)?;
    match download_and_stage(
        &agent,
        &stage_dir,
        &authorization.data,
        &manifest,
        &manifest_bytes,
        &signature_bytes,
    ) {
        Ok(bundle) => Ok(bundle),
        Err(error) => {
            cleanup_stage_directory(&stage_dir);
            Err(error)
        }
    }
}

fn validate_success_envelope<T>(response: &ApiSuccess<T>) -> Result<()> {
    if response.api_version != "v1"
        || response.request_id.is_empty()
        || response.request_id.len() > 128
        || !response
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid_response: commercial API response identity is invalid");
    }
    Ok(())
}

fn validate_auth(auth: &CommercialArtifactAuth<'_>) -> Result<()> {
    let token = auth
        .authorization
        .strip_prefix("Bearer ")
        .filter(|token| !token.is_empty())
        .or_else(|| {
            auth.authorization
                .strip_prefix("CtxTrial ")
                .filter(|token| token.len() >= 16)
        });
    if token.is_none()
        || auth.authorization.len() > 16 * 1024
        || auth
            .authorization
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        bail!("authentication_required: commercial access token is unavailable");
    }
    let _ = api_url(auth.api_base_url, "/v1/artifacts/manifest")?;
    Ok(())
}

fn api_url(base: &str, path: &str) -> Result<Url> {
    let base = Url::parse(base).context("invalid_request: commercial API URL is invalid")?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
    {
        bail!("invalid_request: commercial API URL must be an HTTPS origin");
    }
    base.join(path)
        .context("invalid_request: commercial API route is invalid")
}

fn post_json_bounded(
    agent: &ureq::Agent,
    url: &Url,
    authorization: &str,
    body: &serde_json::Value,
) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(body).context("invalid_request: encode commercial request")?;
    let response = agent
        .post(url.as_str())
        .set("accept", COMMERCIAL_ACCEPT)
        .set("authorization", authorization)
        .set("content-type", "application/json")
        .set("idempotency-key", &new_idempotency_key("artifact")?)
        .send_bytes(&body);
    let response =
        response.map_err(|error| commercial_http_error(error, "commercial API request"))?;
    read_bounded_response(response, MAX_API_RESPONSE_BYTES, "commercial API response")
}

fn safe_artifact_download_error(error: ureq::Error) -> anyhow::Error {
    match error {
        ureq::Error::Status(status, _) => {
            anyhow!("service_unavailable: artifact download returned status {status}")
        }
        ureq::Error::Transport(_) => anyhow!("service_unavailable: artifact download failed"),
    }
}

fn read_bounded_response(response: ureq::Response, maximum: u64, label: &str) -> Result<Vec<u8>> {
    if let Some(length) = response.header("content-length") {
        let length = length
            .parse::<u64>()
            .with_context(|| format!("invalid_response: {label} length is invalid"))?;
        if length > maximum {
            bail!("invalid_response: {label} exceeds maximum size");
        }
    }
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("service_unavailable: read {label}"))?;
    if bytes.len() as u64 > maximum {
        bail!("invalid_response: {label} exceeds maximum size");
    }
    Ok(bytes)
}

fn verify_envelope(
    envelope: ManifestEnvelope,
    trust: ReleaseTrust,
) -> Result<(ProManifest, Vec<u8>, Vec<u8>)> {
    if envelope.schema_version != 1
        || envelope.manifest_base64.len() as u64 > MAX_MANIFEST_BYTES.saturating_mul(2)
        || envelope.signature_base64.len() as u64 > MAX_SIGNATURE_BYTES
    {
        bail!("invalid_response: artifact manifest envelope is outside allowed bounds");
    }
    let manifest_bytes = BASE64
        .decode(envelope.manifest_base64)
        .context("invalid_response: artifact manifest is not base64")?;
    let signature_bytes = format!("{}\n", envelope.signature_base64).into_bytes();
    let manifest = verified_manifest_for_trust(&manifest_bytes, &signature_bytes, trust)?;
    Ok((manifest, manifest_bytes, signature_bytes))
}

fn validate_download_authorization(
    authorization: &DownloadAuthorization,
    api_base_url: &str,
    manifest: &ProManifest,
    manifest_digest: &str,
) -> Result<()> {
    let expected_url = api_url(api_base_url, "/v1/artifacts/object")?;
    let observed_url = Url::parse(&authorization.url)
        .context("invalid_response: artifact download URL is invalid")?;
    if authorization.schema_version != 1
        || authorization.channel != manifest.channel
        || authorization.version != manifest.version
        || authorization.target != manifest.target
        || authorization.build_identity != manifest.build_identity
        || authorization.protocol_fingerprint != manifest.protocol_fingerprint
        || authorization.artifact_object != manifest.artifact_object
        || observed_url != expected_url
        || !authorization.authorization.starts_with("CtxArtifact ")
        || authorization.authorization.len() > MAX_DOWNLOAD_AUTHORIZATION_BYTES
        || authorization.manifest_sha256 != manifest_digest
        || authorization.artifact_size != manifest.artifact_size
        || authorization.artifact_sha256 != manifest.artifact_sha256
    {
        bail!("invalid_response: artifact download authorization does not match manifest");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("invalid_response: system clock is before Unix epoch")?
        .as_secs();
    if authorization.expires_at_unix <= now
        || authorization.expires_at_unix > now.saturating_add(MAX_DOWNLOAD_LIFETIME_SECONDS)
    {
        bail!("invalid_response: artifact download authorization has invalid expiry");
    }
    Ok(())
}

fn create_stage_directory(data_root: &Path) -> Result<PathBuf> {
    let layout = ProFilesystemLayout::new(data_root);
    let pro = layout.pro_root();
    let parent = layout.downloads_dir();
    if parent.exists() {
        let metadata = fs::symlink_metadata(&parent)
            .context("invalid_request: inspect Pro download directory")?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("invalid_request: Pro download directory is unsafe");
        }
    } else {
        fs::create_dir_all(&parent).context("invalid_request: create Pro download directory")?;
    }
    for directory in [data_root, pro.as_path(), parent.as_path()] {
        restrict_private_directory(directory)
            .context("invalid_request: protect Pro download directory")?;
    }
    let stage = parent.join(format!("bundle-{}", Uuid::new_v4()));
    fs::create_dir(&stage).context("invalid_request: create Pro download staging directory")?;
    restrict_private_directory(&stage)
        .context("invalid_request: protect Pro download staging directory")?;
    Ok(stage)
}

fn download_and_stage(
    agent: &ureq::Agent,
    stage_dir: &Path,
    authorization: &DownloadAuthorization,
    manifest: &ProManifest,
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<VerifiedArtifactBundle> {
    let response = agent
        .get(&authorization.url)
        .set("authorization", &authorization.authorization)
        .call()
        .map_err(safe_artifact_download_error)?;
    if response.header("content-type") != Some("application/octet-stream") {
        bail!("invalid_response: artifact download content type is invalid");
    }
    let length = response
        .header("content-length")
        .ok_or_else(|| anyhow!("invalid_response: artifact download length is missing"))?
        .parse::<u64>()
        .context("invalid_response: artifact download length is invalid")?;
    if length != manifest.artifact_size || length > MAX_ARTIFACT_BYTES {
        bail!("invalid_response: artifact download length does not match manifest");
    }

    let artifact = stage_dir.join(if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    });
    let mut output = create_stage_file(&artifact, 0o700)?;
    let mut reader = response.into_reader().take(manifest.artifact_size + 1);
    let mut digest = Sha256::new();
    let mut written = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .context("service_unavailable: read artifact download")?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > manifest.artifact_size {
            bail!("invalid_response: artifact download exceeds signed length");
        }
        output
            .write_all(&buffer[..read])
            .context("invalid_request: stage Pro artifact")?;
        digest.update(&buffer[..read]);
    }
    if written != manifest.artifact_size
        || !format!("{:x}", digest.finalize()).eq_ignore_ascii_case(&manifest.artifact_sha256)
    {
        bail!("invalid_response: artifact download digest or length does not match manifest");
    }
    output
        .sync_all()
        .context("invalid_request: sync staged Pro artifact")?;

    let manifest_path = stage_dir.join("manifest.json");
    write_stage_file(&manifest_path, manifest_bytes, 0o600)?;
    let signature_path = stage_dir.join("manifest.sig");
    write_stage_file(&signature_path, signature_bytes, 0o600)?;
    sync_stage_directory(stage_dir)?;
    Ok(VerifiedArtifactBundle {
        stage_dir: stage_dir.to_path_buf(),
        artifact,
        manifest: manifest_path,
        signature: signature_path,
    })
}

fn create_stage_file(path: &Path, unix_mode: u32) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(unix_mode);
    }
    #[cfg(not(unix))]
    let _ = unix_mode;
    let file = options
        .open(path)
        .context("invalid_request: create Pro download staging file")?;
    if unix_mode & 0o100 != 0 {
        restrict_private_executable(path)
            .context("invalid_request: protect Pro download staging executable")?;
    } else {
        restrict_private_file(path)
            .context("invalid_request: protect Pro download staging file")?;
    }
    Ok(file)
}

fn write_stage_file(path: &Path, bytes: &[u8], unix_mode: u32) -> Result<()> {
    let mut file = create_stage_file(path, unix_mode)?;
    file.write_all(bytes)
        .context("invalid_request: write Pro download staging file")?;
    file.sync_all()
        .context("invalid_request: sync Pro download staging file")
}

fn sync_stage_directory(path: &Path) -> Result<()> {
    #[cfg(not(windows))]
    let directory = File::open(path).context("invalid_request: open Pro staging directory")?;
    #[cfg(windows)]
    let directory = {
        use std::os::windows::fs::OpenOptionsExt;
        OpenOptions::new()
            .write(true)
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .context("invalid_request: open Pro staging directory")?
    };
    directory
        .sync_all()
        .context("invalid_request: sync Pro download staging directory")
}

fn cleanup_stage_directory(stage: &Path) {
    for name in ["ctx-pro", "ctx-pro.exe", "manifest.json", "manifest.sig"] {
        let _ = fs::remove_file(stage.join(name));
    }
    let _ = fs::remove_dir(stage);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pro::commercial_api::{commercial_retry_after, is_retryable_commercial_failure};

    fn test_response(
        status: u16,
        content_type: &str,
        retry_after: Option<&str>,
        body: &str,
    ) -> ureq::Response {
        let retry_after = retry_after
            .map(|value| format!("Retry-After: {value}\r\n"))
            .unwrap_or_default();
        format!(
            "HTTP/1.1 {status} Test\r\nContent-Type: {content_type}\r\n{retry_after}Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .parse()
        .unwrap()
    }

    fn artifact_api_error(
        status: u16,
        content_type: &str,
        retry_after: Option<&str>,
        body: &str,
    ) -> anyhow::Error {
        commercial_http_error(
            ureq::Error::Status(
                status,
                test_response(status, content_type, retry_after, body),
            ),
            "commercial API request",
        )
    }

    fn worker_problem(code: &str, retryable: bool) -> String {
        json!({
            "api_version": "v1",
            "request_id": "123e4567-e89b-12d3-a456-426614174000",
            "error": {
                "code": code,
                "message": "Worker body detail must stay redacted",
                "retryable": retryable,
            },
        })
        .to_string()
    }

    #[test]
    fn api_and_download_urls_are_exact_and_never_redirectable() {
        let url = api_url("https://pro.ctx.test", "/v1/artifacts/object").unwrap();
        assert_eq!(url.as_str(), "https://pro.ctx.test/v1/artifacts/object");
        for invalid in [
            "http://pro.ctx.test",
            "https://user@pro.ctx.test",
            "https://pro.ctx.test/api/",
            "https://pro.ctx.test?token=secret",
            "file:///tmp/helper",
        ] {
            assert!(api_url(invalid, "/v1/artifacts/object").is_err());
        }
    }

    #[test]
    fn artifact_problem_responses_preserve_safe_non_transient_failures() {
        for (status, code, expected_message) in [
            (
                401,
                "authentication_required",
                "sign in again with `ctx pro`",
            ),
            (
                403,
                "commercial_access_locked",
                "an active trial or subscription is required",
            ),
            (
                404,
                "not_found",
                "the requested commercial resource was not found",
            ),
            (
                409,
                "billing_conflict",
                "multiple active subscriptions need attention",
            ),
            (
                409,
                "commercial_identity_conflict",
                "the billing customer belongs to a different signed-in account",
            ),
        ] {
            let error = artifact_api_error(
                status,
                "application/problem+json; charset=utf-8",
                Some("60"),
                &worker_problem(code, true),
            );
            let rendered = error.to_string();
            assert!(rendered.starts_with(&format!("{code}:")), "{rendered}");
            assert!(rendered.contains(expected_message), "{rendered}");
            assert!(!rendered.contains("Worker body detail"), "{rendered}");
            assert!(!is_retryable_commercial_failure(&error), "{rendered}");
            assert_eq!(commercial_retry_after(&error), None);
        }
    }

    #[test]
    fn artifact_problem_responses_retain_bounded_transient_retry_semantics() {
        let rate_limited = artifact_api_error(
            429,
            "application/problem+json",
            Some("999999"),
            &worker_problem("rate_limited", true),
        );
        assert!(is_retryable_commercial_failure(&rate_limited));
        assert_eq!(
            commercial_retry_after(&rate_limited),
            Some(Duration::from_secs(30 * 60))
        );
        assert!(rate_limited.to_string().starts_with("rate_limited:"));
        assert!(!rate_limited.to_string().contains("Worker body detail"));

        let server_error = artifact_api_error(
            500,
            "application/problem+json",
            Some("75"),
            &worker_problem("internal_error", true),
        );
        assert!(is_retryable_commercial_failure(&server_error));
        assert_eq!(
            commercial_retry_after(&server_error),
            Some(Duration::from_secs(75))
        );
        assert!(server_error.to_string().starts_with("internal_error:"));
        assert!(!server_error.to_string().contains("Worker body detail"));

        let invalid_dependency = artifact_api_error(
            502,
            "application/problem+json",
            Some("60"),
            &worker_problem("dependency_invalid_response", false),
        );
        assert!(!is_retryable_commercial_failure(&invalid_dependency));
        assert_eq!(commercial_retry_after(&invalid_dependency), None);
    }

    #[test]
    fn artifact_malformed_error_bodies_are_bounded_and_never_rendered() {
        let authentication = artifact_api_error(
            401,
            "application/problem+json",
            None,
            "malformed bearer-secret body",
        );
        assert!(!is_retryable_commercial_failure(&authentication));
        assert!(authentication.to_string().starts_with("invalid_response:"));
        assert!(!authentication.to_string().contains("bearer-secret"));

        let proxy = artifact_api_error(
            503,
            "text/html",
            Some("999999"),
            "<html>proxy-secret body</html>",
        );
        assert!(is_retryable_commercial_failure(&proxy));
        assert_eq!(
            commercial_retry_after(&proxy),
            Some(Duration::from_secs(30 * 60))
        );
        assert!(!proxy.to_string().contains("proxy-secret"));

        let oversized = format!(
            "bounded-secret{}",
            "x".repeat(MAX_API_RESPONSE_BYTES as usize)
        );
        let oversized = artifact_api_error(503, "application/problem+json", Some("30"), &oversized);
        assert!(is_retryable_commercial_failure(&oversized));
        assert_eq!(
            commercial_retry_after(&oversized),
            Some(Duration::from_secs(30))
        );
        assert!(!oversized.to_string().contains("bounded-secret"));
    }

    #[test]
    fn stage_directory_is_private_unique_and_removed_with_bundle() {
        let root = tempfile::tempdir().unwrap();
        let stage = create_stage_directory(root.path()).unwrap();
        let artifact = stage.join("ctx-pro");
        let manifest = stage.join("manifest.json");
        let signature = stage.join("manifest.sig");
        write_stage_file(&artifact, b"artifact", 0o700).unwrap();
        write_stage_file(&manifest, b"manifest", 0o600).unwrap();
        write_stage_file(&signature, b"signature", 0o600).unwrap();
        let bundle = VerifiedArtifactBundle {
            stage_dir: stage.clone(),
            artifact,
            manifest,
            signature,
        };
        drop(bundle);
        assert!(!stage.exists());
    }

    #[test]
    fn download_authorization_is_bound_to_the_exact_signed_build() {
        let target = platform_target();
        let manifest = ProManifest {
            schema_version: 1,
            product: "ctx-pro".to_owned(),
            channel: "staging".to_owned(),
            version: "1.2.3".to_owned(),
            source_commit: "1".repeat(40),
            public_source_commit: "3".repeat(40),
            private_source_commit: "1".repeat(40),
            build_identity: "2".repeat(64),
            protocol_min: ctx_pro_host_protocol::PROTOCOL_VERSION,
            protocol_max: ctx_pro_host_protocol::PROTOCOL_VERSION,
            protocol_fingerprint: ctx_pro_host_protocol::PROTOCOL_FINGERPRINT.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            artifact_object: format!(
                "pro/artifacts/staging/1.2.3/{target}/{}",
                if cfg!(windows) {
                    "ctx-pro.exe"
                } else {
                    "ctx-pro"
                }
            ),
            target,
            artifact_size: 3,
            artifact_sha256: "a".repeat(64),
            public_artifact_sha256: "b".repeat(64),
            public_package_sha256: "c".repeat(64),
            private_package_sha256: "d".repeat(64),
            runtime_evidence_sha256: "e".repeat(64),
            runtime_run_id: "12345678-1234-4234-8234-123456789abc".to_owned(),
            release_key_id: "ctx-pro-release-staging-2026-07-21".to_owned(),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut authorization = DownloadAuthorization {
            schema_version: 1,
            channel: manifest.channel.clone(),
            version: manifest.version.clone(),
            target: manifest.target.clone(),
            build_identity: manifest.build_identity.clone(),
            protocol_fingerprint: manifest.protocol_fingerprint.clone(),
            artifact_object: manifest.artifact_object.clone(),
            url: "https://commercial.example/v1/artifacts/object".to_owned(),
            authorization: "CtxArtifact payload.signature".to_owned(),
            expires_at_unix: now + 60,
            manifest_sha256: "b".repeat(64),
            artifact_size: manifest.artifact_size,
            artifact_sha256: manifest.artifact_sha256.clone(),
        };
        validate_download_authorization(
            &authorization,
            "https://commercial.example/",
            &manifest,
            &"b".repeat(64),
        )
        .unwrap();

        authorization.build_identity = "3".repeat(64);
        assert_eq!(
            validate_download_authorization(
                &authorization,
                "https://commercial.example/",
                &manifest,
                &"b".repeat(64),
            )
            .unwrap_err()
            .to_string(),
            "invalid_response: artifact download authorization does not match manifest"
        );
    }
}
