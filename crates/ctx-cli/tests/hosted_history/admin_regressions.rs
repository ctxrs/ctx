use std::{fs, net::TcpListener};

use serde_json::Value;

use super::support::{failure, seed_history, success, Sandbox};

#[test]
fn invitations_create_distinct_users_unless_an_existing_id_is_explicit() {
    let operator = Sandbox::new();
    operator.init();
    let running = operator.start_server();
    let unnamed = success(operator.server().args(["invite", "--format=json"]));
    let another_unnamed = success(operator.server().args(["invite", "--format=json"]));
    let first = success(
        operator
            .server()
            .args(["invite", "same-label", "--format=json"]),
    );
    let second = success(
        operator
            .server()
            .args(["invite", "same-label", "--format=json"]),
    );
    assert_ne!(unnamed["user"], another_unnamed["user"]);
    assert_ne!(first["user"], second["user"]);
    assert_ne!(unnamed["user"], first["user"]);
    let principal = first["user"].as_str().unwrap();
    let additional =
        success(
            operator
                .server()
                .args(["invite", "--user", principal, "--format=json"]),
        );
    assert_eq!(additional["user"], first["user"]);
    assert_ne!(additional["id"], first["id"]);
    for invitation in [&unnamed, &first, &additional] {
        let member = Sandbox::new();
        let connected = success(member.command().args([
            "remote",
            "connect",
            &running.endpoint,
            "--enrollment-file",
            invitation["file"].as_str().unwrap(),
            "--format=json",
        ]));
        assert_eq!(connected["local"]["enabled"], false);
        success(
            member
                .command()
                .args(["remote", "status", "team", "--online", "--format=json"]),
        );
        let protected: Value =
            serde_json::from_slice(&fs::read(invitation["file"].as_str().unwrap()).unwrap())
                .unwrap();
        assert!(!invitation
            .to_string()
            .contains(protected["enrollment"]["secret"].as_str().unwrap()));
        if invitation["id"] == first["id"] {
            let self_invited = success(member.command().args([
                "server",
                "--remote",
                "team",
                "invite",
                "--user",
                principal,
                "--format=json",
            ]));
            assert_eq!(self_invited["user"], first["user"]);
        }
        member.assert_no_local_index();
    }
    operator
        .server()
        .args(["invite", "same-label", "--user", principal, "--output"])
        .arg(operator.path("conflicting.json"))
        .assert()
        .failure();
    assert!(!operator.path("conflicting.json").exists());
    failure(
        operator
            .server()
            .args(["invite", "--user", "missing-user", "--output"])
            .arg(operator.path("missing.json"))
            .arg("--format=json"),
    );
    assert!(!operator.path("missing.json").exists());
    operator.assert_no_local_index();
}

#[test]
fn publish_only_invites_support_new_members_and_same_user_replacement_without_reads() {
    let operator = Sandbox::new();
    operator.init();
    let running = operator.start_server();
    let before = success(operator.server().args(["user", "list", "--format=json"]));
    operator
        .server()
        .args([
            "invite",
            "conflicting",
            "--publish-only",
            "--read-only",
            "--output",
        ])
        .arg(operator.path("conflicting-rights.json"))
        .assert()
        .failure()
        .stderr(predicates::str::contains("cannot be used with"));
    assert!(!operator.path("conflicting-rights.json").exists());
    assert_eq!(
        success(operator.server().args(["user", "list", "--format=json"])),
        before
    );

    let invitation = success(operator.server().args([
        "invite",
        "backup-device",
        "--publish-only",
        "--format=json",
    ]));
    let user = invitation["user"].as_str().unwrap();
    assert_eq!(invitation["grants"]["read"], false);
    assert_eq!(invitation["grants"]["publish"], true);
    assert_eq!(invitation["grants"]["manage"], false);
    let member = Sandbox::new();
    let connected = success(member.command().args([
        "remote",
        "connect",
        &running.endpoint,
        "--enrollment-file",
        invitation["file"].as_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(connected["local"]["enabled"], false);
    member
        .command()
        .args(["search", "synthetic", "--server", "team", "--format=json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("remote history access is denied"));
    success(
        operator
            .server()
            .args(["grant", "--user", user, "--publish", "--format=json"]),
    );

    // An already connected backup-only member can enroll its own replacement
    // without a read or manage grant, and without asking for broader rights.
    let replacement = success(member.command().args([
        "server",
        "--remote",
        "team",
        "invite",
        "--user",
        user,
        "--publish-only",
        "--format=json",
    ]));
    assert_eq!(replacement["user"], invitation["user"]);
    assert_eq!(replacement["grants"], invitation["grants"]);
    assert_ne!(replacement["id"], invitation["id"]);
    success(member.command().args([
        "remote",
        "connect",
        &running.endpoint,
        "--enrollment-file",
        replacement["file"].as_str().unwrap(),
        "--format=json",
    ]));
    success(
        member
            .command()
            .args(["remote", "status", "team", "--online", "--format=json"]),
    );
    member
        .command()
        .args(["search", "synthetic", "--server", "team", "--format=json"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("remote history access is denied"));
    member.assert_no_local_index();
    operator.assert_no_local_index();
}

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
fn online_root_revocation_is_server_wide_and_remote_revocation_stays_scoped() {
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
    for (name, collection) in [("first", first_collection), ("second", second_collection)] {
        success(
            operator
                .server()
                .args([
                    "user",
                    "credential",
                    member_id,
                    "--collection",
                    collection,
                    "--read",
                    "--publish",
                    "--output",
                ])
                .arg(operator.path(&format!("{name}.json")))
                .arg("--format=json"),
        );
    }
    let running = operator.start_server();
    let member = Sandbox::new();
    for (name, collection) in [("first", first_collection), ("second", second_collection)] {
        connect_token(
            &member,
            &operator,
            &running.endpoint,
            name,
            collection,
            &format!("{name}.json"),
        );
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
    assert_eq!(scoped["scope"], "collection");
    assert_eq!(scoped["collection"], first_collection);
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

    // Restore the first collection so whole-user revocation must disable both.
    success(operator.command().args([
        "server",
        "--remote",
        "admin",
        "grant",
        "--user",
        member_id,
        "--read",
        "--publish",
        "--format=json",
    ]));
    success(
        member
            .command()
            .args(["remote", "status", "first", "--online", "--format=json"]),
    );
    let revoked = success(
        operator
            .server()
            .args(["revoke", "--user", member_id, "--format=json"]),
    );
    assert_eq!(revoked["kind"], "user");
    assert_eq!(revoked["scope"], "server");
    assert_eq!(revoked["retained_history_unchanged"], true);
    for name in ["first", "second"] {
        let error =
            failure(
                member
                    .command()
                    .args(["remote", "status", name, "--online", "--format=json"]),
            );
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
    pending.assert_no_local_index();
}

#[test]
fn owner_inventory_is_safe_and_device_revocation_preserves_other_devices() {
    let operator = Sandbox::new();
    let bootstrap = operator.init();
    let offline = success(operator.server().args(["user", "list", "--format=json"]));
    assert_eq!(offline["scope"], "server");
    assert_eq!(offline["users"][0]["principal"], bootstrap["principal"]);
    assert_eq!(offline["users"][0]["server_owner"], true);
    let running = operator.start_server();
    connect_token(
        &operator,
        &operator,
        &running.endpoint,
        "admin",
        bootstrap["collection"].as_str().unwrap(),
        "operator.json",
    );
    let first = success(
        operator
            .server()
            .args(["invite", "two-devices", "--format=json"]),
    );
    let user = first["user"].as_str().unwrap();
    let second = success(operator.server().args([
        "invite",
        "--user",
        user,
        "--credential-ttl-seconds",
        "3600",
        "--format=json",
    ]));
    let devices = [Sandbox::new(), Sandbox::new()];
    let mut secrets = Vec::new();
    for (device, invitation) in devices.iter().zip([&first, &second]) {
        success(device.command().args([
            "remote",
            "connect",
            &running.endpoint,
            "--enrollment-file",
            invitation["file"].as_str().unwrap(),
            "--format=json",
        ]));
        let settings: Value = serde_json::from_slice(
            &fs::read(device.path("history/sharing/team/settings.json")).unwrap(),
        )
        .unwrap();
        secrets.push(settings["credentials"]["read"].as_str().unwrap().to_owned());
    }
    let users = success(operator.command().args([
        "server",
        "--remote",
        "admin",
        "user",
        "list",
        "--format=json",
    ]));
    assert_eq!(users["schema_version"], 1);
    assert_eq!(users["scope"], "server");
    let listed = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["principal"] == user)
        .unwrap();
    assert_eq!(listed["name"], "two-devices");
    assert_eq!(listed["revoked"], false);
    assert_eq!(listed["server_owner"], false);
    let credentials = success(operator.command().args([
        "server",
        "--remote",
        "admin",
        "user",
        "credentials",
        user,
        "--format=json",
    ]));
    assert_eq!(credentials["scope"], "server");
    let entries = credentials["credentials"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let credential = entries
        .iter()
        .find(|entry| entry["enrollment_id"] == first["id"])
        .unwrap();
    let id = credential["credential_id"].as_str().unwrap();
    assert_eq!(credential["expires_at"], 0);
    assert!(entries
        .iter()
        .any(|entry| entry["expires_at"].as_u64().unwrap() > 0));
    for entry in entries {
        assert_eq!(entry["collection"], bootstrap["collection"]);
        assert_eq!(entry["grants"]["read"], true);
        assert_eq!(entry["grants"]["publish"], true);
        assert_eq!(entry["grants"]["manage"], false);
        assert_eq!(entry["revoked"], false);
        assert_eq!(entry["server_owner"], false);
    }
    let text = operator
        .server()
        .args(["user", "credentials", user])
        .assert()
        .success()
        .get_output()
        .clone();
    let text = String::from_utf8(text.stdout).unwrap();
    assert!(text.contains("until revoked"));
    assert!(text.contains(bootstrap["collection"].as_str().unwrap()));
    assert!(text.contains(id));
    let row = text.lines().find(|line| line.starts_with(id)).unwrap();
    assert!(row.contains(&format!("enrollment_id={}", first["id"].as_str().unwrap())));
    assert!(text.contains("read=true publish=true manage=false"));
    assert!(text.contains("revoked=false"));
    for output in [users.to_string(), credentials.to_string(), text] {
        assert!(!output.contains("\"digest\""));
        assert!(!output.contains("\"secret\""));
        for secret in &secrets {
            assert!(!output.contains(secret));
        }
    }
    let page = success(operator.server().args([
        "user",
        "credentials",
        user,
        "--limit",
        "1",
        "--format=json",
    ]));
    assert_eq!(page["credentials"].as_array().unwrap().len(), 1);
    let next = success(operator.server().args([
        "user",
        "credentials",
        user,
        "--limit",
        "1",
        "--after",
        page["next_cursor"].as_str().unwrap(),
        "--format=json",
    ]));
    assert_eq!(next["credentials"].as_array().unwrap().len(), 1);
    assert_ne!(
        page["credentials"][0]["credential_id"],
        next["credentials"][0]["credential_id"]
    );
    assert!(next["next_cursor"].is_null());
    let revoked = success(operator.command().args([
        "server",
        "--remote",
        "admin",
        "revoke",
        "--credential",
        id,
        "--format=json",
    ]));
    assert_eq!(revoked["kind"], "credential");
    assert_eq!(revoked["scope"], "server");
    let denied = failure(devices[0].command().args([
        "remote",
        "status",
        "team",
        "--online",
        "--format=json",
    ]));
    assert_eq!(denied["error"]["code"], "forbidden");
    success(
        devices[1]
            .command()
            .args(["remote", "status", "team", "--online", "--format=json"]),
    );
    let after = success(
        operator
            .server()
            .args(["user", "credentials", user, "--format=json"]),
    );
    for entry in after["credentials"].as_array().unwrap() {
        assert_eq!(entry["revoked"], entry["credential_id"] == id);
    }
    let revoked = success(operator.command().args([
        "server",
        "--remote",
        "admin",
        "revoke",
        "--user",
        user,
        "--server-wide",
        "--format=json",
    ]));
    assert_eq!(revoked["kind"], "user");
    assert_eq!(revoked["scope"], "server");
    failure(
        devices[1]
            .command()
            .args(["remote", "status", "team", "--online", "--format=json"]),
    );
    drop(running);
    let users = success(operator.server().args(["user", "list", "--format=json"]));
    let listed = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["principal"] == user)
        .unwrap();
    assert_eq!(listed["revoked"], true);
    let after = success(
        operator
            .server()
            .args(["user", "credentials", user, "--format=json"]),
    );
    for entry in after["credentials"].as_array().unwrap() {
        assert_eq!(entry["revoked"], true);
    }
    for device in &devices {
        device.assert_no_local_index();
    }
    operator.assert_no_local_index();
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
    // Managing a collection does not confer authority over server-wide users
    // or another member's device credentials.
    for args in [
        vec!["user", "list"],
        vec!["user", "credentials", reader_id],
        vec!["revoke", "--credential", "synthetic-device-id"],
        vec!["revoke", "--user", reader_id, "--server-wide"],
        vec!["invite", "--user", reader_id, "--read-only"],
    ] {
        let error = failure(
            manager
                .command()
                .args(["server", "--remote", "team"])
                .args(args)
                .arg("--format=json"),
        );
        assert_eq!(error["error"]["code"], "forbidden");
    }
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
