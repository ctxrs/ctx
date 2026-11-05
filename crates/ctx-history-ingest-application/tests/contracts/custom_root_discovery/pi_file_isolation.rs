use super::*;

// Authored fixtures exercise filesystem admission, not native format discovery.
fn write_session(path: &Path, id: &str, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{}\n{}\n",
            json!({"type":"session", "version":3, "id":id,
                "timestamp":"2026-08-18T01:00:00Z", "cwd":"/workspace/project"}),
            json!({"type":"message", "id":format!("{id}-message"),
                "timestamp":"2026-08-18T01:00:01Z",
                "message":{"role":"user", "content":body}}),
        ),
    )
    .unwrap();
}

fn import(temp: &TempDir, root: &Path) -> std::process::Output {
    ctx(temp)
        .args([
            "import",
            "--provider",
            "pi",
            "--path",
            root.to_str().unwrap(),
            "--no-blame",
            "--format=json",
            "--progress",
            "none",
        ])
        .output()
        .unwrap()
}

fn report(output: std::process::Output, success: bool) -> Value {
    assert_eq!(output.status.success(), success, "{output:?}");
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("expected an import report: {error}; {output:?}"))
}

fn assert_file_failure(report: &Value, path: &Path, documents: u64) {
    assert_eq!(
        report["outcome"], "completed_with_source_failures",
        "{report:#}"
    );
    assert_eq!(report["totals"]["failed_sources"], 1, "{report:#}");
    assert_eq!(
        report["totals"]["current_indexed_documents"], documents,
        "{report:#}"
    );
    assert_eq!(report["totals"]["removed_source_count"], 0, "{report:#}");
    let source = &report["sources"][0];
    assert_eq!(source["status"], "partial", "{report:#}");
    assert_eq!(
        source["source_selector"],
        path.to_str().unwrap(),
        "{report:#}"
    );
    assert!(
        source["detail"]
            .as_str()
            .is_some_and(|detail| !detail.is_empty()),
        "{report:#}"
    );
}

fn records(temp: &TempDir) -> Vec<ctx_history_index::CoreRecord> {
    provider_core_records(&data_root(temp), "pi")
}

#[test]
fn pi_explicit_non_session_jsonl_is_file_local_and_repairable() {
    let temp = daemon_test_root();
    let root = temp.path().join("sessions");
    write_session(&root.join("healthy.jsonl"), "healthy", "healthy history");
    let sidecar = root.join("decisions.jsonl");
    fs::write(&sidecar, "{\"type\":\"decision\"}\n").unwrap();
    let failed = report(import(&temp, &root), false);
    assert_file_failure(&failed, &sidecar, 1);
    assert_eq!(
        records(&temp)[0].content.normalized_body.as_deref(),
        Some("healthy history")
    );

    write_session(&sidecar, "repaired", "repaired history");
    let repaired = report(import(&temp, &root), true);
    assert_eq!(repaired["totals"]["failed_sources"], 0);
    assert_eq!(records(&temp).len(), 2);
}

#[cfg(unix)]
fn deny_read(path: &Path) -> Option<fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = fs::metadata(path).unwrap().permissions();
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    // Root and other privileged runners cannot exercise permission denial.
    if fs::File::open(path).is_ok() {
        fs::set_permissions(path, permissions).unwrap();
        return None;
    }
    assert_eq!(
        fs::File::open(path).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    Some(permissions)
}

#[cfg(unix)]
#[test]
fn pi_explicit_unreadable_sidecar_is_file_local_and_repairable() {
    let temp = daemon_test_root();
    let root = temp.path().join("sessions");
    write_session(&root.join("healthy.jsonl"), "healthy", "healthy history");
    let sidecar = root.join("decisions.jsonl");
    fs::write(&sidecar, "{\"type\":\"decision\"}\n").unwrap();
    let Some(permissions) = deny_read(&sidecar) else {
        return;
    };
    let output = import(&temp, &root);
    fs::set_permissions(&sidecar, permissions).unwrap();
    assert_file_failure(&report(output, false), &sidecar, 1);
    assert_eq!(
        records(&temp)[0].content.normalized_body.as_deref(),
        Some("healthy history")
    );

    write_session(&sidecar, "repaired", "repaired history");
    let repaired = report(import(&temp, &root), true);
    assert_eq!(repaired["totals"]["failed_sources"], 0);
    assert_eq!(records(&temp).len(), 2);
}

#[cfg(unix)]
#[test]
fn pi_explicit_unreadable_session_preserves_records_and_retries() {
    let temp = daemon_test_root();
    let root = temp.path().join("sessions");
    let prior = root.join("prior.jsonl");
    write_session(&prior, "prior", "retained history");
    report(import(&temp, &root), true);
    let retained = records(&temp);
    write_session(&root.join("peer.jsonl"), "peer", "healthy history");
    let Some(permissions) = deny_read(&prior) else {
        return;
    };
    let outputs: Vec<_> = (0..3).map(|_| import(&temp, &root)).collect();
    fs::set_permissions(&prior, permissions).unwrap();
    for output in outputs {
        assert_file_failure(&report(output, false), &prior, 2);
    }
    assert!(retained
        .iter()
        .all(|record| records(&temp).contains(record)));

    fs::OpenOptions::new()
        .append(true)
        .open(&prior)
        .unwrap()
        .write_all(
            format!(
                "{}\n",
                json!({"type":"message", "id":"appended-message",
            "timestamp":"2026-08-18T01:00:02Z",
            "message":{"role":"user", "content":"appended history"}})
            )
            .as_bytes(),
        )
        .unwrap();
    let repaired = report(import(&temp, &root), true);
    assert_eq!(repaired["totals"]["failed_sources"], 0);
    let after = records(&temp);
    assert_eq!(after.len(), 3);
    assert!(retained.iter().all(|record| after.contains(record)));
    let repeated = report(import(&temp, &root), true);
    assert_eq!(repeated["totals"]["change"], "no_op");
    assert_eq!(records(&temp), after);
}

#[cfg(unix)]
#[test]
fn pi_explicit_symlink_is_file_local_without_traversal() {
    for dangling in [false, true] {
        let temp = daemon_test_root();
        let root = temp.path().join("sessions");
        write_session(&root.join("healthy.jsonl"), "healthy", "healthy history");
        let outside = temp.path().join("outside.jsonl");
        if !dangling {
            write_session(&outside, "outside", "must not be imported");
        }
        let link = root.join("link.jsonl");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert_file_failure(&report(import(&temp, &root), false), &link, 1);
        assert_eq!(
            records(&temp)[0].content.normalized_body.as_deref(),
            Some("healthy history")
        );
    }
}

#[cfg(unix)]
#[test]
fn pi_explicit_unreadable_directories_stay_fatal_and_preserve_records() {
    for directory in ["", "nested", "nested.jsonl"] {
        let temp = daemon_test_root();
        let root = temp.path().join("sessions");
        let blocked = root.join(directory);
        write_session(&blocked.join("prior.jsonl"), "prior", "retained history");
        report(import(&temp, &root), true);
        let retained = records(&temp);
        write_session(&root.join("peer.jsonl"), "peer", "healthy history");
        let Some(permissions) = deny_read(&blocked) else {
            return;
        };
        let output = import(&temp, &root);
        fs::set_permissions(&blocked, permissions).unwrap();
        assert!(!output.status.success(), "{output:?}");
        assert_eq!(records(&temp), retained);
        let recovered = report(import(&temp, &root), true);
        assert_eq!(recovered["totals"]["failed_sources"], 0);
        assert_eq!(records(&temp).len(), 2);
    }
}
