use std::{fs, net::TcpListener};

use serde_json::Value;

use super::support::{failure, seed_history, success, Sandbox};

#[test]
fn failed_bind_preserves_admin_destination_and_sends_no_credential_to_occupied_port() {
    let operator = Sandbox::new();
    operator.init();
    let running = operator.start_server();
    let previous_endpoint = running.endpoint.clone();
    let previous = fs::read(operator.path("server/admin/settings.json")).unwrap();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    occupied.set_nonblocking(true).unwrap();
    let address = occupied.local_addr().unwrap().to_string();
    drop(running);

    failure(
        operator
            .server()
            .args(["run", "--bind", &address, "--format=json"]),
    );
    assert_eq!(
        fs::read(operator.path("server/admin/settings.json")).unwrap(),
        previous
    );
    // The last genuine server is stopped. A normal admin command must fail
    // there rather than transmit its bearer to the listener that blocked run.
    let error = failure(
        operator
            .server()
            .args(["invite", "not-delivered", "--format=json"]),
    );
    assert_eq!(error["error"]["code"], "unavailable");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains(&previous_endpoint));
    assert!(message.contains("check or start the listener"));
    assert!(message.contains("retry"));
    assert!(
        matches!(occupied.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );
    assert_eq!(
        fs::read(operator.path("server/admin/settings.json")).unwrap(),
        previous
    );
    operator.assert_no_local_index();
}

#[test]
fn ephemeral_listener_saves_the_actual_bound_port_and_keeps_operator_identity() {
    let operator = Sandbox::new();
    let bootstrap = operator.init();
    let original = fs::read(operator.path("operator.json")).unwrap();
    let running = operator.start_server_ephemeral();
    let invitation =
        success(
            operator
                .server()
                .args(["invite", "ephemeral-member", "--format=json"]),
        );
    let member = Sandbox::new();
    let connected = success(member.command().args([
        "remote",
        "connect",
        &running.endpoint,
        "--enrollment-file",
        invitation["file"].as_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(
        connected["connection"]["collection"],
        bootstrap["collection"]
    );
    assert_eq!(connected["local"]["enabled"], false);
    assert_eq!(fs::read(operator.path("operator.json")).unwrap(), original);
    operator.assert_no_local_index();
    member.assert_no_local_index();
}

#[test]
fn root_wide_revocation_requires_stopped_server_and_remote_revocation_stays_scoped() {
    let operator = Sandbox::new();
    let bootstrap = operator.init();
    let first_collection = bootstrap["collection"].as_str().unwrap();
    let second =
        success(
            operator
                .server()
                .args(["collection", "create", "second", "--format=json"]),
        );
    let second_collection = second["collection"].as_str().unwrap();
    success(operator.server().args([
        "grant",
        "--user",
        bootstrap["principal"].as_str().unwrap(),
        "--collection",
        second_collection,
        "--read",
        "--publish",
        "--manage",
        "--format=json",
    ]));
    let running = operator.start_server();
    let invitation =
        success(
            operator
                .server()
                .args(["invite", "two-collections", "--format=json"]),
        );
    let member_id = invitation["user"].as_str().unwrap();
    let invitation_path = invitation["file"].as_str().unwrap();
    drop(running);
    success(operator.server().args([
        "grant",
        "--user",
        member_id,
        "--collection",
        second_collection,
        "--read",
        "--publish",
        "--format=json",
    ]));
    success(
        operator
            .server()
            .args([
                "user",
                "credential",
                member_id,
                "--read",
                "--publish",
                "--output",
            ])
            .arg(operator.path("two-collections.json"))
            .arg("--format=json"),
    );
    let running = operator.start_server();
    let member = Sandbox::new();
    for (name, collection) in [("first", first_collection), ("second", second_collection)] {
        connect_token(
            &member,
            &operator,
            &running.endpoint,
            name,
            collection,
            "two-collections.json",
        );
        success(
            member
                .command()
                .args(["remote", "status", name, "--online", "--format=json"]),
        );
    }
    let error = failure(
        operator
            .server()
            .args(["revoke", "--user", member_id, "--format=json"]),
    );
    assert!(error["error"]["message"].as_str().unwrap().contains("stop"));
    for name in ["first", "second"] {
        success(
            member
                .command()
                .args(["remote", "status", name, "--online", "--format=json"]),
        );
    }
    connect_token(
        &operator,
        &operator,
        &running.endpoint,
        "admin",
        first_collection,
        "operator.json",
    );
    let scoped = success(operator.command().args([
        "server",
        "--remote",
        "admin",
        "revoke",
        "--user",
        member_id,
        "--format=json",
    ]));
    assert_eq!(scoped["kind"], "membership");
    failure(
        member
            .command()
            .args(["remote", "status", "first", "--online", "--format=json"]),
    );
    success(
        member
            .command()
            .args(["remote", "status", "second", "--online", "--format=json"]),
    );

    drop(running);
    let revoked = success(
        operator
            .server()
            .args(["revoke", "--user", member_id, "--format=json"]),
    );
    assert_eq!(revoked["kind"], "user");
    let running = operator.start_server();
    let revoked_member = Sandbox::new();
    for (name, collection) in [("first", first_collection), ("second", second_collection)] {
        connect_token(
            &revoked_member,
            &operator,
            &running.endpoint,
            name,
            collection,
            "two-collections.json",
        );
        let error = failure(revoked_member.command().args([
            "remote",
            "status",
            name,
            "--online",
            "--format=json",
        ]));
        assert_eq!(error["error"]["code"], "forbidden");
    }
    let pending = Sandbox::new();
    let error = failure(pending.command().args([
        "remote",
        "connect",
        &running.endpoint,
        "--enrollment-file",
        invitation_path,
        "--format=json",
    ]));
    assert_eq!(error["error"]["code"], "unauthorized");
    success(
        operator
            .server()
            .args(["invite", "still-operational", "--format=json"]),
    );
    operator.assert_no_local_index();
    member.assert_no_local_index();
    revoked_member.assert_no_local_index();
    pending.assert_no_local_index();
}

#[test]
fn read_only_manager_can_administer_but_cannot_share_and_plain_reader_cannot_administer() {
    let operator = Sandbox::new();
    operator.init();
    let record = seed_history(&operator);
    let source = record
        .source
        .identity()
        .digest()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let running = operator.start_server();
    success(
        operator
            .command()
            .args(["remote", "connect", &running.endpoint, "--token-file"])
            .arg(operator.path("operator.json"))
            .arg("--format=json"),
    );
    success(operator.command().args([
        "remote",
        "share",
        "team",
        "--source",
        &source,
        "--whole-source",
        "--format=json",
    ]));
    success(
        operator
            .command()
            .args(["remote", "sync", "team", "--format=json"]),
    );
    let invitation = success(operator.server().args([
        "invite",
        "reader-manager",
        "--read-only",
        "--manage",
        "--format=json",
    ]));
    let manager = Sandbox::new();
    seed_history(&manager);
    let connected = success(manager.command().args([
        "remote",
        "connect",
        &running.endpoint,
        "--enrollment-file",
        invitation["file"].as_str().unwrap(),
        "--format=json",
    ]));
    let collection = connected["connection"]["collection"].as_str().unwrap();
    let settings: Value = serde_json::from_slice(
        &fs::read(manager.path("history/sharing/team/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(settings["credentials"]["publish"].is_null());
    let inventory = success(manager.command().args([
        "server",
        "--remote",
        "team",
        "publications",
        "--format=json",
    ]));
    let publication = inventory["publications"][0]["publication"]
        .as_str()
        .unwrap();

    // A read-only reconnect must keep management useful without restoring the
    // local upload credential slot or enabling a policy.
    let token = manager.private_file(
        "reader-manager-token",
        settings["credentials"]["read"].as_str().unwrap().as_bytes(),
    );
    success(
        manager
            .command()
            .args([
                "remote",
                "connect",
                &running.endpoint,
                "--collection",
                collection,
                "--read-only",
                "--token-file",
            ])
            .arg(token)
            .arg("--format=json"),
    );
    let error = failure(manager.command().args([
        "remote",
        "share",
        "team",
        "--source",
        &source,
        "--whole-source",
        "--format=json",
    ]));
    assert_eq!(error["error"]["code"], "credentials");
    let status = success(
        manager
            .command()
            .args(["remote", "status", "team", "--format=json"]),
    );
    assert_eq!(status["local"]["enabled"], false);
    assert_eq!(status["local"]["pending"], 0);

    let reader_invite = success(manager.command().args([
        "server",
        "--remote",
        "team",
        "invite",
        "ordinary-reader",
        "--read-only",
        "--format=json",
    ]));
    let reader_id = reader_invite["user"].as_str().unwrap();
    success(manager.command().args([
        "server",
        "--remote",
        "team",
        "grant",
        "--user",
        reader_id,
        "--read",
        "--format=json",
    ]));
    success(manager.command().args([
        "server",
        "--remote",
        "team",
        "revoke",
        "--user",
        reader_id,
        "--format=json",
    ]));
    success(manager.command().args([
        "server",
        "--remote",
        "team",
        "grant",
        "--user",
        reader_id,
        "--read",
        "--format=json",
    ]));
    success(manager.command().args([
        "server",
        "--remote",
        "team",
        "withdraw",
        "--publication",
        publication,
        "--format=json",
    ]));
    let reader = Sandbox::new();
    success(reader.command().args([
        "remote",
        "connect",
        &running.endpoint,
        "--enrollment-file",
        reader_invite["file"].as_str().unwrap(),
        "--format=json",
    ]));
    for args in [vec!["publications"], vec!["invite", "denied-invitation"]] {
        let error = failure(
            reader
                .command()
                .args(["server", "--remote", "team"])
                .args(args)
                .arg("--format=json"),
        );
        assert_eq!(error["error"]["code"], "forbidden");
    }
    let settings: Value = serde_json::from_slice(
        &fs::read(manager.path("history/sharing/team/settings.json")).unwrap(),
    )
    .unwrap();
    assert!(settings["credentials"]["publish"].is_null());
    assert!(settings["policy"].is_null());
    reader.assert_no_local_index();
}

fn connect_token(
    client: &Sandbox,
    owner: &Sandbox,
    endpoint: &str,
    name: &str,
    collection: &str,
    file: &str,
) {
    success(
        client
            .command()
            .args([
                "remote",
                "connect",
                endpoint,
                "--name",
                name,
                "--collection",
                collection,
                "--token-file",
            ])
            .arg(owner.path(file))
            .arg("--format=json"),
    );
}
