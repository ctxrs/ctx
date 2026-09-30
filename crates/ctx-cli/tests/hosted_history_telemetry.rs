#[path = "hosted_history/support.rs"]
mod support;

use std::{collections::HashSet, fs, net::TcpListener, path::PathBuf};

use assert_cmd::Command;
use serde_json::{json, Value};
use support::{failure, seed_history, success, Sandbox};

const USER: &str = "canary-private-member";
const PATH: &str = "canary-private-path";
const TOKEN: &str = "canary-private-token-not-a-real-credential";
const QUERY: &str = "canary-private-query";
const HISTORY: &str = "portable meadow observatory fixture";
const INVALID_URL: &str = "https://canary-private-host.invalid/canary-private-path?query=canary-private-query&token=canary-private-token-not-a-real-credential";

fn listener() -> TcpListener {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    listener
}

fn endpoint(listener: &TcpListener) -> String {
    format!("http://{}", listener.local_addr().unwrap())
}

fn assert_no_connection(listener: &TcpListener) {
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
        "an offline hosted operation unexpectedly opened a connection"
    );
}

fn command(sandbox: &Sandbox, telemetry: &TcpListener) -> Command {
    let mut command = sandbox.command();
    command
        .env("CTX_ANALYTICS_ENABLED", "true")
        .env("CTX_ANALYTICS_ENDPOINT", endpoint(telemetry))
        // Exercise the hosted bypass itself, without masking daemon startup.
        .env_remove("CTX_DAEMON_AUTOSTART_OFF");
    command
}

fn init_command(sandbox: &Sandbox, telemetry: &TcpListener) -> Command {
    let mut command = command(sandbox, telemetry);
    command
        .args(["server", "--root"])
        .arg(sandbox.path("server"))
        .args(["init", USER, "--credentials-out"])
        .arg(sandbox.path("operator.json"));
    command
}

fn connect_command(sandbox: &Sandbox, telemetry: &TcpListener, url: &str) -> Command {
    let token = sandbox.path("canary-private-path-token");
    if !token.exists() {
        sandbox.private_file("canary-private-path-token", TOKEN.as_bytes());
    }
    let mut command = command(sandbox, telemetry);
    command
        .args([
            "remote",
            "connect",
            url,
            "--name",
            USER,
            "--collection",
            USER,
            "--token-file",
        ])
        .arg(token)
        .arg("--format=json");
    command
}

fn device_state(sandbox: &Sandbox) -> PathBuf {
    if cfg!(target_os = "macos") {
        sandbox.path("home/Library/Application Support/ctx")
    } else {
        sandbox.path("state/ctx")
    }
}

fn foreground_enqueue_only(sandbox: &Sandbox) {
    use std::io::Write;
    let state = device_state(sandbox);
    ctx_history_platform::platform_security::create_private_directory_all(&state).unwrap();
    let mut hint = ctx_history_platform::platform_security::create_private_file_new(
        &state.join("analytics-launch-v1.json"),
    )
    .unwrap();
    // An unfamiliar private launch hint defers only the delivery child, so the
    // foreground outbox assertions cannot race a drain. Daemon startup stays unmasked.
    hint.write_all(br#"{"schema_version":2,"next_allowed_at":0}"#)
        .unwrap();
}

fn assert_private_values_absent(value: &Value, forbidden: &[&str]) {
    match value {
        Value::String(text) => {
            for canary in forbidden {
                assert!(
                    !text.contains(*canary),
                    "private canary appeared in telemetry"
                );
            }
        }
        Value::Array(values) => {
            for value in values {
                assert_private_values_absent(value, forbidden);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                assert_private_values_absent(&Value::String(key.clone()), forbidden);
                assert_private_values_absent(value, forbidden);
            }
        }
        _ => {}
    }
}

fn events(sandbox: &Sandbox, extra_canaries: &[&str]) -> Vec<Value> {
    let outbox: Value = serde_json::from_slice(
        &fs::read(device_state(sandbox).join("analytics-outbox-v1.json")).unwrap(),
    )
    .unwrap();
    let root = sandbox.root().to_string_lossy();
    let mut forbidden = vec![
        USER,
        PATH,
        TOKEN,
        QUERY,
        HISTORY,
        "canary-private-host",
        &root,
    ];
    forbidden.extend_from_slice(extra_canaries);
    let mut events = Vec::new();
    for entry in outbox["entries"].as_array().unwrap() {
        assert_eq!(entry["attempts"], 0, "hosted telemetry must only enqueue");
        let payload: Value = serde_json::from_str(entry["payload"].as_str().unwrap()).unwrap();
        assert_private_values_absent(&payload, &forbidden);
        events.extend(payload["events"].as_array().unwrap().iter().cloned());
    }
    let ids: HashSet<_> = events
        .iter()
        .map(|event| event["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), events.len(), "duplicate terminal event IDs");
    events
}

fn assert_event(event: &Value, operation: &str, outcome: &str, properties: Value) {
    assert_eq!(event["event_name"], "operation_completed");
    assert_eq!(event["event_version"], 1);
    assert_eq!(event["surface"], "cli");
    assert_eq!(event["operation"], operation);
    assert_eq!(event["outcome"], outcome);
    let mut actual = event["properties"].as_object().unwrap().clone();
    // Timing varies across processes; the wire value must be a closed bucket.
    let native_duration = actual
        .remove("native_total_duration_bucket")
        .expect("missing native duration bucket");
    assert!(matches!(
        native_duration.as_str(),
        Some(
            "lt_1ms"
                | "1ms-5ms"
                | "5ms-10ms"
                | "10ms-25ms"
                | "25ms-50ms"
                | "50ms-100ms"
                | "100ms-250ms"
                | "250ms-1s"
                | "1s-5s"
                | "5s-30s"
                | "30s-2m"
                | "2m-10m"
                | "10m-1h"
                | "1h+"
        )
    ));
    // The shared sender may attach one complete, content-free capability snapshot.
    if let Some(schema) = actual.remove("capability_snapshot_schema") {
        assert_eq!(schema, json!(1));
        let capability_fields: &[(&str, &[&str])] = &[
            (
                "available_parallelism_bucket",
                &[
                    "unknown", "1", "2", "3-4", "5-8", "9-16", "17-32", "33-64", "65+",
                ],
            ),
            (
                "host_memory_bucket",
                &[
                    "unknown", "lt_4gb", "4-8gb", "8-16gb", "16-32gb", "32-64gb", "64gb+",
                ],
            ),
            (
                "cpu_vector_tier",
                &["avx512", "avx2", "x86_baseline", "arm_neon", "other"],
            ),
            (
                "acceleration_candidate",
                &["apple_ane", "nvidia_cuda", "not_detected", "unknown"],
            ),
        ];
        for (name, allowed) in capability_fields {
            let value = actual
                .remove(*name)
                .expect("incomplete capability snapshot");
            assert!(
                value.as_str().is_some_and(|value| allowed.contains(&value)),
                "invalid capability field: {name}"
            );
        }
    }
    assert_eq!(Value::Object(actual), properties);
    assert!(matches!(
        event["duration_bucket"].as_str(),
        Some("lt_100ms" | "lt_1s" | "lt_5s" | "lt_30s" | "lt_2m" | "lt_10m" | "lt_1h" | "gte_1h")
    ));
}

fn assert_no_daemon(sandbox: &Sandbox) {
    for relative in ["history/daemon", "home/.ctx"] {
        assert!(
            !sandbox.path(relative).exists(),
            "unexpected state: {relative}"
        );
    }
    assert_eq!(fs::read_dir(sandbox.path("runtime")).unwrap().count(), 0);
}

fn archive_fixture(sandbox: &Sandbox) -> PathBuf {
    seed_history(sandbox);
    let archive = sandbox.path("snapshot");
    success(
        sandbox
            .command()
            .args(["archive", "export", "--origin", USER, "--output"])
            .arg(&archive)
            .arg("--format=json"),
    );
    archive
}

fn checkpoint_fixture(sandbox: &Sandbox) -> PathBuf {
    sandbox.init();
    let checkpoint = sandbox.path("checkpoint");
    success(
        sandbox
            .server()
            .args(["backup", "--output"])
            .arg(&checkpoint)
            .arg("--format=json"),
    );
    checkpoint
}

#[test]
fn malformed_archive_with_analytics_leaves_absent_and_empty_targets_retryable() {
    let source = Sandbox::new();
    let archive = archive_fixture(&source);
    let malformed = source.path("malformed");
    fs::create_dir(&malformed).unwrap();
    fs::write(malformed.join("manifest.json"), b"{}").unwrap();
    let telemetry = listener();
    for empty in [false, true] {
        let destination = Sandbox::new();
        foreground_enqueue_only(&destination);
        let root = destination.path("history");
        if empty {
            ctx_history_platform::platform_security::create_private_directory_all(&root).unwrap();
        }
        let rejected = failure(
            command(&destination, &telemetry)
                .args(["archive", "restore"])
                .arg(&malformed)
                .arg("--format=json"),
        );
        // The absent members directory is an archive I/O failure, before JSON parsing.
        assert_eq!(rejected["error"]["code"], "archive_io");
        assert_eq!(root.exists(), empty);
        if empty {
            assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        }
        assert!(!device_state(&destination)
            .join("analytics-outbox-v1.json")
            .exists());
        let restored = success(
            command(&destination, &telemetry)
                .args(["archive", "restore"])
                .arg(&archive)
                .arg("--format=json"),
        );
        assert_eq!(restored["receipt"]["imported_members"], 1);
        let recorded = events(&destination, &[&source.root().to_string_lossy()]);
        assert_eq!(recorded.len(), 1);
        assert_event(
            &recorded[0],
            "archive_restore",
            "success",
            json!({"output": "json", "output_delivery": "known_complete"}),
        );
        // Once owned, the same malformed input can record a failure without
        // damaging the installed identity or the restored generation.
        let identity = fs::read(root.join("install.json")).unwrap();
        failure(
            command(&destination, &telemetry)
                .args(["archive", "restore"])
                .arg(&malformed)
                .arg("--format=json"),
        );
        assert_eq!(fs::read(root.join("install.json")).unwrap(), identity);
        let retried = success(
            command(&destination, &telemetry)
                .args(["archive", "restore"])
                .arg(&archive)
                .arg("--format=json"),
        );
        assert_eq!(retried["receipt"]["imported_members"], 0);
        assert_eq!(retried["receipt"]["unchanged_members"], 1);
        let after = events(&destination, &[]);
        assert_eq!(after.len(), 3);
        assert_eq!(after[0], recorded[0]);
        assert_event(
            &after[1],
            "archive_restore",
            "failure",
            json!({
                "output": "json", "output_delivery": "unknown",
                "hosted_failure_stage": "operation", "failure_type": "io"
            }),
        );
        assert_event(
            &after[2],
            "archive_restore",
            "success",
            json!({"output": "json", "output_delivery": "known_complete"}),
        );
        assert_no_daemon(&destination);
    }
    assert_no_daemon(&source);
    assert_no_connection(&telemetry);
}

#[test]
fn server_restore_with_analytics_supports_shared_history_root_and_malformed_retry() {
    let source = Sandbox::new();
    let checkpoint = checkpoint_fixture(&source);
    let malformed = source.path("malformed-checkpoint");
    fs::create_dir(&malformed).unwrap();
    fs::write(malformed.join("checkpoint.json"), b"{malformed checkpoint").unwrap();
    let telemetry = listener();
    for same_root in [true, false] {
        for malformed_first in [false, true] {
            let destination = Sandbox::new();
            foreground_enqueue_only(&destination);
            let root = destination.path(if same_root { "history" } else { "server" });
            let restore = |input: &std::path::Path| {
                let mut cmd = command(&destination, &telemetry);
                cmd.arg("--data-root")
                    .arg(destination.path("history"))
                    .args(["server", "--root"])
                    .arg(&root)
                    .arg("restore")
                    .arg(input)
                    .arg("--format=json");
                cmd
            };
            if malformed_first {
                let rejected = failure(&mut restore(&malformed));
                assert_eq!(rejected["error"]["code"], "server_error");
                assert!(!root.exists());
                assert!(!destination.path("history").exists());
                assert!(!device_state(&destination)
                    .join("analytics-outbox-v1.json")
                    .exists());
            }
            let restored = success(&mut restore(&checkpoint));
            assert_eq!(restored["operation"], "server_restore");
            assert_eq!(restored["root"], json!(root));
            assert_eq!(restored["previous_access_revoked"], true);
            let credentials = fs::read(root.join("operator.json")).unwrap();
            let credential: Value = serde_json::from_slice(&credentials).unwrap();
            let secret = credential["credential"]["secret"].as_str().unwrap();
            let recorded = events(&destination, &[secret, &source.root().to_string_lossy()]);
            assert_eq!(recorded.len(), 1);
            assert_event(
                &recorded[0],
                "server_restore",
                "success",
                json!({"output": "json", "output_delivery": "known_complete"}),
            );
            let catalog = fs::read(root.join("authority.sqlite")).unwrap();
            let identity = fs::read(destination.path("history/install.json")).unwrap();
            let rejected = failure(&mut restore(&checkpoint));
            assert_eq!(rejected["error"]["code"], "conflict");
            assert_eq!(fs::read(root.join("authority.sqlite")).unwrap(), catalog);
            assert_eq!(fs::read(root.join("operator.json")).unwrap(), credentials);
            assert_eq!(
                fs::read(destination.path("history/install.json")).unwrap(),
                identity
            );
            let after = events(&destination, &[secret]);
            assert_eq!(after.len(), 2);
            assert_eq!(after[0], recorded[0]);
            assert_event(
                &after[1],
                "server_restore",
                "failure",
                json!({
                    "output": "json", "output_delivery": "unknown",
                    "hosted_failure_stage": "operation", "failure_type": "conflict"
                }),
            );
            destination.assert_no_local_index();
            assert_no_daemon(&destination);
        }
    }
    source.assert_no_local_index();
    assert_no_daemon(&source);
    assert_no_connection(&telemetry);
}

#[test]
fn successful_init_connect_and_archives_append_one_terminal_each_without_network() {
    let sandbox = Sandbox::new();
    foreground_enqueue_only(&sandbox);
    let telemetry = listener();
    let remote = listener();
    let url = endpoint(&remote);
    let initialized = success(init_command(&sandbox, &telemetry).arg("--format=json"));
    assert_eq!(initialized["initialized"], true);
    let credentials: Value =
        serde_json::from_slice(&fs::read(sandbox.path("operator.json")).unwrap()).unwrap();
    let secret = credentials["credential"]["secret"].as_str().unwrap();
    let canaries = [&*url, secret];
    let first = events(&sandbox, &canaries);
    assert_eq!(first.len(), 1);
    assert_event(
        &first[0],
        "server_init",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );

    let connected = success(&mut connect_command(&sandbox, &telemetry, &url));
    assert_eq!(connected["local"]["connected"], true);
    assert_eq!(connected["local"]["enabled"], false);
    let second = events(&sandbox, &canaries);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], first[0]);
    assert_event(
        &second[1],
        "remote_connect",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    sandbox.assert_no_local_index();

    // Only the explicitly seeded/restored archive fixture owns an index.
    let expected = seed_history(&sandbox);
    // The shared fixture uses fs::write; passive consent reads require a private config.
    ctx_history_platform::platform_security::restrict_private_file(
        &sandbox.path("history/config.toml"),
    )
    .unwrap();
    let archive = sandbox.path(PATH);
    let exported = success(
        command(&sandbox, &telemetry)
            .args(["archive", "export", "--origin", USER, "--output"])
            .arg(&archive)
            .arg("--format=json"),
    );
    assert_eq!(exported["manifest"]["records"], 1);
    let third = events(&sandbox, &canaries);
    assert_eq!(third.len(), 3);
    assert_eq!(&third[..2], &second);
    assert_event(
        &third[2],
        "archive_export",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    success(
        command(&sandbox, &telemetry)
            .args(["archive", "verify"])
            .arg(&archive)
            .arg("--format=json"),
    );
    let fourth = events(&sandbox, &canaries);
    assert_eq!(fourth.len(), 4);
    assert_eq!(&fourth[..3], &third);
    assert_event(
        &fourth[3],
        "archive_verify",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );

    let destination = Sandbox::new();
    foreground_enqueue_only(&destination);
    let restored = success(
        command(&destination, &telemetry)
            .args(["archive", "restore"])
            .arg(&archive)
            .arg("--format=json"),
    );
    assert_eq!(restored["receipt"]["imported_members"], 1);
    let restored_events = events(&destination, &[&url, &sandbox.root().to_string_lossy()]);
    assert_eq!(restored_events.len(), 1);
    assert_event(
        &restored_events[0],
        "archive_restore",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    let verified =
        ctx_history_index::VerifiedIndex::open_pinned(destination.path("history/search/lexical"))
            .unwrap();
    let binding = ctx_history_archive::ImportBinding {
        namespace: "personal".into(),
        identity: ctx_history_archive::ArchiveIdentity {
            origin: USER.into(),
            view: "personal".into(),
        },
    };
    let id = ctx_history_archive::mapped_event(&binding, expected.session_id, expected.event_id)
        .unwrap();
    assert_eq!(
        verified
            .core_record_by_id(id.as_uuid())
            .unwrap()
            .unwrap()
            .content,
        expected.content
    );
    assert_no_daemon(&sandbox);
    assert_no_daemon(&destination);
    assert_no_connection(&remote);
    assert_no_connection(&telemetry);
}

#[test]
fn invalid_endpoint_records_typed_failure_without_copying_arguments() {
    let sandbox = Sandbox::new();
    foreground_enqueue_only(&sandbox);
    let telemetry = listener();
    let rejected = failure(&mut connect_command(&sandbox, &telemetry, INVALID_URL));
    assert_eq!(rejected["error"]["code"], "invalid_request");
    let recorded = events(&sandbox, &[INVALID_URL]);
    assert_eq!(recorded.len(), 1);
    assert_event(
        &recorded[0],
        "remote_connect",
        "failure",
        json!({
            "output": "json", "output_delivery": "unknown",
            "hosted_failure_stage": "operation", "failure_type": "invalid_request"
        }),
    );
    // The nearest valid input succeeds with the same token and consent settings.
    let remote = listener();
    success(&mut connect_command(
        &sandbox,
        &telemetry,
        &endpoint(&remote),
    ));
    let recovered = events(&sandbox, &[&endpoint(&remote)]);
    assert_eq!(recovered.len(), 2);
    assert_eq!(recovered[0], recorded[0]);
    assert_event(
        &recovered[1],
        "remote_connect",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    sandbox.assert_no_local_index();
    assert_no_daemon(&sandbox);
    assert_no_connection(&remote);
    assert_no_connection(&telemetry);
}

#[test]
fn ordinary_member_invite_is_forbidden_but_operator_invite_succeeds() {
    let sandbox = Sandbox::new();
    let telemetry = listener();
    sandbox.init();
    let server = sandbox.start_server();
    success(
        sandbox
            .command()
            .args([
                "remote",
                "connect",
                &server.endpoint,
                "--name",
                "owner",
                "--token-file",
            ])
            .arg(sandbox.path("operator.json"))
            .arg("--format=json"),
    );
    foreground_enqueue_only(&sandbox);
    let invitation = sandbox.path("canary-private-path-invitation.json");
    let invited = success(
        command(&sandbox, &telemetry)
            .args(["server", "--remote", "owner", "invite", USER, "--output"])
            .arg(&invitation)
            .arg("--format=json"),
    );
    assert_eq!(invited["grants"]["publish"], true);
    assert_eq!(invited["grants"]["manage"], false);
    let first = events(&sandbox, &[&server.endpoint]);
    assert_eq!(first.len(), 1);
    assert_event(
        &first[0],
        "server_invite",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    success(
        command(&sandbox, &telemetry)
            .args([
                "remote",
                "connect",
                &server.endpoint,
                "--name",
                "member",
                "--enrollment-file",
            ])
            .arg(&invitation)
            .arg("--format=json"),
    );
    let second = events(&sandbox, &[&server.endpoint]);
    assert_eq!(second.len(), 2);
    assert_eq!(second[0], first[0]);
    assert_event(
        &second[1],
        "remote_connect",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    let rejected = failure(
        command(&sandbox, &telemetry)
            .args(["server", "--remote", "member", "invite", USER, "--output"])
            .arg(sandbox.path("denied-invitation.json"))
            .arg("--format=json"),
    );
    assert_eq!(rejected["error"]["code"], "forbidden");
    assert!(!sandbox.path("denied-invitation.json").exists());
    let recorded = events(&sandbox, &[&server.endpoint]);
    assert_eq!(recorded.len(), 3);
    assert_eq!(&recorded[..2], &second);
    assert_event(
        &recorded[2],
        "server_invite",
        "failure",
        json!({
            "output": "json", "output_delivery": "unknown",
            "hosted_failure_stage": "operation", "failure_type": "forbidden"
        }),
    );
    sandbox.assert_no_local_index();
    assert_no_daemon(&sandbox);
    assert_no_connection(&telemetry);
}

#[test]
fn disabled_or_unreadable_consent_creates_no_analytics_state_and_hosted_still_works() {
    for (case, config, enabled, dry_run) in [
        (
            "config opt-out",
            "[analytics]\nenabled = false\n",
            None,
            false,
        ),
        (
            "environment opt-out",
            "[analytics]\nenabled = true\n",
            Some("false"),
            false,
        ),
        (
            "malformed environment",
            "[analytics]\nenabled = true\n",
            Some("not-a-boolean"),
            false,
        ),
        (
            "dry-run",
            "[analytics]\nenabled = true\n",
            Some("true"),
            true,
        ),
        (
            "malformed config",
            "[analytics\nmalformed",
            Some("true"),
            false,
        ),
    ] {
        let sandbox = Sandbox::new();
        let telemetry = listener();
        let remote = listener();
        ctx_history_platform::platform_security::create_private_directory_all(
            &sandbox.path("history"),
        )
        .unwrap();
        sandbox.private_file("history/config.toml", config.as_bytes());
        let policy = |command: &mut Command| {
            command.env_remove("CTX_ANALYTICS_ENABLED");
            if let Some(enabled) = enabled {
                command.env("CTX_ANALYTICS_ENABLED", enabled);
            }
            if dry_run {
                command.env("CTX_ANALYTICS_DRY_RUN", "1");
            }
        };
        let mut initialize = init_command(&sandbox, &telemetry);
        policy(&mut initialize);
        assert_eq!(
            success(initialize.arg("--format=json"))["initialized"],
            true,
            "{case}"
        );
        let mut connect = connect_command(&sandbox, &telemetry, &endpoint(&remote));
        policy(&mut connect);
        assert_eq!(success(&mut connect)["local"]["connected"], true, "{case}");
        let mut status = command(&sandbox, &telemetry);
        policy(&mut status);
        assert_eq!(
            success(status.args(["remote", "status", USER, "--format=json"]))["local"]["enabled"],
            false,
            "{case}"
        );
        assert!(
            !device_state(&sandbox).exists(),
            "{case}: created device analytics state"
        );
        assert!(
            !sandbox.path("history/install.json").exists(),
            "{case}: created root identity"
        );
        assert_eq!(
            fs::read_to_string(sandbox.path("history/config.toml")).unwrap(),
            config,
            "{case}: rewrote configuration"
        );
        sandbox.assert_no_local_index();
        assert_no_daemon(&sandbox);
        assert_no_connection(&remote);
        assert_no_connection(&telemetry);
    }
}

#[cfg(target_os = "linux")]
fn fail_stdout(command: &mut std::process::Command) {
    use std::{
        os::unix::fs::FileTypeExt,
        process::Stdio,
        time::{Duration, Instant},
    };
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    // Never create or replace a path in /dev. Only open the existing full device.
    let full = fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap();
    assert!(full.metadata().unwrap().file_type().is_char_device());
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::from(full));
    // assert_cmd replaces stdout with a pipe; use a bounded raw child here.
    let mut child = Child(command.spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "output-failure child hung");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
}

#[cfg(target_os = "linux")]
fn output_failure(format: &str) {
    let sandbox = Sandbox::new();
    foreground_enqueue_only(&sandbox);
    let telemetry = listener();
    fail_stdout(
        sandbox
            .std_command()
            .env("CTX_ANALYTICS_ENABLED", "true")
            .env("CTX_ANALYTICS_ENDPOINT", endpoint(&telemetry))
            .env_remove("CTX_DAEMON_AUTOSTART_OFF")
            .args(["server", "--root"])
            .arg(sandbox.path("server"))
            .args(["init", USER, "--credentials-out"])
            .arg(sandbox.path("operator.json"))
            .args(["--format", format]),
    );
    assert!(
        sandbox.path("operator.json").is_file(),
        "operation should succeed before output fails"
    );
    let recorded = events(&sandbox, &[]);
    assert_eq!(recorded.len(), 1);
    assert_event(
        &recorded[0],
        "server_init",
        "failure",
        json!({
            "output": if format == "json" { "json" } else { "human" },
            "output_delivery": "failed",
            "hosted_failure_stage": "output", "failure_type": "io"
        }),
    );
    // A normal output sink still works against the initialized server.
    let recovered = success(init_command(&sandbox, &telemetry).arg("--format=json"));
    assert_eq!(recovered["initialized"], false);
    let after = events(&sandbox, &[]);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], recorded[0]);
    assert_event(
        &after[1],
        "server_init",
        "success",
        json!({"output": "json", "output_delivery": "known_complete"}),
    );
    sandbox.assert_no_local_index();
    assert_no_daemon(&sandbox);
    assert_no_connection(&telemetry);
}

#[cfg(target_os = "linux")]
#[test]
fn human_output_failure_is_a_terminal_not_a_success() {
    output_failure("text");
}

#[cfg(target_os = "linux")]
#[test]
fn json_output_failure_is_a_terminal_not_a_success() {
    output_failure("json");
}

#[cfg(target_os = "linux")]
#[test]
fn archive_restore_output_failure_records_after_ownership_and_allows_retry() {
    let source = Sandbox::new();
    let archive = archive_fixture(&source);
    let telemetry = listener();
    for format in ["text", "json"] {
        let destination = Sandbox::new();
        foreground_enqueue_only(&destination);
        fail_stdout(
            destination
                .std_command()
                .env("CTX_ANALYTICS_ENABLED", "true")
                .env("CTX_ANALYTICS_ENDPOINT", endpoint(&telemetry))
                .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                .args(["archive", "restore"])
                .arg(&archive)
                .args(["--format", format]),
        );
        assert!(destination.path("history/archive-root.json").is_file());
        let recorded = events(&destination, &[]);
        assert_eq!(recorded.len(), 1);
        assert_event(
            &recorded[0],
            "archive_restore",
            "failure",
            json!({
                "output": if format == "json" { "json" } else { "human" },
                "output_delivery": "failed", "hosted_failure_stage": "output", "failure_type": "io"
            }),
        );
        let retried = success(
            command(&destination, &telemetry)
                .args(["archive", "restore"])
                .arg(&archive)
                .arg("--format=json"),
        );
        assert_eq!(retried["receipt"]["imported_members"], 0);
        assert_eq!(retried["receipt"]["unchanged_members"], 1);
        let after = events(&destination, &[]);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], recorded[0]);
        assert_event(
            &after[1],
            "archive_restore",
            "success",
            json!({"output": "json", "output_delivery": "known_complete"}),
        );
        assert_no_daemon(&destination);
    }
    assert_no_connection(&telemetry);
}

#[cfg(target_os = "linux")]
#[test]
fn server_restore_output_failure_records_after_shared_or_separate_root_ownership() {
    let source = Sandbox::new();
    let checkpoint = checkpoint_fixture(&source);
    let telemetry = listener();
    for same_root in [true, false] {
        let destination = Sandbox::new();
        foreground_enqueue_only(&destination);
        let root = destination.path(if same_root { "history" } else { "server" });
        fail_stdout(
            destination
                .std_command()
                .env("CTX_ANALYTICS_ENABLED", "true")
                .env("CTX_ANALYTICS_ENDPOINT", endpoint(&telemetry))
                .env_remove("CTX_DAEMON_AUTOSTART_OFF")
                .arg("--data-root")
                .arg(destination.path("history"))
                .args(["server", "--root"])
                .arg(&root)
                .arg("restore")
                .arg(&checkpoint)
                .arg("--format=json"),
        );
        assert!(root.join("authority.sqlite").is_file());
        assert!(root.join("operator.json").is_file());
        let recorded = events(&destination, &[]);
        assert_eq!(recorded.len(), 1);
        assert_event(
            &recorded[0],
            "server_restore",
            "failure",
            json!({
                "output": "json", "output_delivery": "failed",
                "hosted_failure_stage": "output", "failure_type": "io"
            }),
        );
        let status = success(
            command(&destination, &telemetry)
                .args(["server", "--root"])
                .arg(&root)
                .args(["status", "--format=json"]),
        );
        assert_eq!(status["health"]["collections"], 1);
        let after = events(&destination, &[]);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], recorded[0]);
        assert_event(
            &after[1],
            "server_status",
            "success",
            json!({"output": "json", "output_delivery": "known_complete"}),
        );
        destination.assert_no_local_index();
        assert_no_daemon(&destination);
    }
    assert_no_connection(&telemetry);
}
