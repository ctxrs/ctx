use super::*;

#[test]
fn codex_valid_custom_tool_result_over_16_mib_is_retained() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-valid-large-result");
    let index_root = temp.path().join("index-valid-large-result");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000074";
    let marker = "valid-large-custom-tool-result";
    let mut output = marker.to_owned();
    output.push_str(&" ".repeat((17 * 1024 * 1024) - marker.len()));
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [custom_tool_result("large-custom-tool-call", output)],
    );
    assert!(
        fs::metadata(session_path(&sessions, native_session_id))
            .unwrap()
            .len()
            > 16 * 1024 * 1024
    );
    assert!(
        fs::metadata(session_path(&sessions, native_session_id))
            .unwrap()
            .len()
            < 32 * 1024 * 1024
    );
    let registry = register_tree(&[&sessions]);

    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert!(
        receipt.record_rejections.is_empty(),
        "unexpected rejections: {:?}",
        receipt.record_rejections
    );
    let index = VerifiedIndex::open(&index_root).unwrap();
    let records = records_for(&index, native_session_id);
    assert_eq!(records.len(), 1);
    assert!(matches!(
        &records[0].content.policy_status,
        ctx_history_core::CoreContentPolicyStatus::Omitted { reason }
            if reason == "Codex provider record content exceeds the Core content limit"
    ));
    assert!(records[0].content.normalized_body.is_none());
}

#[test]
fn codex_core_envelope_rejection_preserves_siblings_and_their_ids() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-core-envelope-rejection");
    let index_root = temp.path().join("index-core-envelope-rejection");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000075";
    let path = session_path(&sessions, native_session_id);
    let before_marker = "core-envelope-before";
    let after_marker = "core-envelope-after";
    let oversized_body = "x".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES);
    let repeated_native_id = "core-envelope-repeated-native-id";
    let mut before = message(before_marker);
    before["payload"]["id"] = serde_json::json!(repeated_native_id);
    let mut oversized = message(&oversized_body);
    oversized["payload"]["id"] = serde_json::json!(repeated_native_id);
    let mut after = message(after_marker);
    after["payload"]["id"] = serde_json::json!(repeated_native_id);
    fs::write(
        &path,
        jsonl_bytes([
            session_meta(
                native_session_id,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            before.clone(),
            oversized,
            after.clone(),
        ]),
    )
    .unwrap();
    let registry = register_tree(&[&sessions]);

    let rejected =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();

    assert!(rejected.failed_routes.is_empty());
    assert!(rejected.logical_source_failures.is_empty());
    assert_eq!(rejected.record_rejections.total(), 1);
    let counts = rejected.sources[0].counts();
    assert_eq!(counts.complete_records, 4);
    assert_eq!(counts.retained_records, 2);
    assert_eq!(counts.rejected_records, 1);
    assert_eq!(counts.ignored_records, 1);
    let [diagnostic] = rejected.record_rejections.rejections() else {
        panic!("one bounded Codex rejection diagnostic expected");
    };
    assert_eq!(diagnostic.provider, CaptureProvider::Codex);
    assert_eq!(diagnostic.source_selector, path.display().to_string());
    assert_eq!(diagnostic.line_number, 3);
    assert_eq!(
        diagnostic.class,
        ctx_history_capture_runtime::SourceBackedRecordRejectionClass::UnsupportedRecord
    );
    assert_eq!(
        diagnostic.detail,
        "Codex retained record exceeds the Core selected-content envelope"
    );
    let rejected_index = VerifiedIndex::open(&index_root).unwrap();
    let sibling_ids = [before_marker, after_marker].map(|marker| {
        rejected_index
            .search_event_candidates(marker, 8)
            .unwrap()
            .into_iter()
            .find(|candidate| {
                candidate.event.provider_session_id.as_deref() == Some(native_session_id)
            })
            .unwrap_or_else(|| panic!("missing retained sibling {marker}"))
            .event
            .event_id
    });
    drop(rejected_index);

    fs::write(
        &path,
        jsonl_bytes([
            session_meta(
                native_session_id,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            before,
            after,
        ]),
    )
    .unwrap();
    let (repaired, _) = incremental_refresh(&index_root, &registry, &rejected);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert!(repaired.record_rejections.is_empty());
    let repaired_index = VerifiedIndex::open(&index_root).unwrap();
    for (marker, event_id) in [before_marker, after_marker]
        .into_iter()
        .zip(sibling_ids.iter().copied())
    {
        assert!(repaired_index
            .search_event_candidates(marker, 8)
            .unwrap()
            .into_iter()
            .any(|candidate| candidate.event.event_id == event_id));
    }
    drop(repaired_index);

    let (replayed, _) = incremental_refresh(&index_root, &registry, &repaired);
    assert_eq!(replayed.commit.generation_id, repaired.commit.generation_id);
    assert!(replayed.record_rejections.is_empty());
    let replayed_index = VerifiedIndex::open(&index_root).unwrap();
    for (marker, event_id) in [before_marker, after_marker]
        .into_iter()
        .zip(sibling_ids.iter().copied())
    {
        assert!(replayed_index
            .search_event_candidates(marker, 8)
            .unwrap()
            .into_iter()
            .any(|candidate| candidate.event.event_id == event_id));
    }
}

#[test]
fn inherited_codex_session_metadata_is_admitted_in_both_provider_orders() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-inherited-metadata-orders");
    let index_root = temp.path().join("index-inherited-metadata-orders");
    fs::create_dir_all(&sessions).unwrap();
    let owner_first_id = "019fb000-0000-7000-8000-000000000050";
    let owner_first_parent = "019fb000-0000-7000-8000-000000000051";
    let ancestor_first_id = "019fb000-0000-7000-8000-000000000052";
    let ancestor_first_parent = "019fb000-0000-7000-8000-000000000053";
    let neighbor_id = "019fb000-0000-7000-8000-000000000054";

    fs::write(
        session_path(&sessions, owner_first_id),
        jsonl_bytes([
            session_meta(
                owner_first_id,
                ProviderNativeSessionRelationship::Forked,
                Some(owner_first_parent),
            ),
            message("ownerfirstinheritedmetadatamarker"),
            session_meta(
                owner_first_parent,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            session_meta(
                owner_first_id,
                ProviderNativeSessionRelationship::Forked,
                Some(owner_first_parent),
            ),
        ]),
    )
    .unwrap();
    fs::write(
        session_path(&sessions, ancestor_first_id),
        jsonl_bytes([
            session_meta(
                ancestor_first_parent,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            session_meta(
                ancestor_first_id,
                ProviderNativeSessionRelationship::Forked,
                Some(ancestor_first_parent),
            ),
            message("ancestorfirstinheritedmetadatamarker"),
            session_meta(
                ancestor_first_parent,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
        ]),
    )
    .unwrap();
    write_session(
        &sessions,
        neighbor_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("inheritedmetadataneighbormarker")],
    );

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert!(receipt.logical_source_failures.is_empty());
    assert_eq!(receipt.sources.len(), 3);

    let index = VerifiedIndex::open(&index_root).unwrap();
    for (native_session_id, marker) in [
        (owner_first_id, "ownerfirstinheritedmetadatamarker"),
        (ancestor_first_id, "ancestorfirstinheritedmetadatamarker"),
        (neighbor_id, "inheritedmetadataneighbormarker"),
    ] {
        assert_eq!(records_for(&index, native_session_id).len(), 1);
        assert_eq!(index.search_event_candidates(marker, 8).unwrap().len(), 1);
    }
    for native_session_id in [owner_first_id, ancestor_first_id] {
        let records = records_for(&index, native_session_id);
        let [record] = records.as_slice() else {
            panic!("one inherited-metadata owner record expected");
        };
        assert_eq!(
            record.provider_session_id.as_deref(),
            Some(native_session_id)
        );
        assert_eq!(
            record.session_relationship,
            Some(ProviderNativeSessionRelationship::Forked)
        );
        assert!(record.parent_session_id.is_some());
        assert!(record.root_session_id.is_none());
    }
}

#[test]
fn codex_rollout_ownership_quarantine_retries_after_file_repair() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-malformed-owner-neighbor");
    let index_root = temp.path().join("index-malformed-owner-neighbor");
    fs::create_dir_all(&sessions).unwrap();
    let valid_session_id = "019fb000-0000-7000-8000-000000000060";
    let repairable_session_id = "019fb000-0000-7000-8000-000000000061";
    let conflicting_session_id = "019fb000-0000-7000-8000-000000000062";
    let neighbor_marker = "validneighborretainedmarker";
    let previously_valid_marker = "repairablepreviouslyvalidmarker";
    let late_bad_marker = "latequarantinedprefixmarker";
    let repaired_marker = "repairablecorrectedownermarker";
    write_session(
        &sessions,
        valid_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message(neighbor_marker)],
    );
    let repairable_path = session_path(&sessions, repairable_session_id);
    write_session(
        &sessions,
        repairable_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message(previously_valid_marker)],
    );
    let registry = register_tree(&[&sessions]);
    let initial =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());
    assert!(initial.logical_source_failures.is_empty());
    let initial_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&initial_index, repairable_session_id).len(), 1);
    drop(initial_index);

    // The ownership ambiguity is deliberately beyond catalog's bounded
    // physical prefix. Each metadata record is individually valid; only the
    // complete set proves the later branch is disconnected from its owner.
    for ordinal in 0..33 {
        append_event(
            &repairable_path,
            message(&format!("lateownershipprefix{ordinal}")),
        );
    }
    append_event(&repairable_path, message(late_bad_marker));
    append_event(
        &repairable_path,
        session_meta(
            conflicting_session_id,
            ProviderNativeSessionRelationship::Root,
            None,
        ),
    );

    // The member workset first exercises append checkpoint restoration in the
    // bounded partial path. Quarantine then falls through to exhaustive
    // discovery, which retains the exact prior source until this file repairs.
    let quarantined = incremental_refresh_member(
        &index_root,
        &registry,
        &initial,
        &sessions,
        repairable_path.clone(),
    );
    assert!(
        quarantined.failed_routes.is_empty(),
        "unexpected route failures: {:?}",
        quarantined.failed_routes
    );
    assert_eq!(quarantined.logical_source_failures.total(), 1);
    assert!(quarantined.record_rejections.is_empty());
    let [failure] = quarantined.logical_source_failures.failures() else {
        panic!("one quarantined Codex rollout failure expected");
    };
    assert_eq!(
        failure.class,
        ctx_history_capture_runtime::SourceBackedSourceFailureClass::Unreadable
    );
    assert_eq!(
        failure.detail,
        format!(
            "Codex session ownership is ambiguous or conflicting; quarantined rollout file {}",
            repairable_path.display()
        )
    );
    assert_eq!(failure.source.provider(), CaptureProvider::Codex.as_str());

    let index = VerifiedIndex::open(&index_root).unwrap();
    assert!(index
        .search_event_candidates(neighbor_marker, 32)
        .unwrap()
        .into_iter()
        .any(|candidate| candidate.event.provider_session_id.as_deref() == Some(valid_session_id)));
    assert!(index.manifest().sources.iter().any(|certificate| {
        source_native_session_id(certificate.observation().source()) == Some(repairable_session_id)
    }));
    assert!(index.manifest().sources.iter().all(|certificate| {
        source_native_session_id(certificate.observation().source()) != Some(conflicting_session_id)
    }));
    assert!(source_records_contain(
        &index,
        repairable_session_id,
        previously_valid_marker
    ));
    assert!(index
        .search_event_candidates(late_bad_marker, 8)
        .unwrap()
        .is_empty());
    drop(index);

    fs::write(
        &repairable_path,
        jsonl_bytes([
            session_meta(
                repairable_session_id,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            message(repaired_marker),
        ]),
    )
    .unwrap();

    let repaired = incremental_refresh_member(
        &index_root,
        &registry,
        &quarantined,
        &sessions,
        repairable_path.clone(),
    );
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    let repaired_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&repaired_index, repairable_session_id).len(), 1);
    assert!(!source_records_contain(
        &repaired_index,
        repairable_session_id,
        previously_valid_marker
    ));
    assert_eq!(
        repaired_index
            .search_event_candidates(repaired_marker, 8)
            .unwrap()
            .len(),
        1
    );
    assert!(repaired_index
        .search_event_candidates(neighbor_marker, 8)
        .unwrap()
        .into_iter()
        .any(|candidate| candidate.event.provider_session_id.as_deref() == Some(valid_session_id)));
}

#[test]
fn codex_retrieval_exclusion_survives_raw_append_hydration_and_keeps_ids_stable() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-retrieval-exclusion");
    let index_root = temp.path().join("index-retrieval-exclusion");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-00000000005b";
    let retrieval_call_id = "retrieval-call";
    let ordinary_call_id = "ordinary-call";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [
            turn_context(),
            exec_call_with_command(retrieval_call_id, "ctx search retrievaldiscoverymarker"),
        ],
    );
    let registry = register_tree(&[&sessions]);

    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    let cold_index = VerifiedIndex::open(&index_root).unwrap();
    let cold_records = records_for(&cold_index, native_session_id);
    assert_eq!(cold_records.len(), 1);
    assert_eq!(
        cold_records[0].content.discovery_exclusion,
        Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
    );
    assert!(cold_records[0].content.activity.is_some());
    let retrieval_invocation_id = cold_records[0].event_id;
    assert_eq!(
        certificate_for(&cold_index, native_session_id).parser_revision(),
        CURRENT_PARSER_REVISION
    );
    let (_, _, _, checkpoint) = provider_checkpoint_envelope(&cold_index, native_session_id);
    assert_current_provider_checkpoint(&checkpoint);
    drop(cold_index);

    append_event(
        &path,
        exact_exec_result(retrieval_call_id, "retrievaldiscoverymarker result"),
    );
    let (appended, _) = incremental_refresh(&index_root, &registry, &cold);
    assert!(appended.failed_routes.is_empty());
    let appended_index = VerifiedIndex::open(&index_root).unwrap();
    let appended_records = records_for(&appended_index, native_session_id);
    assert_eq!(appended_records.len(), 2);
    assert_eq!(appended_records[0].event_id, retrieval_invocation_id);
    assert!(appended_records.iter().all(|record| {
        record.content.discovery_exclusion == Some(CoreDiscoveryExclusion::CtxRetrievalDerived)
            && record.content.activity.is_some()
    }));
    assert!(
        appended_index
            .search_event_candidates("retrievaldiscoverymarker", 32)
            .unwrap()
            .into_iter()
            .all(|candidate| candidate.event.provider_session_id.as_deref()
                != Some(native_session_id))
    );
    drop(appended_index);

    append_event(
        &path,
        exec_call_with_command(ordinary_call_id, "ctx status"),
    );
    append_event(
        &path,
        exact_exec_result(ordinary_call_id, "ordinarycontrolmarker result"),
    );
    let controlled =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(controlled.failed_routes.is_empty());
    let controlled_index = VerifiedIndex::open(&index_root).unwrap();
    let controlled_records = records_for(&controlled_index, native_session_id);
    assert_eq!(controlled_records.len(), 4);
    assert_eq!(controlled_records[0].event_id, retrieval_invocation_id);
    let ordinary = controlled_records
        .iter()
        .filter(|record| {
            record
                .content
                .activity
                .as_ref()
                .and_then(|activity| activity.provider_call_id.as_ref())
                == Some(&TypedKey::Utf8(ordinary_call_id.to_owned()))
        })
        .collect::<Vec<_>>();
    assert_eq!(ordinary.len(), 2);
    assert!(ordinary
        .iter()
        .all(|record| record.content.discovery_exclusion.is_none()));
    assert!(
        controlled_index
            .search_event_candidates("ordinarycontrolmarker", 32)
            .unwrap()
            .into_iter()
            .any(|candidate| candidate.event.provider_session_id.as_deref()
                == Some(native_session_id))
    );
}
