use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn finalize_managed_artifact_download_tolerates_parallel_committers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let final_path = temp.path().join("sandbox-machine");
    let first_tmp = temp.path().join("sandbox-machine.download-first");
    let second_tmp = temp.path().join("sandbox-machine.download-second");
    let payload = b"shared-machine-cache";

    fs::write(&first_tmp, payload)
        .await
        .expect("write first tmp payload");
    fs::write(&second_tmp, payload)
        .await
        .expect("write second tmp payload");
    let expected_sha256 = sha256_hex_file(&first_tmp)
        .await
        .expect("compute tmp checksum");

    let (first, second) = tokio::join!(
        finalize_managed_artifact_download(
            &first_tmp,
            &final_path,
            &expected_sha256,
            "managed sandbox machine cache"
        ),
        finalize_managed_artifact_download(
            &second_tmp,
            &final_path,
            &expected_sha256,
            "managed sandbox machine cache"
        ),
    );

    first.expect("first finalization should succeed");
    second.expect("second finalization should succeed");
    assert!(
        verify_managed_artifact_checksum(&final_path, &expected_sha256)
            .await
            .expect("verify final checksum"),
        "final cache artifact should exist with the expected checksum"
    );
    assert!(
        !first_tmp.exists(),
        "first tmp path should be consumed during finalization"
    );
    assert!(
        !second_tmp.exists(),
        "second tmp path should be cleaned up during finalization"
    );
}

#[test]
fn resolve_managed_artifact_download_url_rewrites_non_avf_locked_scheme() {
    let resolved = resolve_managed_artifact_download_url_with_base(
        "locked://providers/codex/macos/aarch64",
        "https://api.ctx.rs/functions/v1/",
    )
    .expect("resolve locked uri");
    assert_eq!(
        resolved,
        "https://api.ctx.rs/functions/v1/providers/codex/macos/aarch64"
    );
}

#[test]
fn resolve_managed_artifact_download_url_rejects_empty_locked_path() {
    let err = resolve_managed_artifact_download_url_with_base(
        "locked://",
        "https://api.ctx.rs/functions/v1",
    )
    .expect_err("empty locked path should fail");
    assert!(format!("{err:#}").contains("missing a path"));
}

#[test]
fn resolve_managed_artifact_download_url_rejects_unresolved_avf_locked_scheme() {
    let err = resolve_managed_artifact_download_url_with_base(
        "locked://runtimes/avf-linux-guest/macos/aarch64/rootfs.raw.zst",
        "https://api.ctx.rs/functions/v1",
    )
    .expect_err("unresolved AVF locked path should fail");
    assert!(format!("{err:#}").contains("managed AVF runtime source is unresolved"));
}

async fn read_http_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let read = socket.read(&mut chunk).await.expect("read request");
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

fn parse_range_start(request: &str) -> Option<u64> {
    request.lines().find_map(|line| {
        let lower = line.to_ascii_lowercase();
        let value = lower.strip_prefix("range: bytes=")?;
        let (start, _) = value.split_once('-')?;
        start.trim().parse::<u64>().ok()
    })
}

async fn write_http_response(
    socket: &mut tokio::net::TcpStream,
    status_line: &str,
    extra_headers: &[String],
    body: &[u8],
) {
    let mut response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for header in extra_headers {
        response.push_str(header);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    socket
        .write_all(response.as_bytes())
        .await
        .expect("write response headers");
    if !body.is_empty() {
        socket.write_all(body).await.expect("write response body");
    }
    let _ = socket.shutdown().await;
}

#[tokio::test]
async fn download_managed_artifact_resumes_existing_partial_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dest = temp.path().join("artifact.partial");
    let payload = b"managed-artifact-payload".to_vec();
    let existing_len = 7usize;
    fs::write(&dest, &payload[..existing_len])
        .await
        .expect("seed partial file");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_for_server = Arc::clone(&requests);
    let payload_for_server = payload.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept connection");
        requests_for_server.fetch_add(1, Ordering::SeqCst);
        let request = read_http_request(&mut socket).await;
        assert_eq!(
            parse_range_start(&request),
            Some(existing_len as u64),
            "resume request should start from the on-disk partial length"
        );
        let remaining = &payload_for_server[existing_len..];
        write_http_response(
            &mut socket,
            "206 Partial Content",
            &[
                "Accept-Ranges: bytes".to_string(),
                format!(
                    "Content-Range: bytes {}-{}/{}",
                    existing_len,
                    payload_for_server.len() - 1,
                    payload_for_server.len()
                ),
            ],
            remaining,
        )
        .await;
    });

    download_managed_artifact(&format!("http://{addr}/artifact"), &dest, None)
        .await
        .expect("resume download should succeed");
    server.await.expect("server task");
    assert_eq!(
        fs::read(&dest).await.expect("read final payload"),
        payload,
        "download should append the remaining bytes to the stable partial file"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn download_managed_artifact_retries_with_range_resume_after_truncated_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dest = temp.path().join("artifact.partial");
    let payload = b"managed-artifact-retry-payload".to_vec();
    let first_chunk_len = 9usize;

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let requests = Arc::new(AtomicUsize::new(0));
    let requests_for_server = Arc::clone(&requests);
    let payload_for_server = payload.clone();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut socket, _) = listener.accept().await.expect("accept connection");
            let request_index = requests_for_server.fetch_add(1, Ordering::SeqCst);
            let request = read_http_request(&mut socket).await;
            if request_index == 0 {
                assert!(
                    parse_range_start(&request).is_none(),
                    "first request should start from byte 0"
                );
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload_for_server.len()
                );
                socket
                    .write_all(headers.as_bytes())
                    .await
                    .expect("write first response headers");
                socket
                    .write_all(&payload_for_server[..first_chunk_len])
                    .await
                    .expect("write first truncated chunk");
                let _ = socket.shutdown().await;
            } else {
                assert_eq!(
                    parse_range_start(&request),
                    Some(first_chunk_len as u64),
                    "retry should resume from the previously written partial bytes"
                );
                let remaining = &payload_for_server[first_chunk_len..];
                write_http_response(
                    &mut socket,
                    "206 Partial Content",
                    &[
                        "Accept-Ranges: bytes".to_string(),
                        format!(
                            "Content-Range: bytes {}-{}/{}",
                            first_chunk_len,
                            payload_for_server.len() - 1,
                            payload_for_server.len()
                        ),
                    ],
                    remaining,
                )
                .await;
            }
        }
    });

    download_managed_artifact(&format!("http://{addr}/artifact"), &dest, None)
        .await
        .expect("retrying download should succeed");
    server.await.expect("server task");
    assert_eq!(
        fs::read(&dest).await.expect("read completed payload"),
        payload,
        "retry path should preserve the first partial bytes and resume with HTTP Range"
    );
    assert_eq!(requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn download_managed_artifact_fails_disk_preflight_before_writing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dest = temp.path().join("artifact.partial");
    let available = fs2::available_space(temp.path()).expect("available space");

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept connection");
        let _request = read_http_request(&mut socket).await;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {available}\r\nConnection: close\r\n\r\n",
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write oversized response");
        let _ = socket.shutdown().await;
    });

    let err = download_managed_artifact(&format!("http://{addr}/artifact"), &dest, None)
        .await
        .expect_err("disk-space preflight should fail before any writes");
    server.await.expect("server task");
    assert!(
        format!("{err:#}").contains("insufficient disk space"),
        "expected a clear disk-space error, got: {err:#}"
    );
    assert!(
        !dest.exists(),
        "preflight failure should happen before the download target is created"
    );
}

#[tokio::test]
async fn managed_artifact_file_lock_waits_for_existing_holder() {
    let temp = tempfile::tempdir().expect("tempdir");
    let lock_path = temp.path().join("artifact.lock");
    let first = acquire_managed_artifact_file_lock(
        &lock_path,
        "test artifact",
        None,
        HarnessSetupPhase::ArtifactDownload,
    )
    .await
    .expect("acquire first lock");

    let acquired = Arc::new(AtomicBool::new(false));
    let acquired_for_waiter = Arc::clone(&acquired);
    let lock_path_for_waiter = lock_path.clone();
    let waiter = tokio::spawn(async move {
        let _second = acquire_managed_artifact_file_lock(
            &lock_path_for_waiter,
            "test artifact",
            None,
            HarnessSetupPhase::ArtifactDownload,
        )
        .await
        .expect("acquire second lock");
        acquired_for_waiter.store(true, Ordering::SeqCst);
    });

    tokio::time::sleep(Duration::from_millis(40)).await;
    assert!(
        !acquired.load(Ordering::SeqCst),
        "second file lock should wait while the first holder is alive"
    );

    drop(first);
    tokio::time::timeout(Duration::from_secs(1), waiter)
        .await
        .expect("second lock should eventually acquire")
        .expect("waiter should finish cleanly");
    assert!(acquired.load(Ordering::SeqCst));
}
