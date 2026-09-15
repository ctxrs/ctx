use std::{
    fs,
    fs::OpenOptions,
    io::{Read, Seek, Write},
    net::{SocketAddr, ToSocketAddrs as _},
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use url::{Host, Url};

use crate::analytics::{AnalyticsDeliveryFailureClass, AnalyticsDeliveryFailureReason};

#[cfg(test)]
const TELEMETRY_HTTP_TIMEOUT: Duration = Duration::from_millis(250);
pub(crate) const DAEMON_TELEMETRY_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
const TELEMETRY_MAX_RETRY_AFTER: Duration = Duration::from_secs(60 * 60);
const MAX_ARTIFACT_REDIRECTS: usize = 5;

#[derive(Clone, Copy)]
struct DeadlineResolver {
    timeout: Duration,
}

impl ureq::Resolver for DeadlineResolver {
    fn resolve(&self, netloc: &str) -> std::io::Result<Vec<SocketAddr>> {
        resolve_with_timeout(netloc, self.timeout, |netloc| {
            netloc.to_socket_addrs().map(Iterator::collect)
        })
    }
}

fn resolve_with_timeout<F>(
    netloc: &str,
    timeout: Duration,
    resolve: F,
) -> std::io::Result<Vec<SocketAddr>>
where
    F: FnOnce(&str) -> std::io::Result<Vec<SocketAddr>> + Send + 'static,
{
    let netloc = netloc.to_owned();
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("ctx-telemetry-dns".to_owned())
        .spawn(move || {
            let result = resolve(&netloc);
            let _ = send.send(result);
        })
        .map_err(|error| std::io::Error::other(format!("start telemetry resolver: {error}")))?;
    receive.recv_timeout(timeout).map_err(|error| match error {
        std::sync::mpsc::RecvTimeoutError::Timeout => std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "telemetry DNS resolution timed out",
        ),
        std::sync::mpsc::RecvTimeoutError::Disconnected => {
            std::io::Error::other("telemetry DNS resolver stopped without a result")
        }
    })?
}

#[derive(Debug)]
pub(crate) struct TelemetryPostError {
    class: AnalyticsDeliveryFailureClass,
    retryable: bool,
    retry_after: Option<Duration>,
    source: anyhow::Error,
    reason: Option<AnalyticsDeliveryFailureReason>,
}

impl TelemetryPostError {
    pub(crate) fn class(&self) -> AnalyticsDeliveryFailureClass {
        self.class
    }

    pub(crate) fn reason(&self) -> Option<AnalyticsDeliveryFailureReason> {
        self.reason
    }

    fn with_reason(mut self, reason: Option<AnalyticsDeliveryFailureReason>) -> Self {
        self.reason = reason.filter(|reason| reason.permits(self.class));
        self
    }

    pub(crate) fn retryable(&self) -> bool {
        self.retryable
    }

    pub(crate) fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    fn permanent(class: AnalyticsDeliveryFailureClass, source: impl Into<anyhow::Error>) -> Self {
        Self {
            class,
            retryable: false,
            retry_after: None,
            source: source.into(),
            reason: None,
        }
    }

    fn retry(
        class: AnalyticsDeliveryFailureClass,
        retry_after: Option<Duration>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            class,
            retryable: true,
            retry_after,
            source: source.into(),
            reason: None,
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
    post_telemetry_json_with_timeout_at(
        endpoint,
        body,
        timeout,
        ctx_history_core::utc_now().timestamp(),
    )
}

fn post_telemetry_json_with_timeout_at(
    endpoint: &str,
    body: &[u8],
    timeout: Duration,
    now_epoch_seconds: i64,
) -> std::result::Result<(), TelemetryPostError> {
    let file_path = file_url_path(endpoint).map_err(|error| {
        TelemetryPostError::permanent(AnalyticsDeliveryFailureClass::Configuration, error)
    })?;
    if let Some(path) = file_path {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("open {}", path.display()))
            .map_err(|error| {
                TelemetryPostError::retry(AnalyticsDeliveryFailureClass::LocalIo, None, error)
                    .with_reason(Some(AnalyticsDeliveryFailureReason::FileOpen))
            })?;
        write_telemetry_file(&mut file, body)?;
        return Ok(());
    }
    require_https_or_localhost(endpoint).map_err(|error| {
        TelemetryPostError::permanent(AnalyticsDeliveryFailureClass::Configuration, error)
    })?;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .resolver(DeadlineResolver { timeout })
        .build();
    let response = match agent
        .post(endpoint)
        // ureq applies this overall deadline to connection establishment too
        // and carries the same deadline into response-body reads.
        .timeout(timeout)
        .set("content-type", "application/json")
        .send_bytes(body)
    {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(error @ ureq::Error::Transport(_)) => {
            return Err(TelemetryPostError::retry(
                AnalyticsDeliveryFailureClass::Transport,
                None,
                anyhow!("POST {endpoint}: {error}"),
            )
            .with_reason(telemetry_transport_reason(&error)));
        }
    };
    let status = response.status();
    let retry_after = response
        .header("retry-after")
        .and_then(|value| parse_retry_after(value, now_epoch_seconds));
    if let Some(error) = telemetry_status_error(endpoint, status, retry_after) {
        return Err(error);
    }
    consume_telemetry_response(response).map_err(|error| {
        TelemetryPostError::retry(
            AnalyticsDeliveryFailureClass::Transport,
            retry_after,
            anyhow!("POST {endpoint}: consume response body: {error}"),
        )
        .with_reason(Some(if error.kind() == std::io::ErrorKind::TimedOut {
            AnalyticsDeliveryFailureReason::ResponseBodyTimeout
        } else {
            AnalyticsDeliveryFailureReason::ResponseBodyIo
        }))
    })?;
    Ok(())
}

fn write_telemetry_file(
    file: &mut impl Write,
    body: &[u8],
) -> std::result::Result<(), TelemetryPostError> {
    for bytes in [body, b"\n"] {
        file.write_all(bytes).map_err(|error| {
            TelemetryPostError::retry(AnalyticsDeliveryFailureClass::LocalIo, None, error)
                .with_reason(Some(AnalyticsDeliveryFailureReason::FileWrite))
        })?;
    }
    file.flush().map_err(|error| {
        TelemetryPostError::retry(AnalyticsDeliveryFailureClass::LocalIo, None, error)
            .with_reason(Some(AnalyticsDeliveryFailureReason::FileFlush))
    })
}

fn telemetry_transport_reason(error: &ureq::Error) -> Option<AnalyticsDeliveryFailureReason> {
    use AnalyticsDeliveryFailureReason as Reason;
    match error.kind() {
        ureq::ErrorKind::Dns => Some(Reason::RequestDns),
        ureq::ErrorKind::ConnectionFailed => Some(Reason::RequestConnect),
        ureq::ErrorKind::Io => {
            let mut cause = std::error::Error::source(error);
            while let Some(error) = cause {
                if let Some(error) = error.downcast_ref::<std::io::Error>() {
                    return Some(if error.kind() == std::io::ErrorKind::TimedOut {
                        Reason::RequestTimeout
                    } else {
                        Reason::RequestIo
                    });
                }
                cause = error.source();
            }
            None
        }
        _ => None,
    }
}

fn telemetry_status_error(
    endpoint: &str,
    status: u16,
    retry_after: Option<Duration>,
) -> Option<TelemetryPostError> {
    match status {
        200..=299 => None,
        408 => Some(
            TelemetryPostError::retry(
                AnalyticsDeliveryFailureClass::Transport,
                retry_after,
                anyhow!("POST {endpoint}: HTTP {status}"),
            )
            .with_reason(Some(AnalyticsDeliveryFailureReason::ResponseStatus408)),
        ),
        429 => Some(TelemetryPostError::retry(
            AnalyticsDeliveryFailureClass::RateLimited,
            retry_after,
            anyhow!("POST {endpoint}: HTTP {status}"),
        )),
        500..=599 => Some(TelemetryPostError::retry(
            AnalyticsDeliveryFailureClass::Server,
            retry_after,
            anyhow!("POST {endpoint}: HTTP {status}"),
        )),
        400..=499 => Some(TelemetryPostError::permanent(
            AnalyticsDeliveryFailureClass::ClientRejection,
            anyhow!("POST {endpoint}: HTTP {status}"),
        )),
        _ => Some(TelemetryPostError::permanent(
            AnalyticsDeliveryFailureClass::Unknown,
            anyhow!("POST {endpoint}: HTTP {status}"),
        )),
    }
}

fn consume_telemetry_response(response: ureq::Response) -> std::io::Result<()> {
    let mut reader = response.into_reader();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        if reader.read(&mut buffer)? == 0 {
            return Ok(());
        }
    }
}

fn parse_retry_after(value: &str, now_epoch_seconds: i64) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(
            seconds.min(TELEMETRY_MAX_RETRY_AFTER.as_secs()),
        ));
    }
    let retry_at = chrono::DateTime::parse_from_rfc2822(value)
        .ok()?
        .timestamp();
    let seconds = retry_at.saturating_sub(now_epoch_seconds).max(0) as u64;
    Some(Duration::from_secs(
        seconds.min(TELEMETRY_MAX_RETRY_AFTER.as_secs()),
    ))
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
    let authority = Url::parse(endpoint).map_err(|_| anyhow!("invalid artifact URL"))?;
    validate_artifact_target(&authority)?;
    let mut current = authority.clone();
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
        validate_artifact_redirect(&authority, &next)?;
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

fn validate_artifact_redirect(authority: &Url, next: &Url) -> Result<()> {
    if next.scheme() != "https" {
        return Err(anyhow!("refusing artifact redirect HTTPS downgrade"));
    }
    validate_artifact_target(next)?;
    if authority.origin() != next.origin() {
        return Err(anyhow!("refusing artifact redirect to a different origin"));
    }
    Ok(())
}

fn validate_artifact_target(url: &Url) -> Result<()> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(anyhow!("artifact URL must not contain credentials"));
    }
    if url.host().is_none() {
        return Err(anyhow!("artifact URL must contain a host"));
    }
    if url.scheme() != "https" {
        return Err(anyhow!("artifact URL must use HTTPS"));
    }
    Ok(())
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
mod diagnostics_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "net/deadline_tests.rs"]
mod deadline_tests;
