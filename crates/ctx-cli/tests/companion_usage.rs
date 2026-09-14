#![cfg(unix)]

use std::{fs, os::unix::fs::PermissionsExt as _, path::Path, process::Command};

#[test]
fn paid_blame_wrapper_records_after_companion_exit_and_remains_controlled() {
    let temp = tempfile::tempdir().unwrap();
    let pro = temp.path().join("ctx-pro");
    fs::write(
        &pro,
        b"#!/bin/sh\nif [ \"$1\" = \"--ctx-pro-protocol-v3\" ] && [ \"$2\" = \"handshake\" ]; then\n  printf '{\"protocol_version\":3}\\n'\n  exit 0\nfi\n[ \"$1\" = \"--ctx-pro-protocol-v3\" ] && [ \"$2\" = \"cli\" ] && exit 0\nexit 91\n",
    )
    .unwrap();
    fs::set_permissions(&pro, fs::Permissions::from_mode(0o700)).unwrap();

    let command = |root: &Path| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ctx"));
        command
            .env_clear()
            .env("HOME", temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .env("XDG_DATA_HOME", temp.path().join("data"))
            .env("XDG_STATE_HOME", temp.path().join("state"))
            .env("XDG_RUNTIME_DIR", temp.path())
            .env("TMPDIR", temp.path())
            .env("CTX_ANALYTICS_ENABLED", "false")
            .env("CTX_DAEMON_AUTOSTART_OFF", "1")
            .env("CTX_PRO_PATH", &pro)
            .arg("--data-root")
            .arg(root)
            .arg("blame")
            .arg("opaque-target");
        command
    };

    let enabled = temp.path().join("enabled");
    fs::create_dir(&enabled).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&enabled).unwrap();
    let status = command(&enabled).status().unwrap();
    assert!(status.success());

    let authority = ctx_client_observability::local_usage::LocalUsageStorageAuthority::new(
        enabled.join("usage.sqlite"),
        "1.0.0",
    );
    let report = ctx_client_observability::local_usage::read_report_authorized(
        &authority,
        &ctx_client_observability::local_usage::UsageControlSnapshot::unversioned(true),
        true,
    );
    let definition = &report.definitions.unwrap()[0];
    assert_eq!(definition.definition_version, 3);
    assert_eq!(definition.summary.calls, 1);
    assert_eq!(definition.summary.successful_calls, 1);
    assert_eq!(definition.summary.not_applicable_calls, 1);
    assert_eq!(definition.summary.result_count, 0);
    assert_eq!(definition.summary.delivered_output_bytes, 0);

    let disabled = temp.path().join("disabled");
    fs::create_dir(&disabled).unwrap();
    ctx_history_platform::platform_security::restrict_private_directory(&disabled).unwrap();
    let status = command(&disabled)
        .env("CTX_LOCAL_USAGE_ENABLED", "false")
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!disabled.join("usage.sqlite").exists());
}
