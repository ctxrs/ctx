#[test]
fn pi_streaming_replacement_preserves_per_session_real_content_admission() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("pi/mixed-sessions.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}{}{}",
            jsonl(json!({
                "type": "session",
                "id": "pi-real",
                "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message",
                "id": "pi-real-message",
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "admitted"}
            })),
            jsonl(json!({
                "type": "session",
                "id": "pi-notice-only",
                "timestamp": "2026-07-14T12:00:02Z"
            })),
            jsonl(json!({
                "type": "model_change",
                "id": "pi-notice",
                "timestamp": "2026-07-14T12:00:03Z",
                "provider": "example",
                "modelId": "notice-only"
            }))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let decision = import_append_capable_provider_file(
        CaptureProvider::Pi,
        &mut store,
        options(
            &path,
            "pi_session_jsonl",
            "pi_session_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
    )
    .unwrap();
    let ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) = decision else {
        panic!("expected non-admitted multi-header replacement");
    };
    assert_eq!(
        result.reason,
        ProviderJsonlReplacementReason::AdditionalSessionHeader
    );
    assert_eq!(result.summary.skipped_sessions, 1);
    assert_eq!(result.summary.skipped_events, 1);
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].external_session_id.as_deref(), Some("pi-real"));
}

#[test]
fn codex_and_pi_replacement_require_a_physical_row_one_header_for_append_admission() {
    let cases = [
        (
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            "{\"type\":\"notice\"}\n".to_owned(),
            format!(
                "{}{}",
                jsonl(codex_header("codex-after-non-header")),
                jsonl(codex_message("user", "codex materializes", 1))
            ),
        ),
        (
            CaptureProvider::Codex,
            "codex_session_jsonl_tree",
            "codex_session_jsonl",
            "{not-json}\n".to_owned(),
            format!(
                "{}{}",
                jsonl(codex_header("codex-after-invalid")),
                jsonl(codex_message("user", "codex materializes", 1))
            ),
        ),
        (
            CaptureProvider::Pi,
            "pi_session_jsonl",
            "pi_session_jsonl",
            jsonl(json!({"type": "notice"})),
            format!(
                "{}{}",
                jsonl(json!({
                    "type": "session",
                    "id": "pi-after-non-header",
                    "timestamp": "2026-07-14T12:00:00Z"
                })),
                jsonl(json!({
                    "type": "message",
                    "id": "pi-user-one",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "message": {"role": "user", "content": "pi materializes"}
                }))
            ),
        ),
        (
            CaptureProvider::Pi,
            "pi_session_jsonl",
            "pi_session_jsonl",
            "{not-json}\n".to_owned(),
            format!(
                "{}{}",
                jsonl(json!({
                    "type": "session",
                    "id": "pi-after-invalid",
                    "timestamp": "2026-07-14T12:00:00Z"
                })),
                jsonl(json!({
                    "type": "message",
                    "id": "pi-user-two",
                    "timestamp": "2026-07-14T12:00:01Z",
                    "message": {"role": "user", "content": "pi materializes"}
                }))
            ),
        ),
    ];

    for (index, (provider, inventory_format, material_format, first_row, valid_rows)) in
        cases.into_iter().enumerate()
    {
        let temp = tempdir().unwrap();
        let path = temp.path().join(format!("row-one-{index}/session.jsonl"));
        write_raw(&path, &format!("{first_row}{valid_rows}"));
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
        match decision {
            ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(result) => {
                assert_eq!(
                    result.reason,
                    ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid
                );
                assert!(result.summary.imported_events > 0);
            }
            other => panic!("expected tolerant uncheckpointed replacement, got {other:?}"),
        }
        assert!(!store.list_sessions().unwrap().is_empty());
    }
}

#[test]
fn codex_multi_header_replacement_without_messages_is_not_append_admitted() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/headers-only.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-header-one")),
            jsonl(codex_header("codex-header-two"))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    assert!(matches!(
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
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
            ProviderAppendFileImportWithoutCheckpoint {
                reason: ProviderJsonlReplacementReason::AdditionalSessionHeader,
                ..
            }
        )
    ));
}

#[test]
fn complete_rejected_codex_row_advances_when_no_semantic_frontier_is_open() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/poison.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-poison")),
            jsonl(codex_message("user", "initial", 1))
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
    append_raw(
        &path,
        "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",oops}\n",
    );
    let rejected = imported(
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
    assert_eq!(rejected.summary.failed, 1);
    assert_eq!(rejected.checkpoint.complete_line_count, 3);
    // Complete deterministic rejected rows are committed poison: advancing
    // through them lets the coordinator converge and elide this unchanged
    // observation instead of retrying the bad row forever.
    assert_eq!(
        rejected.checkpoint.committed_offset,
        fs::metadata(&path).unwrap().len()
    );
}

#[test]
fn codex_parser_stops_at_the_complete_boundary_frozen_by_preflight() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("codex/frozen-boundary.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-frozen")),
            jsonl(codex_message("user", "initial", 1))
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
    append_raw(
        &path,
        &jsonl(codex_message("assistant", "validated delta", 2)),
    );

    let mut reader = match open_provider_jsonl(
        &path,
        ProviderJsonlOpenMode::Append(initial.checkpoint),
    )
    .unwrap()
    {
        ProviderJsonlOpenDecision::Ready(reader) => reader,
        ProviderJsonlOpenDecision::ReplacementRequired(reason) => {
            panic!("unexpected replacement before preflight: {reason}")
        }
    };
    let bootstrap = read_authoritative_codex_header(&mut reader)
        .unwrap()
        .unwrap();
    assert!(!codex_session_reader_has_additional_header(&mut reader).unwrap());
    reader.freeze_at_current_complete_boundary();
    append_raw(&path, &jsonl(codex_header("codex-racing-header")));
    reader.restart_import_position().unwrap();

    let boundary = match import_codex_session_reader_bounded(
        &path,
        &mut reader,
        Some(bootstrap),
        "codex_session_jsonl",
        &mut store,
        None,
        &adapter_context(&path),
        true,
    )
    .unwrap()
    {
        CodexSessionBoundedImport::Imported { boundary, .. } => boundary,
        CodexSessionBoundedImport::ReplacementRequired(reason) => {
            panic!("frozen parser crossed into later append: {reason}")
        }
    };
    assert_eq!(boundary.complete_line_count, 3);
    let checkpoint = reader
        .checkpoint_at(boundary.committed_offset, boundary.complete_line_count)
        .unwrap()
        .unwrap();

    let mut next =
        match open_provider_jsonl(&path, ProviderJsonlOpenMode::Append(checkpoint)).unwrap() {
            ProviderJsonlOpenDecision::Ready(reader) => reader,
            ProviderJsonlOpenDecision::ReplacementRequired(reason) => {
                panic!("unexpected replacement before next preflight: {reason}")
            }
        };
    read_authoritative_codex_header(&mut next).unwrap().unwrap();
    assert!(codex_session_reader_has_additional_header(&mut next).unwrap());
}

#[test]
fn legacy_replacement_wrappers_accept_unterminated_final_records() {
    let temp = tempdir().unwrap();

    let codex_path = temp.path().join("codex/legacy.jsonl");
    write_raw(
        &codex_path,
        &format!(
            "{}{}",
            jsonl(codex_header("codex-legacy-eof")),
            serde_json::to_string(&codex_message("user", "codex eof", 1)).unwrap()
        ),
    );
    let codex = CodexSessionJsonlAdapter
        .normalize_path(&codex_path, &adapter_context(&codex_path))
        .unwrap();
    assert!(provider_normalization_has_real_message(&codex));
    let mut codex_store = Store::open(temp.path().join("codex-fast.sqlite")).unwrap();
    let codex_fast = crate::import_codex_session_jsonl(
        &codex_path,
        &mut codex_store,
        crate::CodexSessionImportOptions {
            imported_at: "2026-07-14T12:00:00Z".parse().unwrap(),
            ..crate::CodexSessionImportOptions::default()
        },
    )
    .unwrap();
    assert_eq!(codex_fast.failed, 0, "{:?}", codex_fast.failures);
    assert_eq!(codex_fast.imported_events, 1);

    let pi_path = temp.path().join("pi/legacy.jsonl");
    write_raw(
        &pi_path,
        &format!(
            "{}{}",
            jsonl(json!({
                "type": "session",
                "id": "pi-legacy-eof",
                "timestamp": "2026-07-14T12:00:00Z"
            })),
            serde_json::to_string(&json!({
                "type": "message",
                "id": "pi-user",
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "pi eof"}
            }))
            .unwrap()
        ),
    );
    let pi = crate::provider::providers::pi::normalize_pi_session_jsonl_file(
        &pi_path,
        &adapter_context(&pi_path),
    )
    .unwrap();
    assert!(provider_normalization_has_real_message(&pi));

    let claude_path = temp.path().join("claude/legacy.jsonl");
    write_raw(
        &claude_path,
        &serde_json::to_string(&json!({
            "sessionId": "claude-legacy-eof",
            "timestamp": "2026-07-14T12:00:01Z",
            "type": "user",
            "uuid": "claude-user",
            "message": {"role": "user", "content": "claude eof"}
        }))
        .unwrap(),
    );
    let claude = crate::provider::providers::claude::normalize_claude_projects_jsonl_file(
        &claude_path,
        &adapter_context(&claude_path),
    )
    .unwrap();
    assert!(provider_normalization_has_real_message(&claude));

    let tabnine_path = temp.path().join("tabnine/legacy.jsonl");
    write_raw(
        &tabnine_path,
        &format!(
            "{}{}",
            jsonl(json!({
                "sessionId": "tabnine-legacy-eof",
                "startTime": "2026-07-14T12:00:00Z"
            })),
            serde_json::to_string(&json!({
                "id": "tabnine-user",
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "user",
                "content": "tabnine eof"
            }))
            .unwrap()
        ),
    );
    let tabnine = crate::provider::providers::native_jsonl::normalize_native_jsonl_session_file(
        &tabnine_path,
        &adapter_context(&tabnine_path),
        CaptureProvider::Tabnine,
        "tabnine_cli_chat_recording_jsonl",
    )
    .unwrap();
    assert!(provider_normalization_has_real_message(&tabnine));
}

#[test]
fn claude_append_persists_an_earlier_delta_start_and_converges_with_replacement() {
    let temp = tempdir().unwrap();
    let replacement_path = temp.path().join("claude/session.jsonl");
    write_raw(
        &replacement_path,
        &format!(
            "{}{}",
            jsonl(json!({
                "sessionId": "claude-persisted-min",
                "timestamp": "2026-07-14T12:00:10Z",
                "type": "user",
                "uuid": "claude-later",
                "message": {"role": "user", "content": "later row first"}
            })),
            jsonl(json!({
                "sessionId": "claude-persisted-min",
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "assistant",
                "uuid": "claude-earlier",
                "message": {"role": "assistant", "content": "earlier row second"}
            }))
        ),
    );
    let earliest: DateTime<Utc> = "2026-07-14T12:00:01Z".parse().unwrap();
    let mut store = Store::open(temp.path().join("claude.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &replacement_path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    let expected_resume_state = Some(ProviderJsonlResumeState::ClaudeProjects(
        ClaudeProjectsJsonlResumeState::new("claude-persisted-min".to_owned(), earliest),
    ));
    assert_eq!(initial.checkpoint.resume_state, expected_resume_state);
    let appended_earliest: DateTime<Utc> = "2026-07-14T11:59:59Z".parse().unwrap();
    append_raw(
        &replacement_path,
        &jsonl(json!({
            "sessionId": "claude-persisted-min",
            "timestamp": appended_earliest,
            "type": "assistant",
            "uuid": "claude-tail",
            "message": {"role": "assistant", "content": "tail moves persisted start earlier"}
        })),
    );
    let delta = imported(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &replacement_path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                admitted(initial.checkpoint.clone()),
            ),
        )
        .unwrap(),
    );
    let expected_delta_resume_state = Some(ProviderJsonlResumeState::ClaudeProjects(
        ClaudeProjectsJsonlResumeState::new("claude-persisted-min".to_owned(), appended_earliest),
    ));
    assert_eq!(delta.checkpoint.resume_state, expected_delta_resume_state);
    assert_eq!(
        store.list_sessions().unwrap()[0].started_at,
        appended_earliest
    );

    let mut replacement_store = Store::open(temp.path().join("claude-full.sqlite")).unwrap();
    let replacement = imported(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut replacement_store,
            options(
                &replacement_path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    assert_eq!(
        replacement.checkpoint.resume_state,
        expected_delta_resume_state
    );
    assert_eq!(
        replacement_store.list_sessions().unwrap()[0].started_at,
        appended_earliest
    );
}

#[test]
fn claude_append_minimum_ignores_rows_without_a_valid_timestamp() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("claude/invalid-time.jsonl");
    let authoritative_started_at: DateTime<Utc> = "2026-07-14T12:00:00Z".parse().unwrap();
    write_raw(
        &path,
        &jsonl(json!({
            "sessionId": "claude-invalid-time",
            "timestamp": authoritative_started_at,
            "type": "user",
            "uuid": "claude-valid-initial",
            "message": {"role": "user", "content": "valid initial time"}
        })),
    );
    let mut store = Store::open(temp.path().join("claude.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    append_raw(
        &path,
        &jsonl(json!({
            "sessionId": "claude-invalid-time",
            "timestamp": "not-a-time",
            "type": "assistant",
            "uuid": "claude-invalid-tail",
            "message": {"role": "assistant", "content": "invalid time must not move start"}
        })),
    );

    let delta = imported(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                admitted(initial.checkpoint),
            ),
        )
        .unwrap(),
    );
    assert_eq!(
        delta.checkpoint.resume_state,
        Some(ProviderJsonlResumeState::ClaudeProjects(
            ClaudeProjectsJsonlResumeState::new(
                "claude-invalid-time".to_owned(),
                authoritative_started_at,
            ),
        ))
    );
    assert_eq!(
        store.list_sessions().unwrap()[0].started_at,
        authoritative_started_at
    );
}

#[test]
fn append_validates_typed_provider_resume_state_before_materialization() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("claude/resume-state.jsonl");
    write_raw(
        &path,
        &jsonl(json!({
            "sessionId": "claude-resume-state",
            "timestamp": "2026-07-14T12:00:00Z",
            "type": "user",
            "uuid": "claude-initial",
            "message": {"role": "user", "content": "initial"}
        })),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );

    let mut missing = initial.checkpoint.clone();
    missing.resume_state = None;
    assert!(matches!(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                admitted(missing),
            ),
        )
        .unwrap(),
        ProviderAppendFileImportDecision::ReplacementRequired(
            ProviderJsonlReplacementReason::AdapterResumeStateMissing
        )
    ));

    let mut wrong_provider = initial.checkpoint.clone();
    wrong_provider.resume_state = Some(ProviderJsonlResumeState::TabnineCli(
        TabnineJsonlResumeState::new(
            "tabnine-wrong-provider".to_owned(),
            "2026-07-14T12:00:00Z".parse().unwrap(),
        ),
    ));
    assert!(matches!(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                admitted(wrong_provider),
            ),
        )
        .unwrap(),
        ProviderAppendFileImportDecision::ReplacementRequired(
            ProviderJsonlReplacementReason::AdapterResumeStateIncompatible
        )
    ));

    let mut unsupported_version = initial.checkpoint;
    let Some(ProviderJsonlResumeState::ClaudeProjects(state)) =
        unsupported_version.resume_state.as_mut()
    else {
        panic!("Claude replacement must produce Claude resume state");
    };
    state.version += 1;
    assert!(matches!(
        import_append_capable_provider_file(
            CaptureProvider::Claude,
            &mut store,
            options(
                &path,
                "claude_projects_jsonl_tree",
                "claude_projects_jsonl_tree",
                admitted(unsupported_version),
            ),
        )
        .unwrap(),
        ProviderAppendFileImportDecision::ReplacementRequired(
            ProviderJsonlReplacementReason::UnsupportedAdapterResumeStateVersion
        )
    ));
}

#[test]
fn claude_and_tabnine_authoritative_identity_changes_are_not_append_admitted() {
    let cases = [
        (
            CaptureProvider::Claude,
            "claude_projects_jsonl_tree",
            "claude_projects_jsonl_tree",
            jsonl(json!({
                "sessionId": "claude-first",
                "timestamp": "2026-07-14T12:00:00Z",
                "type": "user",
                "uuid": "claude-first-message",
                "message": {"role": "user", "content": "first"}
            })),
            jsonl(json!({
                "sessionId": "claude-second",
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "assistant",
                "uuid": "claude-second-message",
                "message": {"role": "assistant", "content": "second"}
            })),
        ),
        (
            CaptureProvider::Tabnine,
            "tabnine_cli_chat_recording_jsonl",
            "tabnine_cli_chat_recording_jsonl",
            format!(
                "{}{}",
                jsonl(json!({
                    "sessionId": "tabnine-first",
                    "startTime": "2026-07-14T12:00:00Z"
                })),
                jsonl(json!({
                    "id": "tabnine-first-message",
                    "timestamp": "2026-07-14T12:00:00Z",
                    "type": "user",
                    "content": "first"
                }))
            ),
            jsonl(json!({
                "sessionId": "tabnine-second",
                "startTime": "2026-07-14T12:00:01Z"
            })),
        ),
    ];

    for (index, (provider, inventory_format, material_format, initial_rows, changed_row)) in
        cases.into_iter().enumerate()
    {
        let temp = tempdir().unwrap();
        let path = temp.path().join(format!("identity-{index}/session.jsonl"));
        write_raw(&path, &initial_rows);
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let initial = imported(
            import_append_capable_provider_file(
                provider,
                &mut store,
                options(
                    &path,
                    inventory_format,
                    material_format,
                    ProviderAppendFileImportMode::AppendCapableReplacement,
                ),
            )
            .unwrap(),
        );
        let event_count = store.export_archive().unwrap().events.len();
        append_raw(&path, &changed_row);
        assert!(matches!(
            import_append_capable_provider_file(
                provider,
                &mut store,
                options(
                    &path,
                    inventory_format,
                    material_format,
                    admitted(initial.checkpoint),
                ),
            )
            .unwrap(),
            ProviderAppendFileImportDecision::ReplacementRequired(
                ProviderJsonlReplacementReason::AuthoritativeSessionChanged
            )
        ));
        assert_eq!(store.export_archive().unwrap().events.len(), event_count);

        let replacement = import_append_capable_provider_file(
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
            replacement,
            ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
                ProviderAppendFileImportWithoutCheckpoint {
                    reason: ProviderJsonlReplacementReason::AuthoritativeSessionChanged,
                    ..
                }
            )
        ));
    }
}
