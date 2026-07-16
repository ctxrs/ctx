#[test]
fn large_pi_claude_and_tabnine_units_emit_bounded_normalization_batches() {
    const ROWS: usize = PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS * 5 + 7;
    let temp = tempdir().unwrap();

    let pi_path = temp.path().join("large/pi.jsonl");
    let mut pi_contents = jsonl(json!({
        "type": "session",
        "id": "pi-large",
        "timestamp": "2026-07-14T12:00:00Z"
    }));
    for index in 0..ROWS {
        pi_contents.push_str(&jsonl(json!({
            "type": "message",
            "id": format!("pi-{index}"),
            "timestamp": "2026-07-14T12:00:01Z",
            "message": {"role": "user", "content": format!("pi row {index}")}
        })));
    }
    write_raw(&pi_path, &pi_contents);
    let mut pi_reader = ProviderJsonlReader::open_append_capable_replacement(&pi_path).unwrap();
    let (mut pi_batches, mut pi_max, mut pi_total) = (0usize, 0usize, 0usize);
    stream_pi_session_jsonl_reader(&mut pi_reader, &adapter_context(&pi_path), None, |batch| {
        pi_batches += 1;
        pi_max = pi_max.max(batch.captures.len());
        pi_total += batch.captures.len();
        Ok(())
    })
    .unwrap();
    assert!(pi_batches > 1);
    assert!(pi_max <= PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS);
    assert_eq!(pi_total, ROWS);

    let claude_path = temp.path().join("large/claude.jsonl");
    let claude_header = json!({
        "sessionId": "claude-large",
        "timestamp": "2026-07-14T12:00:00Z",
        "type": "user",
        "uuid": "claude-header",
        "message": {"role": "user", "content": "header"}
    });
    let mut claude_contents = jsonl(claude_header.clone());
    for index in 0..ROWS {
        claude_contents.push_str(&jsonl(json!({
            "sessionId": "claude-large",
            "timestamp": "2026-07-14T12:00:01Z",
            "type": "assistant",
            "uuid": format!("claude-{index}"),
            "message": {"role": "assistant", "content": format!("claude row {index}")}
        })));
    }
    write_raw(&claude_path, &claude_contents);
    let mut claude_reader =
        ProviderJsonlReader::open_append_capable_replacement(&claude_path).unwrap();
    let (mut claude_batches, mut claude_max, mut claude_total) = (0usize, 0usize, 0usize);
    stream_claude_projects_jsonl_reader(
        &claude_path,
        &mut claude_reader,
        &adapter_context(&claude_path),
        &claude_header,
        "2026-07-14T12:00:00Z".parse().unwrap(),
        |batch| {
            claude_batches += 1;
            claude_max = claude_max.max(batch.captures.len());
            claude_total += batch.captures.len();
            Ok(())
        },
    )
    .unwrap();
    assert!(claude_batches > 1);
    assert!(claude_max <= PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS);
    assert_eq!(claude_total, ROWS + 1);

    let tabnine_path = temp.path().join("large/tabnine.jsonl");
    let tabnine_header = json!({
        "sessionId": "tabnine-large",
        "startTime": "2026-07-14T12:00:00Z"
    });
    let mut tabnine_contents = jsonl(tabnine_header.clone());
    for index in 0..ROWS {
        tabnine_contents.push_str(&jsonl(json!({
            "id": format!("tabnine-{index}"),
            "timestamp": "2026-07-14T12:00:01Z",
            "type": "user",
            "content": format!("tabnine row {index}")
        })));
    }
    write_raw(&tabnine_path, &tabnine_contents);
    let mut tabnine_reader =
        ProviderJsonlReader::open_append_capable_replacement(&tabnine_path).unwrap();
    let (mut tabnine_batches, mut tabnine_max, mut tabnine_total) = (0usize, 0usize, 0usize);
    stream_native_jsonl_session_reader(
        &tabnine_path,
        &mut tabnine_reader,
        &adapter_context(&tabnine_path),
        NativeJsonlStreamOptions {
            provider: CaptureProvider::Tabnine,
            source_format: TABNINE_CLI_SOURCE_FORMAT,
            header: tabnine_header,
            started_at: "2026-07-14T12:00:00Z".parse().unwrap(),
        },
        |batch| {
            tabnine_batches += 1;
            tabnine_max = tabnine_max.max(batch.captures.len());
            tabnine_total += batch.captures.len();
            Ok(())
        },
    )
    .unwrap();
    assert!(tabnine_batches > 1);
    assert!(tabnine_max <= PROVIDER_NORMALIZATION_STREAM_BATCH_UNITS);
    assert_eq!(tabnine_total, ROWS + 1);
}

#[test]
fn streaming_normalizers_preserve_legacy_order_and_identities() {
    fn merge(
        target: &mut crate::ProviderNormalizationResult,
        mut batch: crate::ProviderNormalizationResult,
    ) {
        target.summary.merge(batch.summary);
        target.captures.append(&mut batch.captures);
        target.files_touched.append(&mut batch.files_touched);
    }

    let temp = tempdir().unwrap();
    let pi_path = temp.path().join("equivalence/pi.jsonl");
    write_raw(
        &pi_path,
        &format!(
            "{}{}",
            jsonl(json!({
                "type": "session",
                "id": "pi-equivalent",
                "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message",
                "id": "pi-equivalent-user",
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "same"}
            }))
        ),
    );
    let pi_context = adapter_context(&pi_path);
    let pi_legacy =
        crate::provider::providers::pi::normalize_pi_session_jsonl_file(&pi_path, &pi_context)
            .unwrap();
    let mut pi_streamed = crate::ProviderNormalizationResult::default();
    let mut pi_reader = ProviderJsonlReader::open_replacement(&pi_path).unwrap();
    stream_pi_session_jsonl_reader(&mut pi_reader, &pi_context, None, |batch| {
        merge(&mut pi_streamed, batch);
        Ok(())
    })
    .unwrap();
    assert_eq!(pi_streamed.summary, pi_legacy.summary);
    assert_eq!(pi_streamed.captures, pi_legacy.captures);
    assert_eq!(pi_streamed.files_touched, pi_legacy.files_touched);

    let claude_path = temp.path().join("equivalence/claude.jsonl");
    let claude_header = json!({
        "sessionId": "claude-equivalent",
        "timestamp": "2026-07-14T12:00:10Z",
        "type": "user",
        "uuid": "claude-equivalent-user",
        "message": {"role": "user", "content": "same"}
    });
    write_raw(
        &claude_path,
        &format!(
            "{}{}",
            jsonl(claude_header.clone()),
            jsonl(json!({
                "sessionId": "claude-equivalent",
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "assistant",
                "uuid": "claude-equivalent-assistant",
                "message": {"role": "assistant", "content": "same"}
            }))
        ),
    );
    let claude_context = adapter_context(&claude_path);
    let claude_legacy = crate::provider::providers::claude::normalize_claude_projects_jsonl_file(
        &claude_path,
        &claude_context,
    )
    .unwrap();
    let mut claude_streamed = crate::ProviderNormalizationResult::default();
    let mut claude_reader = ProviderJsonlReader::open_replacement(&claude_path).unwrap();
    stream_claude_projects_jsonl_reader(
        &claude_path,
        &mut claude_reader,
        &claude_context,
        &claude_header,
        "2026-07-14T12:00:01Z".parse().unwrap(),
        |batch| {
            merge(&mut claude_streamed, batch);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(claude_streamed.summary, claude_legacy.summary);
    assert_eq!(claude_streamed.captures, claude_legacy.captures);
    assert_eq!(claude_streamed.files_touched, claude_legacy.files_touched);

    let tabnine_path = temp.path().join("equivalence/tabnine.jsonl");
    let tabnine_header = json!({
        "sessionId": "tabnine-equivalent",
        "startTime": "2026-07-14T12:00:00Z"
    });
    write_raw(
        &tabnine_path,
        &format!(
            "{}{}",
            jsonl(tabnine_header.clone()),
            jsonl(json!({
                "id": "tabnine-equivalent-user",
                "timestamp": "2026-07-14T12:00:01Z",
                "type": "user",
                "content": "same"
            }))
        ),
    );
    let tabnine_context = adapter_context(&tabnine_path);
    let tabnine_legacy =
        crate::provider::providers::native_jsonl::normalize_native_jsonl_session_file(
            &tabnine_path,
            &tabnine_context,
            CaptureProvider::Tabnine,
            TABNINE_CLI_SOURCE_FORMAT,
        )
        .unwrap();
    let mut tabnine_streamed = crate::ProviderNormalizationResult::default();
    let mut tabnine_reader = ProviderJsonlReader::open_replacement(&tabnine_path).unwrap();
    stream_native_jsonl_session_reader(
        &tabnine_path,
        &mut tabnine_reader,
        &tabnine_context,
        NativeJsonlStreamOptions {
            provider: CaptureProvider::Tabnine,
            source_format: TABNINE_CLI_SOURCE_FORMAT,
            header: tabnine_header,
            started_at: "2026-07-14T12:00:00Z".parse().unwrap(),
        },
        |batch| {
            merge(&mut tabnine_streamed, batch);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(tabnine_streamed.summary, tabnine_legacy.summary);
    assert_eq!(tabnine_streamed.captures, tabnine_legacy.captures);
    assert_eq!(tabnine_streamed.files_touched, tabnine_legacy.files_touched);
}

#[test]
fn pi_append_rejects_a_second_session_header() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("pi/session.jsonl");
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(json!({
                "type": "session",
                "id": "pi-one-header",
                "timestamp": "2026-07-14T12:00:00Z"
            })),
            jsonl(json!({
                "type": "message",
                "id": "pi-user",
                "timestamp": "2026-07-14T12:00:01Z",
                "message": {"role": "user", "content": "pi initial user"}
            }))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Pi,
            &mut store,
            options(
                &path,
                "pi_session_jsonl",
                "pi_session_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    append_raw(
        &path,
        &jsonl(json!({
            "type": "session",
            "id": "pi-second-header",
            "timestamp": "2026-07-14T12:00:02Z"
        })),
    );
    let decision = import_append_capable_provider_file(
        CaptureProvider::Pi,
        &mut store,
        options(
            &path,
            "pi_session_jsonl",
            "pi_session_jsonl",
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
}

#[test]
fn replacement_without_authoritative_first_row_is_not_append_admitted() {
    let temp = tempdir().unwrap();

    let claude_path = temp.path().join("claude/session.jsonl");
    write_raw(
        &claude_path,
        &jsonl(json!({
            "sessionId": "claude-no-authoritative-time",
            "type": "user",
            "message": {"role": "user", "content": "claude initial"}
        })),
    );
    let mut claude_store = Store::open(temp.path().join("claude.sqlite")).unwrap();
    let claude_decision = import_append_capable_provider_file(
        CaptureProvider::Claude,
        &mut claude_store,
        options(
            &claude_path,
            "claude_projects_jsonl_tree",
            "claude_projects_jsonl_tree",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
    )
    .unwrap();
    assert!(matches!(
        claude_decision,
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
            ProviderAppendFileImportWithoutCheckpoint {
                reason: ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
                ..
            }
        )
    ));

    let tabnine_path = temp.path().join("tabnine/session.jsonl");
    write_raw(
        &tabnine_path,
        &format!(
            "{}{}",
            jsonl(json!({
                "id": "not-the-header",
                "timestamp": "2026-07-14T12:00:00Z",
                "type": "user",
                "content": "tabnine initial"
            })),
            jsonl(json!({
                "sessionId": "tabnine-late-header",
                "startTime": "2026-07-14T12:00:00Z"
            }))
        ),
    );
    let mut tabnine_store = Store::open(temp.path().join("tabnine.sqlite")).unwrap();
    let tabnine_decision = import_append_capable_provider_file(
        CaptureProvider::Tabnine,
        &mut tabnine_store,
        options(
            &tabnine_path,
            "tabnine_cli_chat_recording_jsonl",
            "tabnine_cli_chat_recording_jsonl",
            ProviderAppendFileImportMode::AppendCapableReplacement,
        ),
    )
    .unwrap();
    assert!(matches!(
        tabnine_decision,
        ProviderAppendFileImportDecision::ImportedWithoutCheckpoint(
            ProviderAppendFileImportWithoutCheckpoint {
                reason: ProviderJsonlReplacementReason::AuthoritativeHeaderInvalid,
                ..
            }
        )
    ));
}

#[test]
fn tabnine_missing_or_invalid_row_one_start_imports_without_append_admission() {
    for (index, start_time) in [None, Some("not-a-timestamp")].into_iter().enumerate() {
        let temp = tempdir().unwrap();
        let path = temp
            .path()
            .join(format!("tabnine-start-{index}/session.jsonl"));
        let mut header = json!({
            "sessionId": format!("tabnine-start-{index}")
        });
        if let Some(start_time) = start_time {
            header["startTime"] = Value::String(start_time.to_owned());
        }
        write_raw(
            &path,
            &format!(
                "{}{}",
                jsonl(header),
                jsonl(json!({
                    "id": format!("tabnine-user-{index}"),
                    "timestamp": "2026-07-14T12:00:01Z",
                    "type": "user",
                    "content": "materialize despite invalid append header"
                }))
            ),
        );
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let decision = import_append_capable_provider_file(
            CaptureProvider::Tabnine,
            &mut store,
            options(
                &path,
                "tabnine_cli_chat_recording_jsonl",
                "tabnine_cli_chat_recording_jsonl",
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
        assert_eq!(store.list_sessions().unwrap().len(), 1);
    }
}

#[test]
fn tabnine_append_uses_the_started_at_persisted_by_replacement() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("tabnine-start/session.jsonl");
    let authoritative_started_at: DateTime<Utc> = "2026-07-14T10:00:00Z".parse().unwrap();
    write_raw(
        &path,
        &format!(
            "{}{}",
            jsonl(json!({
                "sessionId": "tabnine-persisted-start",
                "startTime": authoritative_started_at.to_rfc3339(),
                "timestamp": "2026-07-14T11:00:00Z"
            })),
            jsonl(json!({
                "id": "tabnine-initial",
                "timestamp": "2026-07-14T10:00:01Z",
                "type": "user",
                "content": "initial"
            }))
        ),
    );
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let initial = imported(
        import_append_capable_provider_file(
            CaptureProvider::Tabnine,
            &mut store,
            options(
                &path,
                "tabnine_cli_chat_recording_jsonl",
                "tabnine_cli_chat_recording_jsonl",
                ProviderAppendFileImportMode::AppendCapableReplacement,
            ),
        )
        .unwrap(),
    );
    let expected_resume_state = Some(ProviderJsonlResumeState::TabnineCli(
        TabnineJsonlResumeState::new(
            "tabnine-persisted-start".to_owned(),
            authoritative_started_at,
        ),
    ));
    assert_eq!(initial.checkpoint.resume_state, expected_resume_state);
    assert_eq!(
        store.list_sessions().unwrap()[0].started_at,
        authoritative_started_at
    );

    append_raw(
        &path,
        &jsonl(json!({
            "id": "tabnine-tail",
            "timestamp": "2026-07-14T10:00:02Z",
            "type": "tabnine",
            "content": "tail"
        })),
    );
    let mut append_options = options(
        &path,
        "tabnine_cli_chat_recording_jsonl",
        "tabnine_cli_chat_recording_jsonl",
        admitted(initial.checkpoint),
    );
    append_options.imported_at = "2026-07-15T18:00:00Z".parse().unwrap();
    let appended = imported(
        import_append_capable_provider_file(CaptureProvider::Tabnine, &mut store, append_options)
            .unwrap(),
    );
    assert_eq!(appended.checkpoint.resume_state, expected_resume_state);
    assert_eq!(
        store.list_sessions().unwrap()[0].started_at,
        authoritative_started_at
    );
}
