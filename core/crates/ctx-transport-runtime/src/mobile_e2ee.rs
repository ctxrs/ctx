use anyhow::{anyhow, Result};
use base64::Engine;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use rand_core::RngCore;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};

const HKDF_INFO: &[u8] = b"ctx-mobile-e2ee-v1";
pub const PAIRING_REQUEST_ENCRYPTION: &str = "x25519-hkdf-sha256-xchacha20poly1305-v1";
const PAIRING_REQUEST_AAD_CONTEXT: &[u8] = b"ctx-mobile-pair-request-v1|POST /api/mobile/pair|";

#[derive(Debug, Clone)]
pub struct E2eeKey(pub(crate) [u8; 32]);

#[derive(Debug, Clone)]
pub struct Envelope {
    pub device_id: String,
    pub seq: i64,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

pub fn generate_keypair() -> (String, String) {
    let secret = StaticSecret::random_from_rng(rand_core::OsRng);
    let public = PublicKey::from(&secret);
    let public_b64 = base64::engine::general_purpose::STANDARD.encode(public.as_bytes());
    let secret_b64 = base64::engine::general_purpose::STANDARD.encode(secret.to_bytes());
    (public_b64, secret_b64)
}

pub fn derive_key(
    device_id: &str,
    device_public_b64: &str,
    daemon_private_b64: &str,
) -> Result<E2eeKey> {
    let device_public = decode_key(device_public_b64)?;
    let daemon_private = decode_key(daemon_private_b64)?;
    let device_public = PublicKey::from(device_public);
    let daemon_private = StaticSecret::from(daemon_private);
    let shared = daemon_private.diffie_hellman(&device_public);

    let salt = Sha256::digest(device_id.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .map_err(|_| anyhow!("hkdf expand failed"))?;
    Ok(E2eeKey(out))
}

pub fn derive_client_key(
    device_id: &str,
    device_secret_b64: &str,
    daemon_public_b64: &str,
) -> Result<E2eeKey> {
    let device_secret = decode_key(device_secret_b64)?;
    let daemon_public = decode_key(daemon_public_b64)?;
    let device_secret = StaticSecret::from(device_secret);
    let daemon_public = PublicKey::from(daemon_public);
    let shared = device_secret.diffie_hellman(&daemon_public);

    let salt = Sha256::digest(device_id.as_bytes());
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .map_err(|_| anyhow!("hkdf expand failed"))?;
    Ok(E2eeKey(out))
}

pub fn derive_stream_token(key: &E2eeKey, workspace_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-mobile-stream|");
    hasher.update(workspace_id.as_bytes());
    hasher.update(b"|");
    hasher.update(key.0);
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn encrypt(key: &E2eeKey, device_id: &str, seq: i64, plaintext: &[u8]) -> Result<Envelope> {
    encrypt_with_aad(key, device_id, seq, &[], plaintext)
}

pub fn encrypt_pairing_request(
    key: &E2eeKey,
    device_id: &str,
    device_public_b64: &str,
    plaintext: &[u8],
) -> Result<Envelope> {
    let aad_context = build_pairing_request_aad_context(device_public_b64);
    encrypt_with_aad(key, device_id, 0, &aad_context, plaintext)
}

fn encrypt_with_aad(
    key: &E2eeKey,
    device_id: &str,
    seq: i64,
    aad_context: &[u8],
    plaintext: &[u8],
) -> Result<Envelope> {
    let mut nonce = [0u8; 24];
    rand_core::OsRng.fill_bytes(&mut nonce);
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.0));
    let aad = build_aad(device_id, seq, aad_context);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("encrypt failed"))?;

    Ok(Envelope {
        device_id: device_id.to_string(),
        seq,
        nonce_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce),
        ciphertext_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

pub fn decrypt_pairing_request(
    key: &E2eeKey,
    device_id: &str,
    device_public_b64: &str,
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> Result<Vec<u8>> {
    let aad_context = build_pairing_request_aad_context(device_public_b64);
    decrypt_with_aad(key, device_id, 0, &aad_context, nonce_b64, ciphertext_b64)
}

pub fn decrypt(
    key: &E2eeKey,
    device_id: &str,
    seq: i64,
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> Result<Vec<u8>> {
    decrypt_with_aad(key, device_id, seq, &[], nonce_b64, ciphertext_b64)
}

fn decrypt_with_aad(
    key: &E2eeKey,
    device_id: &str,
    seq: i64,
    aad_context: &[u8],
    nonce_b64: &str,
    ciphertext_b64: &str,
) -> Result<Vec<u8>> {
    let nonce = decode_bytes(nonce_b64)?;
    let ciphertext = decode_bytes(ciphertext_b64)?;
    if nonce.len() != 24 {
        return Err(anyhow!("invalid nonce length"));
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(&key.0));
    let aad = build_aad(device_id, seq, aad_context);
    cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("decrypt failed"))
}

fn decode_key(value: &str) -> Result<[u8; 32]> {
    let bytes = decode_bytes(value)?;
    if bytes.len() != 32 {
        return Err(anyhow!("invalid key length"));
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

fn decode_bytes(value: &str) -> Result<Vec<u8>> {
    let mut normalized = value.trim().replace('-', "+").replace('_', "/");
    while !normalized.len().is_multiple_of(4) {
        normalized.push('=');
    }
    base64::engine::general_purpose::STANDARD
        .decode(normalized.as_bytes())
        .map_err(|_| anyhow!("invalid base64"))
}

fn build_pairing_request_aad_context(device_public_b64: &str) -> Vec<u8> {
    let mut context =
        Vec::with_capacity(PAIRING_REQUEST_AAD_CONTEXT.len() + device_public_b64.len());
    context.extend_from_slice(PAIRING_REQUEST_AAD_CONTEXT);
    context.extend_from_slice(device_public_b64.trim().as_bytes());
    context
}

fn build_aad(device_id: &str, seq: i64, aad_context: &[u8]) -> Vec<u8> {
    let mut aad = format!("{device_id}:{seq}").into_bytes();
    if !aad_context.is_empty() {
        aad.extend_from_slice(b":");
        aad.extend_from_slice(aad_context);
    }
    aad
}
