use super::fixtures::*;
use super::*;

#[tokio::test]
async fn managed_default_image_install_lock_serializes_callers() {
    let lock = managed_default_image_install_lock();
    let guard = lock.lock().await;
    let acquired = Arc::new(AtomicBool::new(false));
    let acquired_clone = Arc::clone(&acquired);

    let waiter = tokio::spawn(async move {
        let _wait_guard = lock.lock().await;
        acquired_clone.store(true, Ordering::SeqCst);
    });

    sleep(Duration::from_millis(30)).await;
    assert!(
        !acquired.load(Ordering::SeqCst),
        "second caller should still be blocked while first holds the lock"
    );
    drop(guard);

    waiter.await.expect("waiter task");
    assert!(
        acquired.load(Ordering::SeqCst),
        "second caller should acquire lock after first releases it"
    );
}

#[tokio::test]
async fn managed_default_image_ensure_is_concurrency_safe() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let body = b"ctx-test-managed-image".to_vec();
    let digest = {
        let mut hasher = Sha256::new();
        hasher.update(&body);
        hex::encode(hasher.finalize())
    };
    let (url, server) = spawn_static_http_server(body.clone()).await;
    let source = bundled_assets::ManagedArtifactSource {
        uri: url,
        sha256: digest,
    };
    let data_root = tmp.path().to_path_buf();

    let root_a = data_root.clone();
    let root_b = data_root.clone();
    let source_a = source.clone();
    let source_b = source.clone();
    let (res_a, res_b) = tokio::join!(
        tokio::spawn(async move {
            ensure_managed_default_container_image_tar_with_source(&root_a, &source_a, None, None)
                .await
        }),
        tokio::spawn(async move {
            ensure_managed_default_container_image_tar_with_source(&root_b, &source_b, None, None)
                .await
        })
    );
    server.abort();

    let path_a = res_a.expect("join a").expect("ensure a");
    let path_b = res_b.expect("join b").expect("ensure b");
    assert_eq!(path_a, path_b);
    let cached = tokio::fs::read(&path_a).await.expect("read cached tar");
    assert_eq!(cached, body);
}
