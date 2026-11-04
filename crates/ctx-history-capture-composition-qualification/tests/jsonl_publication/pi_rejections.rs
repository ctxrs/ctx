use super::*;

#[path = "pi_file_admission.rs"]
mod file_admission;

#[test]
fn pi_oversized_sidecar_does_not_block_a_healthy_session() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let sidecar = root.join("session-pi-automode.jsonl");
    write_openclaw_lifecycle_transcript(
        &sidecar,
        &[json!({"type": "decision", "details": "x".repeat(16 * 1024 * 1024)})],
    );
    write_pi_session(
        &root.join("session.jsonl"),
        "session",
        None,
        "healthy history",
    );
    let index = temp.path().join("index");
    let registry = pi_registry(&root);
    let receipt = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    let failures = receipt.logical_source_failures.failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].source_selector.as_deref(), sidecar.to_str());
    assert!(failures[0]
        .detail
        .contains("16777216 byte JSONL record limit"));
    let records = all_indexed_records(&index);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("healthy history")
    );
}

#[test]
fn pi_incomplete_headers_are_pending_and_complete_without_losing_peers() {
    // Authored native headers, including the physical header after an OMP title.
    for warm in [false, true] {
        for title in ["none", "legacy", "mutable"] {
            for unframed in [false, true] {
                let temp = crate::test_support_paths::tempdir().unwrap();
                let root = temp.path().join("sessions");
                let path = root.join("writing.jsonl");
                let index = temp.path().join("index");
                fs::create_dir_all(&root).unwrap();
                write_pi_session(&path, "writing", None, "retained history");
                let session = fs::read(&path).unwrap();
                let header_end = session.iter().position(|byte| *byte == b'\n').unwrap();
                let prefix = if title == "mutable" {
                    super::pi_omp_title_slot::title_slot("Working")
                } else if title == "legacy" {
                    format!("{}\n", json!({"type":"title", "v":1, "title":"Working", "updatedAt":"2026-11-03T00:00:00Z", "pad":""})).into_bytes()
                } else {
                    Vec::new()
                };
                let complete = [prefix.as_slice(), session.as_slice()].concat();
                fs::write(&path, &complete).unwrap();
                let registry = pi_registry(&root);
                let retained = if warm {
                    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
                    all_indexed_records(&index)
                } else {
                    Vec::new()
                };
                let split = prefix.len() + if unframed { header_end } else { header_end / 2 };
                fs::write(&path, &complete[..split]).unwrap();
                write_pi_session(&root.join("peer.jsonl"), "peer", None, "new peer history");
                for attempt in 0..2 {
                    if title == "mutable" {
                        let mut file = OpenOptions::new().write(true).open(&path).unwrap();
                        file.write_all(&super::pi_omp_title_slot::title_slot(&format!(
                            "Working {attempt}"
                        )))
                        .unwrap();
                    }
                    let receipt =
                        refresh_source_backed_generation(&index, &registry, writer_options())
                            .unwrap();
                    assert!(receipt.failed_routes.is_empty(), "{receipt:?}");
                    assert!(receipt.logical_source_failures.is_empty(), "{receipt:?}");
                    assert_eq!(receipt.record_rejections.total(), 0);
                    assert!(receipt.removals.is_empty());
                    // With prior history, a pending member must prevent the
                    // complete-inventory proof that could authorize deletion.
                    if warm {
                        assert!(receipt.complete_inventory_route_ids.is_empty());
                    }
                    let records = all_indexed_records(&index);
                    assert_eq!(records.len(), retained.len() + 1);
                    assert!(retained.iter().all(|record| records.contains(record)));
                    assert!(records
                        .iter()
                        .any(|record| record.content.normalized_body.as_deref()
                            == Some("new peer history")));
                }
                use std::io::Write;
                fs::OpenOptions::new()
                    .append(true)
                    .open(&path)
                    .unwrap()
                    .write_all(&complete[split..])
                    .unwrap();
                let completed =
                    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
                assert!(completed.failed_routes.is_empty());
                assert!(completed.logical_source_failures.is_empty());
                assert_eq!(
                    completed.successful_route_outcomes[0].logical_source_retryable_failure_total,
                    0
                );
                let records = all_indexed_records(&index);
                assert_eq!(records.len(), 2);
                assert!(retained.iter().all(|record| records.contains(record)));
                let repeated =
                    refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
                assert_eq!(
                    repeated.commit.generation_id,
                    completed.commit.generation_id
                );
                assert_eq!(all_indexed_records(&index), records);
            }
        }
    }
}

#[test]
fn pi_incomplete_headers_without_peers_are_retryable() {
    for prefix in ["", "{\"type\":\"title\",\"v\":1,\"title\":\"Working\",\"updatedAt\":\"2026-11-03T00:00:00Z\",\"pad\":\"\"}\n"] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("writing.jsonl"), format!("{prefix}{{\"type\":\"session\",\"id\":\"writing")).unwrap();
        let error = refresh_source_backed_generation(temp.path().join("index"), &pi_registry(&root), writer_options()).unwrap_err();
        let crate::SourceBackedCoordinatorError::NoUsableSourceRoutes { failed_routes } = error else {
            panic!("unexpected pending outcome: {error:?}");
        };
        assert_eq!(failed_routes.failures().len(), 1);
        assert_eq!(failed_routes.failures()[0].class, SourceBackedSourceFailureClass::SourceChanged);
    }
}

#[test]
fn pi_non_session_files_are_isolated_and_repairable() {
    // Authored extension records: neither artifact names nor session-like names
    // can establish or disqualify a Pi session identity.
    for (name, record) in [
        (
            "subagent-artifacts/run_delegate_0_transcript.jsonl",
            json!({"version": 1, "recordType": "message", "source": "foreground", "runId": "child"}),
        ),
        (
            "2026-11-03T14-30-22-063Z_session-pi-automode.jsonl",
            json!({"type": "decision", "sessionId": "session", "tool": "bash", "outcome": "allow"}),
        ),
        (
            "usage-pi-automode.jsonl",
            json!({"type": "message", "timestamp": "2026-11-03T14:30:22Z",
                "message": {"role": "assistant", "model": "fixture", "usage": {"input": 1}}}),
        ),
        ("unrelated.jsonl", json!({"type": "extension_metadata"})),
    ] {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join("sessions");
        let rejected_path = root.join(name);
        write_openclaw_lifecycle_transcript(&rejected_path, &[record]);
        let session = root.join("2026-11-03T14-30-22-063Z_session.jsonl");
        write_pi_session(&session, "session", None, "readable Pi history");
        let registry = pi_registry(&root);
        let index = temp.path().join("index");

        let first = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(
            first.failed_routes.is_empty(),
            "{name}: {:?}",
            first.failed_routes
        );
        let failures = first.logical_source_failures.failures();
        assert_eq!(first.logical_source_failures.omitted(), 0);
        assert_eq!(failures.len(), 1, "{name}");
        assert_eq!(failures[0].source.provider(), "pi");
        assert_eq!(
            failures[0].source_selector.as_deref(),
            rejected_path.to_str()
        );
        assert!(failures[0].detail.contains("no valid session header"));
        let records = all_indexed_records(&index);
        assert_eq!(records.len(), 1, "{name}");
        assert_eq!(
            records[0].content.normalized_body.as_deref(),
            Some("readable Pi history")
        );
        let first_id = records[0].event_id;

        let repeated =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(repeated.failed_routes.is_empty());
        assert_eq!(repeated.commit.generation_id, first.commit.generation_id);
        assert_eq!(all_indexed_records(&index), records);
        assert_eq!(repeated.logical_source_failures.total(), 1);

        append_openclaw_lifecycle_transcript(
            &session,
            &json!({
                "type": "message", "id": "appended", "timestamp": "2026-11-03T14:30:23Z",
                "message": {"role": "user", "content": "new Pi history"}
            }),
        );
        let appended =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(appended.failed_routes.is_empty());
        assert_eq!(all_indexed_records(&index).len(), 2);
        assert_eq!(all_indexed_records(&index)[0].event_id, first_id);
        assert_eq!(appended.logical_source_failures.total(), 1);

        // A genuine session at the exact rejected pathname must be reconsidered.
        write_pi_session(&rejected_path, "repaired", None, "repaired Pi history");
        let repaired =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert!(repaired.failed_routes.is_empty());
        assert!(repaired.logical_source_failures.is_empty());
        assert_eq!(all_indexed_records(&index).len(), 3);
    }
}
