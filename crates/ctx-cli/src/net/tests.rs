use std::{
    io::{Read as _, Write as _},
    net::TcpListener,
    sync::mpsc,
    thread,
};

use super::*;

pub(super) fn consume_http_request(stream: &mut std::net::TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count != 0, "client closed before completing its request");
        request.extend_from_slice(&buffer[..count]);
        assert!(request.len() <= 64 * 1024, "test request exceeded bound");
        let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") else {
            continue;
        };
        let body_start = header_end + 4;
        let headers = std::str::from_utf8(&request[..header_end]).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        if request.len() >= body_start + content_length {
            return;
        }
    }
}

fn telemetry_response(
    status: u16,
    retry_after: Option<&str>,
) -> std::result::Result<(), TelemetryPostError> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let retry_after = retry_after
        .map(|value| format!("Retry-After: {value}\r\n"))
        .unwrap_or_default();
    let location = if (300..400).contains(&status) {
        "Location: http://127.0.0.1/elsewhere\r\n"
    } else {
        ""
    };
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        consume_http_request(&mut stream);
        write!(
            stream,
            "HTTP/1.1 {status} Test\r\nContent-Length: 0\r\n{retry_after}{location}Connection: close\r\n\r\n"
        )
        .unwrap();
        stream.flush().unwrap();
    });
    let result = post_telemetry_json_with_timeout_at(
        &format!("http://{address}/events"),
        b"{}",
        Duration::from_secs(5),
        1_800_000_000,
    );
    server.join().unwrap();
    result
}

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
    assert!(!post_telemetry_json("http://example.com/events", b"{}")
        .unwrap_err()
        .retryable());
}

#[test]
fn telemetry_statuses_distinguish_retryable_and_permanent_rejections() {
    assert!(telemetry_response(204, None).is_ok());
    for (status, class) in [
        (408, AnalyticsDeliveryFailureClass::Transport),
        (429, AnalyticsDeliveryFailureClass::RateLimited),
        (503, AnalyticsDeliveryFailureClass::Server),
    ] {
        let error = telemetry_response(status, None).unwrap_err();
        assert_eq!(error.class(), class, "HTTP {status}");
        assert!(error.retryable(), "HTTP {status}");
    }
    for status in [302, 400, 401, 422] {
        let error = telemetry_response(status, None).unwrap_err();
        assert!(!error.retryable(), "HTTP {status}");
    }
}

#[test]
fn retry_after_accepts_delta_and_http_date_and_clamps_both() {
    assert_eq!(
        parse_retry_after("120", 1_800_000_000),
        Some(Duration::from_secs(120))
    );
    assert_eq!(
        parse_retry_after("999999999999", 1_800_000_000),
        Some(TELEMETRY_MAX_RETRY_AFTER)
    );
    let retry_at = chrono::DateTime::parse_from_rfc2822("Wed, 02 Sep 2026 12:01:00 GMT")
        .unwrap()
        .timestamp();
    assert_eq!(
        parse_retry_after("Wed, 02 Sep 2026 12:01:00 GMT", retry_at - 60),
        Some(Duration::from_secs(60))
    );
    assert_eq!(
        parse_retry_after("Wed, 02 Sep 2026 12:01:00 GMT", retry_at + 1),
        Some(Duration::ZERO)
    );
    assert_eq!(parse_retry_after("not-a-date", 1_800_000_000), None);

    let error = telemetry_response(429, Some("120")).unwrap_err();
    assert_eq!(error.retry_after(), Some(Duration::from_secs(120)));
}

#[test]
fn artifact_target_validation_requires_https_without_classifying_routing() {
    for endpoint in [
        "https://releases.example.com/file",
        "https://localhost/file",
        "https://127.0.0.1/file",
        "https://10.0.0.1/file",
        "https://198.18.0.23/file",
        "https://[::ffff:198.18.0.23]/file",
        "https://releases.internal/file",
    ] {
        validate_artifact_target(&Url::parse(endpoint).unwrap())
            .unwrap_or_else(|error| panic!("{endpoint} should use platform routing: {error}"));
    }
    for endpoint in [
        "http://releases.example.com/file",
        "https://user@releases.example.com/file",
    ] {
        assert!(
            validate_artifact_target(&Url::parse(endpoint).unwrap()).is_err(),
            "{endpoint} should be rejected"
        );
    }
}

#[test]
fn artifact_redirects_remain_on_the_signed_url_origin() {
    let authority = Url::parse("https://releases.example.com/artifacts/ctx").unwrap();
    for endpoint in [
        "https://releases.example.com/artifacts/ctx-next",
        "https://releases.example.com:443/artifacts/ctx-next",
    ] {
        validate_artifact_redirect(&authority, &Url::parse(endpoint).unwrap())
            .unwrap_or_else(|error| panic!("{endpoint} should be same-origin: {error}"));
    }

    for endpoint in [
        "https://downloads.example.com/artifacts/ctx",
        "https://releases.example.com:444/artifacts/ctx",
    ] {
        let error =
            validate_artifact_redirect(&authority, &Url::parse(endpoint).unwrap()).unwrap_err();
        assert!(error.to_string().contains("different origin"), "{endpoint}");
    }

    assert!(validate_artifact_redirect(
        &authority,
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
fn telemetry_deadline_includes_response_body_consumption() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nx")
            .unwrap();
        stream.flush().unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    });

    let started = Instant::now();
    let error = post_telemetry_json_with_timeout(
        &format!("http://{address}/events"),
        b"{}",
        Duration::from_millis(100),
    )
    .unwrap_err();
    let elapsed = started.elapsed();
    release_tx.send(()).unwrap();
    server.join().unwrap();

    assert_eq!(error.class(), AnalyticsDeliveryFailureClass::Transport);
    assert!(error.retryable());
    assert!(elapsed < Duration::from_secs(1), "request took {elapsed:?}");
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
