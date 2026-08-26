use super::*;

#[test]
fn catalog_owner_rejection_is_retryable_without_a_valid_sibling() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-catalog-owner-rejection");
    let index_root = temp.path().join("index-catalog-owner-rejection");
    fs::create_dir_all(&sessions).unwrap();
    let path = sessions.join("rollout-missing-owner.jsonl");
    fs::write(&path, jsonl_bytes([message("catalogownermissingmarker")])).unwrap();
    let registry = register_tree(&[&sessions]);

    let quarantined =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(quarantined.failed_routes.is_empty());
    assert_eq!(quarantined.logical_source_failures.total(), 1);

    let repaired_id = "019fb000-0000-7000-8000-000000000063";
    fs::write(
        &path,
        jsonl_bytes([
            session_meta(repaired_id, ProviderNativeSessionRelationship::Root, None),
            message("catalogownerrepairedmarker"),
        ]),
    )
    .unwrap();
    let (repaired, _) = incremental_refresh(&index_root, &registry, &quarantined);
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    assert_eq!(
        VerifiedIndex::open(&index_root)
            .unwrap()
            .search_event_candidates("catalogownerrepairedmarker", 8)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn codex_prefix_ownership_quarantine_retains_prior_source_and_repairs() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-prefix-owner-conflict");
    let index_root = temp.path().join("index-prefix-owner-conflict");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000064";
    let conflicting_session_id = "019fb000-0000-7000-8000-000000000065";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("prefixownerinitialmarker")],
    );
    let registry = register_tree(&[&sessions]);
    let initial =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(initial.failed_routes.is_empty());

    fs::write(
        &path,
        jsonl_bytes([
            session_meta(
                native_session_id,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            session_meta(
                conflicting_session_id,
                ProviderNativeSessionRelationship::Root,
                None,
            ),
            message("prefixownerquarantinedmarker"),
        ]),
    )
    .unwrap();
    let (quarantined, _) = incremental_refresh(&index_root, &registry, &initial);
    assert!(quarantined.failed_routes.is_empty());
    assert_eq!(quarantined.logical_source_failures.total(), 1);
    let index = VerifiedIndex::open(&index_root).unwrap();
    assert!(index.manifest().sources.iter().any(|certificate| {
        source_native_session_id(certificate.observation().source()) == Some(native_session_id)
    }));
    assert!(source_records_contain(
        &index,
        native_session_id,
        "prefixownerinitialmarker"
    ));
    assert!(index
        .search_event_candidates("prefixownerquarantinedmarker", 8)
        .unwrap()
        .is_empty());
    drop(index);

    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("prefixownerfixedmarker")],
    );
    let repaired = incremental_refresh_member(
        &index_root,
        &registry,
        &quarantined,
        &sessions,
        path.clone(),
    );
    assert!(repaired.failed_routes.is_empty());
    assert!(repaired.logical_source_failures.is_empty());
    let repaired_index = VerifiedIndex::open(&index_root).unwrap();
    let repaired_records = records_for(&repaired_index, native_session_id);
    assert_eq!(repaired_records.len(), 1);
    assert!(
        source_records_contain(&repaired_index, native_session_id, "prefixownerfixedmarker"),
        "repaired bodies: {:?}",
        repaired_records
            .iter()
            .map(|record| record.content.normalized_body.as_deref())
            .collect::<Vec<_>>()
    );
    assert!(!source_records_contain(
        &repaired_index,
        native_session_id,
        "prefixownerinitialmarker"
    ));
}

#[test]
fn malformed_late_session_meta_quarantines_only_its_rollout() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-malformed-late-meta");
    let index_root = temp.path().join("index-malformed-late-meta");
    fs::create_dir_all(&sessions).unwrap();
    let before_id = "019fb000-0000-7000-8000-000000000069";
    let malformed_id = "019fb000-0000-7000-8000-000000000070";
    let after_id = "019fb000-0000-7000-8000-000000000071";
    write_session(
        &sessions,
        before_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("malformedmetabeforemarker")],
    );
    write_session(
        &sessions,
        after_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("malformedmetaaftermarker")],
    );
    let path = session_path(&sessions, malformed_id);
    write_session(
        &sessions,
        malformed_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("malformedmetaquarantinedmarker")],
    );
    for ordinal in 0..33 {
        append_event(&path, message(&format!("malformedmetaprefix{ordinal}")));
    }
    let mut malformed = session_meta(malformed_id, ProviderNativeSessionRelationship::Root, None);
    malformed["timestamp"] = serde_json::json!("not-a-timestamp");
    malformed["payload"]["timestamp"] = serde_json::json!("not-a-timestamp");
    append_event(&path, malformed);

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.logical_source_failures.total(), 1);
    let index = VerifiedIndex::open(&index_root).unwrap();
    for marker in ["malformedmetabeforemarker", "malformedmetaaftermarker"] {
        assert_eq!(index.search_event_candidates(marker, 8).unwrap().len(), 1);
    }
    assert!(index.manifest().sources.iter().all(|certificate| {
        source_native_session_id(certificate.observation().source()) != Some(malformed_id)
    }));
    assert!(index
        .search_event_candidates("malformedmetaquarantinedmarker", 8)
        .unwrap()
        .is_empty());
}

#[test]
fn selector_ambiguous_session_meta_quarantines_its_rollout() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-selector-ambiguous-meta");
    let index_root = temp.path().join("index-selector-ambiguous-meta");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fb000-0000-7000-8000-000000000072";
    let path = session_path(&sessions, native_session_id);
    write_session(
        &sessions,
        native_session_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("selectorambiguousquarantinedmarker")],
    );
    for ordinal in 0..33 {
        append_event(&path, message(&format!("selectorambiguousprefix{ordinal}")));
    }
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            format!(
                "{{\"timestamp\":\"2026-08-09T12:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{native_session_id}\",\"timestamp\":\"2026-08-09T12:00:00Z\",\"source\":\"cli\"}},\"payl\\u006fad\":{{\"id\":\"{native_session_id}\",\"timestamp\":\"2026-08-09T12:00:00Z\",\"source\":\"cli\"}}}}\n"
            )
            .as_bytes(),
        )
        .unwrap();

    let registry = register_tree(&[&sessions]);
    let receipt =
        refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(receipt.failed_routes.is_empty());
    assert_eq!(receipt.logical_source_failures.total(), 1);
    assert!(VerifiedIndex::open(&index_root)
        .unwrap()
        .search_event_candidates("selectorambiguousquarantinedmarker", 8)
        .unwrap()
        .is_empty());
}

#[test]
fn committed_codex_good_bad_good_retains_prior_source_while_neighbor_advances() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-good-bad-good");
    let index_root = temp.path().join("index-good-bad-good");
    fs::create_dir_all(&sessions).unwrap();
    let repairable_id = "019fb000-0000-7000-8000-000000000063";
    let conflicting_parent = "019fb000-0000-7000-8000-000000000064";
    let neighbor_id = "019fb000-0000-7000-8000-000000000065";
    let repairable_path = session_path(&sessions, repairable_id);
    let neighbor_path = session_path(&sessions, neighbor_id);
    write_session(
        &sessions,
        repairable_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("goodbadgood-original")],
    );
    write_session(
        &sessions,
        neighbor_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("goodbadgood-neighbor-original")],
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());

    fs::write(
        &repairable_path,
        jsonl_bytes([
            session_meta(repairable_id, ProviderNativeSessionRelationship::Root, None),
            session_meta(
                repairable_id,
                ProviderNativeSessionRelationship::Forked,
                Some(conflicting_parent),
            ),
            message("goodbadgood-quarantined"),
        ]),
    )
    .unwrap();
    append_event(&neighbor_path, message("goodbadgood-neighbor-advanced"));

    let (middle, _) = incremental_refresh(&index_root, &registry, &cold);
    assert!(middle.failed_routes.is_empty());
    let middle_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&middle_index, repairable_id).len(), 1);
    assert!(source_records_contain(
        &middle_index,
        repairable_id,
        "goodbadgood-original"
    ));
    assert!(!source_records_contain(
        &middle_index,
        repairable_id,
        "goodbadgood-quarantined"
    ));
    assert!(source_records_contain(
        &middle_index,
        neighbor_id,
        "goodbadgood-neighbor-advanced"
    ));
    drop(middle_index);

    fs::write(
        &repairable_path,
        jsonl_bytes([
            session_meta(repairable_id, ProviderNativeSessionRelationship::Root, None),
            message("goodbadgood-repaired"),
        ]),
    )
    .unwrap();
    let (repaired, _) = incremental_refresh(&index_root, &registry, &middle);
    assert!(repaired.failed_routes.is_empty());
    let repaired_index = VerifiedIndex::open(&index_root).unwrap();
    assert!(!source_records_contain(
        &repaired_index,
        repairable_id,
        "goodbadgood-original"
    ));
    assert!(source_records_contain(
        &repaired_index,
        repairable_id,
        "goodbadgood-repaired"
    ));
    assert!(source_records_contain(
        &repaired_index,
        neighbor_id,
        "goodbadgood-neighbor-advanced"
    ));
}

#[test]
fn committed_codex_good_partial_repaired_retains_prior_source_while_neighbor_advances() {
    let temp = tempdir().unwrap();
    let sessions = temp.path().join("sessions-good-partial-repaired");
    let index_root = temp.path().join("index-good-partial-repaired");
    fs::create_dir_all(&sessions).unwrap();
    let repairable_id = "019fb000-0000-7000-8000-000000000066";
    let neighbor_id = "019fb000-0000-7000-8000-000000000067";
    let repairable_path = session_path(&sessions, repairable_id);
    let neighbor_path = session_path(&sessions, neighbor_id);
    write_session(
        &sessions,
        repairable_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("partial-original")],
    );
    write_session(
        &sessions,
        neighbor_id,
        ProviderNativeSessionRelationship::Root,
        None,
        [message("partial-neighbor-original")],
    );
    let registry = register_tree(&[&sessions]);
    let cold = refresh_source_backed_generation(&index_root, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());

    fs::write(
        &repairable_path,
        br#"{"timestamp":"2026-08-09T12:00:00Z","type":"session_meta","payload":{"id":"019fb000-0000-7000-8000-000000000066""#,
    )
    .unwrap();
    append_event(&neighbor_path, message("partial-neighbor-advanced"));

    let (middle, _) = incremental_refresh(&index_root, &registry, &cold);
    assert!(middle.failed_routes.is_empty());
    let middle_index = VerifiedIndex::open(&index_root).unwrap();
    assert_eq!(records_for(&middle_index, repairable_id).len(), 1);
    assert!(source_records_contain(
        &middle_index,
        repairable_id,
        "partial-original"
    ));
    assert!(source_records_contain(
        &middle_index,
        neighbor_id,
        "partial-neighbor-advanced"
    ));
    drop(middle_index);

    fs::write(
        &repairable_path,
        jsonl_bytes([
            session_meta(repairable_id, ProviderNativeSessionRelationship::Root, None),
            message("partial-repaired"),
        ]),
    )
    .unwrap();
    let (repaired, _) = incremental_refresh(&index_root, &registry, &middle);
    assert!(repaired.failed_routes.is_empty());
    let repaired_index = VerifiedIndex::open(&index_root).unwrap();
    assert!(!source_records_contain(
        &repaired_index,
        repairable_id,
        "partial-original"
    ));
    assert!(source_records_contain(
        &repaired_index,
        repairable_id,
        "partial-repaired"
    ));
    assert!(source_records_contain(
        &repaired_index,
        neighbor_id,
        "partial-neighbor-advanced"
    ));
}
