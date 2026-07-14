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
fn codex_permanent_orphan_keeps_future_large_appends_delta_bounded() {
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
    assert_eq!(first.checkpoint.complete_line_count, 3);
    assert_eq!(
        first.checkpoint.committed_offset,
        fs::metadata(&path).unwrap().len()
    );
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        first.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex replacement must persist typed resume state");
    };
    assert_eq!(state.pending_tool_calls.len(), 1);
    assert_eq!(state.pending_tool_calls[0].call_id, "call-open");

    let mut large_tail = String::new();
    for index in 0..2_048 {
        large_tail.push_str(&jsonl(codex_message(
            "assistant",
            &format!("continuing tail {index}"),
            10,
        )));
    }
    append_raw(&path, &large_tail);
    let continued = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(first.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(continued.summary.imported_events, 2_048);
    assert_eq!(continued.checkpoint.complete_line_count, 2_051);
    assert_eq!(
        continued.checkpoint.committed_offset,
        fs::metadata(&path).unwrap().len()
    );
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        continued.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex continuation must retain typed resume state");
    };
    assert_eq!(state.pending_tool_calls[0].call_id, "call-open");
    assert!(
        continued
            .checkpoint
            .resume_state
            .as_ref()
            .unwrap()
            .encode_persisted_json()
            .unwrap()
            .len()
            <= crate::provider_sources::CODEX_RESUME_MAX_ENCODED_BYTES
    );

    append_raw(
        &path,
        &jsonl(codex_message("assistant", "one final delta", 11)),
    );
    let final_delta = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(continued.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(final_delta.summary.imported_events, 1);
    assert_eq!(final_delta.checkpoint.complete_line_count, 2_052);
}

#[test]
fn codex_late_output_closes_only_its_matching_context_across_refreshes() {
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
    assert_eq!(initial.checkpoint.complete_line_count, 5);
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        initial.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex replacement must persist typed resume state");
    };
    assert_eq!(
        state
            .pending_tool_calls
            .iter()
            .map(|context| context.call_id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-b"]
    );

    append_raw(
        &path,
        &jsonl(codex_message("assistant", "between refreshes", 5)),
    );
    let refreshed = imported(
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
    assert_eq!(refreshed.checkpoint.complete_line_count, 6);
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        refreshed.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex refresh must persist typed resume state");
    };
    assert_eq!(state.pending_tool_calls[0].call_id, "call-b");

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-b",
            "Process exited with code 1\nOutput:\nb failed\n",
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
                admitted(refreshed.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(completed.checkpoint.complete_line_count, 7);
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        completed.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex completion must persist typed resume state");
    };
    assert!(state.pending_tool_calls.is_empty());
    let output = store
        .export_archive()
        .unwrap()
        .events
        .into_iter()
        .find(|event| event.event_type == ctx_history_core::EventType::CommandOutput)
        .expect("failed late output should retain command context");
    assert_eq!(output.payload["body"]["tool"], "exec_command");
    assert_eq!(output.payload["body"]["command"], "cargo test");
}

#[test]
fn codex_resume_overflow_is_oldest_first_bounded_and_restartable() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/resume-overflow.jsonl");
    let mut contents = format!(
        "{}{}",
        jsonl(codex_header("codex-resume-overflow")),
        jsonl(codex_message("user", "many calls", 1))
    );
    for index in 0..80 {
        contents.push_str(&jsonl(codex_call(&format!("call-{index:03}"), 2)));
    }
    write_raw(&path, &contents);
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
    assert_eq!(initial.checkpoint.complete_line_count, 82);
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        initial.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex replacement must persist typed resume state");
    };
    assert_eq!(
        state.pending_tool_calls.len(),
        crate::provider_sources::CODEX_RESUME_MAX_PENDING_TOOL_CALLS
    );
    assert_eq!(state.pending_tool_calls[0].call_id, "call-016");
    assert_eq!(state.pending_tool_calls.last().unwrap().call_id, "call-079");
    assert_eq!(state.dropped_tool_calls, 16);

    let encoded = initial
        .checkpoint
        .resume_state
        .as_ref()
        .unwrap()
        .encode_persisted_json()
        .unwrap();
    assert!(encoded.len() < 65_536);
    let decoded = ProviderJsonlResumeState::decode_persisted_json(&encoded).unwrap();
    let mut restarted_checkpoint = initial.checkpoint;
    restarted_checkpoint.resume_state = Some(decoded);

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-016",
            "Process exited with code 1\nOutput:\nretained late failure\n",
            3,
        )),
    );
    let retained = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(restarted_checkpoint),
            ),
        )
        .unwrap(),
    );
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        retained.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex append must persist typed resume state");
    };
    assert_eq!(state.pending_tool_calls.len(), 63);
    assert_eq!(state.pending_tool_calls[0].call_id, "call-017");
    assert_eq!(state.dropped_tool_calls, 16);

    append_raw(
        &path,
        &jsonl(codex_output(
            "call-000",
            "Process exited with code 1\nOutput:\ndropped late failure\n",
            4,
        )),
    );
    let dropped = imported(
        import_append_capable_provider_file(
            CaptureProvider::Codex,
            &mut store,
            options(
                &path,
                "codex_session_jsonl_tree",
                "codex_session_jsonl",
                admitted(retained.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(dropped.summary.imported_events, 1);
    let archive = store.export_archive().unwrap();
    let dropped_output = archive.events.last().unwrap();
    assert_eq!(
        dropped_output.event_type,
        ctx_history_core::EventType::ToolOutput
    );
    assert_eq!(
        dropped_output.payload["body"]["tool"],
        "function_call_output"
    );
    assert!(dropped_output.payload["body"]["command"].is_null());
}

#[test]
fn codex_blank_orphan_call_id_never_enters_a_persisted_checkpoint() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/blank-call-id.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}{}",
            jsonl(codex_header("codex-blank-call-id")),
            jsonl(codex_call("   ", 1)),
            jsonl(codex_message("user", "keep importing ordinary events", 2))
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
    let initial_resume = initial
        .checkpoint
        .resume_state
        .as_ref()
        .expect("Codex replacement must persist typed resume state");
    initial_resume.validate().unwrap();
    assert_eq!(
        initial_resume,
        &ProviderJsonlResumeState::CodexSession(CodexSessionJsonlResumeState::default())
    );

    append_raw(&path, &jsonl(codex_message("assistant", "next delta", 3)));
    let appended = imported(
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
    appended
        .checkpoint
        .resume_state
        .as_ref()
        .expect("Codex append must persist typed resume state")
        .validate()
        .unwrap();
    assert_eq!(appended.summary.imported_events, 1);
    assert_eq!(store.export_archive().unwrap().events.len(), 3);
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
    assert_eq!(initial.checkpoint.complete_line_count, 6);
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        initial.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex invalid-output import must persist typed resume state");
    };
    assert_eq!(state.pending_tool_calls.len(), 1);
    assert_eq!(state.pending_tool_calls[0].call_id, "call-b");

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
    assert_eq!(completed.summary.failed, 0);
    assert_eq!(completed.checkpoint.complete_line_count, 7);
    assert_eq!(
        completed.checkpoint.committed_offset,
        fs::metadata(&path).unwrap().len()
    );
    let Some(ProviderJsonlResumeState::CodexSession(state)) =
        completed.checkpoint.resume_state.as_ref()
    else {
        panic!("Codex completion must persist typed resume state");
    };
    assert!(state.pending_tool_calls.is_empty());
}
