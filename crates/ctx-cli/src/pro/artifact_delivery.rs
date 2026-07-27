use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_history_core::platform_security::{
    restrict_private_directory, restrict_private_executable, restrict_private_file,
};
use ctx_pro_host_protocol::ProFilesystemLayout;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use uuid::Uuid;

use super::lifecycle::lifecycle_manifest::{
    parse_release_version, platform_target, verified_manifest_for_trust, ProManifest, ReleaseTrust,
    MAX_ARTIFACT_BYTES, MAX_MANIFEST_BYTES, MAX_SIGNATURE_BYTES,
};
use super::lifecycle::ProInstallArgs;

const RELEASE_ACCEPT: &str = "application/json, application/problem+json";
const MAX_RELEASE_RESPONSE_BYTES: u64 = 96 * 1024;

pub(crate) struct ArtifactDeliveryConfig<'a> {
    pub(crate) release_origin: &'a str,
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

#[derive(Debug, thiserror::Error)]
enum ReleaseReadFailure {
    #[error("service_unavailable: anonymous release {operation} failed")]
    Transport { operation: &'static str },
    #[error("rate_limited: anonymous release {operation} returned HTTP 429")]
    RateLimited { operation: &'static str },
    #[error("service_unavailable: anonymous release {operation} returned HTTP {status}")]
    Transient {
        operation: &'static str,
        status: u16,
    },
    #[error("invalid_response: anonymous release {operation} returned HTTP {status}")]
    Rejected {
        operation: &'static str,
        status: u16,
    },
}

pub(crate) fn fetch_latest(
    data_root: &Path,
    installed_version: Option<&str>,
    config: ArtifactDeliveryConfig<'_>,
) -> Result<VerifiedArtifactBundle> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(15))
        .build();
    let target = platform_target();
    let manifest_url = manifest_url(
        config.release_origin,
        config.release_trust.channel.wire_name(),
        &target,
    )?;
    let envelope_bytes = get_manifest_bounded(&agent, &manifest_url)?;
    let envelope: ApiSuccess<ManifestEnvelope> = serde_json::from_slice(&envelope_bytes)
        .context("invalid_response: parse artifact manifest response")?;
    validate_success_envelope(&envelope)?;
    let (manifest, manifest_bytes, signature_bytes) =
        verify_envelope(envelope.data, config.release_trust)?;
    reject_release_rollback(installed_version, &manifest.version)?;
    let artifact_url = artifact_url(config.release_origin, &manifest.artifact_object)?;

    let stage_dir = create_stage_directory(data_root)?;
    match download_and_stage(
        &agent,
        &stage_dir,
        &artifact_url,
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

fn reject_release_rollback(installed_version: Option<&str>, release_version: &str) -> Result<()> {
    let Some(installed_version) = installed_version else {
        return Ok(());
    };
    if parse_release_version(release_version)? < parse_release_version(installed_version)? {
        bail!("invalid_request: Pro update would roll back the installed version");
    }
    Ok(())
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
        bail!("invalid_response: anonymous release response identity is invalid");
    }
    Ok(())
}

fn release_url(base: &str, path: &str) -> Result<Url> {
    let base = Url::parse(base).context("invalid_request: release service origin is invalid")?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.path() != "/"
        || base.query().is_some()
        || base.fragment().is_some()
    {
        bail!("invalid_request: release service must be an HTTPS origin");
    }
    base.join(path)
        .context("invalid_request: release service route is invalid")
}

fn manifest_url(base: &str, channel: &str, target: &str) -> Result<Url> {
    let mut url = release_url(base, "/v1/artifacts/manifest")?;
    url.query_pairs_mut()
        .append_pair("channel", channel)
        .append_pair("target", target);
    Ok(url)
}

fn artifact_url(base: &str, artifact_object: &str) -> Result<Url> {
    let mut url = release_url(base, "/v1/artifacts/object")?;
    if artifact_object.is_empty() {
        bail!("invalid_response: signed manifest contains invalid artifact object");
    }
    {
        let mut path = url.path_segments_mut().map_err(|_| {
            anyhow!("invalid_request: release service origin cannot contain path segments")
        })?;
        for segment in artifact_object.split('/') {
            if segment.is_empty()
                || matches!(segment, "." | "..")
                || segment.contains('\\')
                || segment.bytes().any(|byte| byte.is_ascii_control())
            {
                bail!("invalid_response: signed manifest contains invalid artifact object");
            }
            path.push(segment);
        }
    }
    Ok(url)
}

fn get_manifest_bounded(agent: &ureq::Agent, url: &Url) -> Result<Vec<u8>> {
    let response = agent.get(url.as_str()).set("accept", RELEASE_ACCEPT).call();
    let response =
        response.map_err(|error| anonymous_release_read_error(error, "manifest read"))?;
    let response = require_success_status(response, "manifest read")?;
    read_bounded_response(
        response,
        MAX_RELEASE_RESPONSE_BYTES,
        "release manifest response",
    )
}

fn anonymous_release_read_error(error: ureq::Error, operation: &'static str) -> anyhow::Error {
    match error {
        ureq::Error::Status(status, response) => {
            anonymous_release_status_error(status, response, operation)
        }
        ureq::Error::Transport(_) => ReleaseReadFailure::Transport { operation }.into(),
    }
}

fn require_success_status(
    response: ureq::Response,
    operation: &'static str,
) -> Result<ureq::Response> {
    let status = response.status();
    if status == 200 {
        return Ok(response);
    }
    Err(anonymous_release_status_error(status, response, operation))
}

fn anonymous_release_status_error(
    status: u16,
    response: ureq::Response,
    operation: &'static str,
) -> anyhow::Error {
    discard_bounded_error_response(response);
    if status == 429 {
        ReleaseReadFailure::RateLimited { operation }.into()
    } else if (500..=599).contains(&status) {
        ReleaseReadFailure::Transient { operation, status }.into()
    } else {
        ReleaseReadFailure::Rejected { operation, status }.into()
    }
}

fn discard_bounded_error_response(response: ureq::Response) {
    let mut reader = response.into_reader().take(MAX_RELEASE_RESPONSE_BYTES + 1);
    let mut buffer = [0_u8; 8 * 1024];
    let mut remaining = MAX_RELEASE_RESPONSE_BYTES + 1;
    while remaining > 0 {
        let maximum = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        match reader.read(&mut buffer[..maximum]) {
            Ok(0) | Err(_) => break,
            Ok(read) => remaining = remaining.saturating_sub(read as u64),
        }
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
    artifact_url: &Url,
    manifest: &ProManifest,
    manifest_bytes: &[u8],
    signature_bytes: &[u8],
) -> Result<VerifiedArtifactBundle> {
    let response = agent
        .get(artifact_url.as_str())
        .call()
        .map_err(|error| anonymous_release_read_error(error, "artifact read"))?;
    let response = require_success_status(response, "artifact read")?;
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
    use std::{
        net::{TcpListener, TcpStream},
        sync::Arc,
        thread,
        time::Instant,
    };

    use super::*;

    #[derive(Debug)]
    struct PlaintextTestTls;

    impl ureq::TlsConnector for PlaintextTestTls {
        fn connect(
            &self,
            _dns_name: &str,
            io: Box<dyn ureq::ReadWrite>,
        ) -> std::result::Result<Box<dyn ureq::ReadWrite>, ureq::Error> {
            Ok(io)
        }
    }

    struct RecordedRequest {
        request_line: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    fn accept_with_timeout(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match listener.accept() {
                Ok((stream, _)) => return stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for artifact request"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept artifact request: {error}"),
            }
        }
    }

    fn read_request(stream: &mut TcpStream) -> RecordedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut wire = Vec::new();
        let header_end = loop {
            let mut buffer = [0_u8; 1024];
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0, "artifact connection closed before a request");
            wire.extend_from_slice(&buffer[..read]);
            assert!(
                wire.len() <= 32 * 1024,
                "artifact request headers too large"
            );
            if let Some(offset) = wire.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };

        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut trailing = [0_u8; 1];
        match stream.read(&mut trailing) {
            Ok(0) => {}
            Ok(read) => wire.extend_from_slice(&trailing[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(error) => panic!("read artifact request body: {error}"),
        }

        let header = std::str::from_utf8(&wire[..header_end - 4]).unwrap();
        let mut lines = header.split("\r\n");
        let request_line = lines.next().unwrap().to_owned();
        let headers = lines
            .map(|line| {
                let (name, value) = line.split_once(':').unwrap();
                (name.trim().to_ascii_lowercase(), value.trim().to_owned())
            })
            .collect();
        RecordedRequest {
            request_line,
            headers,
            body: wire[header_end..].to_vec(),
        }
    }

    fn write_response(
        stream: &mut TcpStream,
        status: &str,
        content_type: &str,
        extra_headers: &str,
        body: &[u8],
    ) {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(body).unwrap();
    }

    fn assert_anonymous_bodyless_get(request: &RecordedRequest, expected_target: &str) {
        assert_eq!(
            request.request_line,
            format!("GET {expected_target} HTTP/1.1")
        );
        for forbidden in [
            "authorization",
            "proxy-authorization",
            "idempotency-key",
            "cookie",
            "content-length",
            "transfer-encoding",
        ] {
            assert!(
                request.headers.iter().all(|(name, _)| name != forbidden),
                "anonymous artifact GET sent forbidden header {forbidden}"
            );
        }
        assert!(
            request.body.is_empty(),
            "anonymous artifact GET sent a body"
        );
    }

    fn transport_test_manifest(artifact_object: String, artifact: &[u8]) -> ProManifest {
        ProManifest {
            schema_version: 1,
            product: "ctx-pro".to_owned(),
            channel: "staging".to_owned(),
            version: "1.2.3".to_owned(),
            source_commit: "1".repeat(40),
            public_source_commit: "2".repeat(40),
            private_source_commit: "1".repeat(40),
            build_identity: "3".repeat(64),
            protocol_min: ctx_pro_host_protocol::PROTOCOL_VERSION,
            protocol_max: ctx_pro_host_protocol::PROTOCOL_VERSION,
            protocol_fingerprint: ctx_pro_host_protocol::PROTOCOL_FINGERPRINT.to_owned(),
            target: platform_target(),
            architecture: std::env::consts::ARCH.to_owned(),
            artifact_object,
            artifact_size: artifact.len() as u64,
            artifact_sha256: format!("{:x}", Sha256::digest(artifact)),
            public_artifact_sha256: "4".repeat(64),
            public_package_sha256: "5".repeat(64),
            private_package_sha256: "6".repeat(64),
            runtime_evidence_sha256: "7".repeat(64),
            runtime_run_id: "12345678-1234-4234-8234-123456789abc".to_owned(),
            release_key_id: "ctx-pro-release-staging-test".to_owned(),
        }
    }

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

    fn release_read_error(
        status: u16,
        content_type: &str,
        retry_after: Option<&str>,
        body: &str,
    ) -> anyhow::Error {
        anonymous_release_read_error(
            ureq::Error::Status(
                status,
                test_response(status, content_type, retry_after, body),
            ),
            "manifest read",
        )
    }

    fn is_retryable_release_read_failure(error: &anyhow::Error) -> bool {
        error
            .downcast_ref::<ReleaseReadFailure>()
            .is_some_and(|failure| {
                matches!(
                    failure,
                    ReleaseReadFailure::Transport { .. }
                        | ReleaseReadFailure::RateLimited { .. }
                        | ReleaseReadFailure::Transient { .. }
                )
            })
    }

    #[test]
    fn anonymous_artifact_reads_are_exact_credentialless_bodyless_and_do_not_redirect() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let target = platform_target();
        let artifact_object = format!("pro/artifacts/staging/1.2.3/{target}/ctx-pro");
        let manifest_target = format!("/v1/artifacts/manifest?channel=staging&target={target}");
        let artifact_target = format!("/v1/artifacts/object/{artifact_object}");
        let artifact = b"anonymous pro artifact";
        let server_artifact = artifact.to_vec();

        let server = thread::spawn(move || {
            let mut requests = Vec::new();

            let mut stream = accept_with_timeout(&listener);
            requests.push(read_request(&mut stream));
            write_response(
                &mut stream,
                "200 OK",
                "application/json",
                "",
                br#"{"manifest":"transport-only"}"#,
            );
            drop(stream);

            let mut stream = accept_with_timeout(&listener);
            requests.push(read_request(&mut stream));
            write_response(
                &mut stream,
                "200 OK",
                "application/octet-stream",
                "",
                &server_artifact,
            );
            drop(stream);

            let mut stream = accept_with_timeout(&listener);
            requests.push(read_request(&mut stream));
            write_response(
                &mut stream,
                "302 Found",
                "application/octet-stream",
                &format!("Location: https://{address}/redirect-target\r\n"),
                b"",
            );
            drop(stream);

            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => panic!("anonymous artifact client followed a redirect"),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("check for redirected request: {error}"),
                }
            }
            requests
        });

        let origin = format!("https://{address}");
        let manifest_url = manifest_url(&origin, "staging", &target).unwrap();
        let artifact_url = artifact_url(&origin, &artifact_object).unwrap();
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout_connect(Duration::from_secs(2))
            .timeout_read(Duration::from_secs(2))
            .timeout_write(Duration::from_secs(2))
            .tls_connector(Arc::new(PlaintextTestTls))
            .build();

        assert_eq!(
            get_manifest_bounded(&agent, &manifest_url).unwrap(),
            br#"{"manifest":"transport-only"}"#
        );

        let root = tempfile::tempdir().unwrap();
        let stage = create_stage_directory(root.path()).unwrap();
        let manifest = transport_test_manifest(artifact_object, artifact);
        let bundle = download_and_stage(
            &agent,
            &stage,
            &artifact_url,
            &manifest,
            b"{}",
            b"signature",
        )
        .unwrap();
        assert_eq!(fs::read(&bundle.artifact).unwrap(), artifact);

        let error = get_manifest_bounded(&agent, &manifest_url).unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid_response: anonymous release manifest read returned HTTP 302"
        );

        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert_anonymous_bodyless_get(&requests[0], &manifest_target);
        assert_anonymous_bodyless_get(&requests[1], &artifact_target);
        assert_anonymous_bodyless_get(&requests[2], &manifest_target);
    }

    #[test]
    fn signed_release_version_is_checked_before_artifact_delivery() {
        reject_release_rollback(None, "0.25.0").unwrap();
        reject_release_rollback(Some("0.25.0"), "0.25.0").unwrap();
        reject_release_rollback(Some("0.25.0"), "0.26.0").unwrap();
        assert_eq!(
            reject_release_rollback(Some("0.26.0"), "0.25.0")
                .unwrap_err()
                .to_string(),
            "invalid_request: Pro update would roll back the installed version"
        );
    }

    #[test]
    fn anonymous_artifact_urls_are_exact_same_origin_routes() {
        let manifest = manifest_url(
            "https://pro.ctx.test",
            "staging",
            "x86_64-unknown-linux-gnu",
        )
        .unwrap();
        assert_eq!(
            manifest.as_str(),
            "https://pro.ctx.test/v1/artifacts/manifest?channel=staging&target=x86_64-unknown-linux-gnu"
        );
        let artifact = artifact_url(
            "https://pro.ctx.test",
            "pro/artifacts/staging/1.2.3/x86_64-unknown-linux-gnu/ctx-pro",
        )
        .unwrap();
        assert_eq!(
            artifact.as_str(),
            "https://pro.ctx.test/v1/artifacts/object/pro/artifacts/staging/1.2.3/x86_64-unknown-linux-gnu/ctx-pro"
        );
        assert_eq!(manifest.origin(), artifact.origin());

        for invalid in [
            "http://pro.ctx.test",
            "https://user@pro.ctx.test",
            "https://pro.ctx.test/api/",
            "https://pro.ctx.test?token=secret",
            "file:///tmp/helper",
        ] {
            assert!(manifest_url(invalid, "staging", "target").is_err());
        }
    }

    #[test]
    fn artifact_object_is_appended_as_validated_path_segments() {
        for invalid in [
            "",
            "/absolute",
            "pro//artifact",
            "pro/../artifact",
            "pro/./artifact",
            "pro\\artifact",
            "pro/\nartifact",
        ] {
            assert!(artifact_url("https://pro.ctx.test", invalid).is_err());
        }

        let encoded = artifact_url("https://pro.ctx.test", "pro/artifact?query#fragment").unwrap();
        assert_eq!(
            encoded.as_str(),
            "https://pro.ctx.test/v1/artifacts/object/pro/artifact%3Fquery%23fragment"
        );
    }

    #[test]
    fn anonymous_release_rejections_are_generic_sanitized_and_not_retryable() {
        let error = release_read_error(
            404,
            "application/problem+json",
            Some("60"),
            "release-origin-detail-must-not-escape",
        );
        let rendered = error.to_string();
        assert_eq!(
            rendered,
            "invalid_response: anonymous release manifest read returned HTTP 404"
        );
        assert!(!rendered.contains("release-origin-detail"), "{rendered}");
        assert!(!is_retryable_release_read_failure(&error), "{rendered}");
    }

    #[test]
    fn anonymous_release_reads_retain_bounded_transient_retry_semantics() {
        let rate_limited = release_read_error(
            429,
            "application/problem+json",
            Some("999999"),
            "rate-limit-detail-must-not-escape",
        );
        assert!(is_retryable_release_read_failure(&rate_limited));
        assert!(rate_limited.to_string().starts_with("rate_limited:"));
        assert!(!rate_limited.to_string().contains("rate-limit-detail"));

        let server_error = release_read_error(
            500,
            "application/problem+json",
            Some("75"),
            "server-detail-must-not-escape",
        );
        assert!(is_retryable_release_read_failure(&server_error));
        assert!(server_error.to_string().starts_with("service_unavailable:"));
        assert!(!server_error.to_string().contains("server-detail"));
    }

    #[test]
    fn anonymous_release_error_bodies_are_bounded_and_never_rendered() {
        let proxy = release_read_error(
            503,
            "text/html",
            Some("999999"),
            "<html>proxy-secret body</html>",
        );
        assert!(is_retryable_release_read_failure(&proxy));
        assert!(!proxy.to_string().contains("proxy-secret"));

        let oversized = format!(
            "bounded-secret{}",
            "x".repeat(MAX_RELEASE_RESPONSE_BYTES as usize)
        );
        let oversized = release_read_error(503, "application/problem+json", Some("30"), &oversized);
        assert!(is_retryable_release_read_failure(&oversized));
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
}
