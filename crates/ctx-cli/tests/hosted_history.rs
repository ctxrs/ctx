#[path = "hosted_history/support.rs"]
mod support;

use std::fs;

use predicates::prelude::*;
use serde_json::Value;
use support::{failure, success, Sandbox};

#[test]
fn server_bootstrap_and_checkpoint_restore_keep_local_history_untouched() {
    let sandbox = Sandbox::new();
    let initialized = sandbox.init();
    assert!(initialized["collection"].as_str().is_some());
    let secret: Value =
        serde_json::from_slice(&fs::read(sandbox.path("operator.json")).unwrap()).unwrap();
    let bearer = secret["credential"]["secret"].as_str().unwrap();
    assert!(!initialized.to_string().contains(bearer));
    ctx_history_platform::platform_security::verify_private_file(&sandbox.path("operator.json"))
        .unwrap();

    let before = success(sandbox.server().args(["status", "--format=json"]));
    assert_eq!(before["health"]["recovery_closed"], false);
    assert_eq!(before["health"]["collections"], 1);
    let backup = success(
        sandbox
            .server()
            .args(["backup", "--output"])
            .arg(sandbox.path("checkpoint"))
            .arg("--format=json"),
    );
    assert_eq!(backup["off_host_copy_verified"], false);

    let restored = success(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .arg("restore")
            .arg(sandbox.path("checkpoint"))
            .arg("--format=json"),
    );
    assert_eq!(restored["recovery_closed"], true);
    let after = success(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .args(["status", "--format=json"]),
    );
    assert_eq!(after["health"]["recovery_closed"], true);
    let error = failure(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .args(["user", "create", "blocked", "--format=json"]),
    );
    assert_eq!(error["error"]["code"], "recovery_closed");
    sandbox.assert_no_local_index();
}

#[test]
fn matching_authority_reopens_a_checkpoint_but_later_mutations_block_stale_restore() {
    let sandbox = Sandbox::new();
    let authority = sandbox.path("independent-authority.json");
    success(
        sandbox
            .server()
            .args(["init", "recovery-team", "--credentials-out"])
            .arg(sandbox.path("operator.json"))
            .arg("--authority-file")
            .arg(&authority)
            .arg("--format=json"),
    );
    success(
        sandbox
            .server()
            .args(["backup", "--output"])
            .arg(sandbox.path("checkpoint"))
            .arg("--format=json"),
    );
    let restored = success(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .arg("restore")
            .arg(sandbox.path("checkpoint"))
            .arg("--authority-file")
            .arg(&authority)
            .arg("--format=json"),
    );
    assert_eq!(restored["recovery_closed"], false);
    let health = success(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .args(["status", "--format=json"]),
    );
    assert_eq!(health["health"]["recovery_closed"], false);

    // The original configured authority path is reused without passing it again.
    success(
        sandbox
            .server()
            .args(["user", "create", "after-checkpoint", "--format=json"]),
    );
    let error = failure(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("stale"))
            .arg("restore")
            .arg(sandbox.path("checkpoint"))
            .arg("--authority-file")
            .arg(&authority)
            .arg("--format=json"),
    );
    assert_eq!(error["error"]["code"], "recovery_closed");
    assert!(!sandbox.path("stale").exists());
    sandbox.assert_no_local_index();
}

#[test]
fn server_status_does_not_create_an_uninitialized_root() {
    let sandbox = Sandbox::new();
    failure(sandbox.server().args(["status", "--format=json"]));
    assert!(!sandbox.path("server").exists());
    assert!(!sandbox.path("history").exists());
    sandbox.assert_no_local_index();
}

#[test]
fn admin_credential_is_private_and_existing_output_is_not_overwritten() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let user = success(
        sandbox
            .server()
            .args(["user", "create", "reader", "--format=json"]),
    );
    let user = user["user"].as_str().unwrap();
    let credential = success(
        sandbox
            .server()
            .args(["user", "credential", user, "--read", "--output"])
            .arg(sandbox.path("reader.json"))
            .arg("--format=json"),
    );
    let saved = fs::read(sandbox.path("reader.json")).unwrap();
    let secret: Value = serde_json::from_slice(&saved).unwrap();
    assert!(!credential
        .to_string()
        .contains(secret["secret"].as_str().unwrap()));
    assert_eq!(credential["grants"]["read"], true);
    assert_eq!(credential["grants"]["publish"], false);
    ctx_history_platform::platform_security::verify_private_file(&sandbox.path("reader.json"))
        .unwrap();
    failure(
        sandbox
            .server()
            .args(["user", "credential", user, "--read", "--output"])
            .arg(sandbox.path("reader.json"))
            .arg("--format=json"),
    );
    assert_eq!(fs::read(sandbox.path("reader.json")).unwrap(), saved);
    sandbox.assert_no_local_index();
}

#[test]
fn connect_offline_stores_no_policy_and_ignores_local_config_errors() {
    let sandbox = Sandbox::new();
    fs::create_dir(sandbox.path("history")).unwrap();
    fs::write(
        sandbox.path("history/config.toml"),
        "deliberately invalid local config [",
    )
    .unwrap();
    let token = sandbox.private_file("token", b"synthetic-member-credential\n");
    let connected = success(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "team",
                "--url",
                "https://history.invalid",
                "--collection",
                "synthetic-collection",
                "--token-file",
            ])
            .arg(token)
            .arg("--format=json"),
    );
    assert!(!connected
        .to_string()
        .contains("synthetic-member-credential"));
    let status = success(
        sandbox
            .command()
            .args(["remote", "status", "team", "--format=json"]),
    );
    assert_eq!(status["local"]["connected"], true);
    assert_eq!(status["local"]["enabled"], false);
    assert_eq!(status["local"]["pending"], 0);
    assert!(status["server"].is_null());
    sandbox.assert_no_local_index();
}

#[test]
fn raw_stdin_and_bootstrap_envelopes_are_accepted_without_printing_secrets() {
    let sandbox = Sandbox::new();
    let initialized = sandbox.init();
    let collection = initialized["collection"].as_str().unwrap();
    success(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "operator",
                "--url",
                "http://127.0.0.1:7332",
                "--collection",
                collection,
                "--token-file",
            ])
            .arg(sandbox.path("operator.json"))
            .arg("--format=json"),
    );
    let output = sandbox
        .command()
        .args([
            "remote",
            "connect",
            "reader",
            "--url",
            "https://history.invalid",
            "--collection",
            collection,
            "--token-file=-",
            "--read-only",
            "--format=json",
        ])
        .write_stdin("synthetic-stdin-credential\n")
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(!String::from_utf8_lossy(&output.stdout).contains("synthetic-stdin-credential"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-stdin-credential"));
    sandbox.assert_no_local_index();
}

#[test]
fn unknown_remote_status_is_observational_and_names_cannot_escape_storage() {
    let sandbox = Sandbox::new();
    let status = success(
        sandbox
            .command()
            .args(["remote", "status", "missing", "--format=json"]),
    );
    assert_eq!(status["local"]["connected"], false);
    assert!(!sandbox.path("history").exists());
    failure(
        sandbox
            .command()
            .args(["remote", "status", "../outside", "--format=json"]),
    );
    assert!(!sandbox.path("outside").exists());
    sandbox.assert_no_local_index();
}

#[test]
fn share_requires_explicit_source_backfill_and_payload_scope() {
    let sandbox = Sandbox::new();
    sandbox
        .command()
        .args(["remote", "share", "team", "--mode", "automatic"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("required"));
    assert!(!sandbox.path("history").exists());
}

#[cfg(unix)]
#[test]
fn permissive_and_symlinked_credentials_are_rejected_before_saving_connection() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let sandbox = Sandbox::new();
    let token = sandbox.private_file("token", b"synthetic-secret\n");
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();
    failure(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "team",
                "--url",
                "https://history.invalid",
                "--collection",
                "test",
                "--token-file",
            ])
            .arg(&token)
            .arg("--format=json"),
    );
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&token, sandbox.path("linked-token")).unwrap();
    failure(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "team",
                "--url",
                "https://history.invalid",
                "--collection",
                "test",
                "--token-file",
            ])
            .arg(sandbox.path("linked-token"))
            .arg("--format=json"),
    );
    assert!(!sandbox.path("history").exists());
}
