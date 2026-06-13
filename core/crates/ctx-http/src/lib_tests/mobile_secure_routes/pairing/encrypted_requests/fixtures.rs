use super::*;

pub(super) struct EncryptedPairingHarness {
    pub(super) app: axum::Router,
    daemon: DataRootTestDaemonFixture,
    pub(super) token: &'static str,
    pub(super) token_hash: String,
    pub(super) daemon_public_key: String,
    _home_guard: EnvVarGuard,
    _home: tempfile::TempDir,
    _data_dir: tempfile::TempDir,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

pub(super) struct PairingKeyMaterial {
    pub(super) device_id: &'static str,
    pub(super) device_public_key: String,
    device_secret_key: String,
    daemon_public_key: String,
}

impl EncryptedPairingHarness {
    pub(super) fn daemon(&self) -> &TestDaemon {
        self.daemon.daemon()
    }
}

impl PairingKeyMaterial {
    pub(super) fn generate(daemon_public_key: &str) -> Self {
        let (device_public_key, device_secret_key) =
            ctx_transport_runtime::mobile_e2ee::generate_keypair();
        Self {
            device_id: "33333333-3333-3333-3333-333333333333",
            device_public_key,
            device_secret_key,
            daemon_public_key: daemon_public_key.to_string(),
        }
    }
}

pub(super) async fn encrypted_pairing_harness() -> EncryptedPairingHarness {
    let serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let home_guard = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let daemon = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let profile_id = insert_mobile_profile(daemon.daemon()).await;
    let (daemon_public_key, daemon_private_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    daemon
        .daemon()
        .mobile_access_for_test()
        .seed_default_mobile_access_config_for_test(
            profile_id,
            true,
            daemon_public_key.clone(),
            daemon_private_key,
        )
        .await
        .unwrap();

    let token = "valid-pairing-token";
    let token_hash = daemon
        .daemon()
        .mobile_access_for_test()
        .seed_mobile_pairing_token_for_test(
            "pair-1",
            token,
            chrono::Utc::now() + chrono::Duration::minutes(5),
        )
        .await
        .unwrap();

    EncryptedPairingHarness {
        app: daemon.router(),
        daemon,
        token,
        token_hash,
        daemon_public_key,
        _home_guard: home_guard,
        _home: home,
        _data_dir: data_dir,
        _serial: serial,
    }
}

pub(super) fn valid_encrypted_pair_request(
    harness: &EncryptedPairingHarness,
    key_material: &PairingKeyMaterial,
) -> serde_json::Value {
    encrypted_pair_request_value(
        harness.token,
        key_material.device_id,
        &key_material.daemon_public_key,
        &key_material.device_public_key,
        &key_material.device_secret_key,
    )
}

pub(super) async fn post_mobile_pair(
    app: &axum::Router,
    body: serde_json::Value,
) -> axum::response::Response {
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/pair")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

pub(super) fn decrypt_pairing_response(
    envelope: &serde_json::Value,
    key_material: &PairingKeyMaterial,
) -> serde_json::Value {
    let key = ctx_transport_runtime::mobile_e2ee::derive_client_key(
        key_material.device_id,
        &key_material.device_secret_key,
        &key_material.daemon_public_key,
    )
    .unwrap();
    let plaintext = ctx_transport_runtime::mobile_e2ee::decrypt(
        &key,
        key_material.device_id,
        envelope["seq"].as_i64().unwrap(),
        envelope["nonce"].as_str().unwrap(),
        envelope["ciphertext"].as_str().unwrap(),
    )
    .unwrap();
    serde_json::from_slice(&plaintext).unwrap()
}

pub(super) async fn assert_pairing_token_consumable(harness: &EncryptedPairingHarness) -> bool {
    harness
        .daemon
        .daemon()
        .mobile_access_for_test()
        .consume_mobile_pairing_token_hash_for_test(&harness.token_hash)
        .await
        .unwrap()
}
