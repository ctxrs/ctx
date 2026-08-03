#![cfg(unix)]

use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _},
    path::{Path, PathBuf},
    process::Output,
};

use assert_cmd::Command;
use serde_json::{json, Value};
use tempfile::TempDir;

const CONTROL_ENV: &str = "CTX_PRO_TEST_CONTROL_MANIFEST";
const RECEIPT: &str = "control-receipt.json";
const FIXED_NOW: i64 = 2_000_000_000;

struct Harness {
    root: TempDir,
    data_root: PathBuf,
    manifest: PathBuf,
    browser_marker: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ctx-pro-control-")
            .tempdir()
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let data_root = root.path().join("data");
        let fake_bin = root.path().join("bin");
        let home = root.path().join("home");
        let xdg_config = root.path().join("xdg-config");
        let xdg_data = root.path().join("xdg-data");
        let xdg_state = root.path().join("xdg-state");
        let xdg_runtime = root.path().join("xdg-runtime");
        for directory in [
            &data_root,
            &fake_bin,
            &home,
            &xdg_config,
            &xdg_data,
            &xdg_state,
            &xdg_runtime,
        ] {
            fs::create_dir(directory).unwrap();
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let browser_marker = root.path().join("native-browser-called");
        let browser = fake_bin.join("xdg-open");
        fs::write(
            &browser,
            format!(
                "#!/bin/sh\nprintf called > '{}'\nexit 0\n",
                browser_marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&browser, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            manifest: root.path().join("manifest.json"),
            root,
            data_root,
            browser_marker,
        }
    }

    fn write_manifest(&self, value: Value) {
        write_private(&self.manifest, &canonical_json(value));
    }

    fn run(&self, args: &[&str]) -> Output {
        let fake_bin = self.root.path().join("bin");
        let mut command = Command::cargo_bin("ctx").unwrap();
        command
            .env(CONTROL_ENV, &self.manifest)
            .env("HOME", self.root.path().join("home"))
            .env("USERPROFILE", self.root.path().join("home"))
            .env("XDG_CONFIG_HOME", self.root.path().join("xdg-config"))
            .env("XDG_DATA_HOME", self.root.path().join("xdg-data"))
            .env("XDG_STATE_HOME", self.root.path().join("xdg-state"))
            .env("XDG_RUNTIME_DIR", self.root.path().join("xdg-runtime"))
            .env("LOCALAPPDATA", self.root.path().join("xdg-data"))
            .env("APPDATA", self.root.path().join("xdg-config"))
            .env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/ctx-pro-test",
            )
            .env("PATH", fake_bin)
            .env("NO_COLOR", "1")
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env_remove("CTX_PRO_CHANNEL")
            .env_remove("CTX_PRO_HELPER")
            .arg("--data-root")
            .arg(&self.data_root)
            .args(args)
            .output()
            .unwrap()
    }

    fn receipt(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.path().join(RECEIPT)).unwrap()).unwrap()
    }
}

fn manifest(operation: &str, action: Value, vault: &str, browser: &[&str]) -> Value {
    let mut value = json!({
        "browser": {
            "outcomes": browser,
            "receipt": RECEIPT,
        },
        "clock": {
            "unix_seconds": [FIXED_NOW],
        },
        "entitlement_trust": {
            "issuer": "https://pro-test.ctx.invalid",
            "key_id": "ctx-pro-test-control-v1",
        },
        "fixture_id": operation.replace('.', "-"),
        "helper": null,
        "lifecycle": {
            "manage": null,
            "setup": null,
        },
        "referral": {
            "create": null,
            "payout": null,
            "status": null,
        },
        "schema_version": 1,
        "vault": {
            "state": vault,
        },
    });
    let target = match operation {
        "lifecycle.setup" => &mut value["lifecycle"]["setup"],
        "lifecycle.manage" => &mut value["lifecycle"]["manage"],
        "referral.create" => &mut value["referral"]["create"],
        "referral.status" => &mut value["referral"]["status"],
        "referral.payout" => &mut value["referral"]["payout"],
        _ => panic!("unsupported operation"),
    };
    *target = action;
    value
}

fn success(value: Value, events: Vec<Value>) -> Value {
    json!({
        "events": events,
        "outcome": {
            "kind": "success",
            "value": value,
        },
    })
}

fn scripted_error(code: &str, message: &str) -> Value {
    json!({
        "events": [],
        "outcome": {
            "code": code,
            "kind": "error",
            "message": message,
        },
    })
}

fn manage_value() -> Value {
    json!({
        "access_deadline_unix": FIXED_NOW + 3600,
        "access_state": "active",
        "grace_deadline_unix": FIXED_NOW + 7200,
        "portal_url": "https://billing.example.test/session",
        "refresh_after_unix": FIXED_NOW + 1800,
    })
}

fn status_value() -> Value {
    json!({
        "attributed": 2,
        "codename": "agent-smith",
        "currency": "usd",
        "debt_cents": 0,
        "earned_cents": 2000,
        "manual_review_cents": 0,
        "paid_cents": 0,
        "payable_cents": 2000,
        "payout_state": "eligible",
        "pending_cents": 0,
        "processing_cents": 0,
        "subscribed": 2,
    })
}

#[test]
fn manage_human_and_machine_share_real_output_with_scripted_browser_receipts() {
    let human = Harness::new();
    human.write_manifest(manifest(
        "lifecycle.manage",
        success(manage_value(), vec![]),
        "commercial_credentials_active",
        &["success"],
    ));
    let output = human.run(&["pro", "manage"]);
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("ctx Pro account management is ready"));
    assert!(stdout.contains("https://billing.example.test/session"));
    assert!(stderr.contains("Browser open requested for ctx Pro account management."));
    assert!(!human.browser_marker.exists());
    assert_receipt(&human.receipt(), "lifecycle.manage", &["success"], &[]);

    let machine = Harness::new();
    machine.write_manifest(manifest(
        "lifecycle.manage",
        success(manage_value(), vec![]),
        "commercial_credentials_active",
        &[],
    ));
    let output = machine.run(&["pro", "manage", "--format", "json"]);
    assert_success(&output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["payload_type"], "pro_manage");
    assert_eq!(value["portal_url"], "https://billing.example.test/session");
    assert_eq!(value["access_state"], "active");
    assert_eq!(value["browser_opened"], false);
    assert!(output.stderr.is_empty());
    assert!(!machine.browser_marker.exists());
    assert_receipt(&machine.receipt(), "lifecycle.manage", &[], &[]);
}

#[test]
fn referral_sign_in_uses_real_renderer_and_a_failure_fake_without_native_calls() {
    let harness = Harness::new();
    let event = json!({
        "browser_uri": "https://auth.example.test/device/complete",
        "kind": "device_sign_in",
        "user_code": "TEST-CODE",
        "verification_uri": "https://auth.example.test/device",
    });
    harness.write_manifest(manifest(
        "referral.status",
        success(status_value(), vec![event]),
        "installation_identity_only",
        &["failure"],
    ));
    let output = harness.run(&["referral", "status"]);
    assert_success(&output);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("Sign in to ctx Pro"));
    assert!(stderr.contains("https://auth.example.test/device"));
    assert!(stderr.contains("TEST-CODE"));
    assert!(stderr.contains("A browser could not be opened for ctx Pro sign-in."));
    assert!(stdout.contains("agent-smith"));
    assert!(stdout.contains("Eligible; payout setup available"));
    assert!(!harness.browser_marker.exists());
    assert_receipt(&harness.receipt(), "referral.status", &["failure"], &[]);

    let machine = Harness::new();
    machine.write_manifest(manifest(
        "referral.create",
        success(
            json!({
                "codename": "agent-smith",
                "disposition": "created",
            }),
            vec![],
        ),
        "commercial_credentials_active",
        &[],
    ));
    let output = machine.run(&["referral", "create", "agent-smith", "--format", "json"]);
    assert_success(&output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["payload_type"], "referral_create");
    assert_eq!(value["codename"], "agent-smith");
    assert_eq!(value["disposition"], "created");
    assert_receipt(&machine.receipt(), "referral.create", &[], &[]);
}

#[test]
fn payout_uses_the_manifest_clock_and_suppresses_browsers_for_machine_output() {
    let harness = Harness::new();
    harness.write_manifest(manifest(
        "referral.payout",
        success(
            json!({
                "expires_at_unix": FIXED_NOW + 600,
                "kind": "payout_onboarding_created",
                "payout_state": "onboarding_pending",
                "url": "https://connect.example.test/setup/s/test",
            }),
            vec![],
        ),
        "commercial_credentials_active",
        &[],
    ));
    let output = harness.run(&["referral", "payout", "--format", "json"]);
    assert_success(&output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["payload_type"], "referral_payout");
    assert_eq!(value["expires_at_unix"], FIXED_NOW + 600);
    assert_eq!(value["browser_opened"], false);
    assert!(!harness.browser_marker.exists());
    assert_receipt(&harness.receipt(), "referral.payout", &[], &[FIXED_NOW]);
}

#[test]
fn scripted_errors_are_terminal_and_still_publish_complete_isolation_receipts() {
    let harness = Harness::new();
    harness.write_manifest(manifest(
        "lifecycle.manage",
        scripted_error("service_unavailable", "scripted commercial outage"),
        "commercial_credentials_active",
        &[],
    ));
    let output = harness.run(&["pro", "manage", "--no-open"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("ctx Pro account management is temporarily unavailable"),
        "{stderr}"
    );
    assert!(stderr.contains("ctx pro manage --no-open"), "{stderr}");
    assert!(!stderr.contains("service_unavailable"), "{stderr}");
    assert!(!stderr.contains("scripted commercial outage"), "{stderr}");
    let receipt = harness.receipt();
    assert_eq!(receipt["command_outcome"], "error");
    assert_receipt(&receipt, "lifecycle.manage", &[], &[]);
    assert!(!harness.browser_marker.exists());

    let locked = Harness::new();
    locked.write_manifest(manifest(
        "referral.status",
        success(status_value(), vec![]),
        "locked",
        &[],
    ));
    let output = locked.run(&["referral", "status"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("The secure key store is locked"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Unlock the selected persistent key store"),
        "{stderr}"
    );
    assert!(!stderr.contains("key_store_locked"), "{stderr}");
    let receipt = locked.receipt();
    assert_eq!(receipt["command_outcome"], "error");
    assert_receipt(&receipt, "referral.status", &[], &[]);
}

#[test]
fn parser_rejects_unknown_noncanonical_and_tampered_manifests_before_state() {
    let cases = [
        (
            "unknown",
            {
                let mut value = manifest(
                    "lifecycle.manage",
                    success(manage_value(), vec![]),
                    "commercial_credentials_active",
                    &[],
                );
                value["unknown"] = json!(true);
                canonical_json(value)
            },
            "unknown field",
            "invalid_request",
        ),
        (
            "noncanonical",
            serde_json::to_vec_pretty(&manifest(
                "lifecycle.manage",
                success(manage_value(), vec![]),
                "commercial_credentials_active",
                &[],
            ))
            .unwrap(),
            "canonical JSON",
            "invalid_request",
        ),
        (
            "tampered-trust",
            {
                let mut value = manifest(
                    "lifecycle.manage",
                    success(manage_value(), vec![]),
                    "commercial_credentials_active",
                    &[],
                );
                value["entitlement_trust"]["key_id"] = json!("production-2026-07-v1");
                canonical_json(value)
            },
            "selected commercial channel",
            "invalid_response",
        ),
    ];
    for (name, bytes, expected, expected_code) in cases {
        let harness = Harness::new();
        write_private(&harness.manifest, &bytes);
        let output = harness.run(&["pro", "manage", "--no-open", "--format", "json"]);
        assert!(!output.status.success(), "{name}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        let error: Value = serde_json::from_str(&stderr).unwrap();
        assert_eq!(error["error"], expected_code, "{name}: {stderr}");
        assert_eq!(error["error_code"], expected_code, "{name}: {stderr}");
        assert_eq!(error.as_object().unwrap().len(), 2, "{name}: {stderr}");
        assert!(!stderr.contains(expected), "{name}: {stderr}");
        assert!(!harness.root.path().join(RECEIPT).exists(), "{name}");
        assert!(!harness.data_root.join("pro").exists(), "{name}");
    }

    let harness = Harness::new();
    harness.write_manifest(manifest(
        "lifecycle.manage",
        success(manage_value(), vec![]),
        "commercial_credentials_active",
        &[],
    ));
    let mut command = Command::cargo_bin("ctx").unwrap();
    let output = command
        .env(CONTROL_ENV, "manifest.json")
        .arg("--data-root")
        .arg(&harness.data_root)
        .args(["pro", "manage", "--no-open", "--format", "json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let error: Value = serde_json::from_str(&stderr).unwrap();
    assert_eq!(error["error"], "invalid_request", "{stderr}");
    assert_eq!(error["error_code"], "invalid_request", "{stderr}");
    assert_eq!(error.as_object().unwrap().len(), 2, "{stderr}");
    assert!(!stderr.contains("normalized absolute path"), "{stderr}");
}

#[test]
fn production_fails_closed_and_test_host_keeps_exact_release_build_identity() {
    let harness = Harness::new();
    harness.write_manifest(manifest(
        "lifecycle.manage",
        success(manage_value(), vec![]),
        "commercial_credentials_active",
        &[],
    ));
    let release = std::env::var_os("CTX_RELEASE_TEST_BINARY")
        .map(PathBuf::from)
        .expect("Bazel provides the release ctx binary");
    let output = std::process::Command::new(&release)
        .env(CONTROL_ENV, &harness.manifest)
        .arg("--data-root")
        .arg(&harness.data_root)
        .args(["pro", "manage", "--no-open"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)
        .unwrap()
        .contains("accepted only by ctx_pro_test_host"));
    assert!(!harness.root.path().join(RECEIPT).exists());
    assert!(!harness.data_root.join("install.json").exists());
    assert!(!harness.data_root.join("pro").exists());

    let host_identity = Command::cargo_bin("ctx")
        .unwrap()
        .arg("_release-build-identity")
        .output()
        .unwrap();
    let release_identity = std::process::Command::new(release)
        .arg("_release-build-identity")
        .output()
        .unwrap();
    assert_success(&host_identity);
    assert_success(&release_identity);
    assert_eq!(host_identity.stdout, release_identity.stdout);
    assert_eq!(host_identity.stderr, release_identity.stderr);

    let cleanup_root = harness.root.path().to_path_buf();
    drop(harness);
    assert!(!cleanup_root.exists());
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_receipt(receipt: &Value, operation: &str, browser_results: &[&str], clock_calls: &[i64]) {
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["expected_operation"], operation);
    assert_eq!(receipt["service_calls"], json!([operation]));
    assert_eq!(receipt["vault_backend"], "isolated_process_manifest");
    assert_eq!(receipt["native_vault_calls"], 0);
    assert_eq!(receipt["network_calls"], 0);
    assert_eq!(receipt["native_browser_calls"], 0);
    assert_eq!(receipt["completed"], true);
    assert_eq!(receipt["clock_calls"], json!(clock_calls));
    assert_eq!(
        receipt["browser"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|call| call["result"].as_str().unwrap())
            .collect::<Vec<_>>(),
        browser_results
    );
}

fn canonical_json(value: Value) -> Vec<u8> {
    fn sorted(value: Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.into_iter().map(sorted).collect()),
            Value::Object(values) => {
                let mut entries = values.into_iter().collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                let mut object = serde_json::Map::new();
                for (key, value) in entries {
                    object.insert(key, sorted(value));
                }
                Value::Object(object)
            }
            value => value,
        }
    }
    let mut bytes = serde_json::to_vec(&sorted(value)).unwrap();
    bytes.push(b'\n');
    bytes
}

fn write_private(path: &Path, bytes: &[u8]) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}
