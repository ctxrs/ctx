use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::tempdir;

fn helper_name() -> &'static str {
    if cfg!(windows) {
        "ctx-pro.exe"
    } else {
        "ctx-pro"
    }
}

#[cfg(unix)]
fn protect_pro_directory(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, PermissionsExt::from_mode(0o700)).unwrap();
}

#[cfg(not(unix))]
fn protect_pro_directory(_path: &std::path::Path) {}

#[test]
fn ctx_status_has_one_actionable_pro_machine_contract() {
    let root = tempdir().unwrap();
    let output = Command::cargo_bin("ctx")
        .unwrap()
        .arg("--data-root")
        .arg(root.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let value = &status["pro"];
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["payload_type"], "pro_status");
    assert_eq!(value["state"], "not_setup");
    assert_eq!(value["next_action"]["command"], "ctx pro");
    assert_eq!(value["access_state"], serde_json::Value::Null);
    assert_eq!(value["refresh_after_unix"], serde_json::Value::Null);
    assert_eq!(value["access_deadline_unix"], serde_json::Value::Null);
    assert_eq!(value["grace_deadline_unix"], serde_json::Value::Null);
    assert!(value.get("helper_path").is_none());
}

#[test]
fn commercial_channel_selection_is_exact_and_stable_fails_closed() {
    let root = tempdir().unwrap();
    for arguments in [vec!["pro", "--json"], vec!["pro", "setup", "--json"]] {
        for value in ["", "production", "Staging", "1"] {
            Command::cargo_bin("ctx")
                .unwrap()
                .env("CTX_PRO_CHANNEL", value)
                .arg("--data-root")
                .arg(root.path())
                .args(&arguments)
                .assert()
                .failure()
                .stderr(predicate::str::contains(
                    "CTX_PRO_CHANNEL must be stable or staging",
                ));
        }
    }

    for arguments in [vec!["pro", "--json"], vec!["pro", "setup", "--json"]] {
        Command::cargo_bin("ctx")
            .unwrap()
            .env_remove("CTX_PRO_CHANNEL")
            .arg("--data-root")
            .arg(root.path())
            .args(arguments)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "ctx Pro stable channel is not configured",
            ));
    }
}

#[test]
fn ordinary_uninstall_is_local_without_commercial_configuration_or_vault() {
    let root = tempdir().unwrap();
    let helper = root.path().join("pro").join("bin").join(helper_name());
    fs::create_dir_all(helper.parent().unwrap()).unwrap();
    protect_pro_directory(&root.path().join("pro"));
    fs::write(&helper, b"helper").unwrap();
    let graph = root.path().join("pro").join("ctx-pro.db");
    fs::write(&graph, b"encrypted graph").unwrap();

    let output = Command::cargo_bin("ctx")
        .unwrap()
        .env_remove("CTX_PRO_CHANNEL")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("XDG_RUNTIME_DIR")
        .arg("--data-root")
        .arg(root.path())
        .args(["pro", "uninstall", "--keep-data", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["local_pro_data"], "preserved");
    assert!(value.get("credentials_preserved").is_none());
    assert!(value.get("graph_preserved").is_none());
    assert!(value.get("data_deleted").is_none());
    assert_eq!(value["canonical_history_preserved"], true);
    assert_eq!(value["next_action"]["command"], "ctx pro");
    assert_eq!(value["next_action"]["reason"], "restore_preserved_pro_data");

    assert!(!helper.exists());
    assert!(graph.is_file());

    let status = Command::cargo_bin("ctx")
        .unwrap()
        .arg("--data-root")
        .arg(root.path())
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    let status = &status["pro"];
    assert_eq!(status["state"], "uninstalled_data_preserved");
    assert_eq!(status["installed"], false);
    assert_eq!(
        status["next_action"]["reason"],
        "restore_preserved_pro_data"
    );
}

#[test]
fn never_pro_uninstall_is_a_truthful_noop_for_missing_and_empty_roots() {
    for (root_kind, create_root) in [("missing", false), ("empty", true)] {
        for choice in ["--delete-data", "--keep-data"] {
            let parent = tempdir().unwrap();
            let data_root = parent.path().join(root_kind);
            if create_root {
                ctx_history_core::platform_security::create_private_directory_all(&data_root)
                    .unwrap();
                fs::write(
                    data_root.join("install.json"),
                    br#"{
  "schema_version": 1,
  "install_id": "6a1de1ab-c732-45ed-b3f8-bbf6ab1048e8",
  "created_at": "2026-07-23T00:00:00Z"
}"#,
                )
                .unwrap();
                fs::write(data_root.join("work.sqlite"), b"canonical history").unwrap();
            }

            let output = Command::cargo_bin("ctx")
                .unwrap()
                .env("CTX_ANALYTICS_ENABLED", "false")
                .env("CTX_LOCAL_USAGE_ENABLED", "true")
                .env_remove("DBUS_SESSION_BUS_ADDRESS")
                .env_remove("XDG_RUNTIME_DIR")
                .arg("--data-root")
                .arg(&data_root)
                .args(["pro", "uninstall", choice, "--json"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{root_kind} {choice}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(value["local_pro_data"], "absent");
            assert_eq!(value["helper_removed"], false);
            assert_eq!(value["next_action"], serde_json::Value::Null);
            assert!(
                !data_root.join("pro").exists(),
                "{root_kind} {choice} created a Pro root"
            );
            let usage_path = data_root.join("usage.sqlite");
            assert!(
                usage_path.is_file(),
                "{root_kind} {choice} did not retain the eligible Core usage completion"
            );
            let usage = Connection::open(usage_path).unwrap();
            let (operation, calls): (String, u64) = usage
                .query_row("SELECT operation, calls FROM daily_usage", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .unwrap();
            assert_eq!(operation, "pro_uninstall");
            assert_eq!(calls, 1);
            if create_root {
                assert_eq!(
                    fs::read(data_root.join("work.sqlite")).unwrap(),
                    b"canonical history"
                );
            } else {
                assert!(data_root.is_dir());
            }
        }
    }
}

#[test]
fn pro_help_documents_bare_setup_and_the_explicit_synonym() {
    Command::cargo_bin("ctx")
        .unwrap()
        .args(["pro", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Bare `ctx pro` runs the idempotent setup path",
        ))
        .stdout(predicate::str::contains(
            "setup      Explicit synonym for `ctx pro`",
        ))
        .stdout(predicate::str::contains(
            "`ctx status` does not mutate canonical history or graph data; entitlement authorization may advance nonsecret anti-clock-rollback metadata",
        ));
}

#[test]
fn noninteractive_uninstall_requires_an_explicit_data_choice() {
    let root = tempdir().unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .arg("--data-root")
        .arg(root.path())
        .args(["pro", "uninstall"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "noninteractive uninstall requires --delete-data or --keep-data",
        ));

    Command::cargo_bin("ctx")
        .unwrap()
        .arg("--data-root")
        .arg(root.path())
        .args(["pro", "uninstall", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "noninteractive uninstall requires --delete-data or --keep-data",
        ));
}

#[test]
fn uninstall_data_choice_flags_are_mutually_exclusive() {
    let root = tempdir().unwrap();
    Command::cargo_bin("ctx")
        .unwrap()
        .arg("--data-root")
        .arg(root.path())
        .args(["pro", "uninstall", "--delete-data", "--keep-data", "--json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot be used with '--keep-data'",
        ));
}

#[test]
fn delete_data_fails_closed_without_local_deletion_identity() {
    let root = tempdir().unwrap();
    let helper = root.path().join("pro").join("bin").join(helper_name());
    fs::create_dir_all(helper.parent().unwrap()).unwrap();
    protect_pro_directory(&root.path().join("pro"));
    fs::write(&helper, b"helper").unwrap();
    let graph = root.path().join("pro").join("ctx-pro.db");
    fs::write(&graph, b"encrypted graph").unwrap();

    Command::cargo_bin("ctx")
        .unwrap()
        .env("CTX_PRO_CHANNEL", "not-a-channel")
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("XDG_RUNTIME_DIR")
        .arg("--data-root")
        .arg(root.path())
        .args(["pro", "uninstall", "--delete-data"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("key_store_unavailable"));

    assert!(helper.is_file());
    assert!(graph.is_file());
}

#[test]
fn obsolete_and_manual_repository_lifecycle_flags_are_rejected() {
    let root = tempdir().unwrap();
    for arguments in [
        vec!["pro", "status"],
        vec!["pro", "install"],
        vec!["pro", "update"],
        vec!["pro", "setup", "--repo", "/tmp/repo"],
    ] {
        Command::cargo_bin("ctx")
            .unwrap()
            .arg("--data-root")
            .arg(root.path())
            .args(arguments)
            .assert()
            .failure();
    }
}

#[test]
fn default_disabled_referrals_fail_before_identity_auth_or_browser_side_effects() {
    for arguments in [
        vec!["referral", "create", "agent-smith", "--json"],
        vec!["referral", "status", "--json"],
        vec!["referral", "payout", "--json"],
        vec!["pro", "--referral", "agent-smith", "--json"],
    ] {
        let parent = tempdir().unwrap();
        let data_root = parent.path().join("missing-data-root");
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .env("CTX_PRO_CHANNEL", "staging")
            .arg("--data-root")
            .arg(&data_root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("referral_unavailable:"));
        assert!(!stderr.contains("authentication_required:"));
        assert!(!stderr.contains("Sign in to ctx Pro"));
        assert!(!stderr.contains("http://"));
        assert!(!stderr.contains("https://"));
        assert!(
            !data_root.join("pro").exists(),
            "unavailable referrals must not initialize Pro credentials"
        );
    }
}

#[test]
fn invalid_referral_codenames_are_rejected_without_echoing_the_secret() {
    const INVALID_SECRET: &str = "Private_Referral_Code";
    for arguments in [
        vec!["pro", "--referral", INVALID_SECRET, "--json"],
        vec!["referral", "create", INVALID_SECRET, "--json"],
    ] {
        let parent = tempdir().unwrap();
        let data_root = parent.path().join("missing-data-root");
        let output = Command::cargo_bin("ctx")
            .unwrap()
            .arg("--data-root")
            .arg(&data_root)
            .args(arguments)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid_request: referral codename must be"));
        assert!(!stderr.contains(INVALID_SECRET));
        assert!(!data_root.exists());
    }
}

#[test]
fn referral_input_is_rejected_when_any_explicit_pro_subcommand_follows() {
    for arguments in [
        vec!["pro", "--referral", "agent-smith", "setup", "--json"],
        vec!["pro", "setup", "--referral", "agent-smith", "--json"],
        vec!["pro", "--referral", "agent-smith", "manage", "--json"],
        vec!["pro", "manage", "--referral", "agent-smith", "--json"],
        vec![
            "pro",
            "--referral",
            "agent-smith",
            "uninstall",
            "--keep-data",
            "--json",
        ],
        vec![
            "pro",
            "uninstall",
            "--referral",
            "agent-smith",
            "--keep-data",
            "--json",
        ],
    ] {
        let parent = tempdir().unwrap();
        let data_root = parent.path().join("missing-data-root");
        Command::cargo_bin("ctx")
            .unwrap()
            .arg("--data-root")
            .arg(&data_root)
            .args(&arguments)
            .assert()
            .failure()
            .stderr(
                predicate::str::contains("--referral is accepted only by bare `ctx pro`")
                    .or(predicate::str::contains("unexpected argument '--referral'")),
            );
        assert!(
            !data_root.exists(),
            "invalid referral/subcommand combination mutated local state: {arguments:?}"
        );
    }
}
