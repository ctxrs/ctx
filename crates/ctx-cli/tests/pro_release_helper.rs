#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt};

use assert_cmd::Command;
use ctx_pro_host_protocol::ProFilesystemLayout;
use tempfile::tempdir;

#[test]
fn release_binary_ignores_all_untrusted_helper_overrides() {
    let root = tempdir().unwrap();
    let helper = root.path().join("untrusted-ctx-pro");
    let executed = root.path().join("executed");
    fs::write(
        &helper,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", executed.display()),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_CHANNEL", "staging")
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "status",
            "--format=json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!executed.exists());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let status = &status["pro"];
    assert_eq!(status["installed"], false);
    assert_eq!(status["error_code"], "pro_not_installed");
    let rendered = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!rendered.contains(helper.to_str().unwrap()));
}

#[test]
fn release_binary_rejects_an_unsigned_helper_at_the_canonical_path() {
    let root = tempdir().unwrap();
    let layout = ProFilesystemLayout::new(root.path());
    let helper = layout.helper_path();
    let executed = root.path().join("canonical-helper-executed");
    fs::create_dir_all(layout.bin_dir()).unwrap();
    for directory in [
        root.path().to_path_buf(),
        layout.pro_root(),
        layout.bin_dir(),
    ] {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(
        &helper,
        format!("#!/bin/sh\n: > '{}'\nexit 99\n", executed.display()),
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_CHANNEL", "staging")
        .env("CTX_PRO_HELPER", &helper)
        .args([
            "--data-root",
            root.path().to_str().unwrap(),
            "status",
            "--format=json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(!executed.exists());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["pro"]["installed"], false);
    assert_eq!(status["pro"]["error_code"], "invalid_response");
}
