#[path = "hosted_history/support.rs"]
mod support;

use std::time::{Duration, Instant};

use serde_json::Value;
use support::{failure, seed_history, success, Sandbox};

#[test]
fn running_server_invite_share_search_pause_and_revoke_use_one_connection_contract() {
    let operator = Sandbox::new();
    let bootstrap = operator.init();
    let collection = bootstrap["collection"].as_str().unwrap();
    let server = operator.start_server();
    success(
        operator
            .command()
            .args([
                "remote",
                "connect",
                "team",
                "--url",
                &server.endpoint,
                "--collection",
                collection,
                "--token-file",
            ])
            .arg(operator.path("operator.json"))
            .arg("--format=json"),
    );
    let invitation = success(
        operator
            .command()
            .args([
                "server",
                "--remote",
                "team",
                "user",
                "invite",
                "synthetic-publisher",
                "--output",
            ])
            .arg(operator.path("member-invitation.json"))
            .arg("--format=json"),
    );
    let member_id = invitation["user"].as_str().unwrap();
    assert_eq!(invitation["grants"]["read"], true);
    assert_eq!(invitation["grants"]["publish"], true);
    assert!(invitation.get("secret").is_none());

    let publisher = Sandbox::new();
    let record = seed_history(&publisher);
    success(
        publisher
            .command()
            .args([
                "remote",
                "connect",
                "team",
                "--url",
                &server.endpoint,
                "--collection",
                collection,
                "--enrollment-file",
            ])
            .arg(operator.path("member-invitation.json"))
            .arg("--format=json"),
    );
    let before = success(publisher.command().args([
        "remote",
        "status",
        "team",
        "--online",
        "--format=json",
    ]));
    assert_eq!(before["local"]["enabled"], false);
    assert_eq!(before["server"]["stored_sequence"], 0);
    let source = record
        .source
        .identity()
        .digest()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let shared = success(publisher.command().args([
        "remote",
        "share",
        "team",
        "--source",
        &source,
        "--mode",
        "automatic",
        "--backfill",
        "all",
        "--whole-source",
        "--include-future",
        "--format=json",
    ]));
    assert_eq!(shared["policy"]["mode"]["kind"], "automatic");
    let synced = success(
        publisher
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(synced["local"]["pending"], 0);
    assert_eq!(synced["local"]["stored_sessions"], 1);
    assert_eq!(synced["local"]["selection"]["counts"]["selected"], 1);
    publisher
        .command()
        .args(["remote", "status", "team"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "Queued revisions: 0 pending, 0 held.",
        ))
        .stdout(predicates::str::contains("1 selected, 0 held, 0 excluded."));

    let published_status = wait_for_searchable(&publisher, "initial publication", 1);
    // A separate reader starts with no local index or provider configuration.
    let reader_invite = success(
        operator
            .command()
            .args([
                "server",
                "--remote",
                "team",
                "user",
                "invite",
                "synthetic-reader",
                "--read-only",
                "--output",
            ])
            .arg(operator.path("reader-invitation.json"))
            .arg("--format=json"),
    );
    let reader = Sandbox::new();
    success(
        reader
            .command()
            .args([
                "remote",
                "connect",
                "team",
                "--url",
                &server.endpoint,
                "--collection",
                collection,
                "--enrollment-file",
            ])
            .arg(operator.path("reader-invitation.json"))
            .args(["--read-only", "--format=json"]),
    );
    let hits = search_at(&reader, "initial reader search", &published_status);
    let hit = &hits["results"][0];
    assert!(hit["snippet"]
        .as_str()
        .unwrap()
        .contains("portable meadow observatory fixture"));
    assert_eq!(hit["content_status"], "selected");
    assert!(hit["snippet_truncated"].is_boolean());
    assert!(hit.get("record").is_none());
    let citation = hit["citation"].as_str().unwrap();
    let human = reader
        .command()
        .args(["search", "observatory", "--server", "team"])
        .assert()
        .success()
        .get_output()
        .clone();
    let human = String::from_utf8(human.stdout).unwrap();
    assert!(human.contains("portable meadow observatory fixture"));
    assert!(human.contains(citation));
    assert!(!human.contains("Structured content:"));
    let shown = success(reader.command().args([
        "show",
        "event",
        citation,
        "--server",
        "team",
        "--format=json",
    ]));
    assert_eq!(shown["citation"], hit["citation"]);
    assert_eq!(
        shown["record"]["content"],
        serde_json::to_value(&record.content).unwrap()
    );
    reader
        .command()
        .args(["show", "event", citation, "--server", "team"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Structured content:"))
        .stdout(predicates::str::contains("complete"));
    reader.assert_no_local_index();

    let paused = success(
        publisher
            .command()
            .args(["remote", "pause", "team", "--format=json"]),
    );
    assert_eq!(paused["paused"], true);
    let blocked = failure(
        publisher
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(blocked["error"]["code"], "policy_denied");
    success(operator.command().args([
        "server",
        "--remote",
        "team",
        "revoke",
        "--user",
        member_id,
        "--format=json",
    ]));
    let blocked = failure(publisher.command().args([
        "remote",
        "status",
        "team",
        "--online",
        "--format=json",
    ]));
    assert_eq!(blocked["error"]["code"], "forbidden");

    // Revoking one member retains shared history and another member's access.
    let retained = search_at(
        &reader,
        "reader search after publisher revocation",
        &published_status,
    );
    assert!(retained["results"][0]["snippet"]
        .as_str()
        .unwrap()
        .contains("portable meadow observatory fixture"));
    let publication = retained["results"][0]["provenance"]["publication"]
        .as_str()
        .unwrap();
    let conflict = failure(operator.command().args([
        "server",
        "--remote",
        "team",
        "withdraw",
        "--publication",
        publication,
        "--expected-revision",
        "wrong-predecessor",
        "--format=json",
    ]));
    assert_eq!(conflict["error"]["code"], "conflict");
    // Withdrawal binds the exact accepted occurrence, not only its content digest.
    let withdrawal = success(operator.command().args([
        "server",
        "--remote",
        "team",
        "withdraw",
        "--publication",
        publication,
        "--format=json",
    ]));
    assert_eq!(
        withdrawal["receipt"]["operation"]["expected_sequence"],
        synced["local"]["last_accepted_sequence"]
    );
    let withdrawn_status = wait_for_searchable(
        &reader,
        "withdrawal projection",
        withdrawal["receipt"]["sequence"].as_u64().unwrap(),
    );
    let withdrawn = search_at(
        &reader,
        "reader search after withdrawal projection",
        &withdrawn_status,
    );
    assert!(withdrawn["results"].as_array().unwrap().is_empty());
    success(operator.command().args([
        "server",
        "--remote",
        "team",
        "revoke",
        "--user",
        reader_invite["user"].as_str().unwrap(),
        "--format=json",
    ]));
    failure(
        reader
            .command()
            .args(["remote", "status", "team", "--online", "--format=json"]),
    );
    reader.assert_no_local_index();
}

fn wait_for_searchable(sandbox: &Sandbox, stage: &'static str, minimum_sequence: u64) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let status = success_at(
            sandbox
                .command()
                .args(["remote", "status", "team", "--online", "--format=json"]),
            stage,
        );
        let server = &status["server"];
        let stored = server["stored_sequence"].as_u64().unwrap();
        let searchable = server["searchable_sequence"].as_u64().unwrap();
        if server["reads_available"] == true && stored >= minimum_sequence && searchable == stored {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "{stage} did not become readable: {status}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn search_at(sandbox: &Sandbox, stage: &'static str, last_status: &Value) -> Value {
    let output = sandbox
        .command()
        .args(["search", "observatory", "--server", "team", "--format=json"])
        .assert()
        .append_context("stage", stage)
        .append_context("last observed status", last_status.to_string())
        .success()
        .get_output()
        .clone();
    serde_json::from_slice(&output.stdout).unwrap()
}

fn success_at(command: &mut assert_cmd::Command, stage: &'static str) -> Value {
    let output = command
        .assert()
        .append_context("stage", stage)
        .success()
        .get_output()
        .clone();
    serde_json::from_slice(&output.stdout).unwrap()
}
