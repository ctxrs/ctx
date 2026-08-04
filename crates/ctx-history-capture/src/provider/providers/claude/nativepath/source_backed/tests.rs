use super::super::invocation_evidence::CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL;
use super::*;
use ctx_history_core::{
    CertifiedSource, RepositoryFileInvocationKind, ScannedSourceCounts, SourceObservation,
};
use ctx_history_index::{GenerationWriter, WriterOptions};

fn initialized_test_repository() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    for arguments in [
        &["init", "-q"][..],
        &["config", "user.name", "ctx test"],
        &["config", "user.email", "ctx@example.invalid"],
    ] {
        assert!(std::process::Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(temp.path().join("tracked.txt"), "tracked\n").unwrap();
    for arguments in [&["add", "tracked.txt"][..], &["commit", "-qm", "fixture"]] {
        assert!(std::process::Command::new("/usr/bin/git")
            .args(arguments)
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success());
    }
    temp
}

#[test]
fn claude_body_above_the_page_target_is_retained_whole() {
    let body = format!(
        "{}claude-full-body-tail",
        "x".repeat(8 * 1024 * 1024 + 64 * 1024)
    );
    let bytes = serde_json::json!({
        "type": "user",
        "sessionId": "large-body-session",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "user", "content": body},
    })
    .to_string()
    .into_bytes();
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("large-body-session.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };

    let parsed = parse_native_record(&bytes, 0, &locator).unwrap();

    assert_eq!(parsed.rows.len(), 1);
    assert_eq!(parsed.rows[0].body.as_deref(), Some(body.as_str()));
    assert!(parsed.rows[0]
        .body
        .as_deref()
        .is_some_and(|value| value.ends_with("claude-full-body-tail")));
}

#[test]
fn claude_oversized_command_abstains_without_session_cwd_fallback() {
    let temp = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("/usr/bin/git")
        .args(["init", "-q"])
        .current_dir(temp.path())
        .status()
        .unwrap()
        .success());
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "uuid": "oversized-command-record",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "cwd": temp.path(),
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "oversized-command",
                "name": "Bash",
                "input": {"command": "x".repeat(1024 * 1024 + 1)}
            }]
        }
    }))
    .unwrap();
    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one Claude tool-call record");
    };
    assert!(record.repository_bindings.is_empty());
    assert!(record
        .repository_abstentions
        .iter()
        .any(|abstention| { abstention.reason == RepositoryAbstentionReason::CommandTooLarge }));
}

#[test]
fn exact_file_invocations_emit_per_call_provider_neutral_evidence() {
    let temp = initialized_test_repository();

    let calls = serde_json::json!([
        {
            "type": "tool_use",
            "id": "read-call",
            "name": "Read",
            "input": {"file_path": "src/read.rs"}
        },
        {
            "type": "tool_use",
            "id": "edit-call",
            "name": "Edit",
            "input": {
                "file_path": "src/edit.rs",
                "old_string": "before",
                "new_string": "after"
            }
        },
        {
            "type": "tool_use",
            "id": "write-call",
            "name": "Write",
            "input": {"file_path": "src/write.rs", "content": "complete"}
        },
        {
            "type": "tool_use",
            "id": "delete-call",
            "name": "Delete",
            "input": {"path": "src/delete.rs"}
        },
        {
            "type": "tool_use",
            "id": "create-call",
            "name": "Create",
            "input": {"path": "src/create.rs", "content": "new"}
        },
        {
            "type": "tool_use",
            "id": "rename-call",
            "name": "Rename",
            "input": {"old_path": "src/old.rs", "new_path": "src/new.rs"}
        }
    ]);
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "uuid": "exact-file-calls",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "cwd": temp.path(),
        "message": {"role": "assistant", "content": calls}
    }))
    .unwrap();
    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let expected = [
        (
            "Read",
            RepositoryFileInvocationKind::Read,
            "src/read.rs",
            None,
        ),
        (
            "Edit",
            RepositoryFileInvocationKind::Modify,
            "src/edit.rs",
            None,
        ),
        (
            "Write",
            RepositoryFileInvocationKind::Write,
            "src/write.rs",
            None,
        ),
        (
            "Delete",
            RepositoryFileInvocationKind::Delete,
            "src/delete.rs",
            None,
        ),
        (
            "Create",
            RepositoryFileInvocationKind::Create,
            "src/create.rs",
            None,
        ),
        (
            "Rename",
            RepositoryFileInvocationKind::Rename,
            "src/new.rs",
            Some("src/old.rs"),
        ),
    ];
    assert_eq!(emitted.len(), expected.len());
    for (index, (record, (tool_name, kind, path, prior_path))) in
        emitted.iter().zip(expected).enumerate()
    {
        let [evidence] = record.repository_file_invocation_evidence.as_slice() else {
            panic!("expected one exact invocation for {tool_name}");
        };
        assert_eq!(evidence.operation_ordinal, u32::try_from(index).unwrap());
        assert_eq!(evidence.tool_name.as_deref(), Some(tool_name));
        assert_eq!(evidence.kind, kind);
        assert_eq!(evidence.relative_path, path);
        assert_eq!(evidence.prior_relative_path.as_deref(), prior_path);
        let range = evidence.normalized_text_range.unwrap();
        let body = record.content.normalized_body.as_deref().unwrap();
        let selected = &body[range.start as usize..range.end as usize];
        let input = record
            .content
            .structured_content
            .as_ref()
            .unwrap()
            .get("input")
            .unwrap();
        assert_eq!(selected, serde_json::to_string(input).unwrap());
        assert!(!record.repository_file_observations.is_empty());
        record.validate_contract().unwrap();
    }
}

#[test]
fn exact_multi_path_call_emits_one_call_local_vector_with_one_complete_input_range() {
    let temp = initialized_test_repository();
    let input = serde_json::json!({
        "paths": ["src/first.rs", "src/second.rs"],
        "content": "same native call"
    });
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "uuid": "exact-multi-path-call",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "cwd": temp.path(),
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": "write-multiple",
            "name": "Write",
            "input": &input,
        }]}
    }))
    .unwrap();
    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected one multi-path tool-call record");
    };
    assert!(record.repository_file_observations.is_empty());
    assert_eq!(record.repository_file_invocation_evidence.len(), 2);
    assert!(record
        .repository_file_invocation_evidence
        .iter()
        .all(|evidence| {
            evidence.operation_ordinal == 0
                && evidence.kind == RepositoryFileInvocationKind::Write
                && evidence.tool_name.as_deref() == Some("Write")
        }));
    assert_eq!(
        record
            .repository_file_invocation_evidence
            .iter()
            .map(|evidence| evidence.relative_path.as_str())
            .collect::<Vec<_>>(),
        vec!["src/first.rs", "src/second.rs"]
    );
    let body = record.content.normalized_body.as_deref().unwrap();
    let expected_input = serde_json::to_string(&input).unwrap();
    for evidence in &record.repository_file_invocation_evidence {
        let range = evidence.normalized_text_range.unwrap();
        assert_eq!(
            &body[range.start as usize..range.end as usize],
            expected_input
        );
    }
    record.validate_contract().unwrap();
}

#[test]
fn legacy_file_touches_remain_additive_for_non_strict_shapes() {
    let content = serde_json::json!([
        {
            "type": "tool_use",
            "id": "glob-call",
            "name": "Glob",
            "input": {"path": "src"}
        },
        {
            "type": "tool_use",
            "id": "grep-call",
            "name": "Grep",
            "input": {"file_path": "src/lib.rs"}
        },
        {
            "type": "tool_use",
            "id": "patch-call",
            "name": "apply_patch",
            "input": {
                "patch": "*** Update File: src/a.rs\n*** Add File: src/b.rs\n*** Delete File: src/c.rs"
            }
        },
        {
            "type": "tool_use",
            "id": "ambiguous-read",
            "name": "Read",
            "input": {"file_path": "src/one.rs", "path": "src/two.rs"}
        }
    ]);
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "uuid": "legacy-file-touches",
        "sessionId": "test-session",
        "message": {"role": "assistant", "content": content}
    }))
    .unwrap();
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("legacy-file-touches.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };

    let parsed = parse_native_record(&bytes, 0, &locator).unwrap();
    assert_eq!(parsed.rows.len(), 4);
    let calls = parsed
        .rows
        .iter()
        .map(|row| row.tool_call.as_ref().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(calls[0].file_touches[0].path, "src");
    assert_eq!(calls[1].file_touches[0].path, "src/lib.rs");
    assert_eq!(
        calls[2]
            .file_touches
            .iter()
            .map(|touch| touch.path.as_str())
            .collect::<Vec<_>>(),
        vec!["src/a.rs", "src/b.rs", "src/c.rs"]
    );
    assert_eq!(calls[3].file_touches.len(), 2);
    assert!(calls
        .iter()
        .all(|call| call.exact_file_invocations.is_empty()));
}

fn fallback_row(body: &str, ordinal: u64) -> ClaudeRetainedRow {
    let bytes = serde_json::json!({
        "type": "user",
        "sessionId": "fallback-session",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "user", "content": body},
    })
    .to_string()
    .into_bytes();
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("fallback-session.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: ordinal + 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };
    parse_native_record(&bytes, ordinal, &locator)
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap()
}

fn fallback_tool_call_row() -> ClaudeRetainedRow {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "sessionId": "fallback-session",
        "timestamp": "2026-07-31T12:00:00Z",
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "name": "Write",
            "input": {"file_path": "src/lib.rs", "content": "baseline"}
        }]},
    }))
    .unwrap();
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("fallback-session.jsonl"),
        byte_start: 0,
        byte_end_exclusive: bytes.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(&bytes).into(),
    };
    parse_native_record(&bytes, 0, &locator)
        .unwrap()
        .rows
        .into_iter()
        .next()
        .unwrap()
}

#[test]
fn idless_tool_call_fallback_identity_excludes_additive_invocation_cache() {
    let row = fallback_tool_call_row();
    let call = row.tool_call.as_ref().unwrap();
    assert!(!call.exact_file_invocations.is_empty());
    assert!(serde_json::to_value(call)
        .unwrap()
        .get("exact_file_invocations")
        .is_none());

    let mut baseline_row = row.clone();
    baseline_row
        .tool_call
        .as_mut()
        .unwrap()
        .exact_file_invocations = Default::default();
    assert_eq!(
        fallback_event_digest(&row).unwrap(),
        fallback_event_digest(&baseline_row).unwrap()
    );

    let key = ClaudeSessionKey {
        root_session_id: "fallback-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let source = source_key(&key).unwrap();
    let session_id = session_identity(&source, &session_typed_key(&key).unwrap()).unwrap();
    let (event_id, _) = fallback_event_id(
        &row,
        &source,
        session_id,
        &mut FallbackEventIdentityState::default(),
    );
    let (baseline_event_id, _) = fallback_event_id(
        &baseline_row,
        &source,
        session_id,
        &mut FallbackEventIdentityState::default(),
    );
    assert_eq!(event_id, baseline_event_id);
}

fn fallback_event_ids(bodies: &[&str]) -> Vec<StableEntityId> {
    let key = ClaudeSessionKey {
        root_session_id: "fallback-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let source = source_key(&key).unwrap();
    let session_id = session_identity(&source, &session_typed_key(&key).unwrap()).unwrap();
    let mut state = FallbackEventIdentityState::default();
    bodies
        .iter()
        .enumerate()
        .map(|(ordinal, body)| {
            let row = fallback_row(body, ordinal as u64);
            let fallback = next_fallback_event_identity(&row, &source, session_id, &mut state)
                .unwrap()
                .unwrap();
            let native_item_key = native_item_key(&row, Some(fallback)).unwrap();
            derive_event_id(EventIdentityInput {
                source: &source,
                session_id,
                logical_item_kind: LOGICAL_EVENT_KIND,
                native_item_key: &native_item_key,
                subrecord_selector: None,
            })
            .unwrap()
        })
        .collect()
}

fn fallback_event_id(
    row: &ClaudeRetainedRow,
    source: &SourceKey,
    session_id: StableEntityId,
    state: &mut FallbackEventIdentityState,
) -> (StableEntityId, TypedKey) {
    let fallback = next_fallback_event_identity(row, source, session_id, state)
        .unwrap()
        .unwrap();
    let native_item_key = native_item_key(row, Some(fallback)).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    let native_event_id = native_event_typed_key(row, Some(fallback)).unwrap();
    (event_id, native_event_id)
}

fn base_lookup_with_events(
    source: &SourceKey,
    session_id: StableEntityId,
    events: &[(StableEntityId, TypedKey)],
) -> (tempfile::TempDir, BaseEventIdentityLookup) {
    let temp = tempfile::tempdir().unwrap();
    let options = WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    };
    let mut writer = GenerationWriter::open(temp.path(), options.clone())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for (index, (event_id, native_event_id)) in events.iter().enumerate() {
        let event_sequence = u64::try_from(index).unwrap() + 1;
        let mut record = CoreRecord::new_selected(
            *event_id,
            session_id,
            session_id,
            source.clone(),
            event_sequence,
            "message",
            "primary",
            true,
            PARSER_REVISION,
            "Claude fallback lookup test",
        )
        .unwrap();
        record.provider_session_id = Some("fallback-session".to_owned());
        record.native_event_id = Some(native_event_id.clone());
        record.occurred_at_unix_ms = Some(i64::try_from(event_sequence).unwrap());
        record.role = Some("user".to_owned());
        writer.add_core_record(record).unwrap();
    }
    let observation =
        SourceObservation::new(source.clone(), "fallback-test-source-v1", vec![1]).unwrap();
    let count = u64::try_from(events.len()).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                PARSER_REVISION,
                [1; 32],
                ScannedSourceCounts {
                    complete_records: count,
                    retained_records: count,
                    indexed_documents: count,
                    certified_bytes: count,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    let writer = GenerationWriter::open(temp.path(), options)
        .unwrap()
        .into_writer()
        .unwrap();
    let lookup = writer.base_event_identity_lookup();
    drop(writer);
    (temp, lookup)
}

#[test]
fn duplicate_call_ids_are_ambiguous_and_result_linkage_abstains() {
    let mut pending_calls = HashMap::new();
    let mut capacity_exceeded = false;
    for command in ["git commit -m first", "git commit -m second"] {
        remember_pending_call(
            &mut pending_calls,
            &mut capacity_exceeded,
            "duplicate-call",
            PendingCallState::Exact(PendingCall {
                command: Some(command.to_owned()),
                command_too_large: false,
                declared_workdir: Some("/tmp/repository".to_owned()),
                event_sequence: 1,
            }),
        );
    }
    assert!(matches!(
        pending_calls.get("duplicate-call"),
        Some(PendingCallState::Ambiguous)
    ));

    let mut input = AttributionInput::default();
    let (context, abstained) = resolve_pending_call(
        &mut pending_calls,
        Some("duplicate-call"),
        capacity_exceeded,
        &mut input,
    );
    assert!(context.is_none());
    assert!(abstained);
    let annotation = RepositoryAttributor::default().attribute(input);
    assert!(annotation.repository_vcs_observations.is_empty());
    assert!(annotation.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::ProviderOutputUnjoined
            && abstention.detail.as_deref() == Some("claude_tool_result_call_id_is_ambiguous")
    }));
}

fn test_projector() -> ClaudeProjector {
    let key = ClaudeSessionKey {
        root_session_id: "test-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let binding = Binding {
        project_dir: PathBuf::from("/tmp/project"),
        key: key.clone(),
        layout: SessionLayout::Primary,
    };
    ClaudeProjector {
        source: source_key(&key).unwrap(),
        source_path: "test-session.jsonl".to_owned(),
        identities: identities(&binding).unwrap(),
        binding,
        session: ClaudeSessionMetadata::new(key),
        attributor: RepositoryAttributor::default(),
        pending_calls: HashMap::new(),
        linkage_capacity_exceeded: false,
        rejected_records: 0,
        fallback_identities: FallbackEventIdentityState::default(),
    }
}

#[test]
fn sixty_five_small_result_blocks_are_all_emitted() {
    let content = (0..65)
        .map(|index| {
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": format!("call-{index}"),
                "content": format!("result-{index}"),
                "is_error": false,
            })
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "sixty-five-results",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "message": {"role": "user", "content": content},
    }))
    .unwrap();
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert_eq!(emitted.len(), 65);
    assert_eq!(projector.rejected_records(), 0);
    assert_eq!(
        emitted.first().unwrap().content.normalized_body.as_deref(),
        Some("result-0")
    );
    assert_eq!(
        emitted.last().unwrap().content.normalized_body.as_deref(),
        Some("result-64")
    );
}

#[test]
fn exact_multi_path_overflow_abstains_without_truncating_or_rejecting_the_record() {
    let temp = initialized_test_repository();
    let paths = (0..=CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL)
        .map(|index| format!("src/file-{index}.rs"))
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "uuid": "exact-file-overflow",
        "sessionId": "test-session",
        "cwd": temp.path(),
        "message": {"role": "assistant", "content": [{
            "type": "tool_use",
            "id": "read-overflow",
            "name": "Read",
            "input": {"paths": paths},
        }]},
    }))
    .unwrap();

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    let [record] = emitted.as_slice() else {
        panic!("expected the overflowing native call to remain available");
    };
    assert!(record.repository_file_invocation_evidence.is_empty());
    assert!(record.repository_bindings.is_empty());
    assert!(record.repository_abstentions.iter().any(|abstention| {
        abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded
            && abstention.detail.as_deref()
                == Some("claude_exact_file_invocation_capacity_exceeded")
    }));
    assert_eq!(projector.rejected_records(), 0);
}

#[test]
fn exact_file_invocation_boundaries_are_typed_and_fail_closed() {
    let temp = initialized_test_repository();
    for index in 0..CLAUDE_MAX_EXACT_FILE_INVOCATIONS_PER_CALL {
        std::fs::write(temp.path().join(format!("file-{index}.rs")), "fixture\n").unwrap();
    }

    for count in [32, 33, 64, 65] {
        let paths = (0..count)
            .map(|index| format!("file-{index}.rs"))
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "type": "assistant",
            "uuid": format!("exact-file-boundary-{count}"),
            "sessionId": "test-session",
            "cwd": temp.path(),
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": format!("read-{count}"),
                "name": "Read",
                "input": {"paths": paths},
            }]},
        }))
        .unwrap();
        let mut projector = test_projector();
        let mut emitted = Vec::new();
        projector
            .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
                emitted.push(record);
                Ok(())
            })
            .unwrap();
        let [record] = emitted.as_slice() else {
            panic!("expected one boundary record for {count}");
        };
        if count == 32 {
            assert_eq!(record.repository_file_invocation_evidence.len(), 32);
            assert_eq!(record.repository_bindings.len(), 1);
            assert!(!record.repository_abstentions.iter().any(|abstention| {
                abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded
            }));
        } else {
            assert!(record.repository_file_invocation_evidence.is_empty());
            assert!(record.repository_bindings.is_empty());
            assert!(record.repository_abstentions.iter().any(|abstention| {
                abstention.reason == RepositoryAbstentionReason::CandidateLimitExceeded
            }));
        }
    }

    for (input, reason) in [
        (
            serde_json::json!({"paths": ["x".repeat(16 * 1024 + 1)]}),
            RepositoryAbstentionReason::CandidateLimitExceeded,
        ),
        (
            serde_json::json!({"paths": ["file-0.rs", "file-0.rs"]}),
            RepositoryAbstentionReason::Ambiguous,
        ),
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "type": "assistant",
            "uuid": format!("exact-file-inexact-{reason:?}"),
            "sessionId": "test-session",
            "cwd": temp.path(),
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "inexact",
                "name": "Read",
                "input": input,
            }]},
        }))
        .unwrap();
        let mut projector = test_projector();
        let mut emitted = Vec::new();
        projector
            .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
                emitted.push(record);
                Ok(())
            })
            .unwrap();
        let [record] = emitted.as_slice() else {
            panic!("expected one inexact record");
        };
        assert!(record.repository_bindings.is_empty());
        assert!(record.repository_file_invocation_evidence.is_empty());
        assert!(record
            .repository_abstentions
            .iter()
            .any(|abstention| abstention.reason == reason));
    }
}

#[test]
fn malformed_empty_and_row_overflow_records_are_counted_rejections() {
    let malformed = b"{\"type\":\"user\",\"sessionId\":\"test-session\",\"message\":";
    let empty =
        br#"{"type":"user","sessionId":"test-session","message":{"role":"user","content":[]}}"#;
    let overflow_content = (0..=CLAUDE_MAX_RECORD_ROWS)
        .map(|_| serde_json::json!({"type": "tool_result"}))
        .collect::<Vec<_>>();
    let overflow = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "overflow-results",
        "sessionId": "test-session",
        "message": {"role": "user", "content": overflow_content},
    }))
    .unwrap();
    assert!(overflow.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);
    let locator = ClaudePhysicalLocator {
        path: PathBuf::from("overflow-results.jsonl"),
        byte_start: 0,
        byte_end_exclusive: overflow.len() as u64,
        line_number: 1,
        record_sha256: Sha256::digest(&overflow).into(),
    };
    let overflow_error = parse_native_record(&overflow, 0, &locator).unwrap_err();
    assert!(overflow_error
        .to_string()
        .contains("Claude result exceeds the representable row limit"));

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(malformed, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    assert_eq!(projector.rejected_records(), 1);
    projector
        .project(JsonlRecordRef::for_test(empty, 1), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    assert_eq!(projector.rejected_records(), 2);
    projector
        .project(JsonlRecordRef::for_test(&overflow, 2), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert!(emitted.is_empty());
    assert_eq!(projector.rejected_records(), 3);
}

#[test]
fn exact_linked_unknown_tool_result_is_emitted_without_outcome_evidence() {
    let call = br#"{"type":"assistant","uuid":"call-record","sessionId":"test-session","message":{"role":"assistant","content":[{"type":"tool_use","id":"unknown-call","name":"Read","input":{"file_path":"src/lib.rs"}}]}}"#;
    let result = br#"{"type":"user","uuid":"result-record","sessionId":"test-session","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"unknown-call","content":"exact unknown output"}]}}"#;
    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(call, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();
    projector
        .project(JsonlRecordRef::for_test(result, 1), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    assert_eq!(emitted.len(), 2);
    assert!(emitted[1]
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("exact unknown output")));
    assert!(emitted[1].repository_vcs_observations.is_empty());
}

#[test]
fn over_8_mib_tool_result_is_admitted_complete_without_structured_body_duplication() {
    let tail = "claude_large_result_tail_complete";
    let full_result = format!("{} {tail}", "x".repeat(9 * 1024 * 1024));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "large-result-record",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "message": {
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": "large-result-call",
                "content": full_result,
                "is_error": false
            }]
        }
    }))
    .unwrap();
    assert!(bytes.len() > 8 * 1024 * 1024);
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one Claude result record");
    };
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(full_result.as_str())
    );
    let structured = record.content.structured_content.as_ref().unwrap();
    assert_eq!(structured["result_content_location"], "normalized_body");
    assert_eq!(structured["result_content_complete"], true);
    let encoded_structured = serde_json::to_vec(structured).unwrap();
    assert!(encoded_structured.len() < 4 * 1024);
    assert!(!String::from_utf8(encoded_structured)
        .unwrap()
        .contains(tail));
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
}

#[test]
fn claude_large_tool_arguments_preserve_body_and_identity_within_aggregate_limit() {
    let tail = "claude_large_tool_argument_tail_complete";
    let full_argument = format!("{}{tail}", "x".repeat(8 * 1024 * 1024));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "assistant",
        "uuid": "large-tool-call-record",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "message": {
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "large-tool-call",
                "name": "custom_complete_tool",
                "input": {"prompt": &full_argument}
            }]
        }
    }))
    .unwrap();
    assert!(bytes.len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

    let mut projector = test_projector();
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one Claude tool-call record");
    };
    let normalized = record.content.normalized_body.as_deref().unwrap();
    let expected_native_parts = vec![
        TypedKey::utf8("large-tool-call-record").unwrap(),
        TypedKey::U64(0),
    ];
    let expected_native_event_id = TypedKey::composite(expected_native_parts.clone()).unwrap();
    let expected_event_id = derive_event_id(EventIdentityInput {
        source: &record.source,
        session_id: record.session_id,
        logical_item_kind: "claude-event",
        native_item_key: &NativeItemKey::composite("claude.event", expected_native_parts).unwrap(),
        subrecord_selector: None,
    })
    .unwrap();
    let duplicate_structured = serde_json::json!({
        "type": "tool_use",
        "id": "large-tool-call",
        "name": "custom_complete_tool",
        "input": {"prompt": &full_argument},
    });
    assert!(normalized.contains(tail));
    assert_eq!(record.event_id, expected_event_id);
    assert_eq!(
        record.native_event_id.as_ref(),
        Some(&expected_native_event_id)
    );
    assert!(record.content.structured_content.is_none());
    assert!(
        normalized.len() + serde_json::to_vec(&duplicate_structured).unwrap().len()
            > ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
    assert!(
        record.content.encoded_content_bytes().unwrap() <= ctx_history_core::MAX_CORE_CONTENT_BYTES
    );
    record.validate_contract().unwrap();
    record.encode_stored().unwrap();
}

#[test]
fn source_storage_project_path_never_becomes_core_workspace() {
    let source_storage = "/home/private-user/.claude/projects/-home-private-user-secret-project";
    let logical_workspace = "/workspace/provider-native-project";
    let bytes = serde_json::to_vec(&serde_json::json!({
        "type": "user",
        "uuid": "privacy-record",
        "sessionId": "test-session",
        "timestamp": "2026-08-01T12:00:00Z",
        "cwd": logical_workspace,
        "message": {"role": "user", "content": "privacy-safe message"}
    }))
    .unwrap();
    let mut projector = test_projector();
    projector.binding.project_dir = PathBuf::from(source_storage);
    projector.source_path = format!("{source_storage}/test-session.jsonl");
    let mut emitted = Vec::new();
    projector
        .project(JsonlRecordRef::for_test(&bytes, 0), &mut |record| {
            emitted.push(record);
            Ok(())
        })
        .unwrap();

    let [record] = emitted.as_slice() else {
        panic!("expected exactly one Claude message record");
    };
    assert_eq!(record.workspace, None);
    assert_eq!(record.cwd.as_deref(), Some(logical_workspace));
    let encoded = String::from_utf8(record.encode_stored().unwrap()).unwrap();
    assert!(!encoded.contains(source_storage));
    assert!(!encoded.contains("/home/private-user/.claude/projects"));
}

#[test]
fn checkpoint_byte_overflow_degrades_to_typed_linkage_capacity() {
    let mut projector = test_projector();
    projector.remember_pending_call(
        "oversized-call",
        PendingCallState::Exact(PendingCall {
            command: Some("x".repeat(MAX_PROJECTOR_CHECKPOINT_BYTES)),
            command_too_large: false,
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 1,
        }),
    );

    assert!(projector.pending_calls.is_empty());
    assert!(projector.linkage_capacity_exceeded);
    assert!(encode_projector_checkpoint(&projector).is_ok());
    let mut input = AttributionInput::default();
    let (context, abstained) = resolve_pending_call(
        &mut projector.pending_calls,
        Some("oversized-call"),
        projector.linkage_capacity_exceeded,
        &mut input,
    );
    assert!(context.is_none());
    assert!(abstained);
    assert!(input
        .outcome_abstentions
        .iter()
        .any(|(reason, _)| { *reason == RepositoryAbstentionReason::LinkageCapacityExceeded }));
}

#[test]
fn fallback_event_ids_survive_insert_and_delete_before_with_stable_duplicates() {
    let baseline = fallback_event_ids(&["prefix", "target", "suffix"]);
    let inserted = fallback_event_ids(&["inserted", "prefix", "target", "suffix"]);
    let deleted = fallback_event_ids(&["target", "suffix"]);
    assert_eq!(baseline[1], inserted[2]);
    assert_eq!(baseline[1], deleted[0]);
    assert_eq!(baseline[2], inserted[3]);
    assert_eq!(baseline[2], deleted[1]);

    let duplicates = fallback_event_ids(&["duplicate", "duplicate"]);
    let replayed = fallback_event_ids(&["duplicate", "duplicate"]);
    assert_ne!(duplicates[0], duplicates[1]);
    assert_eq!(duplicates, replayed);
}

#[test]
fn append_after_prior_duplicate_probes_base_and_restores_call_ambiguity() {
    let key = ClaudeSessionKey {
        root_session_id: "fallback-session".to_owned(),
        workflow_run_id: None,
        agent_id: None,
    };
    let binding = Binding {
        project_dir: PathBuf::from("/tmp/project"),
        key: key.clone(),
        layout: SessionLayout::Primary,
    };
    let source = source_key(&key).unwrap();
    let identities = identities(&binding).unwrap();
    let mut cold_identity_state = FallbackEventIdentityState::default();
    let prefix_events = [fallback_row("duplicate", 0), fallback_row("duplicate", 1)]
        .iter()
        .map(|row| {
            fallback_event_id(
                row,
                &source,
                identities.session_id,
                &mut cold_identity_state,
            )
        })
        .collect::<Vec<_>>();
    let (_base, base_lookup) =
        base_lookup_with_events(&source, identities.session_id, &prefix_events);
    let mut pending_calls = HashMap::new();
    let mut linkage_capacity_exceeded = false;
    remember_pending_call(
        &mut pending_calls,
        &mut linkage_capacity_exceeded,
        "cross-append-call",
        PendingCallState::Exact(PendingCall {
            command: Some("git commit -m prefix".to_owned()),
            command_too_large: false,
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 0,
        }),
    );
    let mut projector = ClaudeProjector {
        source: source.clone(),
        source_path: "fallback-session.jsonl".to_owned(),
        binding: binding.clone(),
        identities,
        session: ClaudeSessionMetadata::new(key),
        attributor: RepositoryAttributor::default(),
        pending_calls,
        linkage_capacity_exceeded,
        rejected_records: 0,
        fallback_identities: FallbackEventIdentityState::default(),
    };
    let checkpoint = encode_projector_checkpoint(&projector).unwrap();
    for occurrence in 0_u64..1_024 {
        let mut digest = [0; 32];
        digest[..8].copy_from_slice(&occurrence.to_be_bytes());
        projector
            .fallback_identities
            .next_occurrences
            .insert(digest, occurrence + 1);
    }
    assert_eq!(encode_projector_checkpoint(&projector).unwrap(), checkpoint);
    let mut restored = decode_projector_checkpoint(&checkpoint, &binding).unwrap();

    let suffix_row = fallback_row("duplicate", 2);
    let mut append_identity_state = FallbackEventIdentityState::new(Some(base_lookup));
    let suffix_event = fallback_event_id(
        &suffix_row,
        &source,
        projector.identities.session_id,
        &mut append_identity_state,
    );
    let replayed = fallback_event_ids(&["duplicate", "duplicate", "duplicate"]);
    assert_eq!(prefix_events[0].0, replayed[0]);
    assert_eq!(prefix_events[1].0, replayed[1]);
    assert_eq!(suffix_event.0, replayed[2]);
    assert_ne!(prefix_events[0].0, suffix_event.0);
    assert_ne!(prefix_events[1].0, suffix_event.0);

    remember_pending_call(
        &mut restored.pending_calls,
        &mut restored.linkage_capacity_exceeded,
        "cross-append-call",
        PendingCallState::Exact(PendingCall {
            command: Some("git commit -m suffix".to_owned()),
            command_too_large: false,
            declared_workdir: Some("/tmp/project".to_owned()),
            event_sequence: 1_u64 << 16,
        }),
    );
    let mut input = AttributionInput::default();
    let (context, abstained) = resolve_pending_call(
        &mut restored.pending_calls,
        Some("cross-append-call"),
        restored.linkage_capacity_exceeded,
        &mut input,
    );
    assert!(context.is_none());
    assert!(abstained);
    assert_eq!(
        input.outcome_abstentions,
        vec![(
            RepositoryAbstentionReason::ProviderOutputUnjoined,
            "claude_tool_result_call_id_is_ambiguous"
        )]
    );
}
