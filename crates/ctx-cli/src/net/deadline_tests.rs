use super::tests::consume_http_request;
use super::*;
use std::{net::TcpListener, sync::mpsc, thread};

#[test]
fn telemetry_dns_resolution_obeys_the_complete_request_deadline() {
    let started = Instant::now();
    let error = resolve_with_timeout("blocked.example:443", Duration::from_millis(20), |_| {
        thread::sleep(Duration::from_secs(1));
        Ok(Vec::new())
    })
    .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[test]
fn permanent_rejection_does_not_become_retryable_when_its_body_stalls() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        consume_http_request(&mut stream);
        stream
            .write_all(b"HTTP/1.1 422 Rejected\r\nContent-Length: 4\r\nConnection: close\r\n\r\nx")
            .unwrap();
        stream.flush().unwrap();
        release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    });

    let error = post_telemetry_json_with_timeout(
        &format!("http://{address}/events"),
        b"{}",
        Duration::from_millis(100),
    )
    .unwrap_err();
    release_tx.send(()).unwrap();
    server.join().unwrap();

    assert_eq!(
        error.class(),
        AnalyticsDeliveryFailureClass::ClientRejection
    );
    assert!(!error.retryable());
}
