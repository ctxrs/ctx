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
                &server.endpoint,
                "--name",
                "team",
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
                &server.endpoint,
                "--name",
                "team",
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
    let unmatched = failure(publisher.command().args([
        "remote",
        "share",
        "team",
        "--profile-root",
        "unregistered-profile",
        "--whole-source",
        "--format=json",
    ]));
    assert_eq!(unmatched["error"]["code"], "policy_denied");
    let message = unmatched["error"]["message"].as_str().unwrap();
    assert!(message.contains("no indexed source matches"));
    assert!(message.contains("list sources with ctx sources"));
    assert!(message.contains("ctx sources add NAME --provider PROVIDER --root /absolute/profile"));
    assert!(message.contains("ctx import --all"));
    assert!(message.contains("registered --profile-root"));
    let unchanged =
        success(
            publisher
                .command()
                .args(["remote", "status", "team", "--format=json"]),
        );
    assert_eq!(unchanged["local"]["enabled"], false);
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
            "This client's upload queue: 0 pending revisions, 0 held.",
        ))
        .stdout(predicates::str::contains("within the selected scope only"))
        .stdout(predicates::str::contains("1 selected, 0 held, 0 excluded."));

    let published_status = wait_for_searchable(&publisher, "initial publication", 1);
    let inventory =
        success(
            operator
                .server()
                .args(["publications", "--limit", "1", "--format=json"]),
        );
    assert_eq!(inventory["publications"].as_array().unwrap().len(), 1);
    assert_eq!(inventory["publications"][0]["withdrawn"], false);
    assert!(inventory["publications"][0]["session_citations"][0]
        .as_str()
        .is_some());
    // A separate reader starts with no local index or provider configuration.
    let reader_invite = success(
        operator
            .command()
            .args([
                "server",
                "--remote",
                "team",
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
                &server.endpoint,
                "--name",
                "team",
                "--collection",
                collection,
                "--enrollment-file",
            ])
            .arg(operator.path("reader-invitation.json"))
            .args(["--read-only", "--format=json"]),
    );
    reader
        .command()
        .args(["remote", "status", "team", "--online"])
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "This client's publication state: 0 stored sessions.",
        ))
        .stdout(predicates::str::contains(
            "This client's upload queue: 0 pending revisions, 0 held.",
        ))
        .stdout(predicates::str::contains("Server stored sequence: 1."));
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
    let withdrawal = success(operator.server().args([
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

#[test]
fn new_enrollment_cannot_reuse_sharing_policy_but_token_rotation_keeps_it() {
    let operator = Sandbox::new();
    let bootstrap = operator.init();
    success(
        operator
            .server()
            .args([
                "user",
                "credential",
                bootstrap["principal"].as_str().unwrap(),
                "--read",
                "--publish",
                "--manage",
                "--output",
            ])
            .arg(operator.path("replacement-token.json"))
            .arg("--format=json"),
    );
    let server = operator.start_server();
    let member = Sandbox::new();
    let record = seed_history(&member);
    let source = record
        .source
        .identity()
        .digest()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    // Using an operator token here exercises an ordinary same-principal rotation.
    success(
        member
            .command()
            .args(["remote", "connect", &server.endpoint, "--token-file"])
            .arg(operator.path("operator.json"))
            .arg("--format=json"),
    );
    let shared = success(member.command().args([
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
    let rotation = success(
        member
            .command()
            .args([
                "remote",
                "connect",
                &server.endpoint,
                "--collection",
                bootstrap["collection"].as_str().unwrap(),
                "--token-file",
            ])
            .arg(operator.path("replacement-token.json"))
            .arg("--format=json"),
    );
    assert_eq!(rotation["local"]["enabled"], true);
    let invitation = success(
        operator
            .server()
            .args(["invite", "replacement", "--format=json"]),
    );
    let path = invitation["file"].as_str().unwrap();
    let before = std::fs::read(member.path("history/sharing/team/settings.json")).unwrap();
    let error = failure(member.command().args([
        "remote",
        "connect",
        &server.endpoint,
        "--enrollment-file",
        path,
        "--format=json",
    ]));
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("remote remove team"));
    assert_eq!(
        std::fs::read(member.path("history/sharing/team/settings.json")).unwrap(),
        before
    );
    success(
        member
            .command()
            .args(["remote", "remove", "team", "--format=json"]),
    );
    // The rejected replacement did not consume the single-use invitation.
    let connected = success(member.command().args([
        "remote",
        "connect",
        &server.endpoint,
        "--enrollment-file",
        path,
        "--format=json",
    ]));
    assert_eq!(connected["local"]["enabled"], false);
    assert_eq!(connected["local"]["pending"], 0);
    let selected = success(member.command().args([
        "remote",
        "share",
        "team",
        "--source",
        &source,
        "--whole-source",
        "--format=json",
    ]));
    assert_eq!(selected["policy"]["mode"]["kind"], "reviewed");
    assert_ne!(
        selected["policy"]["archive_identity"],
        shared["policy"]["archive_identity"]
    );
    let synced = success(
        member
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    assert_eq!(synced["local"]["stored_sessions"], 1);
}

#[test]
fn enrollment_destination_checks_precede_redemption_and_read_scope_is_authoritative() {
    let operator = Sandbox::new();
    operator.init();
    let server = operator.start_server();
    let invitation =
        success(
            operator
                .server()
                .args(["invite", "reader", "--read-only", "--format=json"]),
        );
    let path = invitation["file"].as_str().unwrap();
    let member = Sandbox::new();
    for args in [vec!["--collection", "wrong"], vec!["--name", "../outside"]] {
        failure(
            member
                .command()
                .args([
                    "remote",
                    "connect",
                    &server.endpoint,
                    "--enrollment-file",
                    path,
                    "--format=json",
                ])
                .args(args),
        );
    }
    let connected = success(member.command().args([
        "remote",
        "connect",
        &server.endpoint,
        "--enrollment-file",
        path,
        "--format=json",
    ]));
    assert_eq!(
        connected["connection"]["collection"],
        invitation["collection"]
    );
    let settings: Value = serde_json::from_slice(
        &std::fs::read(member.path("history/sharing/team/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(settings["credentials"]["publish"].is_null());
    assert_eq!(settings["policy"], Value::Null);
    let replacement =
        success(
            operator
                .server()
                .args(["invite", "new-reader", "--read-only", "--format=json"]),
        );
    let reconnected = success(member.command().args([
        "remote",
        "connect",
        &server.endpoint,
        "--enrollment-file",
        replacement["file"].as_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(reconnected["local"]["enabled"], false);
    member.assert_no_local_index();
}

#[cfg(unix)]
#[test]
fn terminal_enrollment_hides_paste_and_restores_echo_after_success_error_and_cancel() {
    let operator = Sandbox::new();
    operator.init();
    let server = operator.start_server();
    let invitation =
        success(
            operator
                .server()
                .args(["invite", "terminal-member", "--format=json"]),
        );
    let input = std::fs::read(invitation["file"].as_str().unwrap()).unwrap();
    let secret: Value = serde_json::from_slice(&input).unwrap();
    let secret = secret["enrollment"]["secret"].as_str().unwrap();
    let member = Sandbox::new();
    let output = prompt_connect(&member, &server.endpoint, &input);
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(secret));
    let connected: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(connected["local"]["enabled"], false);
    member.assert_no_local_index();
    for input in [
        b"{\"synthetic-never-echo\":\n".as_slice(),
        b"synthetic-never-echo\x03",
        b"\x04",
    ] {
        let sandbox = Sandbox::new();
        let output = prompt_connect(&sandbox, &server.endpoint, input);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("synthetic-never-echo"));
        assert!(!sandbox.path("history").exists());
    }
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

/// A real CLI child with terminal stdin/stderr and separately captured JSON stdout.
#[cfg(unix)]
fn prompt_connect(sandbox: &Sandbox, endpoint: &str, input: &[u8]) -> std::process::Output {
    use std::{
        fs,
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        process::Stdio,
        time::Instant,
    };
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let (mut master_fd, mut slave_fd) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
            )
        },
        0
    );
    let mut master = unsafe { fs::File::from_raw_fd(master_fd) };
    let slave = unsafe { fs::File::from_raw_fd(slave_fd) };
    assert_ne!(
        unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        -1
    );
    let mut child = Child(
        sandbox
            .std_command()
            .args(["remote", "connect", endpoint, "--format=json"])
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut transcript = Vec::new();
    let mut sent = false;
    let status = loop {
        let mut chunk = [0; 4096];
        match master.read(&mut chunk) {
            Ok(count) => transcript.extend_from_slice(&chunk[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(error) => panic!("read terminal: {error}"),
        }
        if !sent && String::from_utf8_lossy(&transcript).contains("then press Enter:") {
            let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
            assert_eq!(
                unsafe { libc::tcgetattr(slave.as_raw_fd(), mode.as_mut_ptr()) },
                0
            );
            assert_eq!(unsafe { mode.assume_init() }.c_lflag & libc::ECHO, 0);
            master.write_all(input).unwrap();
            sent = true;
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "terminal command hung");
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut rest = Vec::new();
    let _ = master.read_to_end(&mut rest);
    transcript.extend(rest);
    let mut mode = std::mem::MaybeUninit::<libc::termios>::uninit();
    assert_eq!(
        unsafe { libc::tcgetattr(slave.as_raw_fd(), mode.as_mut_ptr()) },
        0
    );
    assert_ne!(unsafe { mode.assume_init() }.c_lflag & libc::ECHO, 0);
    let mut stdout = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    assert!(sent, "child never presented the hidden prompt");
    std::process::Output {
        status,
        stdout,
        stderr: transcript,
    }
}
