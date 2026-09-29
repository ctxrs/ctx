#[path = "hosted_history/support.rs"]
mod support;

#[path = "hosted_history/admin_regressions.rs"]
mod admin_regressions;

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
    assert_eq!(before["health"]["collections"], 1);
    let backup = success(
        sandbox
            .server()
            .args(["backup", "--output"])
            .arg(sandbox.path("checkpoint"))
            .arg("--format=json"),
    );
    assert_eq!(backup["off_host_copy_verified"], false);

    let malformed = sandbox.path("not-a-checkpoint.json");
    fs::write(&malformed, b"{}").unwrap();
    let error = failure(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .arg("restore")
            .arg(&malformed)
            .arg("--format=json"),
    );
    assert_eq!(error["error"]["code"], "server_error");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains(malformed.to_str().unwrap()));
    assert!(message.contains("storage error"));
    assert!(message.contains("checkpoint directory"));
    assert!(message.contains("ctx server backup"));
    assert!(!sandbox.path("recovered").exists());

    let damaged = sandbox.path("damaged-checkpoint");
    fs::create_dir(&damaged).unwrap();
    fs::copy(
        sandbox.path("checkpoint/checkpoint.json"),
        damaged.join("checkpoint.json"),
    )
    .unwrap();
    let mut catalog = fs::read(sandbox.path("checkpoint/authority.sqlite")).unwrap();
    catalog[0] ^= 1;
    fs::write(damaged.join("authority.sqlite"), catalog).unwrap();
    let error = failure(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .arg("restore")
            .arg(&damaged)
            .arg("--format=json"),
    );
    assert_eq!(error["error"]["code"], "invalid_request");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains(damaged.to_str().unwrap()));
    assert!(message.contains("invalid request: checkpoint catalog checksum mismatch"));
    assert!(message.contains("checkpoint directory"));
    assert!(message.contains("ctx server backup"));
    assert!(!sandbox.path("recovered").exists());
    // Correcting only the input succeeds at the same destination without repair.
    let restored = success(
        sandbox
            .command()
            .args(["server", "--root"])
            .arg(sandbox.path("recovered"))
            .arg("restore")
            .arg(sandbox.path("checkpoint"))
            .arg("--format=json"),
    );
    assert_eq!(restored["previous_access_revoked"], true);
    assert_ne!(restored["principal"], initialized["principal"]);
    assert_eq!(restored["collection"], initialized["collection"]);
    let fresh: Value =
        serde_json::from_slice(&fs::read(sandbox.path("recovered/operator.json")).unwrap())
            .unwrap();
    assert_ne!(
        fresh["credential"]["secret"],
        secret["credential"]["secret"]
    );
    let server = sandbox.start_server_at(
        &sandbox.path("recovered"),
        &sandbox.path("recovered/operator.json"),
    );
    for (name, path) in [
        ("old", "operator.json"),
        ("fresh", "recovered/operator.json"),
    ] {
        success(
            sandbox
                .command()
                .args([
                    "remote",
                    "connect",
                    &server.endpoint,
                    "--name",
                    name,
                    "--token-file",
                ])
                .arg(sandbox.path(path))
                .arg("--format=json"),
        );
    }
    failure(
        sandbox
            .command()
            .args(["remote", "status", "old", "--online", "--format=json"]),
    );
    let status =
        success(
            sandbox
                .command()
                .args(["remote", "status", "fresh", "--online", "--format=json"]),
        );
    assert_eq!(status["server"]["stored_sequence"], 0);
    sandbox.assert_no_local_index();
}

#[test]
fn authority_file_and_nested_user_invite_are_removed() {
    let sandbox = Sandbox::new();
    for args in [
        vec!["server", "init", "--authority-file", "unused"],
        vec!["server", "run", "--authority-file", "unused"],
        vec!["server", "restore", "unused", "--authority-file", "unused"],
        vec!["server", "user", "invite", "alice"],
    ] {
        sandbox.command().args(args).assert().failure();
    }
    assert!(!sandbox.path("history").exists());
}

#[test]
fn oversized_credential_input_is_bounded_without_saving_state() {
    let sandbox = Sandbox::new();
    let input = "synthetic-too-long".repeat(1100);
    let error = failure(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "https://history.invalid",
                "--collection",
                "example",
                "--token-file=-",
                "--format=json",
            ])
            .write_stdin(input),
    );
    assert!(error["error"]["message"].as_str().unwrap().contains("size"));
    assert!(!error.to_string().contains("synthetic-too-long"));
    assert!(!sandbox.path("history").exists());
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
fn default_init_is_private_repeatable_and_uses_native_data_root() {
    let sandbox = Sandbox::new();
    let first = success(sandbox.command().env_remove("CTX_DATA_ROOT").args([
        "server",
        "init",
        "--format=json",
    ]));
    let root = sandbox.path("home/.ctx/server");
    assert_eq!(first["root"], root.to_str().unwrap());
    assert_eq!(first["initialized"], true);
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["feature_maturity"], "beta");
    assert_eq!(first["sharing"], "opt_in");
    let credential_path = root.join("operator.json");
    let credential = fs::read(&credential_path).unwrap();
    let saved = fs::read(root.join("admin/settings.json")).unwrap();
    ctx_history_platform::platform_security::verify_private_file(&credential_path).unwrap();
    ctx_history_platform::platform_security::verify_private_file(&root.join("admin/settings.json"))
        .unwrap();
    let repeated = success(sandbox.command().env_remove("CTX_DATA_ROOT").args([
        "server",
        "init",
        "--format=json",
    ]));
    assert_eq!(repeated["initialized"], false);
    assert_eq!(repeated["principal"], first["principal"]);
    assert_eq!(repeated["collection"], first["collection"]);
    assert_eq!(fs::read(credential_path).unwrap(), credential);
    assert_eq!(fs::read(root.join("admin/settings.json")).unwrap(), saved);
    assert!(!sandbox.path("home/.ctx/search").exists());
    assert!(!sandbox.path("home/.ctx/daemon").exists());
    assert!(!sandbox.path("home/.ctx/config.toml").exists());
}

#[test]
fn repeated_init_rejects_scoped_owner_credential_without_replacing_admin_access() {
    let sandbox = Sandbox::new();
    let initialized = sandbox.init();
    let operator_path = sandbox.path("operator.json");
    let original = fs::read(&operator_path).unwrap();
    let admin_path = sandbox.path("server/admin/settings.json");
    let admin = fs::read(&admin_path).unwrap();
    let pointer_path = sandbox.path("server/operator-file.json");
    let pointer = fs::read(&pointer_path).unwrap();

    // Even the owner's own read+publish+manage credential remains scoped to
    // its collection. Put it at the saved operator path so a path mismatch
    // cannot mask an incorrect publication-list authorization check.
    success(
        sandbox
            .server()
            .args([
                "user",
                "credential",
                initialized["principal"].as_str().unwrap(),
                "--read",
                "--publish",
                "--manage",
                "--output",
            ])
            .arg(sandbox.path("scoped-owner.json"))
            .arg("--format=json"),
    );
    let mut scoped: Value = serde_json::from_slice(&original).unwrap();
    scoped["credential"] =
        serde_json::from_slice(&fs::read(sandbox.path("scoped-owner.json")).unwrap()).unwrap();
    let scoped_bytes = serde_json::to_vec(&scoped).unwrap();
    fs::write(&operator_path, &scoped_bytes).unwrap();
    ctx_history_platform::platform_security::verify_private_file(&operator_path).unwrap();

    let error = failure(sandbox.server().args(["init", "--format=json"]));
    assert_eq!(error["error"]["code"], "forbidden");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("server-owner authority"));
    assert!(!error
        .to_string()
        .contains(scoped["credential"]["secret"].as_str().unwrap()));
    assert_eq!(fs::read(&operator_path).unwrap(), scoped_bytes);
    assert_eq!(fs::read(&admin_path).unwrap(), admin);
    assert_eq!(fs::read(&pointer_path).unwrap(), pointer);

    // Restoring the genuine protected bootstrap file is sufficient; retrying
    // init preserves the original identity, credential and admin connection.
    fs::write(&operator_path, &original).unwrap();
    let repeated = success(sandbox.server().args(["init", "--format=json"]));
    assert_eq!(repeated["initialized"], false);
    assert_eq!(repeated["principal"], initialized["principal"]);
    assert_eq!(repeated["collection"], initialized["collection"]);
    assert_eq!(fs::read(operator_path).unwrap(), original);
    assert_eq!(fs::read(admin_path).unwrap(), admin);
    assert_eq!(fs::read(pointer_path).unwrap(), pointer);
    sandbox.assert_no_local_index();
}

#[test]
fn first_init_and_help_explain_beta_and_opt_in_without_repeating_warnings() {
    let sandbox = Sandbox::new();
    let note = "Hosted history is beta. Sharing is opt-in.";
    sandbox
        .server()
        .args(["init", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains(note));
    assert!(!sandbox.path("server").exists());
    sandbox
        .server()
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains(note));
    let original = fs::read(sandbox.path("server/operator.json")).unwrap();
    for command in ["init", "status"] {
        sandbox
            .server()
            .arg(command)
            .assert()
            .success()
            .stdout(predicate::str::contains(note).not());
    }
    assert_eq!(
        fs::read(sandbox.path("server/operator.json")).unwrap(),
        original
    );
    sandbox.assert_no_local_index();
}

#[test]
fn invitation_text_shows_the_returned_utc_deadline_without_the_secret() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let _running = sandbox.start_server();
    let path = sandbox.path("deadline-invitation.json");
    let output = sandbox
        .server()
        .args([
            "invite",
            "deadline-member",
            "--ttl-seconds",
            "60",
            "--output",
        ])
        .arg(&path)
        .assert()
        .success()
        .get_output()
        .clone();
    let invitation: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let deadline = text
        .lines()
        .find_map(|line| line.strip_prefix("Enrollment expires at "))
        .unwrap()
        .strip_suffix(" UTC.")
        .unwrap();
    let parsed = chrono::NaiveDateTime::parse_from_str(deadline, "%Y-%m-%d %H:%M:%S")
        .unwrap()
        .and_utc()
        .timestamp();
    assert_eq!(
        parsed as u64,
        invitation["enrollment"]["expires_at"].as_u64().unwrap()
    );
    let secret = invitation["enrollment"]["secret"].as_str().unwrap();
    assert!(!text.contains(secret));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    sandbox.assert_no_local_index();
}

#[test]
fn explicit_data_root_works_without_home_and_preserves_existing_credentials_on_error() {
    let sandbox = Sandbox::new();
    let initialized = success(
        sandbox
            .command()
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .args(["server", "init", "--format=json"]),
    );
    assert_eq!(
        initialized["root"],
        sandbox.path("history/server").to_str().unwrap()
    );
    let original = fs::read(sandbox.path("history/server/operator.json")).unwrap();
    failure(
        sandbox
            .command()
            .args(["server", "init", "--credentials-out"])
            .arg(sandbox.path("missing/operator.json"))
            .arg("--format=json"),
    );
    assert_eq!(
        fs::read(sandbox.path("history/server/operator.json")).unwrap(),
        original
    );
    sandbox.assert_no_local_index();
}

#[test]
fn connect_without_explicit_input_fails_promptly_in_scripts_and_redacts_bad_inputs() {
    let sandbox = Sandbox::new();
    let error = failure(
        sandbox
            .command()
            .timeout(std::time::Duration::from_secs(3))
            .args([
                "remote",
                "connect",
                "https://history.invalid",
                "--format=json",
            ]),
    );
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--enrollment-file"));
    for input in [
        "{\"secret\":\"synthetic-never-print-this\",",
        "two synthetic-never-print-this tokens\n",
    ] {
        let error = failure(
            sandbox
                .command()
                .args([
                    "remote",
                    "connect",
                    "https://history.invalid",
                    "--enrollment-file=-",
                    "--format=json",
                ])
                .write_stdin(input),
        );
        assert!(!error.to_string().contains("synthetic-never-print-this"));
    }
    failure(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "https://history.invalid",
                "--enrollment-file",
            ])
            .arg(sandbox.path("absent/invitation.json"))
            .arg("--format=json"),
    );
    sandbox
        .command()
        .args([
            "remote",
            "connect",
            "https://history.invalid",
            "--name",
            "../outside",
            "--token-file=-",
            "--collection",
            "test",
            "--format=json",
        ])
        .write_stdin("synthetic-never-print-this")
        .assert()
        .failure();
    assert!(!sandbox.path("history").exists());
}

#[test]
fn token_envelopes_allow_explicit_other_collection_without_enabling_sharing() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let connected = success(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                "https://history.invalid",
                "--collection",
                "another-authorized-collection",
                "--token-file",
            ])
            .arg(sandbox.path("operator.json"))
            .arg("--format=json"),
    );
    assert_eq!(connected["name"], "team");
    assert_eq!(
        connected["connection"]["collection"],
        "another-authorized-collection"
    );
    assert_eq!(connected["local"]["enabled"], false);
    assert_eq!(connected["local"]["pending"], 0);
    sandbox.assert_no_local_index();
}

#[test]
fn custom_bind_updates_admin_endpoint_without_rotating_identity() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let original = fs::read(sandbox.path("operator.json")).unwrap();
    let repeated = success(sandbox.server().args(["init", "--format=json"]));
    assert_eq!(
        repeated["credentials_file"],
        sandbox.path("operator.json").to_str().unwrap()
    );
    assert_eq!(fs::read(sandbox.path("operator.json")).unwrap(), original);
    let server = sandbox.start_server();
    let invite = success(
        sandbox
            .server()
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove("CTX_DATA_ROOT")
            .args(["invite", "alice", "--format=json"]),
    );
    assert!(invite["file"]
        .as_str()
        .unwrap()
        .starts_with(sandbox.path("server/invitations").to_str().unwrap()));
    assert_eq!(fs::read(sandbox.path("operator.json")).unwrap(), original);
    let member = Sandbox::new();
    let connected = success(member.command().args([
        "remote",
        "connect",
        &server.endpoint,
        "--enrollment-file",
        invite["file"].as_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(connected["connection"]["collection"], invite["collection"]);
    assert_eq!(connected["local"]["enabled"], false);
    member.assert_no_local_index();
    sandbox.assert_no_local_index();
    let endpoint = server.endpoint.clone();
    drop(server);
    let repeated = success(sandbox.server().args(["init", "--format=json"]));
    assert_eq!(repeated["admin_endpoint"], endpoint);
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
    let denied = failure(
        sandbox
            .server()
            .args(["user", "credential", user, "--read", "--output"])
            .arg(sandbox.path("reader.json"))
            .arg("--format=json"),
    );
    assert_eq!(denied["error"]["code"], "forbidden");
    assert!(!sandbox.path("reader.json").exists());
    success(
        sandbox
            .server()
            .args(["grant", "--user", user, "--read", "--format=json"]),
    );
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
                "https://history.invalid",
                "--name",
                "team",
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
                "http://127.0.0.1:7332",
                "--name",
                "operator",
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
            "https://history.invalid",
            "--name",
            "reader",
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
                "https://history.invalid",
                "--name",
                "team",
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
                "https://history.invalid",
                "--name",
                "team",
                "--collection",
                "test",
                "--token-file",
            ])
            .arg(sandbox.path("linked-token"))
            .arg("--format=json"),
    );
    assert!(!sandbox.path("history").exists());
}
