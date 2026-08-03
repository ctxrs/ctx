use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use ctx_pro_host_protocol::ProFilesystemLayout;
use fs2::FileExt as _;
use ring::{
    rand::SystemRandom,
    signature::{RsaKeyPair, RSA_PKCS1_SHA256},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;

const CORE_GENERATION_SENTINEL: &[u8] = b"generation-bound Core snapshot authority";
const SEMANTIC_INDEX_SENTINEL: &[u8] = b"v0.26 disposable semantic index";

struct EpochStorageFixture {
    core_generation: PathBuf,
    semantic_index: PathBuf,
}

impl EpochStorageFixture {
    fn write(data_root: &Path) -> Self {
        let core_generation = data_root
            .join("search/lexical")
            .join("ctx-generations")
            .join("core-generation.sentinel");
        let semantic_index = data_root
            .join("search/semantic")
            .join("fresh-epoch.sentinel");
        fs::create_dir_all(core_generation.parent().unwrap()).unwrap();
        fs::create_dir_all(semantic_index.parent().unwrap()).unwrap();
        fs::write(&core_generation, CORE_GENERATION_SENTINEL).unwrap();
        fs::write(&semantic_index, SEMANTIC_INDEX_SENTINEL).unwrap();
        Self {
            core_generation,
            semantic_index,
        }
    }

    fn assert_preserved(&self) {
        assert_eq!(
            fs::read(&self.core_generation).unwrap(),
            CORE_GENERATION_SENTINEL
        );
        assert_eq!(
            fs::read(&self.semantic_index).unwrap(),
            SEMANTIC_INDEX_SENTINEL
        );
    }
}

const TEST_PRIVATE_KEY_PEM: &str = r#"-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC4czAqM5XMipjl
QxTatkq8VmeS13e2aEpqT1v/XGL17o43i624H80xEbvB5tV/YzpO5N8sb4wEUj9h
yNzB5/U4S6SM/QadcA9fk/V7KeBOcz15PvZaU0UNp/dKVvzEFtxv/rjQCfA80C2N
30lTwti8pts4IulxVeB7BkIvqs3XADV5zBVwRACHWt5MKcMrXfBcmKRy8TLdNeml
lPgU3V2pj4c54KQ0aoy3/970+ry3P+eT8BlatU4k8R+pS0Oy4s3Ezczj9UrPCREd
1m2tAqaw8B0wRoei+nHEPWqbbzgx8fepv38U9LXmzYpCjSWSZ+zcZ4YBsXlyab3a
2PjyZ42HAgMBAAECggEAHQvis1qhRe8zibMJJzIazdLrh5fP3dVJlrk9mxag7Oqu
0bd42WyEoywQPcZMq71kEsV/EZ/VVF7hZVQ803pkRwO+e4djEcryWNJTj5w2GxSR
wzSzleDUGITxb+8H6hdRin95+iT+hI0iB1v4z6x49ihukEYLLhJgge8n4BrNRISa
P+SInTo/UzO5NIzh8HdQBJqkammS4c/Eij0jVw9onMpOFWKAxcs0hmk1SSy6KouD
yDBqp6m6ILlAuggZutkn+7X4QUzvgBQePYy6BNX57dmFpBWt/8DVc5m4Ciwd+s1L
CLRL86X6YLtc5wTQvdX/xHbW9m/FUXk5EvK2eQ+IyQKBgQD7B4aFQFwHiRjO323d
I7FUcSgsBEz/pYiucEF5c+GQUpSq/ORgFg7sYLAv3312nbu/TdIw2O0KxhhfUX6j
iRGe5NzSogUpRHk3Rq/tbQKULezDi9Lc7ROUuMYRpsHSjiVLB+zYdRDZULBqAdSo
3A0c0/xfCKB0efIJt4SfTVtcvwKBgQC8Git0ry8csFgmwmuxHL1nBmxXBLyZ04Ko
PQ+WyLPgL8cVP3Bf19zXDtmeoPSD8bZODys4UKit3zpZDEKN9S8JeN2E1h5MTgKN
wmOxdimAo0xKHJ/EnvxzfR5UzbrGiuajCFvIDPjItl3gSJ2av1cwQ8ljZBtOoqdX
KiTNCw7ZOQKBgQCTEuSom32P2K4VPmiC4M+blrSfnWFzgoujEBf8TX2BbjC2QXaY
KTRTH476bWl3npCKU9DrV50B6/AJoJievcb6HkKWkeCOPhT64speQ7j4EjQemYRQ
dgI750n8u4PhlfCZlioY4/WcLR8+7JWo3Uw9cKHzF/3SYEQDl2b3Yn49xwKBgFda
g+HNVUCqeFWPpnl60k6dAgUrUvbQ7fV5Xdr1W+t55KdubZ5k3c8Vu2RadRMtVi9M
BhNCCgOtDii6c9H/EhgBBEajNTDUbYUtyCRqrn1p2Iz2XA/wkWaErWhOnjWD3fXK
dO0jcQms/02gC2kJANGOOWEp5TCQgswM60g5oWypAoGADlZTP+97w9NcOJoQdZVi
+I5NLRKHUjAvax4BALtH5uuVIwj6cSwheRkBzd7rU1aQ65yuUYwIznDsC2rir26x
ehIUvhTehZf04otZbIo7UUvFhohRmX5k4/Idf/njMa/dA5afBMM1xE7IkoeHQyLc
3I9zapKTmyq90XvKHvA9eyA=
-----END PRIVATE KEY-----"#;

const TEST_PUBLIC_KEY_PEM: &str = r#"-----BEGIN RSA PUBLIC KEY-----
MIIBCgKCAQEAuHMwKjOVzIqY5UMU2rZKvFZnktd3tmhKak9b/1xi9e6ON4utuB/N
MRG7webVf2M6TuTfLG+MBFI/Ycjcwef1OEukjP0GnXAPX5P1eyngTnM9eT72WlNF
Daf3Slb8xBbcb/640AnwPNAtjd9JU8LYvKbbOCLpcVXgewZCL6rN1wA1ecwVcEQA
h1reTCnDK13wXJikcvEy3TXppZT4FN1dqY+HOeCkNGqMt//e9Pq8tz/nk/AZWrVO
JPEfqUtDsuLNxM3M4/VKzwkRHdZtrQKmsPAdMEaHovpxxD1qm284MfH3qb9/FPS1
5s2KQo0lkmfs3GeGAbF5cmm92tj48meNhwIDAQAB
-----END RSA PUBLIC KEY-----"#;

struct SignedBundle {
    args: ProInstallArgs,
}

type ManifestMutation = (&'static str, fn(&mut Value));

fn pem_der(pem: &str) -> Vec<u8> {
    let body: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .map(str::trim)
        .collect();
    BASE64.decode(body).unwrap()
}

fn sign(bytes: &[u8]) -> String {
    let key_pair = RsaKeyPair::from_pkcs8(&pem_der(TEST_PRIVATE_KEY_PEM)).unwrap();
    let mut signature = vec![0; key_pair.public().modulus_len()];
    key_pair
        .sign(
            &RSA_PKCS1_SHA256,
            &SystemRandom::new(),
            bytes,
            &mut signature,
        )
        .unwrap();
    BASE64.encode(signature)
}

fn manifest(artifact: &[u8], version: &str) -> Value {
    json!({
        "schema_version": 1,
        "product": "ctx-pro",
        "channel": "test",
        "version": version,
        "source_commit": "0123456789abcdef0123456789abcdef01234567",
        "public_source_commit": "1123456789abcdef0123456789abcdef01234567",
        "private_source_commit": "0123456789abcdef0123456789abcdef01234567",
        "build_identity": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "protocol_min": ctx_pro_host_protocol::PROTOCOL_VERSION,
        "protocol_max": ctx_pro_host_protocol::PROTOCOL_VERSION,
        "protocol_fingerprint": ctx_pro_host_protocol::PROTOCOL_FINGERPRINT,
        "target": platform_target(),
        "architecture": std::env::consts::ARCH,
        "artifact_object": format!(
            "pro/artifacts/test/{version}/{}/{}",
            platform_target(),
            if cfg!(windows) { "ctx-pro.exe" } else { "ctx-pro" },
        ),
        "artifact_size": artifact.len(),
        "artifact_sha256": format!("{:x}", Sha256::digest(artifact)),
        "public_artifact_sha256": "1".repeat(64),
        "public_package_sha256": "2".repeat(64),
        "private_package_sha256": "3".repeat(64),
        "runtime_evidence_sha256": "4".repeat(64),
        "runtime_run_id": "12345678-1234-4234-8234-123456789abc",
        "release_key_id": "test-v1",
    })
}

fn write_bundle(directory: &Path, name: &str, artifact: &[u8], manifest: Value) -> SignedBundle {
    let artifact_path = directory.join(format!("{name}.bin"));
    let manifest_path = directory.join(format!("{name}.json"));
    let signature_path = directory.join(format!("{name}.sig"));
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(&artifact_path, artifact).unwrap();
    fs::write(&manifest_path, &manifest_bytes).unwrap();
    fs::write(&signature_path, sign(&manifest_bytes)).unwrap();
    SignedBundle {
        args: ProInstallArgs {
            artifact: artifact_path,
            manifest: manifest_path,
            signature: signature_path,
        },
    }
}

fn install_bundle(
    bundle: &SignedBundle,
    data_root: &Path,
    update: bool,
) -> Result<serde_json::Value> {
    install_bundle_with_persistence(bundle, data_root, update, &mut Persistence::default())
}

fn install_bundle_with_persistence(
    bundle: &SignedBundle,
    data_root: &Path,
    update: bool,
    persistence: &mut Persistence,
) -> Result<serde_json::Value> {
    install_with_key(
        &bundle.args,
        data_root,
        update,
        TEST_PUBLIC_KEY_PEM,
        persistence,
    )
}

fn target_bytes(data_root: &Path) -> Vec<u8> {
    fs::read(default_helper_path(data_root)).unwrap()
}

fn reconcile(data_root: &Path) -> Result<Option<ValidatedPair>> {
    reconcile_installation(
        &default_helper_path(data_root),
        TEST_PUBLIC_KEY_PEM,
        &mut Persistence::default(),
    )
}

fn classify_explicit_setup(data_root: &Path) -> Result<SetupInstallation> {
    let target = default_helper_path(data_root);
    let _lifecycle_lock = LifecycleLock::acquire(&target, true)?
        .ok_or_else(|| anyhow!("invalid_request: failed to create Pro lifecycle lock"))?;
    reconcile_setup_installation_locked(&target, TEST_PUBLIC_KEY_PEM, &mut Persistence::default())
}

fn repair_bundle(bundle: &SignedBundle, data_root: &Path) -> Result<serde_json::Value> {
    let target = default_helper_path(data_root);
    let _lifecycle_lock = LifecycleLock::acquire(&target, true)?
        .ok_or_else(|| anyhow!("invalid_request: failed to create Pro lifecycle lock"))?;
    let installation = reconcile_setup_installation_locked(
        &target,
        TEST_PUBLIC_KEY_PEM,
        &mut Persistence::default(),
    )?;
    install_for_setup_with_key_locked(
        &bundle.args,
        data_root,
        installation,
        TEST_PUBLIC_KEY_PEM,
        &mut Persistence::default(),
    )
}

fn assert_automated_repair_status(data_root: &Path) {
    let value = lifecycle_status_json_with_key(data_root, TEST_PUBLIC_KEY_PEM);
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["state"], "repair_required");
    assert_eq!(value["installed"], false);
    assert_eq!(value["error_code"], "invalid_response");
    assert_eq!(value["next_action"]["command"], "ctx pro");
    assert_eq!(value["next_action"]["reason"], "helper_artifacts_invalid");
}

fn assert_manual_diagnosis_status(data_root: &Path) {
    let value = lifecycle_status_json_with_key(data_root, TEST_PUBLIC_KEY_PEM);
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["state"], "unavailable");
    assert_eq!(value["installed"], false);
    assert_eq!(value["error_code"], "invalid_response");
    assert_eq!(value["next_action"]["command"], Value::Null);
    assert_eq!(value["next_action"]["reason"], "manual_diagnosis_required");
}

fn installed_pair(data_root: &Path) -> ValidatedPair {
    reconcile(data_root).unwrap().unwrap()
}

// These journals coordinate signed Pro helper publication only. They never carry
// history or serve as a fallback source of truth for the fresh epoch.
fn assert_no_transaction_files(data_root: &Path) {
    let target = default_helper_path(data_root);
    for path in [
        transaction_journal_path(&target).unwrap(),
        transaction_journal_next_path(&target).unwrap(),
        transaction_helper_path(&target).unwrap(),
        transaction_marker_path(&target).unwrap(),
        publish_helper_path(&target).unwrap(),
        publish_marker_path(&target).unwrap(),
        rollback_helper_stage_path(&target).unwrap(),
        rollback_marker_stage_path(&target).unwrap(),
    ] {
        assert!(!path.exists(), "orphaned transaction path: {path:?}");
    }
}

fn pending_recheck_target(data_root: &Path) -> Option<String> {
    let path = data_root.join("daemon/jobs/pro-catch-up-recheck.json");
    let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    value["target_helper_sha256"].as_str().map(str::to_owned)
}

fn boundary_position(boundaries: &[&str], name: &str) -> usize {
    boundaries
        .iter()
        .position(|boundary| *boundary == name)
        .map(|index| index + 1)
        .unwrap_or_else(|| panic!("missing {name} in {boundaries:?}"))
}

fn assert_secure_permissions(_data_root: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let target = default_helper_path(_data_root);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(install_marker_path(&target).unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(target.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}

fn run_crashing_process(bundle: &SignedBundle, data_root: &Path, update: bool, crash_after: usize) {
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("pro::lifecycle::tests::crash_worker_process")
        .env("CTX_PRO_CRASH_WORKER", "1")
        .env("CTX_PRO_CRASH_ARTIFACT", &bundle.args.artifact)
        .env("CTX_PRO_CRASH_MANIFEST", &bundle.args.manifest)
        .env("CTX_PRO_CRASH_SIGNATURE", &bundle.args.signature)
        .env("CTX_PRO_CRASH_DATA_ROOT", data_root)
        .env("CTX_PRO_CRASH_UPDATE", if update { "1" } else { "0" })
        .env("CTX_PRO_CRASH_AFTER", crash_after.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(86), "crash worker did not terminate");
}

fn run_lock_probe(data_root: &Path, expect_blocked: bool) {
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("pro::lifecycle::tests::lifecycle_lock_probe_worker")
        .env("CTX_PRO_LOCK_PROBE", "1")
        .env("CTX_PRO_LOCK_DATA_ROOT", data_root)
        .env(
            "CTX_PRO_LOCK_EXPECT_BLOCKED",
            if expect_blocked { "1" } else { "0" },
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "lifecycle lock probe failed: {status}");
}

#[test]
fn lifecycle_lock_probe_worker() {
    if std::env::var_os("CTX_PRO_LOCK_PROBE").is_none() {
        return;
    }
    let root = PathBuf::from(std::env::var_os("CTX_PRO_LOCK_DATA_ROOT").unwrap());
    let path = ProFilesystemLayout::new(&root).lifecycle_lock_path();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    let result = file.try_lock_exclusive();
    if std::env::var_os("CTX_PRO_LOCK_EXPECT_BLOCKED").as_deref() == Some(std::ffi::OsStr::new("1"))
    {
        assert!(result.is_err(), "cross-process lifecycle lock was not held");
    } else {
        result.unwrap();
        fs2::FileExt::unlock(&file).unwrap();
    }
}

#[test]
fn lifecycle_lock_serializes_other_processes_and_remains_persistent() {
    let root = TempDir::new().unwrap();
    let target = default_helper_path(root.path());
    let guard = LifecycleLock::acquire(&target, true).unwrap().unwrap();
    let lock_path = ProFilesystemLayout::new(root.path()).lifecycle_lock_path();
    assert!(lock_path.is_file());
    run_lock_probe(root.path(), true);
    drop(guard);
    run_lock_probe(root.path(), false);
    assert!(lock_path.is_file());
}

#[test]
fn layout_rejects_alias_and_non_authoritative_helper_paths() {
    let root = TempDir::new().unwrap();
    let aliased = root.path().join("child").join("..").join("pro/bin/ctx-pro");
    assert!(layout_for_target(&aliased).is_err());
    let wrong = root.path().join("pro/bin/not-ctx-pro");
    assert!(layout_for_target(&wrong).is_err());
}

#[test]
fn crash_worker_process() {
    if std::env::var_os("CTX_PRO_CRASH_WORKER").is_none() {
        return;
    }
    let path = |name: &str| PathBuf::from(std::env::var_os(name).unwrap());
    let args = ProInstallArgs {
        artifact: path("CTX_PRO_CRASH_ARTIFACT"),
        manifest: path("CTX_PRO_CRASH_MANIFEST"),
        signature: path("CTX_PRO_CRASH_SIGNATURE"),
    };
    let data_root = path("CTX_PRO_CRASH_DATA_ROOT");
    let update = matches!(std::env::var("CTX_PRO_CRASH_UPDATE").as_deref(), Ok("1"));
    let crash_after = std::env::var("CTX_PRO_CRASH_AFTER")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let mut persistence = Persistence {
        crash_after: Some(crash_after),
        boundaries: Vec::new(),
        hard_exit: true,
    };
    let result = install_with_key(
        &args,
        &data_root,
        update,
        TEST_PUBLIC_KEY_PEM,
        &mut persistence,
    );
    panic!("crash boundary was not reached: {result:?}");
}

#[test]
fn signed_install_and_replacement_retain_trusted_rollback() {
    let temp = TempDir::new().unwrap();
    let first = write_bundle(
        temp.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let receipt = install_bundle(&first, temp.path(), false).unwrap();
    assert_eq!(receipt["version"], "1.0.0");
    assert_eq!(target_bytes(temp.path()), b"first helper");

    let second = write_bundle(
        temp.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&second, temp.path(), true).unwrap();
    let target = default_helper_path(temp.path());
    assert_eq!(fs::read(&target).unwrap(), b"second helper");
    assert_eq!(
        fs::read(previous_helper_path(&target).unwrap()).unwrap(),
        b"first helper"
    );
    let marker = load_pair_at(
        &target,
        &install_marker_path(&target).unwrap(),
        TEST_PUBLIC_KEY_PEM,
    )
    .unwrap()
    .unwrap();
    let previous_marker = load_pair_at(
        &previous_helper_path(&target).unwrap(),
        &previous_marker_path(&target).unwrap(),
        TEST_PUBLIC_KEY_PEM,
    )
    .unwrap()
    .unwrap();
    assert_eq!(marker.manifest.version, "2.0.0");
    assert_eq!(previous_marker.manifest.version, "1.0.0");
    assert_secure_permissions(temp.path());
    assert_no_transaction_files(temp.path());
}

#[test]
fn signature_tamper_and_wrong_key_are_rejected_without_installing() {
    let temp = TempDir::new().unwrap();
    let mut bundle = write_bundle(
        temp.path(),
        "tamper",
        b"helper",
        manifest(b"helper", "1.0.0"),
    );
    fs::write(&bundle.args.manifest, b"{}").unwrap();
    let error = install_bundle(&bundle, temp.path(), false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("signature verification failed"));
    assert!(!default_helper_path(temp.path()).exists());

    bundle = write_bundle(
        temp.path(),
        "wrong-key",
        b"helper",
        manifest(b"helper", "1.0.0"),
    );
    let manifest_bytes = fs::read(&bundle.args.manifest).unwrap();
    let signature = fs::read(&bundle.args.signature).unwrap();
    let error = verify_signature_with_key(
        &manifest_bytes,
        &signature,
        PRO_RELEASE_STAGING_PUBLIC_KEY_PEM,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("signature verification failed"));
}

#[test]
fn runtime_selection_accepts_only_the_installed_signed_pair() {
    let temp = TempDir::new().unwrap();
    let bundle = write_bundle(
        temp.path(),
        "runtime",
        b"trusted runtime helper",
        manifest(b"trusted runtime helper", "1.0.0"),
    );
    install_bundle(&bundle, temp.path(), false).unwrap();

    assert_eq!(
        validated_installed_helper_path_with_key(temp.path(), TEST_PUBLIC_KEY_PEM).unwrap(),
        default_helper_path(temp.path())
    );

    fs::write(default_helper_path(temp.path()), b"untrusted replacement").unwrap();
    let error = validated_installed_helper_path_with_key(temp.path(), TEST_PUBLIC_KEY_PEM)
        .unwrap_err()
        .to_string();
    assert!(error.contains("trusted pair"));
    assert!(!error.contains(temp.path().to_str().unwrap()));
}

#[test]
fn signed_manifest_identity_exact_protocol_hash_and_length_are_enforced() {
    let mutations: [ManifestMutation; 8] = [
        ("schema", |value| value["schema_version"] = json!(2)),
        ("hash", |value| {
            value["artifact_sha256"] = json!("0".repeat(64))
        }),
        ("length", |value| value["artifact_size"] = json!(7)),
        ("target", |value| value["target"] = json!("not-this-target")),
        ("protocol-min", |value| {
            value["protocol_min"] = json!(ctx_pro_host_protocol::PROTOCOL_VERSION + 1)
        }),
        ("protocol-max", |value| {
            value["protocol_max"] = json!(ctx_pro_host_protocol::PROTOCOL_VERSION + 1)
        }),
        ("protocol-fingerprint", |value| {
            value["protocol_fingerprint"] = json!("0".repeat(64))
        }),
        ("version", |value| value["version"] = json!("01.0.0")),
    ];
    for (name, mutate) in mutations {
        let temp = TempDir::new().unwrap();
        let artifact = b"helper";
        let mut value = manifest(artifact, "1.0.0");
        mutate(&mut value);
        let bundle = write_bundle(temp.path(), name, artifact, value);
        let error = install_bundle(&bundle, temp.path(), false)
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("invalid_response:") || error.starts_with("protocol_mismatch:"),
            "unexpected error for {name}: {error}"
        );
        assert!(!default_helper_path(temp.path()).exists());
    }
}

#[test]
fn newly_installed_artifacts_remain_current_fingerprint_only() {
    let temp = TempDir::new().unwrap();
    let artifact = b"stale helper";
    let mut stale = manifest(artifact, "1.0.0");
    stale["protocol_fingerprint"] = json!("0".repeat(64));
    let bundle = write_bundle(temp.path(), "stale-download", artifact, stale);
    let error = install_bundle(&bundle, temp.path(), false)
        .unwrap_err()
        .to_string();
    assert!(error.starts_with("protocol_mismatch:"));
    assert!(!default_helper_path(temp.path()).exists());
}

#[test]
fn installed_signed_stale_fingerprint_is_rejected() {
    let temp = TempDir::new().unwrap();
    let artifact = b"installed helper";
    let bundle = write_bundle(
        temp.path(),
        "installed-current",
        artifact,
        manifest(artifact, "1.0.0"),
    );
    install_bundle(&bundle, temp.path(), false).unwrap();
    let target = default_helper_path(temp.path());
    let marker_path = install_marker_path(&target).unwrap();

    let write_signed_marker = |fingerprint: &str| {
        let mut value = manifest(artifact, "1.0.0");
        value["protocol_fingerprint"] = json!(fingerprint);
        let manifest_bytes = serde_json::to_vec(&value).unwrap();
        let signature = sign(&manifest_bytes);
        let marker = ProInstallMarker::new(&manifest_bytes, signature.as_bytes()).unwrap();
        fs::write(&marker_path, serde_json::to_vec(&marker).unwrap()).unwrap();
    };

    write_signed_marker(&"0".repeat(64));
    let error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(error.starts_with("invalid_response:"), "{error}");
}

#[test]
fn oversized_and_truncated_inputs_are_rejected() {
    let temp = TempDir::new().unwrap();
    let oversized_manifest = temp.path().join("oversized.json");
    let file = fs::File::create(&oversized_manifest).unwrap();
    file.set_len(MAX_MANIFEST_BYTES + 1).unwrap();
    let args = ProInstallArgs {
        artifact: temp.path().join("unused.bin"),
        manifest: oversized_manifest,
        signature: temp.path().join("unused.sig"),
    };
    let error = install_with_key(
        &args,
        temp.path(),
        false,
        TEST_PUBLIC_KEY_PEM,
        &mut Persistence::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("manifest exceeds maximum size"));

    let artifact = b"helper";
    let mut declared = manifest(artifact, "1.0.0");
    declared["artifact_size"] = json!(artifact.len() + 1);
    let truncated_artifact = write_bundle(temp.path(), "truncated-artifact", artifact, declared);
    let error = install_bundle(&truncated_artifact, temp.path(), false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("artifact size does not match"));

    let truncated_manifest_path = temp.path().join("truncated-manifest.json");
    let truncated_signature_path = temp.path().join("truncated-manifest.sig");
    let truncated = br#"{"schema_version":1"#;
    fs::write(&truncated_manifest_path, truncated).unwrap();
    fs::write(&truncated_signature_path, sign(truncated)).unwrap();
    let truncated_args = ProInstallArgs {
        artifact: truncated_artifact.args.artifact.clone(),
        manifest: truncated_manifest_path,
        signature: truncated_signature_path,
    };
    let error = install_with_key(
        &truncated_args,
        temp.path(),
        false,
        TEST_PUBLIC_KEY_PEM,
        &mut Persistence::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("parse signed Pro manifest"));

    let oversized_artifact = temp.path().join("oversized.bin");
    fs::File::create(&oversized_artifact)
        .unwrap()
        .set_len(MAX_ARTIFACT_BYTES + 1)
        .unwrap();
    let mut maximum_manifest = manifest(b"x", "1.0.0");
    maximum_manifest["artifact_size"] = json!(MAX_ARTIFACT_BYTES);
    let mut oversized_bundle = write_bundle(
        temp.path(),
        "oversized-artifact",
        b"placeholder",
        maximum_manifest,
    );
    oversized_bundle.args.artifact = oversized_artifact;
    let error = install_bundle(&oversized_bundle, temp.path(), false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("artifact exceeds maximum size"));
}

#[test]
fn update_rejects_rollback_and_preserves_current_helper_on_failure() {
    let temp = TempDir::new().unwrap();
    let current = write_bundle(
        temp.path(),
        "current",
        b"trusted current",
        manifest(b"trusted current", "2.0.0"),
    );
    install_bundle(&current, temp.path(), false).unwrap();
    let target = default_helper_path(temp.path());
    let marker_path = install_marker_path(&target).unwrap();
    let original_marker = fs::read(&marker_path).unwrap();

    let rollback = write_bundle(
        temp.path(),
        "rollback",
        b"older helper",
        manifest(b"older helper", "1.9.9"),
    );
    let error = install_bundle(&rollback, temp.path(), true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("roll back"));
    assert_eq!(target_bytes(temp.path()), b"trusted current");
    assert_eq!(fs::read(&marker_path).unwrap(), original_marker);

    let mut bad_hash_manifest = manifest(b"new helper", "3.0.0");
    bad_hash_manifest["artifact_sha256"] = json!("f".repeat(64));
    let bad_hash = write_bundle(temp.path(), "bad-hash", b"new helper", bad_hash_manifest);
    let error = install_bundle(&bad_hash, temp.path(), true)
        .unwrap_err()
        .to_string();
    assert!(error.contains("digest does not match"));
    assert_eq!(target_bytes(temp.path()), b"trusted current");
    assert_eq!(fs::read(marker_path).unwrap(), original_marker);
}

#[test]
fn target_intent_precedes_visibility_and_survives_interrupted_update() {
    let discovery = TempDir::new().unwrap();
    let first = write_bundle(
        discovery.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        discovery.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, discovery.path(), false).unwrap();
    let mut recording = Persistence::default();
    install_bundle_with_persistence(&second, discovery.path(), true, &mut recording).unwrap();
    let crash_after = boundary_position(&recording.boundaries, "after_publish_pro_recheck_intent");

    let temp = TempDir::new().unwrap();
    let first = write_bundle(
        temp.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        temp.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, temp.path(), false).unwrap();
    let mut interrupted = Persistence {
        crash_after: Some(crash_after),
        ..Persistence::default()
    };
    let error =
        install_bundle_with_persistence(&second, temp.path(), true, &mut interrupted).unwrap_err();
    assert!(error.to_string().contains("simulated_termination"));
    assert_eq!(target_bytes(temp.path()), b"first helper");
    assert_eq!(
        pending_recheck_target(temp.path()).unwrap(),
        format!("{:x}", Sha256::digest(b"second helper"))
    );

    install_bundle(&second, temp.path(), true).unwrap();
    assert_eq!(target_bytes(temp.path()), b"second helper");
    assert_eq!(
        pending_recheck_target(temp.path()).unwrap(),
        format!("{:x}", Sha256::digest(b"second helper"))
    );
}

#[test]
fn installed_target_with_unwoken_intent_is_recoverable_on_setup_retry() {
    let discovery = TempDir::new().unwrap();
    let first = write_bundle(
        discovery.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        discovery.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, discovery.path(), false).unwrap();
    let mut recording = Persistence::default();
    install_bundle_with_persistence(&second, discovery.path(), true, &mut recording).unwrap();
    let crash_after = boundary_position(
        &recording.boundaries,
        "after_install_before_pro_recheck_wake",
    );

    let retry = TempDir::new().unwrap();
    let first = write_bundle(
        retry.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        retry.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, retry.path(), false).unwrap();
    let mut interrupted = Persistence {
        crash_after: Some(crash_after),
        ..Persistence::default()
    };
    install_bundle_with_persistence(&second, retry.path(), true, &mut interrupted).unwrap_err();
    assert_eq!(target_bytes(retry.path()), b"second helper");
    assert_eq!(
        pending_recheck_target(retry.path()).unwrap(),
        format!("{:x}", Sha256::digest(b"second helper"))
    );

    install_bundle(&second, retry.path(), true).unwrap();
    assert_eq!(target_bytes(retry.path()), b"second helper");
}

#[test]
fn recheck_intent_write_failure_leaves_visibility_unchanged_and_setup_retryable() {
    let temp = TempDir::new().unwrap();
    fs::write(temp.path().join("daemon"), b"blocks daemon jobs directory").unwrap();
    let bundle = write_bundle(
        temp.path(),
        "initial",
        b"initial helper",
        manifest(b"initial helper", "1.0.0"),
    );

    let error = install_bundle(&bundle, temp.path(), false).unwrap_err();
    assert!(error.to_string().contains("daemon"), "{error:#}");
    assert!(!default_helper_path(temp.path()).exists());
    assert!(pending_recheck_target(temp.path()).is_none());

    fs::remove_file(temp.path().join("daemon")).unwrap();
    install_bundle(&bundle, temp.path(), false).unwrap();
    assert_eq!(target_bytes(temp.path()), b"initial helper");
    assert_eq!(
        pending_recheck_target(temp.path()).unwrap(),
        format!("{:x}", Sha256::digest(b"initial helper"))
    );
}

#[test]
fn every_initial_install_persistence_boundary_recovers_in_a_fresh_manager() {
    let discovery = TempDir::new().unwrap();
    let discovery_bundle = write_bundle(
        discovery.path(),
        "initial",
        b"initial helper",
        manifest(b"initial helper", "1.0.0"),
    );
    let mut recording = Persistence::default();
    install_bundle_with_persistence(&discovery_bundle, discovery.path(), false, &mut recording)
        .unwrap();
    let boundaries = recording.boundaries;
    #[cfg(unix)]
    assert_eq!(boundaries.len(), 40);
    #[cfg(windows)]
    assert_eq!(boundaries.len(), 33);
    assert_boundary_classes(&boundaries);

    for crash_after in 1..=boundaries.len() {
        let temp = TempDir::new().unwrap();
        let bundle = write_bundle(
            temp.path(),
            "initial",
            b"initial helper",
            manifest(b"initial helper", "1.0.0"),
        );
        run_crashing_process(&bundle, temp.path(), false, crash_after);

        let first_recovery = reconcile(temp.path()).unwrap();
        if let Some(pair) = &first_recovery {
            assert_eq!(pair.manifest.version, "1.0.0");
            assert_eq!(pair.artifact, b"initial helper");
            assert_secure_permissions(temp.path());
            install_bundle(&bundle, temp.path(), true).unwrap();
        } else {
            install_bundle(&bundle, temp.path(), false).unwrap();
        }
        let identity = installed_pair(temp.path()).identity;
        assert_eq!(installed_pair(temp.path()).identity, identity);
        assert_eq!(target_bytes(temp.path()), b"initial helper");
        assert_no_transaction_files(temp.path());
    }
}

#[test]
fn every_update_persistence_boundary_recovers_old_or_new_signed_pair() {
    let discovery = TempDir::new().unwrap();
    let first = write_bundle(
        discovery.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        discovery.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, discovery.path(), false).unwrap();
    let mut recording = Persistence::default();
    install_bundle_with_persistence(&second, discovery.path(), true, &mut recording).unwrap();
    let boundaries = recording.boundaries;
    #[cfg(unix)]
    assert_eq!(boundaries.len(), 53);
    #[cfg(windows)]
    assert_eq!(boundaries.len(), 44);
    assert_boundary_classes(&boundaries);

    for crash_after in 1..=boundaries.len() {
        let temp = TempDir::new().unwrap();
        let first = write_bundle(
            temp.path(),
            "first",
            b"first helper",
            manifest(b"first helper", "1.0.0"),
        );
        let second = write_bundle(
            temp.path(),
            "second",
            b"second helper",
            manifest(b"second helper", "2.0.0"),
        );
        install_bundle(&first, temp.path(), false).unwrap();
        run_crashing_process(&second, temp.path(), true, crash_after);

        let recovered = reconcile(temp.path()).unwrap().unwrap();
        assert!(matches!(
            recovered.manifest.version.as_str(),
            "1.0.0" | "2.0.0"
        ));
        match recovered.manifest.version.as_str() {
            "1.0.0" => assert_eq!(recovered.artifact, b"first helper"),
            "2.0.0" => assert_eq!(recovered.artifact, b"second helper"),
            _ => unreachable!(),
        }
        assert_secure_permissions(temp.path());
        let identity = recovered.identity;
        assert_eq!(installed_pair(temp.path()).identity, identity);
        install_bundle(&second, temp.path(), true).unwrap();
        assert_eq!(installed_pair(temp.path()).manifest.version, "2.0.0");
        assert_eq!(target_bytes(temp.path()), b"second helper");
        assert_no_transaction_files(temp.path());
    }
}

fn assert_boundary_classes(boundaries: &[&str]) {
    for class in ["write_", "fsync_", "rename_"] {
        assert!(
            boundaries
                .iter()
                .any(|boundary| boundary.starts_with(class)),
            "missing {class} boundary in {boundaries:?}"
        );
    }
}

#[test]
fn committed_stage_tamper_falls_back_to_known_good_and_journal_is_bounded() {
    let discovery_temp = TempDir::new().unwrap();
    let discovery_first = write_bundle(
        discovery_temp.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let discovery_second = write_bundle(
        discovery_temp.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&discovery_first, discovery_temp.path(), false).unwrap();
    let mut discovery = Persistence::default();
    install_bundle_with_persistence(
        &discovery_second,
        discovery_temp.path(),
        true,
        &mut discovery,
    )
    .unwrap();
    let commit_boundary = discovery
        .boundaries
        .iter()
        .enumerate()
        .filter(|(_, boundary)| **boundary == "rename_transaction_journal")
        .nth(1)
        .map(|(index, _)| index + 1)
        .unwrap();

    let temp = TempDir::new().unwrap();
    let first = write_bundle(
        temp.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        temp.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, temp.path(), false).unwrap();
    run_crashing_process(&second, temp.path(), true, commit_boundary);
    let target = default_helper_path(temp.path());
    let journal: InstallTransaction =
        serde_json::from_slice(&fs::read(transaction_journal_path(&target).unwrap()).unwrap())
            .unwrap();
    assert_eq!(journal.state, TransactionState::Committed);
    fs::write(transaction_helper_path(&target).unwrap(), b"tampered").unwrap();
    let recovered = reconcile(temp.path()).unwrap().unwrap();
    assert_eq!(recovered.manifest.version, "1.0.0");
    assert_eq!(recovered.artifact, b"first helper");
    assert_no_transaction_files(temp.path());

    let journal_path = transaction_journal_path(&target).unwrap();
    let file = fs::File::create(&journal_path).unwrap();
    file.set_len(MAX_TRANSACTION_JOURNAL_BYTES + 1).unwrap();
    let error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(error.contains("transaction journal exceeds maximum size"));
    let status = lifecycle_status_json_with_key(temp.path(), TEST_PUBLIC_KEY_PEM);
    assert_eq!(status["state"], "unavailable");
    assert_eq!(status["error_code"], "invalid_request");
    assert_eq!(status["next_action"]["command"], Value::Null);
    assert_eq!(status["next_action"]["reason"], "manual_diagnosis_required");
    assert_eq!(target_bytes(temp.path()), b"first helper");
    fs::remove_file(journal_path).unwrap();
    assert_eq!(installed_pair(temp.path()).manifest.version, "1.0.0");
}

#[test]
fn mismatched_or_tampered_signed_pairs_are_never_accepted() {
    let temp = TempDir::new().unwrap();
    let first = write_bundle(
        temp.path(),
        "first",
        b"first helper",
        manifest(b"first helper", "1.0.0"),
    );
    let second = write_bundle(
        temp.path(),
        "second",
        b"second helper",
        manifest(b"second helper", "2.0.0"),
    );
    install_bundle(&first, temp.path(), false).unwrap();
    install_bundle(&second, temp.path(), true).unwrap();
    let target = default_helper_path(temp.path());
    fs::write(install_marker_path(&target).unwrap(), b"{}").unwrap();
    let repaired = reconcile(temp.path()).unwrap().unwrap();
    assert_eq!(repaired.manifest.version, "1.0.0");
    assert_eq!(repaired.artifact, b"first helper");

    fs::write(&target, b"tampered current").unwrap();
    fs::write(install_marker_path(&target).unwrap(), b"{}").unwrap();
    fs::write(previous_helper_path(&target).unwrap(), b"tampered rollback").unwrap();
    fs::write(previous_marker_path(&target).unwrap(), b"{}").unwrap();
    let error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(error.starts_with("invalid_response:"));
    assert!(!error.contains("tampered"));
}

#[derive(Debug, Clone, Copy)]
enum InvalidCurrentPair {
    MissingHelper,
    MissingMarker,
    CorruptHelper,
    CorruptMarker,
}

fn assert_explicit_setup_repairs_invalid_current_pair(case: InvalidCurrentPair) {
    let temp = TempDir::new().unwrap();
    let initial = write_bundle(
        temp.path(),
        "initial",
        b"initial helper",
        manifest(b"initial helper", "1.0.0"),
    );
    install_bundle(&initial, temp.path(), false).unwrap();

    let layout = ProFilesystemLayout::new(temp.path());
    let target = layout.helper_path();
    let marker = layout.helper_marker_path();
    let previous_helper = layout.previous_helper_path();
    let previous_marker = layout.previous_marker_path();
    assert!(!previous_helper.exists());
    assert!(!previous_marker.exists());

    let graph = layout.graph_path();
    fs::write(&graph, b"encrypted graph data").unwrap();
    let epoch = EpochStorageFixture::write(temp.path());
    crate::pro::local_deletion::write_local_pro_initialization_indicator(temp.path()).unwrap();

    match case {
        InvalidCurrentPair::MissingHelper => fs::remove_file(&target).unwrap(),
        InvalidCurrentPair::MissingMarker => fs::remove_file(&marker).unwrap(),
        InvalidCurrentPair::CorruptHelper => fs::write(&target, b"corrupt helper").unwrap(),
        InvalidCurrentPair::CorruptMarker => fs::write(&marker, b"{}").unwrap(),
    }
    let invalid_helper = fs::read(&target).ok();
    let invalid_marker = fs::read(&marker).ok();

    let strict_error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(strict_error.starts_with("invalid_response:"));
    assert_automated_repair_status(temp.path());
    assert!(matches!(
        classify_explicit_setup(temp.path()).unwrap(),
        SetupInstallation::RepairRequired
    ));
    assert_eq!(fs::read(&target).ok(), invalid_helper);
    assert_eq!(fs::read(&marker).ok(), invalid_marker);

    let bad_replacement = write_bundle(
        temp.path(),
        "bad-replacement",
        b"wrong replacement bytes",
        manifest(b"right replacement bytes", "2.0.0"),
    );
    let bad_artifact = fs::read(&bad_replacement.args.artifact).unwrap();
    let error = repair_bundle(&bad_replacement, temp.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("artifact digest does not match signed manifest"));
    assert_eq!(fs::read(&target).ok(), invalid_helper);
    assert_eq!(fs::read(&marker).ok(), invalid_marker);
    assert_eq!(
        fs::read(&bad_replacement.args.artifact).unwrap(),
        bad_artifact
    );
    assert_eq!(fs::read(&graph).unwrap(), b"encrypted graph data");
    epoch.assert_preserved();
    assert!(
        crate::pro::local_deletion::local_pro_initialization_indicator_exists(temp.path()).unwrap()
    );

    let replacement = write_bundle(
        temp.path(),
        "replacement",
        b"verified replacement helper",
        manifest(b"verified replacement helper", "2.0.0"),
    );
    let result = repair_bundle(&replacement, temp.path()).unwrap();
    assert_eq!(result["installed"], true);
    assert_eq!(result["updated"], true);
    assert_eq!(installed_pair(temp.path()).manifest.version, "2.0.0");
    assert_eq!(target_bytes(temp.path()), b"verified replacement helper");
    assert!(!previous_helper.exists());
    assert!(!previous_marker.exists());
    assert_eq!(fs::read(graph).unwrap(), b"encrypted graph data");
    epoch.assert_preserved();
    assert_eq!(
        fs::read(&bad_replacement.args.artifact).unwrap(),
        bad_artifact
    );
    assert!(
        crate::pro::local_deletion::local_pro_initialization_indicator_exists(temp.path()).unwrap()
    );
    assert_no_transaction_files(temp.path());
}

#[test]
fn explicit_setup_repairs_a_missing_current_helper_without_a_previous_pair() {
    assert_explicit_setup_repairs_invalid_current_pair(InvalidCurrentPair::MissingHelper);
}

#[test]
fn explicit_setup_repairs_a_missing_current_marker_without_a_previous_pair() {
    assert_explicit_setup_repairs_invalid_current_pair(InvalidCurrentPair::MissingMarker);
}

#[test]
fn explicit_setup_repairs_a_corrupt_current_helper_without_a_previous_pair() {
    assert_explicit_setup_repairs_invalid_current_pair(InvalidCurrentPair::CorruptHelper);
}

#[test]
fn explicit_setup_repairs_a_corrupt_current_marker_without_a_previous_pair() {
    assert_explicit_setup_repairs_invalid_current_pair(InvalidCurrentPair::CorruptMarker);
}

#[derive(Debug, Clone, Copy)]
enum InvalidRollbackPair {
    MissingHelper,
    MissingMarker,
    CorruptHelper,
    CorruptMarker,
}

fn assert_explicit_setup_repairs_invalid_rollback_pair(case: InvalidRollbackPair) {
    let temp = TempDir::new().unwrap();
    let initial = write_bundle(
        temp.path(),
        "initial",
        b"initial helper",
        manifest(b"initial helper", "1.0.0"),
    );
    let update = write_bundle(
        temp.path(),
        "update",
        b"updated helper",
        manifest(b"updated helper", "2.0.0"),
    );
    install_bundle(&initial, temp.path(), false).unwrap();
    install_bundle(&update, temp.path(), true).unwrap();

    let layout = ProFilesystemLayout::new(temp.path());
    let target = layout.helper_path();
    let marker = layout.helper_marker_path();
    let previous_helper = layout.previous_helper_path();
    let previous_marker = layout.previous_marker_path();
    fs::write(&target, b"corrupt current helper").unwrap();
    fs::write(&marker, b"corrupt current marker").unwrap();
    match case {
        InvalidRollbackPair::MissingHelper => fs::remove_file(&previous_helper).unwrap(),
        InvalidRollbackPair::MissingMarker => fs::remove_file(&previous_marker).unwrap(),
        InvalidRollbackPair::CorruptHelper => {
            fs::write(&previous_helper, b"corrupt rollback helper").unwrap()
        }
        InvalidRollbackPair::CorruptMarker => {
            fs::write(&previous_marker, b"corrupt rollback marker").unwrap()
        }
    }

    let graph = layout.graph_path();
    fs::write(&graph, b"encrypted graph data").unwrap();
    let epoch = EpochStorageFixture::write(temp.path());
    crate::pro::local_deletion::write_local_pro_initialization_indicator(temp.path()).unwrap();

    let invalid_helper = fs::read(&target).ok();
    let invalid_marker = fs::read(&marker).ok();
    let invalid_previous_helper = fs::read(&previous_helper).ok();
    let invalid_previous_marker = fs::read(&previous_marker).ok();

    let strict_error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(strict_error.starts_with("invalid_response:"));
    assert_automated_repair_status(temp.path());
    assert!(matches!(
        classify_explicit_setup(temp.path()).unwrap(),
        SetupInstallation::RepairRequired
    ));
    assert_eq!(fs::read(&target).ok(), invalid_helper);
    assert_eq!(fs::read(&marker).ok(), invalid_marker);
    assert_eq!(fs::read(&previous_helper).ok(), invalid_previous_helper);
    assert_eq!(fs::read(&previous_marker).ok(), invalid_previous_marker);

    let bad_replacement = write_bundle(
        temp.path(),
        "bad-replacement",
        b"wrong replacement bytes",
        manifest(b"right replacement bytes", "3.0.0"),
    );
    let bad_artifact = fs::read(&bad_replacement.args.artifact).unwrap();
    let error = repair_bundle(&bad_replacement, temp.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("artifact digest does not match signed manifest"));
    assert_eq!(fs::read(&target).ok(), invalid_helper);
    assert_eq!(fs::read(&marker).ok(), invalid_marker);
    assert_eq!(fs::read(&previous_helper).ok(), invalid_previous_helper);
    assert_eq!(fs::read(&previous_marker).ok(), invalid_previous_marker);
    assert_eq!(
        fs::read(&bad_replacement.args.artifact).unwrap(),
        bad_artifact
    );
    assert_eq!(fs::read(&graph).unwrap(), b"encrypted graph data");
    epoch.assert_preserved();
    assert!(
        crate::pro::local_deletion::local_pro_initialization_indicator_exists(temp.path()).unwrap()
    );

    let replacement = write_bundle(
        temp.path(),
        "replacement",
        b"verified replacement helper",
        manifest(b"verified replacement helper", "3.0.0"),
    );
    let result = repair_bundle(&replacement, temp.path()).unwrap();
    assert_eq!(result["installed"], true);
    assert_eq!(result["updated"], true);
    assert_eq!(installed_pair(temp.path()).manifest.version, "3.0.0");
    assert_eq!(target_bytes(temp.path()), b"verified replacement helper");
    assert_eq!(fs::read(&graph).unwrap(), b"encrypted graph data");
    epoch.assert_preserved();
    assert_eq!(
        fs::read(&bad_replacement.args.artifact).unwrap(),
        bad_artifact
    );
    assert!(
        crate::pro::local_deletion::local_pro_initialization_indicator_exists(temp.path()).unwrap()
    );
    assert_no_transaction_files(temp.path());
}

#[test]
fn explicit_setup_repairs_a_missing_rollback_helper() {
    assert_explicit_setup_repairs_invalid_rollback_pair(InvalidRollbackPair::MissingHelper);
}

#[test]
fn explicit_setup_repairs_a_missing_rollback_marker() {
    assert_explicit_setup_repairs_invalid_rollback_pair(InvalidRollbackPair::MissingMarker);
}

#[test]
fn explicit_setup_repairs_a_corrupt_rollback_helper() {
    assert_explicit_setup_repairs_invalid_rollback_pair(InvalidRollbackPair::CorruptHelper);
}

#[test]
fn explicit_setup_repairs_a_corrupt_rollback_marker() {
    assert_explicit_setup_repairs_invalid_rollback_pair(InvalidRollbackPair::CorruptMarker);
}

#[test]
fn explicit_setup_repairs_an_unrecoverable_committed_journal() {
    let temp = TempDir::new().unwrap();
    let initial = write_bundle(
        temp.path(),
        "initial",
        b"initial helper",
        manifest(b"initial helper", "1.0.0"),
    );
    install_bundle(&initial, temp.path(), false).unwrap();

    let layout = ProFilesystemLayout::new(temp.path());
    let target = layout.helper_path();
    let marker = layout.helper_marker_path();
    fs::write(&target, b"corrupt current helper").unwrap();
    fs::write(&marker, b"corrupt current marker").unwrap();
    let journal_path = transaction_journal_path(&target).unwrap();
    let journal = InstallTransaction {
        schema_version: 1,
        transaction_id: uuid::Uuid::new_v4(),
        state: TransactionState::Committed,
        old: None,
        new: PairIdentity {
            artifact_size: 17,
            artifact_sha256: "a".repeat(64),
            marker_sha256: "b".repeat(64),
            version: "2.0.0".to_owned(),
        },
    };
    fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();

    let graph = layout.graph_path();
    fs::write(&graph, b"encrypted graph data").unwrap();
    let epoch = EpochStorageFixture::write(temp.path());
    crate::pro::local_deletion::write_local_pro_initialization_indicator(temp.path()).unwrap();

    let invalid_helper = fs::read(&target).unwrap();
    let invalid_marker = fs::read(&marker).unwrap();
    let invalid_journal = fs::read(&journal_path).unwrap();
    let strict_error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(strict_error.contains("no recoverable signed helper pair"));
    assert_automated_repair_status(temp.path());
    assert!(matches!(
        classify_explicit_setup(temp.path()).unwrap(),
        SetupInstallation::RepairRequired
    ));
    assert_eq!(fs::read(&target).unwrap(), invalid_helper);
    assert_eq!(fs::read(&marker).unwrap(), invalid_marker);
    assert_eq!(fs::read(&journal_path).unwrap(), invalid_journal);

    let bad_replacement = write_bundle(
        temp.path(),
        "bad-replacement",
        b"wrong replacement bytes",
        manifest(b"right replacement bytes", "3.0.0"),
    );
    let bad_artifact = fs::read(&bad_replacement.args.artifact).unwrap();
    let error = repair_bundle(&bad_replacement, temp.path())
        .unwrap_err()
        .to_string();
    assert!(error.contains("artifact digest does not match signed manifest"));
    assert_eq!(fs::read(&target).unwrap(), invalid_helper);
    assert_eq!(fs::read(&marker).unwrap(), invalid_marker);
    assert_eq!(fs::read(&journal_path).unwrap(), invalid_journal);
    assert_eq!(
        fs::read(&bad_replacement.args.artifact).unwrap(),
        bad_artifact
    );
    assert_eq!(fs::read(&graph).unwrap(), b"encrypted graph data");
    epoch.assert_preserved();
    assert!(
        crate::pro::local_deletion::local_pro_initialization_indicator_exists(temp.path()).unwrap()
    );

    let replacement = write_bundle(
        temp.path(),
        "replacement",
        b"verified replacement helper",
        manifest(b"verified replacement helper", "3.0.0"),
    );
    let result = repair_bundle(&replacement, temp.path()).unwrap();
    assert_eq!(result["installed"], true);
    assert_eq!(result["updated"], true);
    assert_eq!(installed_pair(temp.path()).manifest.version, "3.0.0");
    assert_eq!(target_bytes(temp.path()), b"verified replacement helper");
    assert_eq!(fs::read(&graph).unwrap(), b"encrypted graph data");
    epoch.assert_preserved();
    assert_eq!(
        fs::read(&bad_replacement.args.artifact).unwrap(),
        bad_artifact
    );
    assert!(
        crate::pro::local_deletion::local_pro_initialization_indicator_exists(temp.path()).unwrap()
    );
    assert_no_transaction_files(temp.path());
}

#[test]
fn malformed_journals_remain_fail_closed_for_explicit_setup() {
    let temp = TempDir::new().unwrap();
    let initial = write_bundle(
        temp.path(),
        "initial",
        b"initial helper",
        manifest(b"initial helper", "1.0.0"),
    );
    install_bundle(&initial, temp.path(), false).unwrap();
    let journal_path = transaction_journal_path(&default_helper_path(temp.path())).unwrap();
    fs::write(&journal_path, b"{").unwrap();

    let strict_error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(strict_error.contains("parse Pro transaction journal"));
    let setup_error = classify_explicit_setup(temp.path())
        .unwrap_err()
        .to_string();
    assert!(setup_error.contains("parse Pro transaction journal"));
    assert_manual_diagnosis_status(temp.path());
    assert_eq!(fs::read(journal_path).unwrap(), b"{");
}

#[test]
fn unsafe_installation_types_remain_fail_closed_for_explicit_setup() {
    let temp = TempDir::new().unwrap();
    let target = default_helper_path(temp.path());
    fs::create_dir_all(&target).unwrap();

    let strict_error = reconcile(temp.path()).unwrap_err().to_string();
    assert!(strict_error.contains("unsafe file type"));
    let setup_error = classify_explicit_setup(temp.path())
        .unwrap_err()
        .to_string();
    assert!(setup_error.contains("unsafe file type"));
    assert_manual_diagnosis_status(temp.path());
    assert!(target.is_dir());
}

#[test]
fn error_messages_do_not_expose_signed_or_artifact_contents() {
    let temp = TempDir::new().unwrap();
    let secret = "do-not-echo-this-secret";
    let artifact = secret.as_bytes();
    let mut value = manifest(artifact, "1.0.0");
    value["artifact_sha256"] = json!("a".repeat(64));
    let bundle = write_bundle(temp.path(), "secret", artifact, value);
    let error = install_bundle(&bundle, temp.path(), false)
        .unwrap_err()
        .to_string();
    assert!(!error.contains(secret));
    assert!(error.starts_with("invalid_response:"));
}

#[test]
fn configured_release_trust_key_has_no_runtime_override() {
    let manifest_bytes = serde_json::to_vec(&manifest(b"helper", "1.0.0")).unwrap();
    let signature = sign(&manifest_bytes);
    assert!(verify_signature_with_key(
        &manifest_bytes,
        signature.as_bytes(),
        PRO_RELEASE_STAGING_PUBLIC_KEY_PEM,
    )
    .is_err());
}
