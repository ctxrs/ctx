use super::*;

#[test]
fn transport_reasons_use_ureq_and_io_types_not_messages() {
    use std::io::{Error, ErrorKind};
    for (kind, reason) in [
        (
            ErrorKind::TimedOut,
            AnalyticsDeliveryFailureReason::RequestTimeout,
        ),
        (
            ErrorKind::ConnectionReset,
            AnalyticsDeliveryFailureReason::RequestIo,
        ),
    ] {
        let error: ureq::Error = Error::new(kind, "DNS TLS /private token=secret").into();
        assert_eq!(telemetry_transport_reason(&error), Some(reason));
    }
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .resolver(|_: &str| Err(Error::new(ErrorKind::TimedOut, "private resolver")))
        .build();
    let error = agent.get("http://fixture.invalid/").call().unwrap_err();
    assert_eq!(
        telemetry_transport_reason(&error),
        Some(AnalyticsDeliveryFailureReason::RequestDns)
    );
    // TCP port zero cannot have a listening service; no other local endpoint is touched.
    let error = ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .build()
        .get("http://127.0.0.1:0/")
        .timeout(Duration::from_millis(250))
        .call()
        .unwrap_err();
    assert_eq!(
        telemetry_transport_reason(&error),
        Some(AnalyticsDeliveryFailureReason::RequestConnect)
    );
    assert_eq!(
        telemetry_status_error("http://localhost/private", 408, None)
            .unwrap()
            .reason(),
        Some(AnalyticsDeliveryFailureReason::ResponseStatus408)
    );
    for status in [200, 302, 400, 429, 500] {
        assert!(
            telemetry_status_error("http://localhost/private", status, None)
                .and_then(|error| error.reason())
                .is_none()
        );
    }
}

struct FailingWriter(bool);
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0 {
            Ok(bytes.len())
        } else {
            Err(std::io::ErrorKind::StorageFull.into())
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::PermissionDenied.into())
    }
}

#[test]
fn file_operations_preserve_the_actual_failing_call() {
    let temp = tempfile::tempdir().unwrap();
    let endpoint = Url::from_file_path(temp.path().join("missing/fixture")).unwrap();
    let error = post_telemetry_json_with_timeout(endpoint.as_str(), b"{}", Duration::from_secs(1))
        .unwrap_err();
    assert_eq!(
        error.reason(),
        Some(AnalyticsDeliveryFailureReason::FileOpen)
    );
    for (writer, reason) in [
        (false, AnalyticsDeliveryFailureReason::FileWrite),
        (true, AnalyticsDeliveryFailureReason::FileFlush),
    ] {
        let error = write_telemetry_file(&mut FailingWriter(writer), b"{}").unwrap_err();
        assert_eq!(error.reason(), Some(reason));
        assert!(error.retryable());
        assert_eq!(error.class(), AnalyticsDeliveryFailureClass::LocalIo);
    }
}

#[test]
fn response_body_timeout_and_truncation_are_not_request_errors() {
    for timeout in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0; 4096];
            assert!(stream.read(&mut buffer).unwrap() > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nx")
                .unwrap();
            if timeout {
                std::thread::sleep(Duration::from_millis(500));
            }
        });
        let error = post_telemetry_json_with_timeout(&endpoint, b"{}", Duration::from_millis(200))
            .unwrap_err();
        assert_eq!(
            error.reason(),
            Some(if timeout {
                AnalyticsDeliveryFailureReason::ResponseBodyTimeout
            } else {
                AnalyticsDeliveryFailureReason::ResponseBodyIo
            })
        );
        assert!(error.retryable());
        peer.join().unwrap();
    }
}
