#[test]
fn canonical_material_formats_preserve_existing_capture_identities() {
    assert_eq!(
        provider_canonical_material_source_format(
            CaptureProvider::Codex,
            "codex_session_jsonl_tree"
        ),
        Some("codex_session_jsonl")
    );
    assert_eq!(
        provider_canonical_material_source_format(CaptureProvider::Codex, "codex_session_jsonl"),
        Some("codex_session_jsonl")
    );
    assert_eq!(
        provider_canonical_material_source_format(CaptureProvider::Pi, "pi_session_jsonl"),
        Some("pi_session_jsonl")
    );
    assert_eq!(
        provider_canonical_material_source_format(
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree"
        ),
        Some("claude_projects_jsonl_tree")
    );
    assert_eq!(
        provider_canonical_material_source_format(
            CaptureProvider::Tabnine,
            "tabnine_cli_chat_recording_jsonl"
        ),
        Some("tabnine_cli_chat_recording_jsonl")
    );
    assert_eq!(
        provider_file_mutation_contract(CaptureProvider::Codex, "codex_history_jsonl"),
        ProviderFileMutationContract::WholeReplacement
    );
}

#[test]
fn committed_import_with_failed_checkpoint_retains_summary_without_advancing() {
    let mut summary = ProviderImportSummary::default();
    summary.imported_events = 3;

    let decision = finish_import(
        summary,
        Err(ProviderJsonlReplacementReason::FileShrank),
        None,
        None,
    );

    match decision {
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
            assert_eq!(result.summary.imported_events, 3);
            assert_eq!(result.reason, ProviderJsonlReplacementReason::FileShrank);
        }
        other => panic!("expected imported-without-checkpoint decision, got {other:?}"),
    }
}

#[test]
fn post_materialization_mutation_dominates_adapter_certification_failure() {
    let mut summary = ProviderImportSummary::default();
    summary.imported_events = 2;

    let decision = finish_import(
        summary,
        Err(ProviderJsonlReplacementReason::FileShrank),
        None,
        Some(ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid),
    );

    assert!(matches!(
        decision,
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
            ProviderAppendFileImportWithoutCheckpoint {
                reason: ProviderJsonlReplacementReason::FileShrank,
                ..
            }
        )
    ));
}

#[test]
fn truncate_after_tolerant_materialization_dominates_invalid_header_certification() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("mutation/truncated.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}{}",
            jsonl(json!({"type": "notice"})),
            jsonl(codex_header("codex-truncated-after-import")),
            jsonl(codex_message("user", "materialized before truncate", 1))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let decision = import_append_capable_provider_file_with_post_materialization(
        CaptureProvider::Codex,
        &mut store,
        options(
            &path,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
        |_| fs::write(&path, b"").unwrap(),
    )
    .unwrap();

    match decision {
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
            assert_eq!(result.reason, ProviderJsonlReplacementReason::FileShrank);
            assert!(result.summary.imported_events > 0);
        }
        other => panic!("expected mutation to dominate certification, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn identity_swap_after_tolerant_materialization_dominates_invalid_header_certification() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("mutation/replaced.jsonl");
    let displaced = temp.path().join("mutation/displaced.jsonl");
    let contents = format!(
        "{}{}{}",
        jsonl(json!({"type": "notice"})),
        jsonl(codex_header("codex-replaced-after-import")),
        jsonl(codex_message("user", "materialized before replacement", 1))
    );
    write_raw(&path, &contents);
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let decision = import_append_capable_provider_file_with_post_materialization(
        CaptureProvider::Codex,
        &mut store,
        options(
            &path,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
        |_| {
            fs::rename(&path, displaced).unwrap();
            fs::write(&path, contents).unwrap();
        },
    )
    .unwrap();

    match decision {
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
            assert_eq!(
                result.reason,
                ProviderJsonlReplacementReason::StableIdentityChanged
            );
            assert!(result.summary.imported_events > 0);
        }
        other => panic!("expected mutation to dominate certification, got {other:?}"),
    }
}

#[test]
fn checkpoint_io_fault_after_commit_retains_the_import_summary() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("checkpoint/fault.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-checkpoint-fault")),
            jsonl(codex_message(
                "user",
                "committed before checkpoint fault",
                1
            ))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let decision = import_append_capable_provider_file_with_post_materialization(
        CaptureProvider::Codex,
        &mut store,
        options(
            &path,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
        |reader| {
            reader.inject_checkpoint_failure(ProviderJsonlReplacementReason::CheckpointHashIo);
        },
    )
    .unwrap();

    match decision {
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
            assert_eq!(
                result.reason,
                ProviderJsonlReplacementReason::CheckpointHashIo
            );
            assert_eq!(result.summary.failed, 0);
            assert_eq!(result.summary.imported_events, 1);
            assert_eq!(store.export_archive().unwrap().events.len(), 1);
        }
        other => panic!("expected committed import without checkpoint, got {other:?}"),
    }
}

#[test]
fn append_coordinator_withholds_checkpoint_for_committed_search_maintenance() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("checkpoint/search-maintenance.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-search-maintenance")),
            jsonl(codex_message("user", "durably committed", 1))
        ),
    );
    let db_path = temp.path().join("work.sqlite");
    let mut store =
        Store::open_with_busy_timeout(&db_path, std::time::Duration::from_millis(10)).unwrap();
    let pinned = Connection::open(&db_path).unwrap();
    pinned.execute_batch("BEGIN").unwrap();
    pinned
        .query_row("SELECT COUNT(*) FROM event_search", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();

    let decision = import_append_capable_provider_file(
        CaptureProvider::Codex,
        &mut store,
        options(
            &path,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
    )
    .unwrap();

    match decision {
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
            assert_eq!(
                result.reason,
                ProviderJsonlReplacementReason::CommittedMaintenanceIncomplete
            );
            assert_eq!(result.summary.imported_events, 1);
            assert_eq!(result.summary.maintenance_warnings.len(), 1);
            assert_eq!(
                result.summary.maintenance_warnings[0].kind,
                crate::ProviderImportMaintenanceKind::EventSearchFinalization
            );
            assert_eq!(store.export_archive().unwrap().events.len(), 1);
        }
        other => panic!("expected committed maintenance outcome, got {other:?}"),
    }
    pinned.execute_batch("ROLLBACK").unwrap();
}

fn assert_outer_checkpoint_error_after_commit(
    error: CaptureError,
    expected_reason: ProviderJsonlReplacementReason,
) {
    let temp = tempdir().unwrap();
    let path = temp.path().join("checkpoint/outer-fault.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-outer-checkpoint-fault")),
            jsonl(codex_message("user", "committed before outer fault", 1))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let decision = import_append_capable_provider_file_with_post_materialization(
        CaptureProvider::Codex,
        &mut store,
        options(
            &path,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
        move |reader| reader.inject_checkpoint_error(error),
    )
    .unwrap();

    match decision {
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
            assert_eq!(result.reason, expected_reason);
            assert_eq!(result.summary.failed, 0);
            assert_eq!(result.summary.imported_events, 1);
            assert_eq!(store.export_archive().unwrap().events.len(), 1);
        }
        other => panic!("expected committed import without checkpoint, got {other:?}"),
    }
}

#[test]
fn unexpected_checkpoint_io_error_after_commit_is_typed_and_retains_summary() {
    assert_outer_checkpoint_error_after_commit(
        CaptureError::Io(std::io::Error::other("injected checkpoint I/O fault")),
        ProviderJsonlReplacementReason::CheckpointUnexpectedIo,
    );
}

#[test]
fn unexpected_checkpoint_permanent_error_after_commit_is_typed_and_retains_summary() {
    assert_outer_checkpoint_error_after_commit(
        CaptureError::InvalidPayload("injected checkpoint invariant fault".to_owned()),
        ProviderJsonlReplacementReason::CheckpointUnexpectedPermanentFailure,
    );
}

#[test]
fn replacement_header_plus_partial_first_message_is_deferred() {
    let cases = [
            (
                CaptureProvider::Codex,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                format!(
                    "{}{{\"timestamp\":\"2026-07-14T12:00:01Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[",
                    jsonl(codex_header("codex-partial"))
                ),
            ),
            (
                CaptureProvider::Pi,
                "pi_session_jsonl",
                "pi_session_jsonl",
                format!(
                    "{}{{\"type\":\"message\",\"id\":\"pi-user",
                    jsonl(json!({
                        "type": "session",
                        "id": "pi-partial",
                        "timestamp": "2026-07-14T12:00:00Z"
                    }))
                ),
            ),
            (
                CaptureProvider::Claude,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                format!(
                    "{}{{\"sessionId\":\"claude-partial\",\"type\":\"user\",\"message\":",
                    jsonl(json!({
                        "sessionId": "claude-partial",
                        "timestamp": "2026-07-14T12:00:00Z",
                        "type": "system"
                    }))
                ),
            ),
            (
                CaptureProvider::Tabnine,
                "tabnine_cli_chat_recording_jsonl",
                "tabnine_cli_chat_recording_jsonl",
                format!(
                    "{}{{\"id\":\"tabnine-user\",\"type\":\"user\",\"content\":",
                    jsonl(json!({
                        "sessionId": "tabnine-partial",
                        "startTime": "2026-07-14T12:00:00Z"
                    }))
                ),
            ),
        ];

    for (index, (provider, inventory_format, material_format, contents)) in
        cases.into_iter().enumerate()
    {
        let temp = tempdir().unwrap();
        let path = temp.path().join(format!("case-{index}/session.jsonl"));
        write_raw(&path, &contents);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let decision = import_append_capable_provider_file(
            provider,
            &mut store,
            options(
                &path,
                inventory_format,
                material_format,
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap();
        assert!(
            matches!(decision, ProviderAppendFileImportDecision::DeferredPartial),
            "unexpected decision for {provider:?}: {decision:?}"
        );
        assert!(store.list_sessions().unwrap().is_empty());
    }
}

#[test]
fn codex_checkpoint_stops_at_and_replays_an_open_tool_frontier() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/session.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}{}",
            jsonl(codex_header("codex-frontier")),
            jsonl(codex_message("user", "frontier user", 1)),
            jsonl(codex_call("call-open", 2))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let first = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    assert_eq!(first.checkpoint.complete_line_count, 2);

    let replay = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(first.checkpoint.clone()),
            ),
        )
        .unwrap(),
    );
    assert_eq!(replay.checkpoint, first.checkpoint);

    append_raw(&path, "{\"type\":\"response_item\",oops}\n");
    let malformed_while_open = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(replay.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(malformed_while_open.checkpoint.complete_line_count, 2);

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-open",
            "Chunk ID: ok\nProcess exited with code 0\nOutput:\npassed\n",
            3,
        )),
    );
    let successful_output = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(malformed_while_open.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(successful_output.checkpoint.complete_line_count, 5);

    append_raw(&path, &jsonl(codex_call("call-failed", 4)));
    let open_again = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(successful_output.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(open_again.checkpoint.complete_line_count, 5);

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-failed",
            "Process exited with code 1\nOutput:\nfailed\n",
            5,
        )),
    );
    let failed_output = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(open_again.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(failed_output.checkpoint.complete_line_count, 7);
}

#[test]
fn codex_output_clears_only_its_matching_call_context() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/multiple-calls.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}{}{}{}",
            jsonl(codex_header("codex-multiple-calls")),
            jsonl(codex_message("user", "two calls", 1)),
            jsonl(codex_call("call-a", 2)),
            jsonl(codex_call("call-b", 3)),
            jsonl(codex_output(
                "call-a",
                "Process exited with code 0\nOutput:\na done\n",
                4,
            ))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    assert_eq!(initial.checkpoint.complete_line_count, 2);

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-b",
            "Process exited with code 0\nOutput:\nb done\n",
            5,
        )),
    );
    let completed = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(initial.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(completed.checkpoint.complete_line_count, 6);
}

#[test]
fn codex_invalid_timestamp_output_closes_its_tool_frontier_without_an_event() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/invalid-output-timestamp.jsonl");
    let mut invalid_output = codex_output(
        "call-invalid-time",
        "Process exited with code 1\nOutput:\nfailed\n",
        3,
    );
    invalid_output["timestamp"] = Value::String("not-a-timestamp".to_owned());
    write_raw(
        &path,
        &format!(
            "{}{}{}{}{}",
            jsonl(codex_header("codex-invalid-output-time")),
            jsonl(codex_message("user", "before call", 1)),
            jsonl(codex_call("call-invalid-time", 2)),
            jsonl(invalid_output),
            jsonl(codex_message("assistant", "after invalid output", 4))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    assert_eq!(initial.summary.failed, 1);
    assert_eq!(initial.checkpoint.complete_line_count, 5);
    assert_eq!(
        initial.checkpoint.committed_offset,
        fs::metadata(&path).unwrap().len()
    );
    assert_eq!(store.export_archive().unwrap().events.len(), 3);

    let unchanged = open_provider_jsonl(
        &path,
        ProviderJsonlOpenMode::Append(initial.checkpoint.clone()),
    )
    .unwrap();
    assert!(matches!(
        unchanged,
        ProviderJsonlOpenDecision::ReplacementRequired(
            ProviderJsonlReplacementReason::EqualLengthObservation
        )
    ));

    append_raw(
        &path,
        &jsonl(codex_message("user", "next append starts after poison", 5)),
    );
    let replay = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(initial.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(replay.summary.failed, 0);
    assert_eq!(replay.checkpoint.complete_line_count, 6);
    assert_eq!(store.export_archive().unwrap().events.len(), 4);
}

#[test]
fn codex_invalid_timestamp_output_closes_only_its_matching_tool_frontier() {
    let temp = tempdir().unwrap();
    let path = temp
        .path()
        .join("codex/invalid-output-multiple-calls.jsonl");
    let mut invalid_output =
        codex_output("call-a", "Process exited with code 1\nOutput:\nfailed\n", 4);
    invalid_output["timestamp"] = Value::String("not-a-timestamp".to_owned());
    write_raw(
        &path,
        &format!(
            "{}{}{}{}{}{}",
            jsonl(codex_header("codex-invalid-output-multiple-calls")),
            jsonl(codex_message("user", "before calls", 1)),
            jsonl(codex_call("call-a", 2)),
            jsonl(codex_call("call-b", 3)),
            jsonl(invalid_output),
            jsonl(codex_message("assistant", "call b is still unresolved", 5))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    assert_eq!(initial.summary.failed, 1);
    assert_eq!(initial.checkpoint.complete_line_count, 2);

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-b",
            "Process exited with code 0\nOutput:\nfinished\n",
            6,
        )),
    );
    let completed = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(initial.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(completed.summary.failed, 1);
    assert_eq!(completed.checkpoint.complete_line_count, 7);
    assert_eq!(
        completed.checkpoint.committed_offset,
        fs::metadata(&path).unwrap().len()
    );
}

#[test]
fn codex_append_rejects_a_second_session_header_before_commit() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/second-header.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-original")),
            jsonl(codex_message("user", "original session", 1))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    let event_count = store.export_archive().unwrap().events.len();
    append_raw(
            &path,
            &format!(
                "{}{}",
                jsonl(codex_message("assistant", "must roll back", 2)),
                "{\"type\": \"session_meta\", \"payload\": {\"id\": \"codex-second\", \"timestamp\": \"2026-07-14T12:00:03Z\"}}\n"
            ),
        );
    let decision = import_append_capable_provider_file(
        CaptureProvider::Codex,
        &mut store,
        options(
            &path,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            admitted(initial.checkpoint),
        ),
    )
    .unwrap();
    assert!(matches!(
        decision,
        ProviderAppendFileImportDecision::ReplacementRequired(
            ProviderJsonlReplacementReason::AdditionalSessionHeader
        )
    ));
    assert_eq!(store.list_sessions().unwrap().len(), 1);
    assert_eq!(store.export_archive().unwrap().events.len(), event_count);
}

#[test]
fn codex_and_pi_multi_header_replacements_commit_without_append_admission() {
    let cases = [
        (
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            format!(
                "{}{}{}{}",
                jsonl(codex_header("codex-first")),
                jsonl(codex_message("user", "first session", 1)),
                jsonl(codex_header("codex-second")),
                jsonl(codex_message("user", "second session", 2))
            ),
        ),
        (
            CaptureProvider::Pi,
            "pi_session_jsonl",
            "pi_session_jsonl",
            format!(
                "{}{}{}{}",
                jsonl(json!({
                    "type": "session",
                    "id": "pi-first",
                    "timestamp": "2026-07-14T12:00:00Z"
                })),
                jsonl(json!({
                    "type": "message",
                    "id": "pi-first-message",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "message": {"role": "user", "content": "first session"}
                })),
                jsonl(json!({
                    "type": "session",
                    "id": "pi-second",
                    "timestamp": "2026-07-14T12:00:02Z"
                })),
                jsonl(json!({
                    "type": "message",
                    "id": "pi-second-message",
                    "timestamp": "2026-07-14T12:00:03Z",
                    "message": {"role": "user", "content": "second session"}
                }))
            ),
        ),
    ];

    for (index, (provider, inventory_format, material_format, contents)) in
        cases.into_iter().enumerate()
    {
        let temp = tempdir().unwrap();
        let path = temp.path().join(format!("multi-{index}/session.jsonl"));
        write_raw(&path, &contents);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        for _ in 0..2 {
            let decision = import_append_capable_provider_file(
                provider,
                &mut store,
                options(
                    &path,
                    inventory_format,
                    material_format,
                    ProviderAppendFileImportMode::AppendCapableReplacement,
                ),
            )
            .unwrap();
            assert!(matches!(
                decision,
                ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
                    ProviderAppendFileImportWithoutCheckpoint {
                        reason: ProviderJsonlReplacementReason::AdditionalSessionHeader,
                        ..
                    }
                )
            ));
        }
        assert!(!store.list_sessions().unwrap().is_empty());
    }
}
