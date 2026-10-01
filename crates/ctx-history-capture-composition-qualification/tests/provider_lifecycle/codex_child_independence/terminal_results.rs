use super::*;

#[test]
fn legacy_mcp_dual_carrier_append_preserves_activity_then_duplicate_withdraws_it() {
    // Codex e07e58c8429019de78b138d7138deaaf7f3ef22c persists the legacy
    // McpToolCallEnd and renders the same execution as FunctionCallOutput.
    for mcp_first in [false, true] {
        for direct_append in [false, true] {
            for duplicate_mcp in [false, true] {
                let temp = tempdir().unwrap();
                let sessions = temp.path().join("sessions");
                let index_root = temp.path().join("index");
                fs::create_dir(&sessions).unwrap();
                let session = "019fb000-0000-7000-8000-000000000061";
                let call_id = "legacy-mcp";
                let arguments = serde_json::json!({"query":"terminal uniqueness"});
                let invocation = serde_json::json!({"type":"response_item","payload":{
                    "type":"function_call","name":"mcp__ctx__search","call_id":call_id,
                    "arguments":arguments.to_string()
                }});
                let mcp = exact_mcp_result(call_id, "legacy dual witness");
                let model_output = serde_json::json!([
                    {"type":"input_text","text":"Wall time: 0.0000 seconds\nOutput:"},
                    {"type":"input_text","text":"legacy dual witness"}
                ]);
                let model = serde_json::json!({"type":"response_item","payload":{
                    "type":"function_call_output","call_id":call_id,"output":model_output
                }});
                let (first, second) = if mcp_first {
                    (&mcp, &model)
                } else {
                    (&model, &mcp)
                };
                write_session(
                    &sessions,
                    session,
                    ProviderNativeSessionRelationship::Root,
                    None,
                    [turn_context(), invocation, first.clone()],
                );
                let registry = register_tree(&[&sessions]);
                let prefix =
                    refresh_source_backed_generation(&index_root, &registry, writer_options())
                        .unwrap();
                assert!(prefix.failed_routes.is_empty());
                let index = VerifiedIndex::open_pinned(&index_root).unwrap();
                let before = records_for(&index, session);
                assert_eq!(before.len(), 2);
                assert!(before.iter().all(|record| record
                    .content
                    .activity
                    .as_ref()
                    .unwrap()
                    .provider_call_id
                    .is_some()));
                drop(index);
                let path = session_path(&sessions, session);
                append_event(&path, second.clone());
                let (paired, completed) = if direct_append {
                    let (receipt, completed) = incremental_refresh(&index_root, &registry, &prefix);
                    (receipt, Some(completed))
                } else {
                    (
                        refresh_source_backed_generation(&index_root, &registry, writer_options())
                            .unwrap(),
                        None,
                    )
                };
                assert!(paired.failed_routes.is_empty());
                let index = VerifiedIndex::open_pinned(&index_root).unwrap();
                let paired_records = records_for(&index, session);
                assert_eq!(paired_records.len(), 3);
                if let Some(completed) = completed {
                    assert_eq!(completed, 3);
                }
                assert_eq!(&paired_records[..2], &before);
                for record in &paired_records {
                    record.validate_contract().unwrap();
                    assert_eq!(
                        record.content.activity.as_ref().unwrap().provider_call_id,
                        Some(TypedKey::Utf8(call_id.to_owned()))
                    );
                }
                assert_eq!(
                    paired_records[0]
                        .content
                        .activity
                        .as_ref()
                        .unwrap()
                        .invocation
                        .as_ref()
                        .unwrap()
                        .arguments,
                    ctx_history_core::ActivityJsonCapture::Present {
                        value: serde_json::json!(arguments.to_string())
                    }
                );
                let mcp_record = &paired_records[if mcp_first { 1 } else { 2 }];
                let mcp_activity = mcp_record.content.activity.as_ref().unwrap();
                let mcp_invocation = mcp_activity.invocation.as_ref().unwrap();
                assert_eq!(mcp_invocation.protocol.as_deref(), Some("mcp"));
                assert_eq!(mcp_invocation.server.as_deref(), Some("ctx"));
                assert_eq!(mcp_invocation.tool, "search");
                assert_eq!(
                    mcp_invocation.arguments,
                    ctx_history_core::ActivityJsonCapture::Present { value: arguments }
                );
                assert_eq!(
                    mcp_activity.result.as_ref().unwrap().structured_content,
                    ctx_history_core::ActivityJsonCapture::Present {
                        value: mcp["payload"]["result"].clone()
                    }
                );
                assert_eq!(
                    mcp_record.content.discovery_exclusion,
                    Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
                );
                let model_record = &paired_records[if mcp_first { 2 } else { 1 }];
                assert_eq!(
                    model_record
                        .content
                        .activity
                        .as_ref()
                        .unwrap()
                        .result
                        .as_ref()
                        .unwrap()
                        .structured_content,
                    ctx_history_core::ActivityJsonCapture::Present {
                        value: model_output
                    }
                );
                drop(index);
                append_event(
                    &path,
                    if duplicate_mcp {
                        mcp.clone()
                    } else {
                        model.clone()
                    },
                );
                let (duplicated, completed) = if direct_append {
                    let (receipt, completed) = incremental_refresh(&index_root, &registry, &paired);
                    (receipt, Some(completed))
                } else {
                    (
                        refresh_source_backed_generation(&index_root, &registry, writer_options())
                            .unwrap(),
                        None,
                    )
                };
                assert!(duplicated.failed_routes.is_empty());
                let index = VerifiedIndex::open_pinned(&index_root).unwrap();
                let records = records_for(&index, session);
                assert_eq!(records.len(), 4);
                if let Some(completed) = completed {
                    assert_eq!(completed, 4);
                }
                for record in &records {
                    record.validate_contract().unwrap();
                    assert!(record
                        .content
                        .activity
                        .as_ref()
                        .is_none_or(|activity| activity.provider_call_id.is_none()
                            && activity.invocation.is_none()
                            && activity.result.is_none()));
                }
                for (record, prior) in records.iter().zip(&paired_records) {
                    assert_eq!(record.event_id, prior.event_id);
                    assert_eq!(record.native_event_id, prior.native_event_id);
                    assert_eq!(
                        record.content.structured_content,
                        prior.content.structured_content
                    );
                    assert_eq!(
                        record.content.normalized_body,
                        prior.content.normalized_body
                    );
                }
                assert!(records[1..]
                    .iter()
                    .all(|record| record.content.discovery_exclusion.is_none()));
                drop(index);
                let reopened = VerifiedIndex::open_pinned(&index_root).unwrap();
                assert_eq!(records_for(&reopened, session), records);
                drop(reopened);
                let cold_root = temp.path().join("cold");
                let cold =
                    refresh_source_backed_generation(&cold_root, &registry, writer_options())
                        .unwrap();
                assert!(cold.failed_routes.is_empty());
                let cold_index = VerifiedIndex::open_pinned(&cold_root).unwrap();
                assert_eq!(records_for(&cold_index, session), records);
                let (repeated, _) = incremental_refresh(&index_root, &registry, &duplicated);
                assert_eq!(
                    repeated.commit.generation_id,
                    duplicated.commit.generation_id
                );
            }
        }
    }
}

#[test]
fn codex_cold_duplicate_direct_and_mcp_terminals_fail_open() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-cold-duplicate-terminals");
    let index_root = temp.path().join("index-cold-duplicate-terminals");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-00000000005c";
    let direct_call_id = "cold-direct-duplicate";
    let mcp_call_id = "cold-mcp-duplicate";
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            turn_context(),
            exec_call_with_command(direct_call_id, "ctx search coldduplicatequery"),
            exact_exec_result(direct_call_id, "colddirectduplicatefirst"),
            exact_exec_result(direct_call_id, "colddirectduplicatesecond"),
            exact_mcp_result(mcp_call_id, "coldmcpduplicatefirst"),
            exact_mcp_result(mcp_call_id, "coldmcpduplicatesecond"),
        ],
    );
    let registry = register_tree(&[&sessions]);

    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let records = records_for(&index, native_session_id);
    assert_eq!(records.len(), 5);
    let invocation = &records[0];
    assert_eq!(invocation.event_type, "tool_call");
    assert_eq!(
        invocation
            .content
            .activity
            .as_ref()
            .unwrap()
            .provider_call_id,
        None
    );
    assert_eq!(
        invocation.content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let terminals = &records[1..];
    assert_eq!(terminals.len(), 4);
    assert!(terminals.iter().all(|record| {
        let activity = record.content.activity.as_ref().unwrap();
        activity.provider_call_id.is_none() && activity.invocation.is_none()
    }));
    assert!(terminals
        .iter()
        .all(|record| record.content.discovery_exclusion.is_none()));
    for marker in ["colddirectduplicatefirst", "coldmcpduplicatefirst"] {
        assert!(search_event_candidates(&index, marker, 32)
            .into_iter()
            .any(|candidate| candidate.event.provider_session_id.as_deref()
                == Some(native_session_id)));
    }
}

#[test]
fn codex_appended_duplicate_direct_and_mcp_terminals_retract_exclusion_with_stable_ids() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-appended-duplicate-terminals");
    let index_root = temp.path().join("index-appended-duplicate-terminals");
    fs::create_dir_all(&sessions).unwrap();
    let direct_session_id = "019fb000-0000-7000-8000-00000000005d";
    let mcp_session_id = "019fb000-0000-7000-8000-00000000005e";
    let direct_call_id = "appended-direct-duplicate";
    let mcp_call_id = "appended-mcp-duplicate";
    write_session(
        &sessions,
        direct_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            turn_context(),
            exec_call_with_command(direct_call_id, "ctx search appenddirectquery"),
            exact_exec_result(direct_call_id, "appenddirectfirst"),
        ],
    );
    write_session(
        &sessions,
        mcp_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            turn_context(),
            exact_mcp_result(mcp_call_id, "appendmcpfirst"),
        ],
    );
    let registry = register_tree(&[&sessions]);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let cold_direct = records_for(&cold_index, direct_session_id);
    let cold_mcp = records_for(&cold_index, mcp_session_id);
    assert_eq!(cold_direct.len(), 2);
    assert_eq!(cold_mcp.len(), 1);
    assert!(cold_direct.iter().chain(&cold_mcp).all(|record| {
        record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    }));
    let direct_invocation_id = cold_direct[0].event_id;
    let direct_result_id = cold_direct[1].event_id;
    let mcp_result_id = cold_mcp[0].event_id;
    drop(cold_index);

    append_event(
        &session_path(&sessions, direct_session_id),
        exact_exec_result(direct_call_id, "appenddirectsecond"),
    );
    append_event(
        &session_path(&sessions, mcp_session_id),
        exact_mcp_result(mcp_call_id, "appendmcpsecond"),
    );
    let (appended, _) = incremental_refresh(&index_root, &registry, &cold);
    assert!(appended.failed_routes.is_empty());
    let appended_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let direct = records_for(&appended_index, direct_session_id);
    let mcp = records_for(&appended_index, mcp_session_id);
    assert_eq!(direct.len(), 3);
    assert_eq!(mcp.len(), 2);
    assert_eq!(direct[0].event_id, direct_invocation_id);
    assert_eq!(direct[1].event_id, direct_result_id);
    assert_eq!(mcp[0].event_id, mcp_result_id);
    assert_ne!(direct[2].event_id, direct_result_id);
    assert_ne!(mcp[1].event_id, mcp_result_id);
    assert_eq!(
        direct[0].content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert_eq!(
        direct[0]
            .content
            .activity
            .as_ref()
            .unwrap()
            .provider_call_id,
        None
    );
    assert_eq!(
        direct[0].content.structured_content,
        cold_direct[0].content.structured_content
    );
    assert!(direct[1..].iter().chain(&mcp).all(|record| {
        let activity = record.content.activity.as_ref().unwrap();
        record.content.discovery_exclusion.is_none()
            && activity.provider_call_id.is_none()
            && activity.invocation.is_none()
    }));
    for marker in ["appenddirectfirst", "appendmcpfirst"] {
        assert!(search_event_candidates(&appended_index, marker, 32)
            .into_iter()
            .any(|candidate| matches!(
                candidate.event.provider_session_id.as_deref(),
                Some(session_id) if session_id == direct_session_id || session_id == mcp_session_id
            )));
    }
}

#[test]
fn codex_incremental_unique_terminal_append_and_restart_stay_suffix_bounded() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-incremental-unique-terminals");
    let index_root = temp.path().join("index-incremental-unique-terminals");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-00000000005f";
    let first_call_id = "incremental-unique-first";
    let second_call_id = "incremental-unique-second";
    let third_call_id = "incremental-unique-third";
    let path = session_path(&sessions, native_session_id);
    let large_prefix_body = "p".repeat(256 * 1024);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            turn_context(),
            message(&large_prefix_body),
            exec_call_with_command(first_call_id, "ctx search incrementalfirstquery"),
            exact_exec_result(first_call_id, "incrementaluniquefirst"),
        ],
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let cold_records = records_for(&cold_index, native_session_id);
    let first_event_id = result_record_for_call(&cold_records, first_call_id).event_id;
    assert_eq!(
        result_record_for_call(&cold_records, first_call_id)
            .content
            .discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    drop(cold_index);

    append_event(
        &path,
        exact_mcp_result(second_call_id, "incrementaluniquesecond"),
    );
    let (appended, completed_records) = incremental_refresh(&index_root, &registry, &cold);
    assert!(appended.failed_routes.is_empty());
    assert_eq!(completed_records, 1);
    let appended_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let appended_records = records_for(&appended_index, native_session_id);
    assert_eq!(
        result_record_for_call(&appended_records, first_call_id).event_id,
        first_event_id
    );
    let second = result_record_for_call(&appended_records, second_call_id);
    let second_event_id = second.event_id;
    assert_eq!(
        second.content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let (_, _, _, appended_checkpoint) =
        provider_checkpoint_envelope(&appended_index, native_session_id);
    assert_current_provider_checkpoint(&appended_checkpoint);
    drop(appended_index);

    append_event(
        &path,
        exec_call_with_command(third_call_id, "ctx search incrementalthirdquery"),
    );
    append_event(
        &path,
        exact_exec_result(third_call_id, "incrementaluniquethird"),
    );
    let restarted_registry = register_tree(&[&sessions]);
    let (restarted, restart_completed_records) =
        incremental_refresh(&index_root, &restarted_registry, &appended);
    assert!(restarted.failed_routes.is_empty());
    assert_eq!(restart_completed_records, 2);
    let restarted_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let restarted_records = records_for(&restarted_index, native_session_id);
    assert_eq!(
        result_record_for_call(&restarted_records, first_call_id).event_id,
        first_event_id
    );
    assert_eq!(
        result_record_for_call(&restarted_records, second_call_id).event_id,
        second_event_id
    );
    for call_id in [first_call_id, second_call_id, third_call_id] {
        assert_eq!(
            result_record_for_call(&restarted_records, call_id)
                .content
                .discovery_exclusion,
            Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
        );
    }
}

#[test]
fn inferred_codex_member_refresh_keeps_released_identity_and_stays_bounded() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-inferred-bounded-member");
    let index_root = temp.path().join("index-inferred-bounded-member");
    fs::create_dir_all(&sessions).unwrap();
    let selected_session_id = "019fb000-0000-7000-8000-000000000071";
    let neighbor_session_id = "019fb000-0000-7000-8000-000000000072";
    let selected_path = session_path(&sessions, selected_session_id);
    write_session(
        &sessions,
        selected_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("inferred bounded initial")],
    );
    write_session(
        &sessions,
        neighbor_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("inferred bounded neighbor")],
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());

    append_event(&selected_path, message("inferred bounded appended"));
    let exhaustive_inventory_runs = Rc::new(Cell::new(0));
    let observed_runs = Rc::clone(&exhaustive_inventory_runs);
    ctx_history_provider_codex::codex::nativepath::source_backed::install_after_codex_metadata_inventory_hook(
        move || observed_runs.set(observed_runs.get() + 1),
    );
    let refreshed =
        incremental_refresh_member(&index_root, &registry, &cold, &sessions, selected_path);
    assert!(refreshed.failed_routes.is_empty());
    assert_eq!(
        exhaustive_inventory_runs.get(),
        0,
        "a member workset should not fall back to whole-tree discovery"
    );
    assert!(source_records_contain(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        selected_session_id,
        "inferred bounded appended"
    ));

    // Consume the thread-local one-shot observer and prove it distinguishes a
    // normal exhaustive refresh from the bounded member path above.
    let exhaustive =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(exhaustive.failed_routes.is_empty());
    assert_eq!(exhaustive_inventory_runs.get(), 1);
}

#[test]
fn codex_incremental_4097th_terminal_saturates_only_the_checkpoint() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-incremental-terminal-saturation");
    let index_root = temp.path().join("index-incremental-terminal-saturation");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000060";
    let first_call_id = "capacity-terminal-0";
    let path = session_path(&sessions, native_session_id);
    let mut events = Vec::with_capacity(4_098);
    events.push(turn_context());
    events.push(exec_call_with_command(
        first_call_id,
        "ctx search capacityfirstquery",
    ));
    events.extend((0..4_096).map(|index| {
        exact_exec_result(
            &format!("capacity-terminal-{index}"),
            "capacityterminalresult",
        )
    }));
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        events,
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let cold_records = records_for(&cold_index, native_session_id);
    let first = result_record_for_call(&cold_records, first_call_id);
    let first_event_id = first.event_id;
    assert_eq!(
        first.content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    let (_, _, _, cold_checkpoint) = provider_checkpoint_envelope(&cold_index, native_session_id);
    assert_current_provider_checkpoint(&cold_checkpoint);
    drop(cold_index);

    append_event(
        &path,
        exact_exec_result("capacity-terminal-4096", "capacityterminaloverflow"),
    );
    let (saturated, completed_records) = incremental_refresh(&index_root, &registry, &cold);
    assert!(saturated.failed_routes.is_empty());
    let saturated_index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let saturated_records = records_for(&saturated_index, native_session_id);
    assert_eq!(completed_records, 1);
    assert_eq!(
        result_record_for_call(&saturated_records, first_call_id).event_id,
        first_event_id
    );
    assert_eq!(
        result_record_for_call(&saturated_records, first_call_id)
            .content
            .discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert!(saturated_records.iter().all(|record| {
        record
            .content
            .activity
            .as_ref()
            .unwrap()
            .provider_call_id
            .is_some()
    }));
    let (_, _, _, saturated_checkpoint) =
        provider_checkpoint_envelope(&saturated_index, native_session_id);
    assert_current_provider_checkpoint(&saturated_checkpoint);
}
