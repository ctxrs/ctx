#[path = "hosted_history/support.rs"]
mod support;

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use serde_json::{json, Value};
use support::{failure, seed_history, success, Sandbox};

fn connect(member: &Sandbox, endpoint: &str, invitation: &Path, args: &[&str]) -> Value {
    success(
        member
            .command()
            .args(["remote", "connect", endpoint, "--enrollment-file"])
            .arg(invitation)
            .args(args)
            .arg("--format=json"),
    )
}

fn connect_owner(operator: &Sandbox, endpoint: &str) {
    success(
        operator
            .command()
            .args(["remote", "connect", endpoint, "--token-file"])
            .arg(operator.path("operator.json"))
            .arg("--format=json"),
    );
}

fn invite(operator: &Sandbox, args: &[&str], file: &str) -> Value {
    success(
        operator
            .command()
            .args(["server", "--remote", "team", "invite"])
            .args(args)
            .arg("--output")
            .arg(operator.path(file))
            .arg("--format=json"),
    )
}

fn settings(member: &Sandbox) -> Value {
    serde_json::from_slice(&fs::read(member.path("history/sharing/team/settings.json")).unwrap())
        .unwrap()
}

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn visit(root: &Path, path: &Path, result: &mut BTreeMap<String, Vec<u8>>) {
        if !path.exists() {
            return;
        }
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, result);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_string_lossy().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn repeated_scripted_enrollment_is_local_and_never_redeems_or_enables_uploads() {
    let operator = Sandbox::new();
    operator.init();
    let server = operator.start_server();
    connect_owner(&operator, &server.endpoint);
    let invitation = invite(&operator, &["scripted-user"], "first.json");
    let member = Sandbox::new();
    let first = connect(&member, &server.endpoint, &operator.path("first.json"), &[]);
    assert_eq!(first["principal"], invitation["user"]);
    assert_eq!(first["already_connected"], false);
    assert_eq!(first["local"]["enabled"], false);
    assert_eq!(first["local"]["pending"], 0);
    member.assert_no_local_index();
    let saved = settings(&member);
    let source: Value =
        serde_json::from_slice(&fs::read(operator.path("first.json")).unwrap()).unwrap();
    assert_eq!(saved["enrollment"]["principal"], invitation["user"]);
    assert_eq!(saved["enrollment"]["enrollment_id"], invitation["id"]);
    assert_eq!(
        saved["enrollment"]["fingerprint"].as_str().unwrap().len(),
        64
    );
    assert!(!saved
        .to_string()
        .contains(source["enrollment"]["secret"].as_str().unwrap()));
    let before = files(&member.path("history/sharing/team"));
    let repeated = connect(&member, &server.endpoint, &operator.path("first.json"), &[]);
    assert_eq!(repeated["already_connected"], true);
    assert_eq!(repeated["principal"], invitation["user"]);
    let local_status =
        success(
            member
                .command()
                .args(["remote", "status", "team", "--format=json"]),
        );
    assert_eq!(local_status["principal"], invitation["user"]);
    member
        .command()
        .args(["remote", "status", "team"])
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "Saved user ID: {}",
            invitation["user"].as_str().unwrap()
        )));
    assert_eq!(files(&member.path("history/sharing/team")), before);
    for secret in [
        source["enrollment"]["secret"].as_str().unwrap(),
        saved["credentials"]["read"].as_str().unwrap(),
    ] {
        assert!(!first.to_string().contains(secret));
        assert!(!repeated.to_string().contains(secret));
    }
    // A different local connection name has no enrollment receipt to recognize.
    let error = failure(
        member
            .command()
            .args([
                "remote",
                "connect",
                &server.endpoint,
                "--name",
                "other",
                "--enrollment-file",
            ])
            .arg(operator.path("first.json"))
            .arg("--format=json"),
    );
    assert_eq!(error["error"]["code"], "unauthorized");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("fresh enrollment"));
    assert!(!member.path("history/sharing/other/settings.json").exists());
    assert_eq!(files(&member.path("history/sharing/team")), before);
    let endpoint = server.endpoint.clone();
    drop(server);
    assert_eq!(
        connect(&member, &endpoint, &operator.path("first.json"), &[])["already_connected"],
        true
    );
    member
        .command()
        .args(["remote", "connect", &endpoint, "--enrollment-file"])
        .arg(operator.path("first.json"))
        .assert()
        .success()
        .stdout(predicates::str::contains("Server access was not checked"));
    assert_eq!(files(&member.path("history/sharing/team")), before);
    assert_eq!(
        connect(
            &member,
            &endpoint,
            &operator.path("first.json"),
            &["--read-only"]
        )["already_connected"],
        true
    );
    assert_eq!(
        settings(&member)["credentials"]["read"],
        saved["credentials"]["read"]
    );
    assert_eq!(settings(&member)["credentials"]["publish"], Value::Null);
    let narrowed = files(&member.path("history/sharing/team"));
    assert_eq!(
        connect(&member, &endpoint, &operator.path("first.json"), &[])["already_connected"],
        true
    );
    assert_eq!(files(&member.path("history/sharing/team")), narrowed);
    member.assert_no_local_index();
}

#[test]
fn changed_destination_or_principal_cannot_relabel_a_saved_enrollment() {
    let operator = Sandbox::new();
    operator.init();
    let server = operator.start_server();
    connect_owner(&operator, &server.endpoint);
    let first = invite(&operator, &["same-label"], "first.json");
    let second = invite(&operator, &["same-label"], "second.json");
    assert_ne!(first["user"], second["user"]);
    let member = Sandbox::new();
    connect(&member, &server.endpoint, &operator.path("first.json"), &[]);
    let before = files(&member.path("history/sharing/team"));
    for (endpoint, args) in [
        ("https://different.example", vec![]),
        (server.endpoint.as_str(), vec!["--collection", "different"]),
    ] {
        failure(
            member
                .command()
                .args(["remote", "connect", endpoint, "--enrollment-file"])
                .arg(operator.path("first.json"))
                .args(args)
                .arg("--format=json"),
        );
    }
    let mut changed: Value =
        serde_json::from_slice(&fs::read(operator.path("first.json")).unwrap()).unwrap();
    changed["principal"] = second["user"].clone();
    let changed = member.private_file("changed.json", &serde_json::to_vec(&changed).unwrap());
    let error = failure(
        member
            .command()
            .args(["remote", "connect", &server.endpoint, "--enrollment-file"])
            .arg(&changed)
            .arg("--format=json"),
    );
    assert_eq!(error["error"]["code"], "credentials");
    let error = failure(
        member
            .command()
            .args(["remote", "connect", &server.endpoint, "--enrollment-file"])
            .arg(operator.path("second.json"))
            .arg("--format=json"),
    );
    assert_eq!(error["error"]["code"], "credentials");
    assert_eq!(files(&member.path("history/sharing/team")), before);
    // Preflight rejection did not consume the other user's invitation.
    let other = Sandbox::new();
    assert_eq!(
        connect(&other, &server.endpoint, &operator.path("second.json"), &[])["already_connected"],
        false
    );
    assert_eq!(settings(&other)["enrollment"]["principal"], second["user"]);
    assert_eq!(
        connect(&member, &server.endpoint, &operator.path("first.json"), &[])["already_connected"],
        true
    );
}

#[test]
fn same_user_fresh_enrollment_preserves_consent_backlog_and_read_only_pause() {
    let operator = Sandbox::new();
    operator.init();
    let server = operator.start_server();
    connect_owner(&operator, &server.endpoint);
    let invited = invite(&operator, &["publisher"], "first.json");
    let user = invited["user"].as_str().unwrap();
    let member = Sandbox::new();
    let record = seed_history(&member);
    connect(&member, &server.endpoint, &operator.path("first.json"), &[]);
    let source = record
        .source
        .identity()
        .digest()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    success(member.command().args([
        "remote",
        "share",
        "team",
        "--source",
        &source,
        "--whole-source",
        "--mode",
        "automatic",
        "--include-future",
        "--format=json",
    ]));
    let original = settings(&member);
    let rotated = invite(&operator, &["--user", user], "rotated.json");
    assert_eq!(rotated["user"], invited["user"]);
    // Capture exactly one real pending operation without starting uploads.
    let collector = ctx_history_sharing::Collector::new(
        member.path("history"),
        member.path("history/sharing/team"),
    );
    assert!(matches!(
        collector.tick(),
        ctx_history_sharing::TickOutcome::Progress
    ));
    success(operator.command().args([
        "server",
        "--remote",
        "team",
        "revoke",
        "--credential",
        original["enrollment"]["credential_id"].as_str().unwrap(),
        "--format=json",
    ]));
    let pending = success(
        member
            .command()
            .args(["remote", "status", "team", "--format=json"]),
    );
    assert!(pending["local"]["pending"].as_u64().unwrap() > 0);
    let backlog = files(&member.path("history/sharing/team/queue"));
    assert!(!backlog.is_empty());
    let connected = connect(
        &member,
        &server.endpoint,
        &operator.path("rotated.json"),
        &[],
    );
    assert_eq!(connected["already_connected"], false);
    assert_eq!(connected["local"]["pending"], pending["local"]["pending"]);
    assert_eq!(files(&member.path("history/sharing/team/queue")), backlog);
    assert_eq!(settings(&member)["policy"], original["policy"]);
    assert_ne!(settings(&member)["credentials"], original["credentials"]);
    assert_eq!(settings(&member)["publisher"], original["publisher"]);

    let publishing = settings(&member);
    let narrowed = connect(
        &member,
        &server.endpoint,
        &operator.path("rotated.json"),
        &["--read-only"],
    );
    assert_eq!(narrowed["already_connected"], true);
    assert_eq!(
        settings(&member)["credentials"]["read"],
        publishing["credentials"]["read"]
    );
    assert_eq!(settings(&member)["credentials"]["publish"], Value::Null);
    assert_eq!(settings(&member)["paused"], true);
    assert_eq!(settings(&member)["policy"], original["policy"]);
    assert_eq!(files(&member.path("history/sharing/team/queue")), backlog);
    let blocked = failure(
        member
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(blocked["error"]["code"], "policy_denied");
    assert!(blocked["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Restore a publishing credential"));
    let narrowed = files(&member.path("history/sharing/team"));
    assert_eq!(
        connect(
            &member,
            &server.endpoint,
            &operator.path("rotated.json"),
            &[]
        )["already_connected"],
        true
    );
    assert_eq!(files(&member.path("history/sharing/team")), narrowed);

    invite(&operator, &["--user", user, "--read-only"], "reader.json");
    connect(
        &member,
        &server.endpoint,
        &operator.path("reader.json"),
        &[],
    );
    assert_eq!(settings(&member)["credentials"]["publish"], Value::Null);
    assert_eq!(settings(&member)["paused"], true);
    assert_eq!(settings(&member)["policy"], original["policy"]);
    assert_eq!(files(&member.path("history/sharing/team/queue")), backlog);
    let reader = files(&member.path("history/sharing/team"));
    assert_eq!(
        connect(
            &member,
            &server.endpoint,
            &operator.path("reader.json"),
            &[]
        )["already_connected"],
        true
    );
    assert_eq!(files(&member.path("history/sharing/team")), reader);

    invite(&operator, &["--user", user], "publisher-again.json");
    member
        .command()
        .args(["remote", "connect", &server.endpoint, "--enrollment-file"])
        .arg(operator.path("publisher-again.json"))
        .assert()
        .success()
        .stdout(predicates::str::contains("paused: true"))
        .stdout(predicates::str::contains("ctx remote pause team --resume"))
        .stdout(predicates::str::contains(format!("Saved user ID: {user}")));
    assert_eq!(settings(&member)["paused"], true);
    assert_eq!(settings(&member)["policy"], original["policy"]);
    assert_eq!(files(&member.path("history/sharing/team/queue")), backlog);
    let blocked = failure(
        member
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(blocked["error"]["code"], "policy_denied");
    assert!(blocked["error"]["message"]
        .as_str()
        .unwrap()
        .contains("ctx remote pause team --resume"));
    assert!(!blocked["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Restore a publishing credential"));
    // Only an explicit resume and sync may publish the retained pending operation.
    success(
        member
            .command()
            .args(["remote", "pause", "team", "--resume", "--format=json"]),
    );
    let synced = success(
        member
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(synced["local"]["pending"], 0);
    assert_eq!(synced["local"]["stored_sessions"], 1);
    assert_eq!(settings(&member)["policy"], original["policy"]);

    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = success(
            member
                .command()
                .args(["remote", "status", "team", "--online", "--format=json"])
                .timeout(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .max(Duration::from_millis(1)),
                ),
        );
        assert_eq!(status["server"]["stored_sequence"], 1);
        if status["server"]["reads_available"] == true
            && status["server"]["searchable_sequence"] == 1
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "rotated publication did not become searchable: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let hits = success(member.command().args([
        "search",
        "observatory",
        "--server",
        "team",
        "--format=json",
    ]));
    assert_eq!(hits["results"].as_array().unwrap().len(), 1);
    let hit = &hits["results"][0];
    assert_eq!(hit["provenance"]["publisher"], user);
    let shown = success(member.command().args([
        "show",
        "event",
        hit["citation"].as_str().unwrap(),
        "--server",
        "team",
        "--format=json",
    ]));
    assert_eq!(shown["citation"], hit["citation"]);
    assert_eq!(
        shown["record"]["content"],
        serde_json::to_value(&record.content).unwrap()
    );
    assert_eq!(shown["provenance"]["publisher"], user);
    let publications = success(operator.command().args([
        "server",
        "--remote",
        "team",
        "publications",
        "--format=json",
    ]));
    assert_eq!(publications["publications"].as_array().unwrap().len(), 1);
    assert_eq!(publications["next_cursor"], Value::Null);
    assert_eq!(
        publications["publications"][0]["publication"],
        shown["provenance"]["publication"]
    );
    assert_eq!(publications["publications"][0]["owner"], user);

    let repeated = success(
        member
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(repeated["local"]["pending"], 0);
    assert_eq!(repeated["local"]["stored_sessions"], 1);
    let status =
        success(
            member
                .command()
                .args(["remote", "status", "team", "--online", "--format=json"]),
        );
    assert_eq!(status["server"]["stored_sequence"], 1);
    assert_eq!(status["server"]["searchable_sequence"], 1);
    let unchanged = success(operator.command().args([
        "server",
        "--remote",
        "team",
        "publications",
        "--format=json",
    ]));
    assert_eq!(unchanged["publications"], publications["publications"]);
    assert_eq!(unchanged["next_cursor"], Value::Null);
}

#[test]
fn piped_input_preserves_hidden_input_rules_and_rejects_forged_identity_claims() {
    let operator = Sandbox::new();
    operator.init();
    let server = operator.start_server();
    connect_owner(&operator, &server.endpoint);
    let invitation = invite(&operator, &["piped-user"], "piped.json");
    let member = Sandbox::new();
    let input = fs::read(operator.path("piped.json")).unwrap();
    for already in [false, true] {
        let result = success(
            member
                .command()
                .args([
                    "remote",
                    "connect",
                    &server.endpoint,
                    "--enrollment-file=-",
                    "--format=json",
                ])
                .write_stdin(input.clone()),
        );
        assert_eq!(result["already_connected"], already);
    }
    let no_input =
        failure(
            member
                .command()
                .args(["remote", "connect", &server.endpoint, "--format=json"]),
        );
    assert!(no_input["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--enrollment-file"));
    let other = invite(&operator, &["other-piped-user"], "other.json");
    let mut forged: Value =
        serde_json::from_slice(&fs::read(operator.path("other.json")).unwrap()).unwrap();
    forged["principal"] = invitation["user"].clone();
    assert_ne!(other["user"], invitation["user"]);
    let before = files(&member.path("history/sharing/team"));
    let error = failure(
        member
            .command()
            .args([
                "remote",
                "connect",
                &server.endpoint,
                "--enrollment-file=-",
                "--format=json",
            ])
            .write_stdin(serde_json::to_vec(&forged).unwrap()),
    );
    assert_eq!(error["error"]["code"], "credentials");
    assert_eq!(files(&member.path("history/sharing/team")), before);
    assert!(!error
        .to_string()
        .contains(forged["enrollment"]["secret"].as_str().unwrap()));
    assert_eq!(
        settings(&member)["enrollment"]["principal"],
        invitation["user"]
    );
    assert_eq!(settings(&member)["policy"], json!(null));
    member.assert_no_local_index();
}
