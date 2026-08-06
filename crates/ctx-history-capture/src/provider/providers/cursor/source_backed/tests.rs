#[cfg(test)]
mod repository_tests {
    use std::{fs, path::PathBuf, process::Command};

    use serde_json::Value;
    use tempfile::TempDir;

    use super::super::*;

    fn repository_named(temp: &TempDir, name: &str) -> PathBuf {
        let path = temp.path().join(name);
        fs::create_dir(&path).unwrap();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(&path)
            .status()
            .unwrap()
            .success());
        fs::create_dir(path.join("src")).unwrap();
        fs::write(path.join("src/lib.rs"), "pub fn native() {}\n").unwrap();
        path
    }

    fn repository(temp: &TempDir) -> PathBuf {
        repository_named(temp, "repo")
    }

    fn repository_files(repository: &Path, count: usize) -> Vec<String> {
        (0..count)
            .map(|index| {
                let path = repository.join("src").join(format!("path-{index:02}.rs"));
                fs::write(&path, format!("pub const PATH_{index}: usize = {index};\n")).unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect()
    }

    fn raw_tool_call(call_id: &str, input: &str) -> String {
        format!(
            r#"{{"role":"assistant","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"{call_id}","name":"write_file","input":{input}}}]}}}}"#
        )
    }

    fn event(value: &str, ordinal: u64) -> CursorNativeEvent {
        project_cursor_jsonl_record(value.as_bytes(), ordinal, ordinal, 0, value.len() as u64)
            .unwrap()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn projector() -> CursorProjector {
        let native_session_id = "cursor-native-contract-test".to_owned();
        let source = source_key(&native_session_id).unwrap();
        let session_id = session_id(&source, &native_session_id).unwrap();
        CursorProjector {
            source,
            native_session_id,
            session_id,
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
            result_terminal_authority: CursorResultTerminalAuthority::default(),
            event_identities: CursorEventIdentityState::default(),
        }
    }

    fn project_cursor_records(
        projector: &mut CursorProjector,
        records: &[Value],
    ) -> Vec<CoreRecord> {
        let mut authority = CursorResultTerminalAuthority {
            available: true,
            ..CursorResultTerminalAuthority::default()
        };
        for (ordinal, record) in records.iter().enumerate() {
            let encoded = serde_json::to_vec(record).unwrap();
            if let Some(events) = project_cursor_jsonl_record(
                &encoded,
                ordinal as u64,
                ordinal as u64,
                0,
                encoded.len() as u64,
            )
            .unwrap()
            {
                for event in events {
                    if let CursorEventBody::ToolOutput {
                        call_id: Some(call_id),
                        ..
                    } = event.body
                    {
                        authority.observe(&call_id, false);
                    }
                }
            }
        }
        projector.result_terminal_authority = authority;
        let mut worker = JsonlFamilyWorkerContext::default();
        let mut emitted = Vec::new();
        for (ordinal, record) in records.iter().enumerate() {
            let encoded = serde_json::to_vec(record).unwrap();
            let record = JsonlRecordRef::for_test(&encoded, ordinal as u64);
            let evidence = record.evidence();
            let events = project_cursor_jsonl_record(
                record.bytes(),
                evidence.physical_ordinal(),
                evidence.physical_ordinal(),
                evidence.byte_start(),
                evidence.byte_end_exclusive(),
            )
            .unwrap()
            .unwrap();
            for event in events {
                let duplicate_occurrence = next_event_occurrence(
                    &event,
                    &projector.source,
                    projector.session_id,
                    &mut projector.event_identities,
                )
                .unwrap();
                let normalized_body = cursor_normalized_body(&event).unwrap();
                let (annotation, contribution) = projector
                    .attribution_for_event_with_normalized_body(
                        &mut worker,
                        &event,
                        normalized_body.as_deref(),
                    );
                if let Some(record) = core_record(
                    &projector.source,
                    projector.session_id,
                    &projector.native_session_id,
                    event,
                    duplicate_occurrence,
                    CursorProjectedContent {
                        annotation,
                        discovery_exclusion: discovery_exclusion_for([contribution]),
                        normalized_body,
                    },
                )
                .unwrap()
                {
                    emitted.push(record);
                }
            }
        }
        emitted
    }

    fn cursor_ctx_call(call_id: &str, tool_name: &str, input: Value) -> Value {
        serde_json::json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": call_id,
                "name": tool_name,
                "input": input,
            }]},
        })
    }

    fn cursor_ctx_result(
        call_id: &str,
        content: &str,
        is_error: Option<bool>,
        extra_member: Option<(&str, Value)>,
    ) -> Value {
        let mut block = serde_json::json!({
            "type": "tool_result",
            "tool_use_id": call_id,
            "content": content,
        });
        if let Some(is_error) = is_error {
            block["is_error"] = Value::Bool(is_error);
        }
        if let Some((key, value)) = extra_member {
            block.as_object_mut().unwrap().insert(key.to_owned(), value);
        }
        serde_json::json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [block]},
        })
    }

    #[test]
    fn cursor_ctx_retrieval_cli_mcp_and_proven_success_payloads_are_excluded() {
        let payload = "complete Cursor retrieval payload";
        let records = project_cursor_records(
            &mut projector(),
            &[
                cursor_ctx_call(
                    "cli",
                    "run_shell_command",
                    serde_json::json!({"command": "ctx.exe show session aabbccdd"}),
                ),
                cursor_ctx_result("cli", payload, Some(false), None),
                cursor_ctx_call(
                    "mcp",
                    "mcp__ctx__query_events",
                    serde_json::json!({"limit": 5}),
                ),
                cursor_ctx_result("mcp", "typed MCP payload", Some(false), None),
                cursor_ctx_call(
                    "ordinary",
                    "run_shell_command",
                    serde_json::json!({"command": "ctx status"}),
                ),
            ],
        );
        assert_eq!(records.len(), 5);
        for record in &records[..4] {
            assert_eq!(
                record.content.discovery_exclusion,
                Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
            );
        }
        assert_eq!(records[4].content.discovery_exclusion, None);
        let result_body: Value =
            serde_json::from_str(records[1].content.normalized_body.as_deref().unwrap()).unwrap();
        assert_eq!(result_body["content"], payload);
        assert_eq!(records[1].content.structured_content, Some(result_body));
    }

    #[test]
    fn cursor_duplicate_result_terminals_fail_open_including_the_earlier_result() {
        let records = project_cursor_records(
            &mut projector(),
            &[
                cursor_ctx_call(
                    "duplicate-result",
                    "run_shell_command",
                    serde_json::json!({"command": "ctx search duplicate-result"}),
                ),
                cursor_ctx_result(
                    "duplicate-result",
                    "first duplicate Cursor payload",
                    Some(false),
                    None,
                ),
                cursor_ctx_result(
                    "duplicate-result",
                    "second duplicate Cursor payload",
                    Some(false),
                    None,
                ),
            ],
        );

        assert_eq!(records.len(), 3);
        assert_eq!(
            records[0].content.discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        );
        assert_eq!(records[1].content.discovery_exclusion, None);
        assert_eq!(records[2].content.discovery_exclusion, None);
        assert!(records[1]
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("first duplicate Cursor payload"));
        assert!(records[2]
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("second duplicate Cursor payload"));
    }

    #[test]
    fn cursor_ctx_retrieval_results_without_exact_clean_success_fail_open() {
        let payload = "searchable Cursor diagnostic payload";
        let cases = [
            ("missing-success", None, None),
            ("failed", Some(true), None),
            (
                "warning",
                Some(false),
                Some(("warning", serde_json::json!("provider warning"))),
            ),
            (
                "stderr",
                Some(false),
                Some(("stderr", serde_json::json!("provider stderr"))),
            ),
            (
                "unknown-member",
                Some(false),
                Some(("future_field", serde_json::json!(true))),
            ),
        ];
        for (call_id, is_error, extra_member) in cases {
            let records = project_cursor_records(
                &mut projector(),
                &[
                    cursor_ctx_call(
                        call_id,
                        "run_shell_command",
                        serde_json::json!({"command": "ctx search fail-open"}),
                    ),
                    cursor_ctx_result(call_id, payload, is_error, extra_member),
                ],
            );
            assert_eq!(
                records[0].content.discovery_exclusion,
                Some(CoreDiscoveryExclusion::CtxRetrievalDerived),
                "invocation did not classify for {call_id}"
            );
            assert_eq!(
                records[1].content.discovery_exclusion, None,
                "result did not fail open for {call_id}"
            );
            let result_body: Value =
                serde_json::from_str(records[1].content.normalized_body.as_deref().unwrap())
                    .unwrap();
            assert_eq!(result_body["content"], payload);
        }
    }

    #[test]
    fn cursor_ctx_retrieval_pending_classification_survives_append_checkpoint() {
        let mut initial = projector();
        let call_records = project_cursor_records(
            &mut initial,
            &[cursor_ctx_call(
                "append",
                "run_shell_command",
                serde_json::json!({"command": "ctx locate event deadbeef"}),
            )],
        );
        assert_eq!(
            call_records[0].content.discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        );
        let checkpoint = encode_cursor_checkpoint(&initial).unwrap();
        let restored = decode_cursor_checkpoint(&checkpoint, &initial.native_session_id).unwrap();
        let mut appended = projector();
        appended.tool_contexts = restored.tool_contexts;
        appended.linkage_capacity_exceeded = restored.linkage_capacity_exceeded;

        let result_records = project_cursor_records(
            &mut appended,
            &[cursor_ctx_result(
                "append",
                "append Cursor payload",
                Some(false),
                None,
            )],
        );
        assert_eq!(
            result_records[0].content.discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        );
        assert!(appended.tool_contexts.is_empty());
    }

    fn has_reason(
        annotation: &ctx_history_core::CoreRecordAnnotation,
        reason: RepositoryAbstentionReason,
    ) -> bool {
        annotation
            .repository_abstentions
            .iter()
            .any(|abstention| abstention.reason == reason)
    }

    fn core_from_event(
        projector: &mut CursorProjector,
        event: CursorNativeEvent,
        annotation: ctx_history_core::CoreRecordAnnotation,
    ) -> CoreRecord {
        let source = projector.source.clone();
        let session_id = projector.session_id;
        let native_session_id = projector.native_session_id.clone();
        let occurrence =
            next_event_occurrence(&event, &source, session_id, &mut projector.event_identities)
                .unwrap();
        let normalized_body = cursor_normalized_body(&event).unwrap();
        core_record(
            &source,
            session_id,
            &native_session_id,
            event,
            occurrence,
            CursorProjectedContent {
                annotation,
                discovery_exclusion: None,
                normalized_body,
            },
        )
        .unwrap()
        .unwrap()
    }

    #[test]
    fn cursor_exact_native_tool_fields_and_result_id_bind_without_fabricating_outcomes() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let call = serde_json::json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "call-1",
                "name": "run_shell_command",
                "input": {
                    "command": "git commit -m bounded",
                    "workdir": repo,
                    "path": "src/lib.rs"
                }
            }]}
        })
        .to_string();
        let result = r#"{"timestamp":"2026-07-31T12:00:01Z","role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"call-1","content":"untrusted prose oid deadbeef"}]}}"#;
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();

        let call_annotation = projector.attribution_for_event(&mut worker, &event(&call, 1));
        assert_eq!(call_annotation.repository_bindings.len(), 1);
        assert_eq!(
            call_annotation
                .repository_candidate_evidence
                .paths(ctx_history_core::RepositoryCandidateKind::DeclaredToolWorkdir)
                .collect::<Vec<_>>(),
            vec![repo.to_string_lossy().as_ref()]
        );
        assert_eq!(
            call_annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );

        let result_annotation = projector.attribution_for_event(&mut worker, &event(result, 2));
        assert_eq!(result_annotation.repository_bindings.len(), 1);
        assert!(result_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &result_annotation,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        ));
        assert!(!has_reason(
            &result_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_provider_neutral_file_invocation_is_exact_and_call_local() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let call = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "write-local",
                "name": "write_file",
                "input": {
                    "workdir": repo,
                    "path": "src/lib.rs",
                    "contents": "pub fn exact() {}\n"
                }
            }]}
        })
        .to_string();
        let result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"write-local","content":"done"}]}}"#;
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        let call_event = event(&call, 1);
        let call_annotation = projector.attribution_for_event(&mut worker, &call_event);
        assert_eq!(call_annotation.repository_file_invocation_evidence.len(), 1);
        assert_eq!(call_annotation.repository_file_observations.len(), 1);
        assert_eq!(
            call_annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );
        assert_eq!(
            worker
                .repository_attributor()
                .full_certification_probe_count(),
            1
        );
        let invocation = &call_annotation.repository_file_invocation_evidence[0];
        assert_eq!(invocation.operation_ordinal, 0);
        assert_eq!(invocation.relative_path, "src/lib.rs");
        assert_eq!(invocation.prior_relative_path, None);
        assert_eq!(
            invocation.kind,
            ctx_history_core::RepositoryFileInvocationKind::Write
        );
        assert_eq!(invocation.tool_name.as_deref(), Some("write_file"));

        let core = core_from_event(&mut projector, call_event, call_annotation);
        let normalized_body = core.content.normalized_body.as_deref().unwrap();
        let range = core.repository_file_invocation_evidence[0]
            .normalized_text_range
            .unwrap();
        assert_eq!(range.start, 0);
        assert_eq!(range.end as usize, normalized_body.len());
        assert_eq!(
            &normalized_body[range.start as usize..range.end as usize],
            normalized_body
        );

        let result_annotation = projector.attribution_for_event(&mut worker, &event(result, 2));
        assert!(result_annotation
            .repository_file_invocation_evidence
            .is_empty());
        assert_eq!(result_annotation.repository_file_observations.len(), 1);
    }

    #[test]
    fn cursor_dynamic_ambiguous_and_rewrite_evidence_abstains_with_typed_reasons() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        let dynamic = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "dynamic",
                "name": "run_shell_command",
                "input": {"command": "cd $REPO && git status", "path": "$REPO/src/lib.rs"}
            }]}
        })
        .to_string();
        let dynamic_annotation = projector.attribution_for_event(&mut worker, &event(&dynamic, 1));
        assert!(dynamic_annotation.repository_bindings.is_empty());
        assert!(has_reason(
            &dynamic_annotation,
            RepositoryAbstentionReason::DynamicPath
        ));

        let rewrite = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "rewrite",
                "name": "run_shell_command",
                "input": {"command": "git commit --amend --no-edit", "workdir": repo}
            }]}
        })
        .to_string();
        projector.attribution_for_event(&mut worker, &event(&rewrite, 2));
        let rewrite_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"rewrite","content":"success without structured replacement lineage"}]}}"#;
        let rewrite_annotation =
            projector.attribution_for_event(&mut worker, &event(rewrite_result, 3));
        assert!(rewrite_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &rewrite_annotation,
            RepositoryAbstentionReason::HistoryRewriteUnlinked
        ));

        let pr = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "pr",
                "name": "run_shell_command",
                "input": {
                    "command": "gh pr create --title bounded --body bounded",
                    "workdir": repo
                }
            }]}
        })
        .to_string();
        projector.attribution_for_event(&mut worker, &event(&pr, 4));
        let pr_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"pr","content":"URL prose is not structured outcome authority"}]}}"#;
        let pr_annotation = projector.attribution_for_event(&mut worker, &event(pr_result, 5));
        assert!(pr_annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &pr_annotation,
            RepositoryAbstentionReason::OutcomeResultInadmissible
        ));

        let ambiguous_result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"rewrite","tool_use_id":"other","content":"ignored"}]}}"#;
        let ambiguous_annotation =
            projector.attribution_for_event(&mut worker, &event(ambiguous_result, 6));
        assert!(has_reason(
            &ambiguous_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_synthetic_native_contract_does_not_establish_real_history_parity() {
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        let relative_only = r#"{"role":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"write_file","input":{"path":"src/unproven.rs"}}]}}"#;
        let annotation = projector.attribution_for_event(&mut worker, &event(relative_only, 1));
        assert!(annotation.repository_bindings.is_empty());
        assert!(annotation.repository_vcs_observations.is_empty());
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::UnscopedFileActivity
        ));
    }

    #[test]
    fn cursor_path_limit_is_exact_at_32_and_abstains_for_array_or_scalar_overflow() {
        let temp = TempDir::new().unwrap();
        let first_repo = repository_named(&temp, "first-repo");
        let second_repo = repository_named(&temp, "second-repo");
        let exact_paths = repository_files(&first_repo, MAX_CURSOR_INPUT_PATHS);
        let second_repo_path = repository_files(&second_repo, 1).remove(0);
        let exact_paths_json = serde_json::to_string(&exact_paths).unwrap();

        let exact_call = raw_tool_call(
            "exact-boundary",
            &format!(r#"{{"paths":{exact_paths_json}}}"#),
        );
        let mut exact_projector = projector();
        let mut exact_worker = JsonlFamilyWorkerContext::default();
        let exact_annotation =
            exact_projector.attribution_for_event(&mut exact_worker, &event(&exact_call, 1));
        assert_eq!(exact_annotation.repository_bindings.len(), 1);
        assert_eq!(
            exact_annotation.repository_file_invocation_evidence.len(),
            MAX_CURSOR_INPUT_PATHS
        );
        assert_eq!(
            exact_annotation.repository_file_observations.len(),
            MAX_CURSOR_INPUT_PATHS
        );
        assert_eq!(
            exact_worker
                .repository_attributor()
                .full_certification_probe_count(),
            1
        );
        assert!(!has_reason(
            &exact_annotation,
            RepositoryAbstentionReason::CandidateLimitExceeded
        ));
        assert!(!has_reason(
            &exact_annotation,
            RepositoryAbstentionReason::Ambiguous
        ));

        let mut array_overflow_paths = exact_paths.clone();
        array_overflow_paths.push(second_repo_path.clone());
        let array_overflow_input = serde_json::json!({"paths": array_overflow_paths}).to_string();
        let scalar_overflow_input = format!(
            r#"{{"paths":{exact_paths_json},"path":{}}}"#,
            serde_json::to_string(&second_repo_path).unwrap()
        );

        for (ordinal, (call_id, input)) in [
            ("array-overflow", array_overflow_input),
            ("scalar-overflow", scalar_overflow_input),
        ]
        .into_iter()
        .enumerate()
        {
            let call = raw_tool_call(call_id, &input);
            let mut overflow_projector = projector();
            let mut overflow_worker = JsonlFamilyWorkerContext::default();
            let call_event = event(&call, ordinal as u64 + 2);
            let native_content = match &call_event.body {
                CursorEventBody::ToolCall { native_content, .. } => native_content.clone(),
                body => panic!("expected tool call, got {body:?}"),
            };
            assert_eq!(
                native_content
                    .pointer("/input/paths")
                    .and_then(serde_json::Value::as_array)
                    .unwrap()
                    .len(),
                MAX_CURSOR_INPUT_PATHS + usize::from(call_id == "array-overflow")
            );
            if call_id == "scalar-overflow" {
                assert_eq!(
                    native_content
                        .pointer("/input/path")
                        .and_then(serde_json::Value::as_str),
                    Some(second_repo_path.as_str())
                );
            }

            let annotation =
                overflow_projector.attribution_for_event(&mut overflow_worker, &call_event);
            assert_eq!(annotation.repository_bindings.len(), 1);
            assert_eq!(
                annotation.repository_file_observations.len(),
                MAX_CURSOR_INPUT_PATHS
            );
            assert!(annotation.repository_file_invocation_evidence.is_empty());
            assert!(has_reason(
                &annotation,
                RepositoryAbstentionReason::CandidateLimitExceeded
            ));
            assert_eq!(
                has_reason(&annotation, RepositoryAbstentionReason::Ambiguous),
                call_id == "scalar-overflow"
            );

            let core = core_from_event(&mut overflow_projector, call_event, annotation);
            let complete_body: serde_json::Value =
                serde_json::from_str(core.content.normalized_body.as_deref().unwrap()).unwrap();
            assert_eq!(
                complete_body.pointer("/input"),
                native_content.pointer("/input")
            );
            assert_eq!(core.parser_revision, PARSER_REVISION);

            let checkpoint = overflow_projector.provider_checkpoint().unwrap().unwrap();
            let restored = decode_cursor_checkpoint(
                &checkpoint,
                overflow_projector.native_session_id.as_str(),
            )
            .unwrap();
            overflow_projector.tool_contexts = restored.tool_contexts;
            overflow_projector.linkage_capacity_exceeded = restored.linkage_capacity_exceeded;

            let result = format!(
                r#"{{"role":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{call_id}","content":"complete result"}}]}}}}"#
            );
            let result_annotation = overflow_projector
                .attribution_for_event(&mut overflow_worker, &event(&result, ordinal as u64 + 10));
            assert!(result_annotation.repository_bindings.is_empty());
            assert!(result_annotation.repository_file_observations.is_empty());
            assert_eq!(
                has_reason(
                    &result_annotation,
                    RepositoryAbstentionReason::CandidateLimitExceeded
                ),
                call_id == "array-overflow"
            );
            assert_eq!(
                has_reason(
                    &result_annotation,
                    RepositoryAbstentionReason::ProviderOutputUnjoined
                ),
                call_id == "scalar-overflow"
            );
        }
    }

    #[test]
    fn cursor_invalid_path_shape_preserves_body_and_independent_workdir_evidence() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let call = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "invalid-path-shape",
                "name": "write_file",
                "input": {
                    "workdir": repo,
                    "paths": ["src/lib.rs", {"unexpected": "shape"}]
                }
            }]}
        })
        .to_string();
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        let call_event = event(&call, 1);
        let native_content = match &call_event.body {
            CursorEventBody::ToolCall { native_content, .. } => native_content.clone(),
            body => panic!("expected tool call, got {body:?}"),
        };
        assert!(native_content
            .pointer("/input/paths/1/unexpected")
            .is_some());

        let annotation = projector.attribution_for_event(&mut worker, &call_event);
        assert_eq!(annotation.repository_bindings.len(), 1);
        assert_eq!(annotation.repository_file_observations.len(), 1);
        assert_eq!(
            annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );
        assert!(annotation.repository_file_invocation_evidence.is_empty());
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::Ambiguous
        ));
        assert!(!has_reason(
            &annotation,
            RepositoryAbstentionReason::CandidateLimitExceeded
        ));

        let core = core_from_event(&mut projector, call_event, annotation);
        let complete_body: serde_json::Value =
            serde_json::from_str(core.content.normalized_body.as_deref().unwrap()).unwrap();
        assert_eq!(complete_body, native_content);

        let result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"invalid-path-shape","content":"complete result"}]}}"#;
        let result_annotation = projector.attribution_for_event(&mut worker, &event(result, 2));
        assert_eq!(result_annotation.repository_bindings.len(), 1);
        assert_eq!(result_annotation.repository_file_observations.len(), 1);
        assert_eq!(
            result_annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );
        assert!(result_annotation
            .repository_file_invocation_evidence
            .is_empty());
        assert!(has_reason(
            &result_annotation,
            RepositoryAbstentionReason::Ambiguous
        ));
        assert!(!has_reason(
            &result_annotation,
            RepositoryAbstentionReason::ProviderOutputUnjoined
        ));
    }

    #[test]
    fn cursor_path_alias_ambiguity_abstains_without_typed_file_evidence_and_preserves_body() {
        let temp = TempDir::new().unwrap();
        let repo = repository(&temp);
        let path = repo.join("src/lib.rs").to_string_lossy().into_owned();
        let call = serde_json::json!({
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "ambiguous-path-aliases",
                "name": "write_file",
                "input": {
                    "path": path,
                    "file_path": path
                }
            }]}
        })
        .to_string();
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        let call_event = event(&call, 1);
        let native_content = match &call_event.body {
            CursorEventBody::ToolCall { native_content, .. } => native_content.clone(),
            body => panic!("expected tool call, got {body:?}"),
        };
        assert_eq!(
            native_content.pointer("/input/path"),
            native_content.pointer("/input/file_path")
        );

        let annotation = projector.attribution_for_event(&mut worker, &call_event);
        assert_eq!(annotation.repository_bindings.len(), 1);
        assert_eq!(annotation.repository_file_observations.len(), 1);
        assert_eq!(
            annotation.repository_file_observations[0].relative_path,
            "src/lib.rs"
        );
        assert!(annotation.repository_file_invocation_evidence.is_empty());
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::Ambiguous
        ));

        let core = core_from_event(&mut projector, call_event, annotation);
        let complete_body: serde_json::Value =
            serde_json::from_str(core.content.normalized_body.as_deref().unwrap()).unwrap();
        assert_eq!(complete_body, native_content);
    }

    #[test]
    fn cursor_checkpoint_byte_overflow_is_a_typed_capacity_abstention() {
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        projector.remember_tool_context(
            "oversized-call",
            CursorToolContextState::Exact(CursorToolContext {
                command: Some("x".repeat(MAX_CURSOR_CHECKPOINT_BYTES)),
                declared_workdir: Some("/tmp/project".to_owned()),
                input_paths: CursorInputPathEvidence::Exact(Vec::new()),
                ctx_retrieval_derived: false,
            }),
        );
        assert!(projector.tool_contexts.is_empty());
        assert!(projector.linkage_capacity_exceeded);
        assert!(encode_cursor_checkpoint(&projector).is_ok());

        let result = r#"{"role":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"oversized-call","content":"exact output"}]}}"#;
        let annotation = projector.attribution_for_event(&mut worker, &event(result, 2));
        assert!(has_reason(
            &annotation,
            RepositoryAbstentionReason::LinkageCapacityExceeded
        ));
    }
}

#[cfg(test)]
mod fidelity_identity_tests {
    use std::{
        collections::BTreeMap,
        fs::{self, OpenOptions},
        io::Write,
        path::PathBuf,
    };

    use ctx_history_core::{CoreRecord, StableEntityId};
    use ctx_history_index::{VerifiedIndex, WriterOptions};
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use super::super::*;
    use crate::{
        provider::source_backed::{
            refresh_source_backed_generation, register_landed_source_backed_route,
            SourceBackedProviderRegistry, SourceBackedRouteSelection,
        },
        ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
        ProviderSourceStatus,
    };

    fn transcript_path(root: &Path, project: &str, session: &str) -> PathBuf {
        root.join("projects")
            .join(project)
            .join("agent-transcripts")
            .join(session)
            .join(format!("{session}.jsonl"))
    }

    fn write_transcript(path: &Path, rows: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut encoded = Vec::new();
        for row in rows {
            serde_json::to_writer(&mut encoded, row).unwrap();
            encoded.push(b'\n');
        }
        fs::write(path, encoded).unwrap();
    }

    fn append_transcript(path: &Path, row: &Value) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, row).unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
    }

    fn registry(root: &Path) -> SourceBackedProviderRegistry {
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route(
            &mut registry,
            ProviderSource {
                provider: CaptureProvider::Cursor,
                path: root.to_path_buf(),
                exists: true,
                source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                source_kind: ProviderSourceKind::NativeHistory,
                import_support: ProviderImportSupport::Native,
                catalog_support: ProviderCatalogSupport::None,
                status: ProviderSourceStatus::Available,
                unsupported_reason: None,
            },
            SourceBackedRouteSelection::Automatic,
        )
        .unwrap();
        registry
    }

    fn writer_options() -> WriterOptions {
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        }
    }

    fn indexed_event_ids(index: &Path, native_session_id: &str) -> Vec<StableEntityId> {
        let source = source_key(native_session_id).unwrap();
        VerifiedIndex::open(index)
            .unwrap()
            .core_source_event_page(&source, None, 64)
            .unwrap()
            .items
            .into_iter()
            .map(|item| item.event_id)
            .collect()
    }

    fn indexed_records(index: &Path, native_session_id: &str) -> Vec<CoreRecord> {
        let source = source_key(native_session_id).unwrap();
        let verified = VerifiedIndex::open(index).unwrap();
        verified
            .core_source_event_page(&source, None, 64)
            .unwrap()
            .items
            .into_iter()
            .map(|item| {
                verified
                    .core_record_by_id(item.event_id.as_uuid())
                    .unwrap()
                    .unwrap()
            })
            .collect()
    }

    fn assert_ids_preserved(previous: &[StableEntityId], current: &[StableEntityId]) {
        for event_id in previous {
            assert!(current.contains(event_id), "prior event identity changed");
        }
    }

    fn assert_all_ids_distinct(event_ids: &[StableEntityId]) {
        for (index, event_id) in event_ids.iter().enumerate() {
            assert!(!event_ids[index + 1..].contains(event_id));
        }
    }

    fn event(row: &Value, ordinal: u64) -> CursorNativeEvent {
        let encoded = serde_json::to_vec(row).unwrap();
        project_cursor_jsonl_record(&encoded, ordinal, ordinal, 0, encoded.len() as u64)
            .unwrap()
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    fn projector() -> CursorProjector {
        let native_session_id = "cursor-fidelity-test".to_owned();
        let source = source_key(&native_session_id).unwrap();
        let session_id = session_id(&source, &native_session_id).unwrap();
        CursorProjector {
            source,
            native_session_id,
            session_id,
            tool_contexts: BTreeMap::new(),
            linkage_capacity_exceeded: false,
            result_terminal_authority: CursorResultTerminalAuthority::default(),
            event_identities: CursorEventIdentityState::default(),
        }
    }

    fn projected_core(row: &Value) -> CoreRecord {
        let mut projector = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        let event = event(row, 0);
        let annotation = projector.attribution_for_event(&mut worker, &event);
        let duplicate_occurrence = next_event_occurrence(
            &event,
            &projector.source,
            projector.session_id,
            &mut projector.event_identities,
        )
        .unwrap();
        let normalized_body = cursor_normalized_body(&event).unwrap();
        core_record(
            &projector.source,
            projector.session_id,
            &projector.native_session_id,
            event,
            duplicate_occurrence,
            CursorProjectedContent {
                annotation,
                discovery_exclusion: None,
                normalized_body,
            },
        )
        .unwrap()
        .unwrap()
    }

    fn message(role: &str, timestamp: &str, text: &str) -> Value {
        json!({
            "timestamp": timestamp,
            "role": role,
            "message": {
                "role": role,
                "content": [{"type": "text", "text": text}]
            }
        })
    }

    fn event_ids(rows: &[Value]) -> Vec<StableEntityId> {
        let native_session_id = "cursor-identity-test";
        let source = source_key(native_session_id).unwrap();
        let session_id = session_id(&source, native_session_id).unwrap();
        let mut identity_state = CursorEventIdentityState::default();
        let mut ids = Vec::new();
        for (ordinal, row) in rows.iter().enumerate() {
            let encoded = serde_json::to_vec(row).unwrap();
            let events = project_cursor_jsonl_record(
                &encoded,
                ordinal as u64,
                ordinal as u64,
                0,
                encoded.len() as u64,
            )
            .unwrap()
            .unwrap();
            for event in events {
                let occurrence =
                    next_event_occurrence(&event, &source, session_id, &mut identity_state)
                        .unwrap();
                let key = event_identity_key(&event, occurrence).unwrap();
                ids.push(event_id(&source, session_id, &key).unwrap());
            }
        }
        ids
    }

    #[test]
    fn cursor_write_file_core_content_preserves_complete_input() {
        let row = json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "write-1",
                "name": "write_file",
                "input": {
                    "path": "src/main.rs",
                    "contents": "fn main() { println!(\"complete\"); }\n",
                    "overwrite": true
                }
            }]}
        });
        let expected = row.pointer("/message/content/0").unwrap().clone();
        let core = projected_core(&row);

        assert_eq!(core.content.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(core.content.normalized_body.as_deref().unwrap())
                .unwrap(),
            expected
        );
        assert!(core
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("println!"));
    }

    #[test]
    fn cursor_large_tool_arguments_preserve_body_and_identity_within_aggregate_limit() {
        let tail = "cursor_large_tool_argument_tail_complete";
        let full_argument = format!("{}{tail}", "x".repeat(8 * 1024 * 1024));
        let row = json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "large-write-1",
                "name": "write_file",
                "input": {
                    "path": "large.txt",
                    "contents": &full_argument
                }
            }]}
        });
        assert!(serde_json::to_vec(&row).unwrap().len() <= crate::MAX_PROVIDER_JSONL_LINE_BYTES);

        let expected_native_event_id = event_identity_key(&event(&row, 0), 0).unwrap();
        let identity_source = source_key("cursor-fidelity-test").unwrap();
        let identity_session_id = session_id(&identity_source, "cursor-fidelity-test").unwrap();
        let expected_event_id = event_id(
            &identity_source,
            identity_session_id,
            &expected_native_event_id,
        )
        .unwrap();
        let duplicate_structured = row.pointer("/message/content/0").unwrap();
        let core = projected_core(&row);
        let normalized = core.content.normalized_body.as_deref().unwrap();
        assert!(normalized.contains(tail));
        assert_eq!(core.event_id, expected_event_id);
        assert_eq!(
            core.native_event_id.as_ref(),
            Some(&expected_native_event_id)
        );
        assert!(core.content.structured_content.is_none());
        assert!(
            normalized.len() + serde_json::to_vec(duplicate_structured).unwrap().len()
                > ctx_history_core::MAX_CORE_CONTENT_BYTES
        );
        assert!(
            core.content.encoded_content_bytes().unwrap()
                <= ctx_history_core::MAX_CORE_CONTENT_BYTES
        );
        core.validate_contract().unwrap();
        core.encode_stored().unwrap();
    }

    #[test]
    fn cursor_shell_result_core_content_preserves_complete_stdout() {
        let stdout = "first line\nsecond line\nexit marker";
        let row = json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "shell-1",
                "content": stdout,
                "is_error": false
            }]}
        });
        let expected = row.pointer("/message/content/0").unwrap().clone();
        let core = projected_core(&row);

        assert_eq!(core.content.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(core.content.normalized_body.as_deref().unwrap())
                .unwrap(),
            expected
        );
        let normalized: Value =
            serde_json::from_str(core.content.normalized_body.as_deref().unwrap()).unwrap();
        assert_eq!(
            normalized.get("content").and_then(Value::as_str),
            Some(stdout)
        );
    }

    #[test]
    fn cursor_provider_redaction_is_retained_without_invented_result_content() {
        let row = json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "redacted-1",
                "content": null,
                "redacted": true
            }]}
        });
        let expected = row.pointer("/message/content/0").unwrap().clone();
        let core = projected_core(&row);

        assert_eq!(core.content.structured_content, Some(expected.clone()));
        assert_eq!(
            serde_json::from_str::<Value>(core.content.normalized_body.as_deref().unwrap())
                .unwrap(),
            expected
        );
        assert!(!core
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("Cursor tool result"));
    }

    #[test]
    fn cursor_logical_event_ids_survive_unrelated_insert_and_distinguish_duplicates() {
        let first = message("user", "2026-07-31T12:00:00Z", "first");
        let second = message("assistant", "2026-07-31T12:00:01Z", "second");
        let inserted = message("user", "2026-07-31T11:59:59Z", "inserted");

        let original = event_ids(&[first.clone(), second.clone()]);
        let with_insert = event_ids(&[inserted, first.clone(), second]);
        assert_eq!(original, with_insert[1..]);

        let duplicates = event_ids(&[first.clone(), first.clone()]);
        assert_ne!(duplicates[0], duplicates[1]);
        assert_eq!(duplicates, event_ids(&[first.clone(), first.clone()]));

        let prefixed = event_ids(&[
            message("user", "2026-07-31T11:59:58Z", "unrelated prefix"),
            first.clone(),
            first.clone(),
        ]);
        assert_eq!(duplicates, prefixed[1..]);

        let separated = event_ids(&[
            first.clone(),
            message("user", "2026-07-31T12:00:02Z", "unrelated"),
            first,
        ]);
        assert_eq!(duplicates[0], separated[0]);
        assert_eq!(duplicates[1], separated[2]);
    }

    #[test]
    fn cursor_unknown_checkpoint_and_occurrence_overflow_fail_closed() {
        assert!(projector().provider_checkpoint().unwrap().is_some());
        let unknown_checkpoint = TypedKey::utf8("cursor.unknown-checkpoint").unwrap();
        let checkpoint_error =
            decode_cursor_checkpoint(&unknown_checkpoint, "cursor-fidelity-test").unwrap_err();
        assert!(checkpoint_error
            .to_string()
            .contains("version is unsupported"));

        let native_session_id = "cursor-overflow-test";
        let source = source_key(native_session_id).unwrap();
        let session_id = session_id(&source, native_session_id).unwrap();
        let event = event(&message("user", "2026-07-31T12:00:00Z", "duplicate"), 0);
        let mut state = CursorEventIdentityState::default();
        state
            .next_occurrences
            .insert(CursorLogicalEventIdentity::from_event(&event), u64::MAX);
        let occurrence_error =
            next_event_occurrence(&event, &source, session_id, &mut state).unwrap_err();
        assert!(occurrence_error
            .to_string()
            .contains("duplicate event occurrence overflowed"));
    }

    #[test]
    fn cursor_append_checkpoint_restores_pending_call_for_suffix_result() {
        let call = json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": "append-call",
                "name": "run_shell_command",
                "input": {"command": "git status", "workdir": "/tmp/project"}
            }]}
        });
        let result = json!({
            "timestamp": "2026-07-31T12:00:01Z",
            "role": "user",
            "message": {"role": "user", "content": [{
                "type": "tool_result",
                "tool_use_id": "append-call",
                "content": "exact append output"
            }]}
        });
        let mut initial = projector();
        let mut worker = JsonlFamilyWorkerContext::default();
        initial.attribution_for_event(&mut worker, &event(&call, 0));
        let checkpoint = encode_cursor_checkpoint(&initial).unwrap();
        let restored = decode_cursor_checkpoint(&checkpoint, &initial.native_session_id).unwrap();
        let mut appended = projector();
        appended.tool_contexts = restored.tool_contexts;
        appended.linkage_capacity_exceeded = restored.linkage_capacity_exceeded;

        let annotation = appended.attribution_for_event(&mut worker, &event(&result, 1));
        assert!(!annotation.repository_abstentions.iter().any(|abstention| {
            matches!(
                abstention.reason,
                RepositoryAbstentionReason::ProviderOutputUnjoined
                    | RepositoryAbstentionReason::LinkageCapacityExceeded
            )
        }));
        assert!(appended.tool_contexts.is_empty());
    }

    #[test]
    fn cursor_append_projects_only_suffix_and_probes_pinned_base_for_duplicate_occurrences() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let transcript = transcript_path(&root, "project", "native-session");
        let first = message("user", "2026-07-31T12:00:00Z", "first");
        let second = message("assistant", "2026-07-31T12:00:01Z", "second");
        write_transcript(&transcript, &[first]);
        let registry = registry(&root);
        let index = temp.path().join("index");
        let source = source_key("native-session").unwrap();

        reset_cursor_projected_records(&source);
        reset_cursor_signature_records();
        reset_cursor_base_identity_probes();
        let cold = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(cold.commit.indexed_documents, 1);
        assert_eq!(take_cursor_projected_records(&source), 1);
        assert_eq!(cursor_base_identity_probes(), 0);
        assert_eq!(
            cursor_signature_records(),
            0,
            "a singleton native session must not be pre-parsed for route comparison"
        );
        let cold_ids = indexed_event_ids(&index, "native-session");

        append_transcript(&transcript, &second);
        reset_cursor_projected_records(&source);
        reset_cursor_signature_records();
        reset_cursor_base_identity_probes();
        let appended =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(appended.commit.indexed_documents, 2);
        assert_eq!(
            take_cursor_projected_records(&source),
            1,
            "Cursor append work must remain bounded to the validated suffix"
        );
        assert_eq!(
            cursor_signature_records(),
            0,
            "singleton append discovery must not rescan transcript content"
        );
        assert_eq!(cursor_base_identity_probes(), 1);
        let appended_ids = indexed_event_ids(&index, "native-session");
        assert_ids_preserved(&cold_ids, &appended_ids);

        append_transcript(&transcript, &second);
        reset_cursor_projected_records(&source);
        reset_cursor_signature_records();
        reset_cursor_base_identity_probes();
        let first_duplicate =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(first_duplicate.commit.indexed_documents, 3);
        assert_eq!(take_cursor_projected_records(&source), 1);
        assert_eq!(cursor_signature_records(), 0);
        assert_eq!(cursor_base_identity_probes(), 2);
        let first_duplicate_ids = indexed_event_ids(&index, "native-session");
        assert_ids_preserved(&appended_ids, &first_duplicate_ids);
        assert_all_ids_distinct(&first_duplicate_ids);

        append_transcript(&transcript, &second);
        reset_cursor_projected_records(&source);
        reset_cursor_signature_records();
        reset_cursor_base_identity_probes();
        let second_duplicate =
            refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        assert_eq!(second_duplicate.commit.indexed_documents, 4);
        assert_eq!(take_cursor_projected_records(&source), 1);
        assert_eq!(cursor_signature_records(), 0);
        assert_eq!(cursor_base_identity_probes(), 3);
        let second_duplicate_ids = indexed_event_ids(&index, "native-session");
        assert_ids_preserved(&first_duplicate_ids, &second_duplicate_ids);
        assert_all_ids_distinct(&second_duplicate_ids);
    }

    #[test]
    fn cursor_late_duplicate_result_forces_replacement_and_corrects_the_earlier_result() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let native_session_id = "late-duplicate-session";
        let transcript = transcript_path(&root, "project", native_session_id);
        let call_id = "late-duplicate-result";
        let call = json!({
            "timestamp": "2026-07-31T12:00:00Z",
            "role": "assistant",
            "message": {"role": "assistant", "content": [{
                "type": "tool_use",
                "id": call_id,
                "name": "run_shell_command",
                "input": {"command": "ctx search late-duplicate"}
            }]}
        });
        let result = |content: &str, timestamp: &str| {
            json!({
                "timestamp": timestamp,
                "role": "user",
                "message": {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "content": content,
                    "is_error": false
                }]}
            })
        };
        write_transcript(
            &transcript,
            &[
                call,
                result(
                    "first late duplicate Cursor payload",
                    "2026-07-31T12:00:01Z",
                ),
            ],
        );
        let registry = registry(&root);
        let index = temp.path().join("index");

        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        let initial = indexed_records(&index, native_session_id);
        assert_eq!(initial.len(), 2);
        assert!(initial.iter().all(|record| {
            record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        }));
        let initial_ids = initial
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>();

        append_transcript(
            &transcript,
            &result(
                "second late duplicate Cursor payload",
                "2026-07-31T12:00:02Z",
            ),
        );
        refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();
        let corrected = indexed_records(&index, native_session_id);
        assert_eq!(corrected.len(), 3);
        assert_eq!(corrected[0].event_id, initial_ids[0]);
        assert_eq!(corrected[1].event_id, initial_ids[1]);
        assert_eq!(
            corrected[0].content.discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        );
        assert_eq!(corrected[1].content.discovery_exclusion, None);
        assert_eq!(corrected[2].content.discovery_exclusion, None);
        assert!(corrected[1]
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("first late duplicate Cursor payload"));
        assert!(corrected[2]
            .content
            .normalized_body
            .as_deref()
            .unwrap()
            .contains("second late duplicate Cursor payload"));
    }

    #[test]
    fn cursor_equivalent_duplicate_routes_cover_move_overlap_deterministically() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let session = "native-session";
        let first = transcript_path(&root, "project-a", session);
        let second = transcript_path(&root, "project-b", session);
        let rows = [message("user", "2026-07-31T12:00:00Z", "same")];
        write_transcript(&first, &rows);

        reset_cursor_signature_records();
        let initial = CursorJsonlAdapter::default().discover(&root).unwrap();
        assert_eq!(initial.leaves().len(), 1);
        let initial_source = initial.leaves()[0].source().clone();
        assert_eq!(initial.leaves()[0].source_path(), first);
        assert_eq!(cursor_signature_records(), 0);

        write_transcript(&second, &rows);
        reset_cursor_signature_records();
        let overlap = CursorJsonlAdapter::default().discover(&root).unwrap();
        assert_eq!(overlap.leaves().len(), 1);
        assert_eq!(overlap.leaves()[0].source_path(), first);
        let overlap_binding = decode_binding(&overlap.leaves()[0]).unwrap();
        assert_eq!(overlap_binding.alias_route_sha256.len(), 1);
        assert_eq!(cursor_signature_records(), 2);

        fs::remove_file(&first).unwrap();
        reset_cursor_signature_records();
        let moved = CursorJsonlAdapter::default().discover(&root).unwrap();
        assert_eq!(moved.leaves().len(), 1);
        assert_eq!(moved.leaves()[0].source_path(), second);
        assert_eq!(cursor_signature_records(), 0);
        assert!(moved.leaves()[0]
            .source()
            .exact_descriptor_eq(&initial_source));
        assert!(decode_binding(&moved.leaves()[0])
            .unwrap()
            .alias_route_sha256
            .is_empty());
    }

    #[test]
    fn cursor_conflicting_duplicate_transcripts_are_rejected() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("cursor-data");
        let session = "native-session";
        write_transcript(
            &transcript_path(&root, "project-a", session),
            &[message("user", "2026-07-31T12:00:00Z", "first")],
        );
        write_transcript(
            &transcript_path(&root, "project-b", session),
            &[message("user", "2026-07-31T12:00:00Z", "conflict")],
        );

        let error = CursorJsonlAdapter::default().discover(&root).unwrap_err();
        assert!(error.to_string().contains("conflicting transcript copies"));
    }
}
