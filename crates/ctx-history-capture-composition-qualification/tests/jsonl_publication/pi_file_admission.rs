use super::*;

fn write_session(path: &Path, id: &str, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    write_pi_session(path, id, None, body);
}

fn assert_file_failure(
    receipt: &crate::SourceBackedRefreshReceipt,
    path: &Path,
    class: SourceBackedSourceFailureClass,
) {
    assert!(receipt.failed_routes.is_empty(), "{receipt:?}");
    assert!(receipt.removals.is_empty());
    assert_eq!(receipt.logical_source_failures.total(), 1);
    let failure = &receipt.logical_source_failures.failures()[0];
    assert_eq!(failure.source.provider(), "pi");
    assert_eq!(failure.class, class);
    assert_eq!(failure.source_selector.as_deref(), path.to_str());
    assert_eq!(
        receipt.successful_route_outcomes[0].logical_source_retryable_failure_total,
        usize::from(class == SourceBackedSourceFailureClass::Unavailable)
    );
}

fn assert_healthy_history(index: &Path, count: usize) {
    let records = all_indexed_records(index);
    assert_eq!(records.len(), count);
    assert!(records
        .iter()
        .any(|record| record.content.normalized_body.as_deref() == Some("healthy history")));
}

#[cfg(unix)]
fn deny_read(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    match fs::File::open(path) {
        // A privileged runner cannot exercise permission denial.
        Ok(_) => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            false
        }
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
            true
        }
    }
}

#[cfg(unix)]
#[test]
fn pi_unreadable_sidecar_is_file_local() {
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(&root.join("healthy.jsonl"), "healthy", "healthy history");
    let sidecar = root.join("session-pi-automode.jsonl");
    fs::write(&sidecar, b"{\"type\":\"decision\"}\n").unwrap();
    if !deny_read(&sidecar) {
        return;
    }
    let index = temp.path().join("index");
    let registry = pi_registry(&root);
    let result = refresh_source_backed_generation(&index, &registry, writer_options());
    fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600)).unwrap();
    assert_file_failure(
        &result.unwrap(),
        &sidecar,
        SourceBackedSourceFailureClass::Unavailable,
    );
    assert_healthy_history(&index, 1);
    write_session(&sidecar, "repaired", "repaired history");
    let repaired = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(repaired.logical_source_failures.is_empty());
    assert_healthy_history(&index, 2);
}

#[cfg(unix)]
#[test]
fn pi_unreadable_prior_session_retains_history_and_imports_peer() {
    use std::os::unix::fs::PermissionsExt;
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let prior = root.join("prior.jsonl");
    write_session(&prior, "prior", "retained history");
    let index = temp.path().join("index");
    let registry = pi_registry(&root);
    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    let retained = all_indexed_records(&index);
    write_session(&root.join("peer.jsonl"), "peer", "healthy history");
    for _ in 0..2 {
        if !deny_read(&prior) {
            return;
        }
        let result = refresh_source_backed_generation(&index, &registry, writer_options());
        fs::set_permissions(&prior, fs::Permissions::from_mode(0o600)).unwrap();
        assert_file_failure(
            &result.unwrap(),
            &prior,
            SourceBackedSourceFailureClass::Unavailable,
        );
        assert_healthy_history(&index, 2);
        let records = all_indexed_records(&index);
        assert!(retained.iter().all(|record| records.contains(record)));
    }
    let repaired = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(repaired.logical_source_failures.is_empty());
    assert_eq!(
        repaired.successful_route_outcomes[0].logical_source_retryable_failure_total,
        0
    );
    assert_healthy_history(&index, 2);
    assert!(retained
        .iter()
        .all(|record| all_indexed_records(&index).contains(record)));
}

#[cfg(unix)]
fn check_symlink_isolation(dangling: bool) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let healthy = root.join("healthy.jsonl");
    write_session(&healthy, "healthy", "healthy history");
    let link = root.join("alias.jsonl");
    let target = if dangling {
        root.join("missing-target")
    } else {
        healthy
    };
    std::os::unix::fs::symlink(target, &link).unwrap();
    let index = temp.path().join("index");
    let registry = pi_registry(&root);
    for _ in 0..2 {
        let receipt =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_file_failure(&receipt, &link, SourceBackedSourceFailureClass::Unreadable);
        assert_healthy_history(&index, 1);
    }
    fs::remove_file(&link).unwrap();
    write_session(&link, "repaired", "repaired history");
    let repaired = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(repaired.logical_source_failures.is_empty());
    assert_healthy_history(&index, 2);
}

#[cfg(unix)]
#[test]
fn pi_selected_symlink_is_file_local() {
    check_symlink_isolation(false);
}

#[cfg(unix)]
#[test]
fn pi_selected_dangling_symlink_is_file_local() {
    check_symlink_isolation(true);
}

fn check_oversized_header(field: &str, diagnostic: &str) {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(&root.join("healthy.jsonl"), "healthy", "healthy history");
    let rejected = root.join("bad-header.jsonl");
    let mut header = json!({"type":"session", "version":3, "id":"session",
        "timestamp":"2026-11-03T00:00:00Z"});
    header[field] = json!("x".repeat(65_537));
    write_openclaw_lifecycle_transcript(&rejected, &[header]);
    let index = temp.path().join("index");
    let registry = pi_registry(&root);
    for _ in 0..2 {
        let receipt =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_file_failure(
            &receipt,
            &rejected,
            SourceBackedSourceFailureClass::Unreadable,
        );
        assert!(receipt.logical_source_failures.failures()[0]
            .detail
            .contains(diagnostic));
        assert_healthy_history(&index, 1);
    }
    write_session(&rejected, "repaired", "repaired history");
    let repaired = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(repaired.logical_source_failures.is_empty());
    assert_healthy_history(&index, 2);
}

#[test]
fn pi_oversized_session_id_is_file_local() {
    check_oversized_header("id", "typed_key_utf8 is too large");
}

#[test]
fn pi_oversized_header_binding_is_file_local() {
    check_oversized_header("cwd", "typed_key_bytes is too large");
}

#[cfg(unix)]
#[test]
fn pi_unreadable_directories_remain_route_failures() {
    use std::os::unix::fs::PermissionsExt;
    for name in ["", "subdirectory", "directory.jsonl"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let denied = root.join(name);
        write_session(&denied.join("prior.jsonl"), "prior", "retained history");
        let index = temp.path().join("index");
        let registry = pi_registry(&root);
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        let before = all_indexed_records(&index);
        write_session(&root.join("peer.jsonl"), "peer", "healthy history");
        if !deny_read(&denied) {
            return;
        }
        let result = refresh_source_backed_generation(&index, &registry, writer_options());
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
        let error = result.unwrap_err();
        assert!(
            matches!(error, crate::SourceBackedCoordinatorError::RouteScan { .. }),
            "{error:?}"
        );
        assert!(
            format!("{error:?}").contains("PermissionDenied"),
            "{error:?}"
        );
        assert_eq!(all_indexed_records(&index), before);
    }
}

#[test]
fn pi_readable_directories_named_jsonl_are_traversed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    write_session(
        &root.join("directory.jsonl/session.jsonl"),
        "nested",
        "nested history",
    );
    write_session(&root.join("healthy.jsonl"), "healthy", "healthy history");
    fs::create_dir(root.join("empty.jsonl")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, root.join("directory-link")).unwrap();
    let index = temp.path().join("index");
    let receipt =
        refresh_source_backed_generation(&index, &pi_registry(&root), writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_healthy_history(&index, 2);
}
