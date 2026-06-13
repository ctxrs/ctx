use super::*;

mod fixtures;

use fixtures::{
    assert_pairing_token_consumable, decrypt_pairing_response, encrypted_pairing_harness,
    post_mobile_pair, valid_encrypted_pair_request, PairingKeyMaterial,
};

#[tokio::test]
async fn pair_mobile_device_accepts_encrypted_pairing_request() {
    let harness = encrypted_pairing_harness().await;
    let key_material = PairingKeyMaterial::generate(&harness.daemon_public_key);
    let body = valid_encrypted_pair_request(&harness, &key_material);
    let serialized = body.to_string();
    let relay_visible = body.as_object().unwrap();
    for legacy_field in ["pairing_token", "device_label", "platform", "app_version"] {
        assert!(
            !relay_visible.contains_key(legacy_field),
            "encrypted pairing request must not expose legacy plaintext field {legacy_field}"
        );
    }

    let mut mixed_body = body.clone();
    let mixed_object = mixed_body.as_object_mut().unwrap();
    mixed_object.insert("pairing_token".to_string(), json!(harness.token));
    mixed_object.insert("device_label".to_string(), json!("phone"));
    mixed_object.insert("platform".to_string(), json!("ios"));
    mixed_object.insert("app_version".to_string(), json!("1.0.0"));
    let res = post_mobile_pair(&harness.app, mixed_body).await;
    assert_eq!(
        res.status(),
        StatusCode::BAD_REQUEST,
        "encrypted pairing must reject mixed relay-visible legacy plaintext fields"
    );

    let res = post_mobile_pair(&harness.app, serde_json::from_str(&serialized).unwrap()).await;
    assert_eq!(res.status(), StatusCode::OK);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let payload = decrypt_pairing_response(&envelope, &key_material);
    assert_eq!(payload["paired"], true);

    let device = harness
        .daemon()
        .mobile_access_for_test()
        .mobile_device_for_test(MobileDeviceId(
            uuid::Uuid::parse_str(key_material.device_id).unwrap(),
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        device.public_key.as_deref(),
        Some(key_material.device_public_key.as_str())
    );
    assert!(!assert_pairing_token_consumable(&harness).await);
}

#[tokio::test]
async fn pair_mobile_device_rejects_plaintext_legacy_pairing_request_without_consuming_token() {
    let harness = encrypted_pairing_harness().await;
    let res = post_mobile_pair(
        &harness.app,
        json!({
            "pairing_token": harness.token,
            "device_id": "33333333-3333-3333-3333-333333333333",
            "device_label": "phone",
            "platform": "ios",
            "public_key": "device-public",
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        assert_pairing_token_consumable(&harness).await,
        "legacy plaintext pairing should not consume a valid pairing token"
    );
}

#[tokio::test]
async fn pair_mobile_device_preserves_token_for_malformed_encrypted_request() {
    let harness = encrypted_pairing_harness().await;
    let (device_public_key, _device_secret_key) =
        ctx_transport_runtime::mobile_e2ee::generate_keypair();
    let res = post_mobile_pair(
        &harness.app,
        json!({
            "device_id": "33333333-3333-3333-3333-333333333333",
            "public_key": device_public_key,
            "seq": 0,
            "nonce": "invalid",
            "ciphertext": "invalid",
        }),
    )
    .await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(
        assert_pairing_token_consumable(&harness).await,
        "malformed encrypted pairing should not consume a valid pairing token"
    );
}
