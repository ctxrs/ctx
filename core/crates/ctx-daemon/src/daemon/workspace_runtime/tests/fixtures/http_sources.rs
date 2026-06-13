use super::*;

pub(in crate::daemon::workspace_runtime::tests) async fn spawn_static_http_server(
    body: Vec<u8>,
) -> (String, JoinHandle<()>) {
    spawn_static_http_server_with_suffix(body, "image.tar").await
}

pub(in crate::daemon::workspace_runtime::tests) async fn spawn_static_http_server_with_suffix(
    body: Vec<u8>,
    suffix: &'static str,
) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local http listener");
    let addr = listener.local_addr().expect("listener local addr");
    let shared = Arc::new(body);
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            let payload = Arc::clone(&shared);
            tokio::spawn(async move {
                let mut req_buf = [0u8; 1024];
                let _ = socket.read(&mut req_buf).await;
                let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    );
                let _ = socket.write_all(headers.as_bytes()).await;
                let _ = socket.write_all(payload.as_slice()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (format!("http://{addr}/{suffix}"), task)
}

pub(in crate::daemon::workspace_runtime::tests) async fn install_test_managed_machine_cache_source(
    body: Vec<u8>,
) -> (TestManagedSandboxMachineCacheSourceGuard, JoinHandle<()>) {
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&body);
        hex::encode(hasher.finalize())
    };
    let (url, server) = spawn_static_http_server(body).await;
    let guard = override_managed_sandbox_machine_cache_source_for_test(
        bundled_assets::ManagedArtifactSource {
            uri: url,
            sha256: digest,
        },
    );
    (guard, server)
}

pub(in crate::daemon::workspace_runtime::tests) async fn install_test_managed_harness_image_source(
    body: Vec<u8>,
) -> (TestManagedCtxHarnessImageSourceGuard, JoinHandle<()>) {
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&body);
        hex::encode(hasher.finalize())
    };
    let (url, server) = spawn_static_http_server_with_suffix(body, "ctx-harness.tar").await;
    let guard =
        override_managed_ctx_harness_image_source_for_test(bundled_assets::ManagedArtifactSource {
            uri: url,
            sha256: digest,
        });
    (guard, server)
}
