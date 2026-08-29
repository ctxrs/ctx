use std::{
    fs,
    fs::OpenOptions,
    io::{Read, Seek, Write},
    net::Ipv4Addr,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use url::{Host, Url};

use crate::analytics::AnalyticsDeliveryFailureClass;

pub(crate) const TELEMETRY_HTTP_TIMEOUT: Duration = Duration::from_millis(250);
pub(crate) const DAEMON_TELEMETRY_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_ARTIFACT_REDIRECTS: usize = 5;

#[derive(Debug)]
pub(crate) struct TelemetryPostError {
    class: AnalyticsDeliveryFailureClass,
    source: anyhow::Error,
}

impl TelemetryPostError {
    pub(crate) fn class(&self) -> AnalyticsDeliveryFailureClass {
        self.class
    }

    fn new(class: AnalyticsDeliveryFailureClass, source: impl Into<anyhow::Error>) -> Self {
        Self {
            class,
            source: source.into(),
        }
    }
}

impl std::fmt::Display for TelemetryPostError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for TelemetryPostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.source()
    }
}

#[cfg(test)]
fn post_telemetry_json(endpoint: &str, body: &[u8]) -> std::result::Result<(), TelemetryPostError> {
    post_telemetry_json_with_timeout(endpoint, body, TELEMETRY_HTTP_TIMEOUT)
}

pub(crate) fn post_telemetry_json_with_timeout(
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
) -> std::result::Result<(), TelemetryPostError> {
    let file_path = file_url_path(endpoint).map_err(|error| {
        TelemetryPostError::new(AnalyticsDeliveryFailureClass::Configuration, error)
    })?;
    if let Some(path) = file_path {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))
            .map_err(|error| {
                TelemetryPostError::new(AnalyticsDeliveryFailureClass::LocalIo, error)
            })?;
        file.write_all(body).map_err(|error| {
            TelemetryPostError::new(AnalyticsDeliveryFailureClass::LocalIo, error)
        })?;
        file.write_all(b"\n").map_err(|error| {
            TelemetryPostError::new(AnalyticsDeliveryFailureClass::LocalIo, error)
        })?;
        return Ok(());
    }
    require_https_or_localhost(endpoint).map_err(|error| {
        TelemetryPostError::new(AnalyticsDeliveryFailureClass::Configuration, error)
    })?;
    let result = ureq::post(endpoint)
        // ureq applies this overall deadline to connection establishment too
        // when no separate connect timeout overrides it.
        .timeout(timeout)
        .set("content-type", "application/json")
        .send_bytes(body);
    match result {
        Ok(_) => Ok(()),
        Err(error) => {
            let class = match &error {
                ureq::Error::Status(429, _) => AnalyticsDeliveryFailureClass::RateLimited,
                ureq::Error::Status(status, _) if (400..500).contains(status) => {
                    AnalyticsDeliveryFailureClass::ClientRejection
                }
                ureq::Error::Status(status, _) if (500..600).contains(status) => {
                    AnalyticsDeliveryFailureClass::Server
                }
                ureq::Error::Status(_, _) => AnalyticsDeliveryFailureClass::Unknown,
                ureq::Error::Transport(_) => AnalyticsDeliveryFailureClass::Transport,
            };
            Err(TelemetryPostError::new(
                class,
                anyhow!("POST {endpoint}: {error}"),
            ))
        }
    }
}

pub fn get_bytes_limited(endpoint: &str, max_bytes: usize) -> Result<Vec<u8>> {
    if let Some(path) = file_url_path(endpoint)? {
        let file = fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
        return read_limited(file, max_bytes, &format!("read {}", path.display()));
    }
    require_https_or_localhost(endpoint)?;
    let response = ureq::get(endpoint)
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .map_err(|err| anyhow!("GET {endpoint}: {err}"))?;
    read_limited(
        response.into_reader(),
        max_bytes,
        &format!("GET {endpoint}"),
    )
}

pub(crate) fn download_artifact(
    endpoint: &str,
    output: &mut fs::File,
    max_bytes: u64,
    timeout: Duration,
) -> Result<u64> {
    if max_bytes == 0 {
        return Err(anyhow!("artifact max bytes must be greater than zero"));
    }
    if output.metadata()?.len() != 0 || output.stream_position()? != 0 {
        return Err(anyhow!("artifact destination must be a new empty file"));
    }
    let started = Instant::now();
    if let Some(path) = file_url_path(endpoint)? {
        let input =
            fs::File::open(&path).with_context(|| format!("open artifact {}", path.display()))?;
        reject_oversized_length(
            input.metadata()?.len(),
            max_bytes,
            &format!("artifact {}", path.display()),
        )?;
        return copy_artifact_limited(
            input,
            output,
            max_bytes,
            timeout,
            started,
            &format!("artifact {}", path.display()),
        );
    }

    let response = get_artifact_response(endpoint, timeout, started)?;
    if let Some(length) = response.header("content-length") {
        let length = length
            .parse::<u64>()
            .map_err(|_| anyhow!("artifact response has an invalid Content-Length"))?;
        reject_oversized_length(length, max_bytes, "artifact response")?;
    }
    copy_artifact_limited(
        response.into_reader(),
        output,
        max_bytes,
        timeout,
        started,
        "artifact response",
    )
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
pub(crate) fn get_to_writer_limited(
    endpoint: &str,
    max_bytes: u64,
    timeout: Duration,
    writer: &mut impl Write,
) -> Result<u64> {
    let started = Instant::now();
    if let Some(path) = file_url_path(endpoint)? {
        let file = fs::File::open(&path).with_context(|| format!("read {}", path.display()))?;
        return copy_limited(
            file,
            writer,
            max_bytes,
            timeout,
            started,
            "read local artifact",
        );
    }
    require_https_or_localhost(endpoint)?;
    let response = ureq::get(endpoint)
        .timeout(timeout)
        .call()
        .map_err(|err| anyhow!("GET artifact: {err}"))?;
    if response
        .header("content-length")
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > max_bytes)
    {
        return Err(anyhow!("GET artifact exceeds max bytes ({max_bytes})"));
    }
    copy_limited(
        response.into_reader(),
        writer,
        max_bytes,
        timeout,
        started,
        "GET artifact",
    )
}

#[cfg_attr(not(any(target_os = "macos", test)), allow(dead_code))]
fn copy_limited(
    mut reader: impl Read,
    writer: &mut impl Write,
    max_bytes: u64,
    timeout: Duration,
    started: Instant,
    label: &str,
) -> Result<u64> {
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!("{label} exceeded time limit"));
        }
        let count = reader
            .read(&mut buffer)
            .with_context(|| format!("{label}: read response"))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or_else(|| anyhow!("{label} size overflow"))?;
        if total > max_bytes {
            return Err(anyhow!("{label} exceeds max bytes ({max_bytes})"));
        }
        writer
            .write_all(&buffer[..count])
            .with_context(|| format!("{label}: write destination"))?;
    }
    Ok(total)
}

fn get_artifact_response(
    endpoint: &str,
    timeout: Duration,
    started: Instant,
) -> Result<ureq::Response> {
    let mut current = Url::parse(endpoint).map_err(|_| anyhow!("invalid artifact URL"))?;
    validate_artifact_target(&current)?;
    let agent = artifact_agent(timeout);

    for redirects in 0..=MAX_ARTIFACT_REDIRECTS {
        let remaining = remaining_timeout(timeout, started, "GET artifact")?;
        let response = agent
            .get(current.as_str())
            .set("accept-encoding", "identity")
            .timeout(remaining)
            .call()
            .map_err(|error| anyhow!("GET artifact: {error}"))?;
        if !matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        if redirects == MAX_ARTIFACT_REDIRECTS {
            return Err(anyhow!(
                "GET artifact exceeded {MAX_ARTIFACT_REDIRECTS} redirects"
            ));
        }
        let location = response
            .header("location")
            .ok_or_else(|| anyhow!("artifact redirect omitted Location"))?;
        let next = current
            .join(location)
            .map_err(|_| anyhow!("artifact redirect has an invalid Location"))?;
        validate_artifact_redirect(&current, &next)?;
        current = next;
    }
    unreachable!("bounded artifact redirect loop")
}

fn reject_oversized_length(length: u64, max_bytes: u64, label: &str) -> Result<()> {
    if length > max_bytes {
        return Err(anyhow!("{label} exceeds max bytes ({max_bytes})"));
    }
    Ok(())
}

fn copy_artifact_limited(
    input: impl Read,
    output: &mut fs::File,
    max_bytes: u64,
    timeout: Duration,
    started: Instant,
    label: &str,
) -> Result<u64> {
    let total = copy_limited(input, output, max_bytes, timeout, started, label)?;
    output.flush().with_context(|| format!("flush {label}"))?;
    Ok(total)
}

fn remaining_timeout(timeout: Duration, started: Instant, label: &str) -> Result<Duration> {
    timeout
        .checked_sub(started.elapsed())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| anyhow!("{label} exceeded time limit"))
}

fn artifact_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .timeout(timeout)
        .build()
}

fn validate_artifact_redirect(current: &Url, next: &Url) -> Result<()> {
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(anyhow!("refusing artifact redirect HTTPS downgrade"));
    }
    validate_artifact_target(next)
}

fn validate_artifact_target(url: &Url) -> Result<()> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!("artifact URL must not contain credentials"));
    }
    let host = url
        .host()
        .ok_or_else(|| anyhow!("artifact URL must contain a host"))?;
    if url.scheme() != "https" {
        return Err(anyhow!("artifact URL must use HTTPS"));
    }
    if !is_public_host(host) {
        return Err(anyhow!("refusing private or local artifact network target"));
    }
    Ok(())
}

fn is_public_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => {
            let domain = domain.trim_end_matches('.').to_ascii_lowercase();
            domain.contains('.')
                && ![
                    "localhost",
                    "local",
                    "localdomain",
                    "internal",
                    "home",
                    "lan",
                ]
                .iter()
                .any(|suffix| domain == *suffix || domain.ends_with(&format!(".{suffix}")))
        }
        Host::Ipv4(address) => is_public_ipv4(address),
        Host::Ipv6(address) => is_public_ipv6(address),
    }
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [first, second, third, _] = address.octets();
    !matches!(
        (first, second, third),
        (0, _, _)
            | (10, _, _)
            | (100, 64..=127, _)
            | (127, _, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 0, 0)
            | (192, 0, 2)
            | (192, 168, _)
            | (198, 18..=19, _)
            | (198, 51, 100)
            | (203, 0, 113)
            | (224..=255, _, _)
    )
}

fn is_public_ipv6(address: std::net::Ipv6Addr) -> bool {
    let segments = address.segments();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        || address.is_unique_local()
        || address.is_unicast_link_local()
        || address
            .to_ipv4_mapped()
            .is_some_and(|mapped| !is_public_ipv4(mapped))
        || segments[0] == 0x2001 && segments[1] == 0x0db8)
}

pub(crate) fn read_limited(
    mut reader: impl Read,
    max_bytes: usize,
    label: &str,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take((max_bytes as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| anyhow!("{label}: {err}"))?;
    if bytes.len() > max_bytes {
        return Err(anyhow!("{label} exceeds max bytes ({max_bytes})"));
    }
    Ok(bytes)
}

pub(crate) fn file_url_path(url: &str) -> Result<Option<PathBuf>> {
    if !url.starts_with("file:") {
        return Ok(None);
    }
    let Some(raw_path) = url.strip_prefix("file://") else {
        return Err(anyhow!("file URL must use an absolute local path: {url}"));
    };
    let parsed = Url::parse(url).map_err(|_| anyhow!("invalid file URL: {url}"))?;
    if parsed.scheme() != "file"
        || parsed.host().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || raw_path.is_empty()
    {
        return Err(anyhow!("file URL must use an absolute local path: {url}"));
    }
    parsed
        .to_file_path()
        .map(Some)
        .map_err(|_| anyhow!("file URL must use an absolute local path: {url}"))
}

pub(crate) fn require_https_or_localhost(url: &str) -> Result<()> {
    let parsed = Url::parse(url).map_err(|_| anyhow!("invalid endpoint URL"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(anyhow!("endpoint URL must not contain credentials"));
    }
    if parsed.host().is_none() {
        return Err(anyhow!("endpoint URL must contain a host"));
    }
    if parsed.scheme() == "https" {
        return Ok(());
    }
    if parsed.scheme() == "http" && parsed.host().is_some_and(is_localhost_host) {
        return Ok(());
    }
    Err(anyhow!(
        "refusing non-HTTPS endpoint; use HTTPS or localhost HTTP"
    ))
}

fn is_localhost_host(host: Host<&str>) -> bool {
    match host {
        Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
        Host::Ipv4(address) => address.is_loopback(),
        Host::Ipv6(address) => address.is_loopback(),
    }
}

#[cfg(test)]
mod tests {
    use std::{net::TcpListener, sync::mpsc, thread};

    use super::*;

    #[test]
    fn file_urls_must_be_absolute_local_paths() {
        assert_eq!(
            file_url_path("file:///tmp/ctx-release-metadata.env")
                .unwrap()
                .unwrap(),
            PathBuf::from("/tmp/ctx-release-metadata.env")
        );
        assert!(file_url_path("file://relative/path").is_err());
        assert!(file_url_path("file://").is_err());
        assert!(file_url_path("file:///tmp/release?query").is_err());
        assert!(file_url_path("file:///tmp/release#fragment").is_err());
        assert!(file_url_path("https://example.com").unwrap().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_file_urls_convert_drive_paths_to_native_paths() {
        assert_eq!(
            file_url_path("file:///C:/CtxVmRuns/release/ctx.exe")
                .unwrap()
                .unwrap(),
            PathBuf::from(r"C:\CtxVmRuns\release\ctx.exe")
        );
    }

    #[test]
    fn endpoint_validation_allows_https_and_localhost_http_only() {
        require_https_or_localhost("https://example.com/releases").unwrap();
        require_https_or_localhost("http://localhost:8080/events").unwrap();
        require_https_or_localhost("http://127.0.0.1/events").unwrap();
        require_https_or_localhost("http://[::1]:8080/events").unwrap();
        assert!(require_https_or_localhost("http://example.com/events").is_err());
        assert!(require_https_or_localhost("http://example.com@localhost/events").is_err());
        assert!(require_https_or_localhost("https://user@example.com/events").is_err());
        assert!(require_https_or_localhost("https://").is_err());
        assert_eq!(
            post_telemetry_json("http://example.com/events", b"{}")
                .unwrap_err()
                .class(),
            AnalyticsDeliveryFailureClass::Configuration
        );
    }

    #[test]
    fn artifact_target_validation_requires_public_https() {
        validate_artifact_target(&Url::parse("https://releases.example.com/file").unwrap())
            .unwrap();
        for endpoint in [
            "http://releases.example.com/file",
            "https://localhost/file",
            "https://127.0.0.1/file",
            "https://10.0.0.1/file",
            "https://198.18.0.23/file",
            "https://[::ffff:198.18.0.23]/file",
            "https://user@releases.example.com/file",
        ] {
            assert!(
                validate_artifact_target(&Url::parse(endpoint).unwrap()).is_err(),
                "{endpoint} should be rejected"
            );
        }
        assert!(validate_artifact_redirect(
            &Url::parse("https://releases.example.com/file").unwrap(),
            &Url::parse("http://releases.example.com/file").unwrap(),
        )
        .is_err());
    }

    #[test]
    fn interactive_telemetry_budget_stays_bounded() {
        assert_eq!(TELEMETRY_HTTP_TIMEOUT, Duration::from_millis(250));
    }

    #[test]
    fn background_daemon_telemetry_can_wait_for_durable_ingest() {
        assert_eq!(DAEMON_TELEMETRY_HTTP_TIMEOUT, Duration::from_secs(2));
    }

    #[test]
    fn telemetry_http_request_times_out_when_response_stalls() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (_stream, _) = listener.accept().unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        });

        let started = Instant::now();
        let result = post_telemetry_json(&format!("http://{address}/events"), b"{}");
        let elapsed = started.elapsed();
        release_tx.send(()).unwrap();
        server.join().unwrap();

        assert_eq!(
            result.unwrap_err().class(),
            AnalyticsDeliveryFailureClass::Transport
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "telemetry request took {elapsed:?}"
        );
    }

    #[test]
    fn get_bytes_limited_rejects_oversized_file_urls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("oversized.bin");
        fs::write(&path, b"12345").unwrap();
        let err = get_bytes_limited(&format!("file://{}", path.display()), 4).unwrap_err();
        assert!(err.to_string().contains("exceeds max bytes (4)"));
    }

    #[test]
    fn streaming_get_enforces_compressed_limit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("artifact.bin");
        fs::write(&path, b"12345").unwrap();
        let mut output = Vec::new();
        let error = get_to_writer_limited(
            &format!("file://{}", path.display()),
            4,
            Duration::from_secs(1),
            &mut output,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exceeds max bytes"));
    }

    #[test]
    fn artifact_stream_copies_bounded_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        let bytes = b"bounded artifact";
        fs::write(&source, bytes).unwrap();
        let mut output = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap();

        let written = download_artifact(
            &format!("file://{}", source.display()),
            &mut output,
            bytes.len() as u64,
            Duration::from_secs(1),
        )
        .unwrap();

        assert_eq!(written, bytes.len() as u64);
        assert_eq!(fs::read(destination).unwrap(), bytes);
    }

    #[test]
    fn artifact_stream_rejects_nonempty_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"artifact").unwrap();
        fs::write(&destination, b"existing").unwrap();
        let mut output = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&destination)
            .unwrap();
        let error = download_artifact(
            &format!("file://{}", source.display()),
            &mut output,
            1024,
            Duration::from_secs(1),
        )
        .unwrap_err();

        assert!(error.to_string().contains("new empty file"));
        assert_eq!(fs::read(destination).unwrap(), b"existing");
    }

    #[test]
    fn artifact_stream_rejects_oversized_source_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.bin");
        let destination = temp.path().join("destination.bin");
        fs::write(&source, b"oversized").unwrap();
        let mut output = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&destination)
            .unwrap();

        let error = download_artifact(
            &format!("file://{}", source.display()),
            &mut output,
            4,
            Duration::from_secs(1),
        )
        .unwrap_err();

        assert!(error.to_string().contains("exceeds max bytes (4)"));
        assert!(fs::read(destination).unwrap().is_empty());
    }
}
