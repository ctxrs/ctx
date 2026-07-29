use super::*;

#[test]
fn recognized_mcp_calls_are_classified_from_the_flushed_response_shape() {
    let mut invocation = McpInvocation::recognized("blame").unwrap();
    invocation.bind_blame_target(&BlameTarget::Commit {
        oid: "abc1234".to_owned(),
        repository: None,
    });
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "matches": [{
                    "kind": "commit",
                    "value": {"predicate": "produced_by", "state": "asserted"}
                }],
                "evidence": [{"number": 1}]
            }
        }
    });
    let completed = invocation.completed(&response, Duration::from_millis(8), 321);
    assert_eq!(completed.surface, Surface::Mcp);
    assert_eq!(completed.target_type, TargetType::Commit);
    assert_eq!(completed.pro_outcome, ProOutcome::Produced);
    assert_eq!(completed.response_bytes, 321);
    assert_eq!(completed.citation_count, 1);
}

#[test]
fn mcp_search_keeps_wire_and_single_structured_result_bytes_separate() {
    let results = json!([
        {"ctx_event_id": "one", "content": "first semantic result"},
        {"ctx_event_id": "two", "content": "second semantic result"}
    ]);
    let semantic_bytes = serde_json::to_vec(&results).unwrap().len() as u64;
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "content": [{
                "type": "text",
                "text": "first semantic result\nsecond semantic result"
            }],
            "structuredContent": {"results": results.clone()}
        }
    });
    let wire_bytes = 4_096;
    let context_bytes = serde_json::to_vec(&json!({"results": results}))
        .unwrap()
        .len() as u64;
    let completed = McpInvocation::recognized("search").unwrap().completed(
        &response,
        Duration::from_millis(8),
        wire_bytes,
    );
    assert_eq!(completed.response_bytes, wire_bytes as u64);
    assert_eq!(completed.response_byte_samples, 1);
    assert_eq!(completed.context_bytes, context_bytes);
    assert_eq!(completed.context_byte_samples, 1);
    assert_eq!(completed.search_result_bytes, semantic_bytes);
    assert_eq!(completed.search_result_byte_samples, 1);
    assert!(completed.search_result_bytes <= completed.context_bytes);
    assert_ne!(completed.search_result_bytes, completed.response_bytes);
    assert_eq!(
        completed.result_action,
        Some(ResultObservationAction::Search)
    );

    let estimates = estimate_usage(EstimateFacts {
        result_bearing_searches: 1,
        semantic_context_eligible_samples: 1,
        semantic_context_bytes: completed.context_bytes,
        semantic_context_byte_samples: completed.context_byte_samples,
        semantic_search_result_bytes: completed.search_result_bytes,
        semantic_search_result_byte_samples: completed.search_result_byte_samples,
        ..EstimateFacts::default()
    })
    .unwrap();
    assert_eq!(
        estimates.approximate_context_tokens.approximate_tokens,
        Some((context_bytes + 3) / 4)
    );
    assert_eq!(
        estimates
            .approximate_avoided_context_tokens
            .approximate_tokens,
        Some(((semantic_bytes + 3) / 4) * 49)
    );
}

#[test]
fn mcp_status_payloads_measure_transport_but_not_semantic_context() {
    for operation in ["status", "pro_status"] {
        let response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{"type": "text", "text": "healthy"}],
                "structuredContent": {"health": "healthy"}
            }
        });
        let completed = McpInvocation::recognized(operation).unwrap().completed(
            &response,
            Duration::from_millis(2),
            512,
        );
        assert_eq!(completed.response_bytes, 512);
        assert_eq!(completed.response_byte_samples, 1);
        assert_eq!(completed.context_bytes, 0);
        assert_eq!(completed.context_byte_samples, 0);
        assert_eq!(completed.search_result_bytes, 0);
        assert_eq!(completed.search_result_byte_samples, 0);
    }
}

#[test]
fn mcp_correlation_uses_one_scope_key_and_never_validates_more_than_found() {
    let root = private_tempdir();
    let mut recorder = McpUsageRecorder::start(root.path().to_path_buf());
    let search_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "results": [{
                    "result_scope": "session",
                    "result_type": "session_result",
                    "ctx_session_id": "session-1",
                    "ctx_event_id": "event-1"
                }]
            }
        }
    });
    let search = McpInvocation::recognized("search").unwrap();
    let mut search_operation = search.completed(&search_response, Duration::ZERO, 200);
    recorder.correlate_delivered(search, &search_response, &mut search_operation);
    assert_eq!(search_operation.context.context_found, 1);

    let show_response = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {"structuredContent": {"events": [{}]}}
    });
    let mut show_session = McpInvocation::recognized("show_session").unwrap();
    show_session.bind_context_target("session-1".to_owned());
    let mut session_operation = show_session.completed(&show_response, Duration::ZERO, 100);
    recorder.correlate_delivered(show_session, &show_response, &mut session_operation);
    assert_eq!(session_operation.context.validated_discoveries, 1);

    let mut show_event = McpInvocation::recognized("show_event").unwrap();
    show_event.bind_context_target("event-1".to_owned());
    let mut event_operation = show_event.completed(&show_response, Duration::ZERO, 100);
    recorder.correlate_delivered(show_event.clone(), &show_response, &mut event_operation);
    assert_eq!(event_operation.context.validated_discoveries, 0);
    let mut duplicate_event_operation = show_event.completed(&show_response, Duration::ZERO, 100);
    recorder.correlate_delivered(show_event, &show_response, &mut duplicate_event_operation);
    assert_eq!(duplicate_event_operation.context.validated_discoveries, 0);

    let repeated_search = McpInvocation::recognized("search").unwrap();
    let mut repeated_operation = repeated_search.completed(&search_response, Duration::ZERO, 200);
    recorder.correlate_delivered(
        repeated_search.clone(),
        &search_response,
        &mut repeated_operation,
    );
    assert_eq!(repeated_operation.context.context_found, 0);

    let event_search_response = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "result": {
            "structuredContent": {
                "results": [{
                    "result_scope": "event",
                    "result_type": "event",
                    "ctx_session_id": "session-1",
                    "ctx_event_id": "event-1"
                }]
            }
        }
    });
    let mut event_search_operation =
        repeated_search.completed(&event_search_response, Duration::ZERO, 200);
    recorder.correlate_delivered(
        repeated_search,
        &event_search_response,
        &mut event_search_operation,
    );
    assert_eq!(event_search_operation.context.context_found, 1);

    let mut show_event = McpInvocation::recognized("show_event").unwrap();
    show_event.bind_context_target("event-1".to_owned());
    let mut event_operation = show_event.completed(&show_response, Duration::ZERO, 100);
    recorder.correlate_delivered(show_event, &show_response, &mut event_operation);
    assert_eq!(event_operation.context.validated_discoveries, 1);

    let found = search_operation
        .context
        .context_found
        .saturating_add(repeated_operation.context.context_found)
        .saturating_add(event_search_operation.context.context_found);
    let validated = session_operation
        .context
        .validated_discoveries
        .saturating_add(event_operation.context.validated_discoveries);
    assert!(validated <= found, "validated={validated} found={found}");
}

#[test]
fn mcp_correlation_commits_with_the_store_and_rearms_after_reset() {
    let root = private_tempdir();
    let mut recorder = McpUsageRecorder::start(root.path().to_path_buf());
    let search_response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "results": [{
                    "result_scope": "event",
                    "result_type": "event",
                    "ctx_event_id": "event-1"
                }]
            }
        }
    });
    let show_response = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {"structuredContent": {"events": [{}]}}
    });
    let status_response = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "result": {"structuredContent": {}}
    });
    let search = McpInvocation::recognized("search").unwrap();
    let mut show = McpInvocation::recognized("show_event").unwrap();
    show.bind_context_target("event-1".to_owned());

    fs::create_dir(usage_path(root.path())).unwrap();
    recorder.record_delivered(search.clone(), &search_response, Duration::ZERO, 200);
    fs::remove_dir(usage_path(root.path())).unwrap();
    recorder.record_delivered(show.clone(), &show_response, Duration::ZERO, 100);
    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.calls, 1);
    assert_eq!(summary.context.context_searches, 0);
    assert_eq!(summary.context.context_found, 0);
    assert_eq!(summary.context.context_opened, 0);
    assert_eq!(summary.context.validated_discoveries, 0);

    recorder.record_delivered(search.clone(), &search_response, Duration::ZERO, 200);
    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.context.context_searches, 1);
    assert_eq!(summary.context.context_found, 1);

    assert!(reset(root.path()).unwrap());
    recorder.record_delivered(
        McpInvocation::recognized("status").unwrap(),
        &status_response,
        Duration::ZERO,
        50,
    );
    recorder.record_delivered(show.clone(), &show_response, Duration::ZERO, 100);
    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.calls, 2);
    assert_eq!(summary.context.context_opened, 0);
    assert_eq!(summary.context.validated_discoveries, 0);

    recorder.record_delivered(search, &search_response, Duration::ZERO, 200);
    recorder.record_delivered(show, &show_response, Duration::ZERO, 100);
    let summary = read_report(root.path(), true, false).summary.unwrap();
    assert_eq!(summary.calls, 4);
    assert_eq!(summary.context.context_searches, 1);
    assert_eq!(summary.context.context_found, 1);
    assert_eq!(summary.context.context_opened, 1);
    assert_eq!(summary.context.validated_discoveries, 1);
}

#[test]
fn mcp_blame_classification_reads_only_typed_match_fields() {
    let response = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "matches": [],
                "evidence": [{"display": "{\"predicate\":\"produced_by\"}"}]
            }
        }
    });

    let mut invocation = McpInvocation::recognized("blame").unwrap();
    invocation.bind_blame_target(&BlameTarget::Commit {
        oid: "abc1234".to_owned(),
        repository: None,
    });
    let completed = invocation.completed(&response, Duration::ZERO, 100);
    assert_eq!(completed.pro_outcome, ProOutcome::None);
    assert_eq!(completed.value_class, ValueClass::Empty);
}

#[test]
fn commit_outcome_classifier_is_closed_and_matches_the_wire_projection() {
    for state in [
        FactState::Asserted,
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        for predicate in [
            CommitPredicate::ProducedBy,
            CommitPredicate::PossiblyProducedBy,
            CommitPredicate::AmendedBy,
            CommitPredicate::CherryPickedFrom,
            CommitPredicate::Reverts,
            CommitPredicate::PushedBy,
            CommitPredicate::InspectedBy,
            CommitPredicate::ReferencedBy,
        ] {
            let structured = json!({
                "matches": [{
                    "kind": "commit",
                    "value": {"predicate": predicate, "state": state}
                }]
            });
            assert_eq!(
                super::super::classify_blame_json(Some(&structured)),
                super::super::classify_commit_predicate(predicate, state),
                "{predicate:?}/{state:?}"
            );
        }
    }
    assert_eq!(
        super::super::classify_commit_predicate(CommitPredicate::ProducedBy, FactState::Asserted),
        ProOutcome::Produced
    );
    assert_eq!(
        super::super::classify_commit_predicate(CommitPredicate::ReferencedBy, FactState::Asserted),
        ProOutcome::Possible
    );
    for state in [
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        assert_ne!(
            super::super::classify_commit_predicate(CommitPredicate::ProducedBy, state),
            ProOutcome::Produced
        );
    }
}

#[test]
fn typed_commit_blame_produces_only_for_asserted_produced_by() {
    let resource = |id: &str, kind| ResourceRef {
        id: id.to_owned(),
        kind,
        display: id.to_owned(),
    };
    let commit = resource("commit:abc1234", ResourceKind::Commit);
    for state in [
        FactState::Asserted,
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        let result = BlameResult {
            target: ResolvedBlameTarget::Commit {
                commit: commit.clone(),
                repository: resource("repository:ctx", ResourceKind::Repository),
            },
            git_snapshot: None,
            matches: vec![BlameMatch::Commit(CommitBlameMatch {
                fact_id: format!("fact:{state:?}"),
                fact_type: CommitFactType::Produced,
                predicate: CommitPredicate::ProducedBy,
                subject: commit.clone(),
                object: Some(resource("session:producer", ResourceKind::Session)),
                fact_occurred_at_ms: None,
                confidence: FactConfidence::Explicit,
                state,
                direct_actor: None,
                owning_root: None,
                evidence_numbers: Vec::new(),
            })],
            evidence: Vec::new(),
            next: None,
        };
        assert_eq!(
            super::super::classify_blame(&result),
            match state {
                FactState::Asserted => ProOutcome::Produced,
                FactState::Ambiguous => ProOutcome::Possible,
                FactState::Contradicted | FactState::Superseded => ProOutcome::None,
            },
            "{state:?}"
        );
    }
}

#[test]
fn file_and_pr_production_states_use_the_same_conservative_classifier() {
    for state in [
        FactState::Asserted,
        FactState::Ambiguous,
        FactState::Contradicted,
        FactState::Superseded,
    ] {
        assert_eq!(
            super::super::classify_production(
                ctx_pro_host_protocol::ProductionRelationship::ProducedBy,
                state,
            ),
            match state {
                FactState::Asserted => ProOutcome::Produced,
                FactState::Ambiguous => ProOutcome::Possible,
                FactState::Contradicted | FactState::Superseded => ProOutcome::None,
            }
        );
        assert_eq!(
            super::super::classify_production(
                ctx_pro_host_protocol::ProductionRelationship::PossiblyProducedBy,
                state,
            ),
            match state {
                FactState::Asserted | FactState::Ambiguous => ProOutcome::Possible,
                FactState::Contradicted | FactState::Superseded => ProOutcome::None,
            }
        );
    }
}

#[test]
fn local_mcp_vocabulary_is_closed() {
    for name in [
        "status",
        "sources",
        "search",
        "sql",
        "show_session",
        "show_event",
        "pro_status",
        "blame",
    ] {
        assert!(McpInvocation::recognized(name).is_some(), "{name}");
    }
    for name in [
        "initialize",
        "ping",
        "tools/list",
        "unknown",
        "private query",
    ] {
        assert!(McpInvocation::recognized(name).is_none(), "{name}");
    }
}

#[test]
fn mcp_recorder_observes_same_size_and_mtime_persistent_disable() {
    let _env = LocalUsageEnvGuard::unset();
    let root = private_tempdir();
    let config_path = root.path().join("config.toml");
    fs::write(&config_path, "[local_usage]\nenabled = true \n").unwrap();
    let original_modified = config_path.metadata().unwrap().modified().unwrap();
    let original_len = config_path.metadata().unwrap().len();
    let mut recorder = super::super::McpUsageRecorder::start(root.path().to_path_buf());
    let invocation = McpInvocation::recognized("status").unwrap();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "structuredContent": {"schema_version": 1}
    }});
    recorder.record_delivered(invocation.clone(), &response, Duration::ZERO, 40);
    fs::write(&config_path, "[local_usage]\nenabled = false\n").unwrap();
    assert_eq!(config_path.metadata().unwrap().len(), original_len);
    fs::File::options()
        .write(true)
        .open(&config_path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_modified))
        .unwrap();
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        1
    );
}

#[test]
fn mcp_recorder_retains_last_known_control_only_on_unrelated_config_failure() {
    let _env = LocalUsageEnvGuard::unset();
    let root = private_tempdir();
    let config_path = root.path().join("config.toml");
    fs::write(&config_path, "[local_usage]\nenabled = true\n").unwrap();
    let mut recorder = super::super::McpUsageRecorder::start(root.path().to_path_buf());
    let invocation = McpInvocation::recognized("status").unwrap();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "structuredContent": {"schema_version": 1}
    }});
    recorder.record_delivered(invocation.clone(), &response, Duration::ZERO, 40);
    fs::write(&config_path, "unrelated malformed line\n").unwrap();
    recorder.record_delivered(invocation.clone(), &response, Duration::ZERO, 40);
    assert_eq!(
        read_report(root.path(), true, false).summary.unwrap().calls,
        2
    );

    let disabled = private_tempdir();
    let disabled_config = disabled.path().join("config.toml");
    fs::write(&disabled_config, "[local_usage]\nenabled = false\n").unwrap();
    let mut recorder = super::super::McpUsageRecorder::start(disabled.path().to_path_buf());
    fs::write(
        &disabled_config,
        "unrelated malformed line without a local usage key\n",
    )
    .unwrap();
    recorder.record_delivered(invocation, &response, Duration::ZERO, 40);
    assert!(!usage_path(disabled.path()).exists());
}

#[test]
fn malformed_local_control_disables_mcp_refresh_and_startup() {
    let _env = LocalUsageEnvGuard::unset();
    let invocation = McpInvocation::recognized("status").unwrap();
    let response = json!({"jsonrpc": "2.0", "id": 1, "result": {
        "structuredContent": {"schema_version": 1}
    }});
    for (name, malformed) in [
        ("invalid_value", "[local_usage]\nenabled = malformed\n"),
        ("bare", "local_usage = true\n"),
        ("inline_table", "local_usage = { enabled = true }\n"),
        ("quoted_dotted", "\"local_usage\".enabled = true\n"),
        (
            "unicode_u_escaped_key",
            "\"local\\u005Fusage\".enabled = false\n",
        ),
        (
            "unicode_upper_u_escaped_table_path",
            "[\"\\U0000006Cocal_usage\".nested]\nvalue = false\n",
        ),
        (
            "owned_prefix_before_malformed_escape",
            "\"local\\u005Fusage.\\uZZZZ\" = false\n",
        ),
        (
            "duplicate_table",
            "[local_usage]\nenabled = true\n[local_usage]\n",
        ),
    ] {
        let root = private_tempdir();
        let config_path = root.path().join("config.toml");
        fs::write(&config_path, "[local_usage]\nenabled = true\n").unwrap();
        let mut recorder = super::super::McpUsageRecorder::start(root.path().to_path_buf());
        recorder.record_delivered(invocation.clone(), &response, Duration::ZERO, 40);
        fs::write(&config_path, malformed).unwrap();
        recorder.record_delivered(invocation.clone(), &response, Duration::ZERO, 40);
        assert_eq!(
            read_report(root.path(), true, false).summary.unwrap().calls,
            1,
            "{name}"
        );

        let startup = private_tempdir();
        fs::write(startup.path().join("config.toml"), malformed).unwrap();
        let mut recorder = super::super::McpUsageRecorder::start(startup.path().to_path_buf());
        recorder.record_delivered(invocation.clone(), &response, Duration::ZERO, 40);
        assert!(!usage_path(startup.path()).exists(), "{name}");
    }
}
