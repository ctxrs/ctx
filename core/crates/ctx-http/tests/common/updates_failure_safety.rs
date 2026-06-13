use std::sync::Mutex;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand_core::{OsRng, RngCore};
use serde_json::json;

use crate::common;

static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

pub struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    pub fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = self.previous.take() {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

pub fn current_platform_key() -> Option<&'static str> {
    ctx_update_service::platform_key()
}

pub struct SignedReleaseManifest {
    pub manifest_body: String,
    pub signature_b64: String,
    pub pubkey_b64: String,
}

fn minisign_pubkey_b64(verifying_key: &VerifyingKey, key_id: [u8; 8]) -> String {
    let mut payload = Vec::with_capacity(42);
    payload.extend_from_slice(&[0x45, 0x64]);
    payload.extend_from_slice(&key_id);
    payload.extend_from_slice(verifying_key.as_bytes());
    let key_text = format!(
        "untrusted comment: minisign public key: TESTKEY0\n{}\n",
        BASE64_STANDARD.encode(payload)
    );
    BASE64_STANDARD.encode(key_text.as_bytes())
}

fn minisign_signature_b64(signing_key: &SigningKey, key_id: [u8; 8], payload: &[u8]) -> String {
    let trusted_comment = "trusted comment: timestamp:1772585616\tfile:latest.json";
    let digest = Blake2b512::digest(payload);
    let signature_bytes = signing_key.sign(digest.as_slice()).to_bytes();
    let mut encoded_signature = Vec::with_capacity(74);
    encoded_signature.extend_from_slice(&[0x45, 0x44]);
    encoded_signature.extend_from_slice(&key_id);
    encoded_signature.extend_from_slice(&signature_bytes);

    let mut trusted_comment_bytes = Vec::with_capacity(
        signature_bytes.len() + trusted_comment.len() - "trusted comment: ".len(),
    );
    trusted_comment_bytes.extend_from_slice(&signature_bytes);
    trusted_comment_bytes.extend_from_slice(
        trusted_comment
            .strip_prefix("trusted comment: ")
            .unwrap_or(trusted_comment)
            .as_bytes(),
    );
    let global_signature = signing_key.sign(&trusted_comment_bytes).to_bytes();

    let signature_text = format!(
        "untrusted comment: signature from minisign secret key\n{}\n{}\n{}\n",
        BASE64_STANDARD.encode(encoded_signature),
        trusted_comment,
        BASE64_STANDARD.encode(global_signature)
    );
    BASE64_STANDARD.encode(signature_text.as_bytes())
}

pub fn sign_release_manifest_body(manifest_body: &str) -> (String, String) {
    let mut secret_key = [0u8; 32];
    OsRng.fill_bytes(&mut secret_key);
    let signing_key = SigningKey::from_bytes(&secret_key);
    let verifying_key = signing_key.verifying_key();
    let key_id = *b"ctxsig01";
    (
        minisign_signature_b64(&signing_key, key_id, manifest_body.as_bytes()),
        minisign_pubkey_b64(&verifying_key, key_id),
    )
}

pub fn release_manifest_for(
    platform: &str,
    appimage_url_path: &str,
    sha256: &str,
) -> SignedReleaseManifest {
    let manifest = json!({
      "channel": "stable",
      "latest_version": "9.9.9",
      "published_at": "2026-02-19T00:00:00Z",
      "platforms": {
        platform: {
          "appimage": {
            "url_path": appimage_url_path,
            "sha256": sha256
          },
          "daemon": {
            "url_path": "/download/stable/9.9.9/ctx-daemon",
            "sha256": "2222222222222222222222222222222222222222222222222222222222222222"
          }
        }
      }
    });
    let manifest_body = serde_json::to_string(&manifest).expect("manifest json");
    let (signature_b64, pubkey_b64) = sign_release_manifest_body(&manifest_body);
    SignedReleaseManifest {
        signature_b64,
        pubkey_b64,
        manifest_body,
    }
}

pub struct UpdateTestApp {
    app: axum::Router,
    _fixture: common::DataRootFakeDaemonFixture,
}

impl UpdateTestApp {
    pub fn app(&self) -> axum::Router {
        self.app.clone()
    }
}

pub async fn test_app_router(data_root: &std::path::Path) -> UpdateTestApp {
    let fixture = common::fake_daemon_fixture_for_data_root(data_root, "http://127.0.0.1:0").await;
    let app = fixture.router();
    UpdateTestApp {
        app,
        _fixture: fixture,
    }
}
