use super::*;

const INDEX: &str = "sessions/sessions.index.json";
const DATABASE: &str = "db/sessions.db";

// Authored adversarial fixture: each catalog owns a different session.
fn write_catalogs(root: &Path) {
    fs::create_dir_all(root.join("sessions")).unwrap();
    fs::create_dir_all(root.join("db")).unwrap();
    fs::write(
        root.join(INDEX),
        serde_json::to_vec(&json!({
            "version": 1,
            "sessions": {"index-only": {"sessionId": "index-only"}}
        }))
        .unwrap(),
    )
    .unwrap();
    rusqlite::Connection::open(root.join(DATABASE))
        .unwrap()
        .execute_batch(
            "CREATE TABLE sessions (session_id TEXT PRIMARY KEY);
             INSERT INTO sessions VALUES ('database-only');",
        )
        .unwrap();
    for session in ["index-only", "database-only"] {
        write_messages(root, session, false);
    }
}

fn write_messages(root: &Path, session: &str, appended: bool) {
    let directory = root.join("sessions").join(session);
    fs::create_dir_all(&directory).unwrap();
    let mut messages = vec![json!({
        "id": "m1", "role": "user",
        "content": [{"type": "text", "text": format!("retained {session} history")}]
    })];
    if appended {
        messages.push(json!({
            "id": "m2", "role": "user",
            "content": [{"type": "text", "text": "appended after repair"}]
        }));
    }
    fs::write(
        directory.join(format!("{session}.messages.json")),
        serde_json::to_vec(&json!({
            "version": 1, "updated_at": "2026-08-18T00:00:00Z",
            "agent": "lead", "sessionId": session, "messages": messages
        }))
        .unwrap(),
    )
    .unwrap();
}

fn import(temp: &TempDir, root: &Path, success: bool) -> Value {
    let output = ctx(temp)
        .args([
            "import",
            "--provider",
            "cline",
            "--path",
            root.to_str().unwrap(),
            "--no-blame",
            "--format=json",
            "--progress",
            "none",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.success(), success, "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn cline_sdk_explicit_single_catalog_roots_remain_supported() {
    for (absent, session) in [(INDEX, "database-only"), (DATABASE, "index-only")] {
        let temp = daemon_test_root();
        let root = temp.path().join("cline-sdk");
        write_catalogs(&root);
        fs::remove_file(root.join(absent)).unwrap();
        let report = import(&temp, &root, true);
        assert_eq!(report["totals"]["failed_sources"], 0, "{report:#}");
        let records = provider_core_records(&data_root(&temp), "cline");
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].content.normalized_body.as_deref(),
            Some(format!("retained {session} history").as_str())
        );
        let repeated = import(&temp, &root, true);
        assert_eq!(repeated["totals"]["change"], "no_op");
        assert_eq!(provider_core_records(&data_root(&temp), "cline"), records);
    }
}

#[cfg(unix)]
fn unreadable_catalog_preserves_membership(blocked_name: &str, session: &str) {
    use std::os::unix::fs::PermissionsExt;

    let temp = daemon_test_root();
    let root = temp.path().join("cline-sdk");
    write_catalogs(&root);
    import(&temp, &root, true);
    let retained = provider_core_records(&data_root(&temp), "cline");
    assert_eq!(retained.len(), 2);
    let blocked = root.join(blocked_name);
    let permissions = fs::metadata(&blocked).unwrap().permissions();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    // Privileged runners cannot exercise permission denial.
    if fs::read(&blocked).is_ok() {
        fs::set_permissions(&blocked, permissions).unwrap();
        return;
    }
    assert_eq!(
        fs::read(&blocked).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    for _ in 0..3 {
        let report = import(&temp, &root, false);
        assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
        assert_eq!(report["totals"]["removed_source_count"], 0, "{report:#}");
        assert_eq!(report["sources"][0]["status"], "failure", "{report:#}");
        assert_eq!(report["sources"][0]["carried_forward"], true, "{report:#}");
        assert_eq!(
            report["sources"][0]["source_failure_class"], "unavailable",
            "{report:#}"
        );
        assert!(
            report["sources"][0]["detail"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "{report:#}"
        );
        assert_eq!(provider_core_records(&data_root(&temp), "cline"), retained);
    }
    fs::set_permissions(&blocked, permissions).unwrap();
    let repaired = import(&temp, &root, true);
    assert_eq!(repaired["totals"]["failed_sources"], 0, "{repaired:#}");
    assert_eq!(provider_core_records(&data_root(&temp), "cline"), retained);

    write_messages(&root, session, true);
    import(&temp, &root, true);
    let recovered = provider_core_records(&data_root(&temp), "cline");
    assert_eq!(recovered.len(), 3);
    // An append changes structured source_revision, while event identity and
    // previously imported text remain stable.
    assert!(retained.iter().all(|record| recovered
        .iter()
        .any(|current| current.event_id == record.event_id
            && current.content.normalized_body == record.content.normalized_body)));
    let repeated = import(&temp, &root, true);
    assert_eq!(repeated["totals"]["change"], "no_op");
    assert_eq!(provider_core_records(&data_root(&temp), "cline"), recovered);
}

#[cfg(unix)]
#[test]
fn cline_sdk_explicit_unreadable_index_preserves_membership_and_retries() {
    unreadable_catalog_preserves_membership(INDEX, "index-only");
}

#[cfg(unix)]
#[test]
fn cline_sdk_explicit_unreadable_database_preserves_membership_and_retries() {
    unreadable_catalog_preserves_membership(DATABASE, "database-only");
}
