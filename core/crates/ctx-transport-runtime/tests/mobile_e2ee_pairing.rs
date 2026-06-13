use ctx_transport_runtime::mobile_e2ee;

#[test]
fn pairing_request_envelope_round_trips_and_binds_device_public_key() {
    let device_id = "33333333-3333-3333-3333-333333333333";
    let (daemon_public, daemon_secret) = mobile_e2ee::generate_keypair();
    let (device_public, device_secret) = mobile_e2ee::generate_keypair();
    let key = mobile_e2ee::derive_client_key(device_id, &device_secret, &daemon_public).unwrap();
    let daemon_key = mobile_e2ee::derive_key(device_id, &device_public, &daemon_secret).unwrap();
    let plaintext = br#"{"pairing_token":"secret"}"#;

    let envelope =
        mobile_e2ee::encrypt_pairing_request(&key, device_id, &device_public, plaintext).unwrap();
    let decrypted = mobile_e2ee::decrypt_pairing_request(
        &daemon_key,
        device_id,
        &device_public,
        &envelope.nonce_b64,
        &envelope.ciphertext_b64,
    )
    .unwrap();
    assert_eq!(decrypted, plaintext);

    let (other_device_public, _) = mobile_e2ee::generate_keypair();
    assert!(mobile_e2ee::decrypt_pairing_request(
        &daemon_key,
        device_id,
        &other_device_public,
        &envelope.nonce_b64,
        &envelope.ciphertext_b64,
    )
    .is_err());
}
