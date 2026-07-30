use super::*;

#[test]
fn malformed_session_owner_quarantines_only_that_source() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let valid_id = "019fa000-0000-7000-8000-000000000009";
    let malformed_id = "019fa000-0000-7000-8000-000000000010";
    write_session(
        &sessions,
        valid_id,
        &[message("assistant", "valid source remains searchable")],
    );
    fs::write(
        session_path(&sessions, malformed_id),
        format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{malformed_id}\"}}\n{}\n",
            message("assistant", "must not be published without an exact owner")
        ),
    )
    .unwrap();

    let cold = ingest_codex_source_backed_inner_v0(
        &sessions,
        &index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(2),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    assert_eq!(cold.commit.indexed_documents, 1);
    assert_eq!(cold.counters.staged_documents, 1);
    assert_eq!(cold.counters.rejected_records_scanned, 2);

    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(verified.document_count(), 1);
    assert_eq!(
        search_event_ids(&verified, "valid source remains searchable").len(),
        1
    );
    assert!(search_event_ids(&verified, "must not be published without an exact owner").is_empty());
    let malformed_source = codex_source_key(malformed_id).unwrap();
    let malformed_certificate = verified
        .manifest()
        .sources
        .iter()
        .find(|source| {
            source
                .observation()
                .source()
                .exact_descriptor_eq(&malformed_source)
        })
        .unwrap();
    assert_eq!(malformed_certificate.counts().complete_records, 2);
    assert_eq!(malformed_certificate.counts().rejected_records, 2);
    assert_eq!(malformed_certificate.counts().indexed_documents, 0);
    assert!(malformed_certificate.frontier().is_none());

    let before_generation = verified.generation_id().to_owned();
    let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replay.commit.generation_id, before_generation);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 1);
}

#[test]
fn source_backed_cold_parallel_matches_single_lane_semantics() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let single_index = temp.path().join("single-index");
    let parallel_index = temp.path().join("parallel-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_ids = [
        "019fa000-0000-7000-8000-000000000011",
        "019fa000-0000-7000-8000-000000000012",
        "019fa000-0000-7000-8000-000000000013",
        "019fa000-0000-7000-8000-000000000014",
    ];
    for (index, native_session_id) in native_session_ids.iter().enumerate() {
        write_session(
            &sessions,
            native_session_id,
            &[
                message(
                    "user",
                    &format!("parallel semantic sentinel source {index}"),
                ),
                message(
                    "assistant",
                    &format!("ordered locator sentinel source {index}"),
                ),
            ],
        );
    }

    let single = ingest_codex_source_backed_inner_v0(
        &sessions,
        &single_index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(1),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    let parallel = ingest_codex_source_backed_inner_v0(
        &sessions,
        &parallel_index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(4),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    assert_eq!(single.counters.scanner_workers, 1);
    assert_eq!(parallel.counters.scanner_workers, 4);
    assert_eq!(single.commit.indexed_documents, 8);
    assert_eq!(parallel.commit.indexed_documents, 8);
    let mut single_counters = single.counters;
    let mut parallel_counters = parallel.counters;
    single_counters.scanner_workers = 0;
    parallel_counters.scanner_workers = 0;
    assert_eq!(single_counters, parallel_counters);

    let single_verified = VerifiedIndex::open(&single_index).unwrap();
    let parallel_verified = VerifiedIndex::open(&parallel_index).unwrap();
    assert_eq!(
        single_verified.manifest().sources,
        parallel_verified.manifest().sources
    );
    assert_eq!(
        single_verified.manifest().generation_id().unwrap(),
        parallel_verified.manifest().generation_id().unwrap()
    );
    assert_eq!(
        single_verified.document_count(),
        parallel_verified.document_count()
    );
    for native_session_id in native_session_ids {
        let source_key = codex_source_key(native_session_id).unwrap();
        let session_id = codex_session_identity(&source_key, native_session_id).unwrap();
        assert_eq!(
            single_verified
                .events_for_session(session_id.as_uuid())
                .unwrap(),
            parallel_verified
                .events_for_session(session_id.as_uuid())
                .unwrap()
        );
    }
    assert_eq!(
        search_event_ids(&single_verified, "parallel semantic sentinel"),
        search_event_ids(&parallel_verified, "parallel semantic sentinel")
    );
    assert_eq!(
        search_event_ids(&single_verified, "ordered locator sentinel"),
        search_event_ids(&parallel_verified, "ordered locator sentinel")
    );
}
#[test]
fn source_backed_incremental_mixed_run_stays_serial() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let first_id = "019fa000-0000-7000-8000-000000000021";
    let second_id = "019fa000-0000-7000-8000-000000000022";
    let cold_id = "019fa000-0000-7000-8000-000000000023";
    write_session(
        &sessions,
        first_id,
        &[message("user", "first initial sentinel")],
    );
    write_session(
        &sessions,
        second_id,
        &[message("user", "second initial sentinel")],
    );
    let initial = ingest_codex_source_backed_inner_v0(
        &sessions,
        &index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(2),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    assert_eq!(initial.counters.scanner_workers, 2);
    assert_eq!(initial.counters.cold_sources, 2);

    let first_path = session_path(&sessions, first_id);
    OpenOptions::new()
        .append(true)
        .open(first_path)
        .unwrap()
        .write_all(format!("{}\n", message("assistant", "append sentinel")).as_bytes())
        .unwrap();
    write_session(&sessions, cold_id, &[message("user", "new cold sentinel")]);

    let mixed = ingest_codex_source_backed_inner_v0(
        &sessions,
        &index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(4),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    assert_eq!(mixed.counters.scanner_workers, 1);
    assert_eq!(mixed.counters.appended_sources, 1);
    assert_eq!(mixed.counters.replayed_sources, 1);
    assert_eq!(mixed.counters.cold_sources, 1);
    assert_eq!(mixed.counters.staged_documents, 2);

    let replay = ingest_codex_source_backed_inner_v0(
        &sessions,
        &index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(4),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    assert_eq!(replay.counters.scanner_workers, 0);
    assert_eq!(replay.counters.replayed_sources, 3);
    assert_eq!(replay.counters.staged_documents, 0);
    assert_eq!(replay.timings.scan_and_stage, Duration::ZERO);
    assert_eq!(VerifiedIndex::open(&index).unwrap().document_count(), 4);
}

#[test]
fn source_backed_worker_failure_does_not_publish_a_generation() {
    let temp = tempfile::tempdir().unwrap();
    let baseline_sessions = temp.path().join("baseline-sessions");
    let failing_sessions = temp.path().join("failing-sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&baseline_sessions).unwrap();
    fs::create_dir_all(&failing_sessions).unwrap();
    let baseline_id = "019fa000-0000-7000-8000-000000000031";
    write_session(
        &baseline_sessions,
        baseline_id,
        &[message("user", "visible baseline sentinel")],
    );
    ingest_codex_source_backed_v0(&baseline_sessions, &index).unwrap();
    let before = VerifiedIndex::open(&index).unwrap();
    let before_generation = before.generation_id().to_owned();
    let before_sources = before.manifest().sources.clone();
    let before_events = search_event_ids(&before, "visible baseline sentinel");

    for (native_session_id, sentinel) in [
        (
            "019fa000-0000-7000-8000-000000000032",
            "uncommittedfailuremarker one",
        ),
        (
            "019fa000-0000-7000-8000-000000000033",
            "uncommittedfailuremarker two",
        ),
    ] {
        write_session(
            &failing_sessions,
            native_session_id,
            &[message("assistant", sentinel)],
        );
    }
    let error = ingest_codex_source_backed_inner_v0(
        &failing_sessions,
        &index,
        ColdParallelOptionsV0 {
            scanner_workers: Some(2),
            fail_source_index: Some(0),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CodexSourceBackedErrorV0::InjectedColdWorkerFailure { source_index: 0 }
    ));

    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.generation_id(), before_generation);
    assert_eq!(after.manifest().sources, before_sources);
    assert_eq!(after.document_count(), 1);
    assert_eq!(
        search_event_ids(&after, "visible baseline sentinel"),
        before_events
    );
    assert!(search_event_ids(&after, "uncommittedfailuremarker").is_empty());
}

#[test]
fn source_backed_cold_append_and_replay_keep_cumulative_counts() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000001";
    let session_path = sessions.join(format!("rollout-{native_session_id}.jsonl"));
    let cold_bytes = format!(
        "{}\n{}\n",
        session_meta(native_session_id),
        message("user", "cold sentinel")
    )
    .into_bytes();
    fs::write(&session_path, &cold_bytes).unwrap();

    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_no_legacy_operations(cold.counters);
    assert_eq!(cold.counters.scanner_workers, 1);
    assert_eq!(cold.counters.cold_sources, 1);
    assert_eq!(cold.counters.staged_documents, 1);
    assert_eq!(cold.commit.indexed_documents, 1);
    let cold_index = VerifiedIndex::open(&index).unwrap();
    assert_eq!(cold_index.document_count(), 1);
    let session_id = codex_session_identity(
        &codex_source_key(native_session_id).unwrap(),
        native_session_id,
    )
    .unwrap();
    let cold_events = cold_index.events_for_session(session_id.as_uuid()).unwrap();
    let cold_event_ids = cold_events
        .iter()
        .map(|event| event.event_id)
        .collect::<Vec<_>>();
    assert_eq!(cold_event_ids.len(), 1);
    let cold_counts = cold_index.manifest().sources[0].counts();
    assert_eq!(cold_counts.complete_records, 2);
    assert_eq!(cold_counts.retained_records, 1);
    assert_eq!(cold_counts.indexed_documents, 1);
    assert_eq!(cold_counts.certified_bytes, cold_bytes.len() as u64);

    let append_offset = cold_bytes.len() as u64;
    let appended_bytes = format!("{}\n", message("assistant", "append sentinel")).into_bytes();
    OpenOptions::new()
        .append(true)
        .open(&session_path)
        .unwrap()
        .write_all(&appended_bytes)
        .unwrap();

    let append = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_no_legacy_operations(append.counters);
    assert_eq!(append.counters.scanner_workers, 1);
    assert_eq!(append.counters.appended_sources, 1);
    assert_eq!(append.counters.staged_documents, 1);
    assert_eq!(append.counters.complete_records_scanned, 1);
    assert_eq!(append.commit.indexed_documents, 2);
    let appended_index = VerifiedIndex::open(&index).unwrap();
    assert_eq!(appended_index.document_count(), 2);
    let appended_events = appended_index
        .events_for_session(session_id.as_uuid())
        .unwrap();
    assert_eq!(
        appended_events
            .iter()
            .map(|event| event.event_id)
            .take(cold_event_ids.len())
            .collect::<Vec<_>>(),
        cold_event_ids
    );
    let appended_event_ids = appended_events
        .iter()
        .map(|event| event.event_id)
        .collect::<Vec<_>>();
    assert_eq!(appended_event_ids.len(), 2);
    let appended_counts = appended_index.manifest().sources[0].counts();
    assert_eq!(appended_counts.complete_records, 3);
    assert_eq!(appended_counts.retained_records, 2);
    assert_eq!(appended_counts.indexed_documents, 2);
    assert_eq!(
        appended_counts.certified_bytes,
        append_offset + appended_bytes.len() as u64
    );

    let replay = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_no_legacy_operations(replay.counters);
    assert_eq!(replay.counters.scanner_workers, 0);
    assert_eq!(replay.counters.replayed_sources, 1);
    assert_eq!(replay.counters.staged_documents, 0);
    assert_eq!(replay.counters.complete_records_scanned, 0);
    assert_eq!(replay.counters.retained_records_scanned, 0);
    assert_eq!(replay.counters.scanner_bytes_read, 0);
    assert_eq!(replay.counters.checkpoint_validation_bytes, 0);
    assert_eq!(replay.counters.structural_json_parses, 0);
    assert_eq!(replay.counters.typed_json_parses, 0);
    assert_eq!(replay.timings.scan_and_stage, Duration::ZERO);
    assert_eq!(replay.commit.indexed_documents, 2);
    assert_eq!(replay.commit.generation_id, append.commit.generation_id);
    let replayed_index = VerifiedIndex::open(&index).unwrap();
    assert_eq!(replayed_index.document_count(), 2);
    assert_eq!(
        replayed_index
            .events_for_session(session_id.as_uuid())
            .unwrap()
            .into_iter()
            .map(|event| event.event_id)
            .collect::<Vec<_>>(),
        appended_event_ids
    );
    assert_eq!(
        replayed_index.manifest().sources[0].counts(),
        appended_counts
    );

    let source = codex_source_key(native_session_id).unwrap();
    let locator = SourceRecordLocator::new(
        source,
        NativeRecordCoordinate::Jsonl {
            byte_offset: append_offset,
            byte_length: appended_bytes.len() as u64,
            physical_ordinal: 2,
            native_session_key: Some(TypedKey::utf8(native_session_id).unwrap()),
            native_event_key: Some(TypedKey::U64(2)),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        None,
        Sha256::digest(&appended_bytes).into(),
    )
    .unwrap();
    let hydrated = hydrate_codex_locator(&sessions, &locator).unwrap();
    assert_eq!(hydrated.provider_bytes, appended_bytes);
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some("append sentinel")
    );
}

#[test]
fn active_source_family_contract_codex_rewrite_with_failed_append_proof_replaces_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000041";
    write_session(
        &sessions,
        native_session_id,
        &[message("user", "rewriteoldmarker")],
    );
    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let before = VerifiedIndex::open(&index).unwrap();
    let before_events = before
        .events_for_session(
            codex_session_identity(
                &codex_source_key(native_session_id).unwrap(),
                native_session_id,
            )
            .unwrap()
            .as_uuid(),
        )
        .unwrap();

    write_session(
        &sessions,
        native_session_id,
        &[
            message("assistant", "rewritereplacementmarker"),
            message("user", "rewrite longer tail sentinel"),
        ],
    );
    let replacement = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replacement.counters.appended_sources, 0);
    assert_eq!(replacement.counters.replaced_sources, 1);
    assert_eq!(replacement.counters.staged_documents, 2);
    assert_ne!(replacement.commit.generation_id, cold.commit.generation_id);

    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.document_count(), 2);
    assert!(search_event_ids(&after, "rewriteoldmarker").is_empty());
    assert_eq!(
        search_event_ids(&after, "rewritereplacementmarker").len(),
        1
    );
    let after_events = after
        .events_for_session(
            codex_session_identity(
                &codex_source_key(native_session_id).unwrap(),
                native_session_id,
            )
            .unwrap()
            .as_uuid(),
        )
        .unwrap();
    assert_eq!(after_events[0].event_id, before_events[0].event_id);
}

#[test]
fn active_source_family_contract_codex_truncation_replaces_the_source_without_stale_documents() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000042";
    write_session(
        &sessions,
        native_session_id,
        &[
            message("user", "truncation retained sentinel"),
            message("assistant", "truncationremovedmarker"),
        ],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    write_session(
        &sessions,
        native_session_id,
        &[message("user", "truncation retained sentinel")],
    );
    let replacement = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replacement.counters.appended_sources, 0);
    assert_eq!(replacement.counters.replaced_sources, 1);
    assert_eq!(replacement.counters.staged_documents, 1);

    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.document_count(), 1);
    assert_eq!(
        search_event_ids(&after, "truncation retained sentinel").len(),
        1
    );
    assert!(search_event_ids(&after, "truncationremovedmarker").is_empty());
}

#[test]
fn source_backed_native_session_replacement_is_one_atomic_generation() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let previous_id = "019fa000-0000-7000-8000-000000000047";
    let replacement_id = "019fa000-0000-7000-8000-000000000048";
    write_session(
        &sessions,
        previous_id,
        &[message("user", "nativeownerbeforemarker")],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();

    fs::write(
        session_path(&sessions, previous_id),
        format!(
            "{}\n{}\n",
            session_meta(replacement_id),
            message("assistant", "native owner after replacement")
        ),
    )
    .unwrap();
    let replacement = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(replacement.counters.cold_sources, 1);
    assert_eq!(replacement.counters.deleted_sources, 1);
    assert_eq!(replacement.commit.certified_sources, 1);

    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.document_count(), 1);
    assert!(search_event_ids(&after, "nativeownerbeforemarker").is_empty());
    assert_eq!(
        search_event_ids(&after, "native owner after replacement").len(),
        1
    );
    assert_eq!(
        after.manifest().sources[0].observation().source(),
        &codex_source_key(replacement_id).unwrap()
    );
}

#[test]
fn source_backed_complete_inventory_certifies_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000043";
    write_session(
        &sessions,
        native_session_id,
        &[message("user", "certified deletion sentinel")],
    );
    let cold = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    fs::remove_file(session_path(&sessions, native_session_id)).unwrap();

    let deletion = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(deletion.counters.deleted_sources, 1);
    assert_ne!(deletion.commit.generation_id, cold.commit.generation_id);
    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.document_count(), 0);
    assert!(after.manifest().sources.is_empty());
    assert!(search_event_ids(&after, "certified deletion sentinel").is_empty());
}

#[test]
fn source_backed_unavailable_root_preserves_the_prior_generation() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let unavailable = temp.path().join("sessions-unavailable");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000044";
    write_session(
        &sessions,
        native_session_id,
        &[message("user", "unavailable root sentinel")],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let before = VerifiedIndex::open(&index).unwrap();
    let before_generation = before.generation_id().to_owned();
    fs::rename(&sessions, &unavailable).unwrap();

    assert!(ingest_codex_source_backed_v0(&sessions, &index).is_err());
    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.generation_id(), before_generation);
    assert_eq!(after.document_count(), 1);
    assert_eq!(
        search_event_ids(&after, "unavailable root sentinel").len(),
        1
    );
}

#[test]
fn source_backed_incomplete_inventory_preserves_the_prior_generation() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000049";
    write_session(
        &sessions,
        native_session_id,
        &[message("user", "incomplete inventory baseline")],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let before = VerifiedIndex::open(&index).unwrap();
    let before_generation = before.generation_id().to_owned();
    fs::write(
        sessions.join("duplicate-native-session.jsonl"),
        format!(
            "{}\n{}\n",
            session_meta(native_session_id),
            message("assistant", "ambiguousduplicatemarker")
        ),
    )
    .unwrap();

    let error = ingest_codex_source_backed_v0(&sessions, &index).unwrap_err();
    assert!(matches!(
        error,
        CodexSourceBackedErrorV0::DuplicateNativeSessionId(id)
            if id == native_session_id
    ));
    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.generation_id(), before_generation);
    assert_eq!(after.document_count(), 1);
    assert_eq!(
        search_event_ids(&after, "incomplete inventory baseline").len(),
        1
    );
    assert!(search_event_ids(&after, "ambiguousduplicatemarker").is_empty());
}

#[test]
fn source_backed_final_inventory_revalidation_blocks_partial_publication() {
    fn insert_source(session_root: &Path) {
        write_session(
            session_root,
            "019fa000-0000-7000-8000-000000000046",
            &[message("assistant", "lateinventorymarker")],
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let baseline_id = "019fa000-0000-7000-8000-000000000045";
    write_session(
        &sessions,
        baseline_id,
        &[message("user", "inventory baseline sentinel")],
    );
    ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    let before = VerifiedIndex::open(&index).unwrap();
    let before_generation = before.generation_id().to_owned();

    let error = ingest_codex_source_backed_inner_v0(
        &sessions,
        &index,
        ColdParallelOptionsV0 {
            before_commit_revalidation: Some(insert_source),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CodexSourceBackedErrorV0::Index(IndexError::SourceInvalidated(_))
    ));

    let after = VerifiedIndex::open(&index).unwrap();
    assert_eq!(after.generation_id(), before_generation);
    assert_eq!(after.document_count(), 1);
    assert_eq!(
        search_event_ids(&after, "inventory baseline sentinel").len(),
        1
    );
    assert!(search_event_ids(&after, "lateinventorymarker").is_empty());
}

#[test]
fn active_source_family_contract_codex_append_publishes_then_catches_up() {
    fn append_after_scan(session_root: &Path) {
        let path = fs::read_dir(session_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
            .unwrap();
        OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(format!("{}\n", message("assistant", "postcommitappendmarker")).as_bytes())
            .unwrap();
    }

    let temp = tempfile::tempdir().unwrap();
    let sessions = temp.path().join("sessions");
    let index = temp.path().join("global-index");
    fs::create_dir_all(&sessions).unwrap();
    let native_session_id = "019fa000-0000-7000-8000-000000000047";
    write_session(
        &sessions,
        native_session_id,
        &[message("user", "frozen generation sentinel")],
    );

    let frozen = ingest_codex_source_backed_inner_v0(
        &sessions,
        &index,
        ColdParallelOptionsV0 {
            before_commit_revalidation: Some(append_after_scan),
            ..ColdParallelOptionsV0::default()
        },
    )
    .unwrap();
    assert_eq!(frozen.commit.indexed_documents, 1);
    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        search_event_ids(&verified, "frozen generation sentinel").len(),
        1
    );
    assert!(search_event_ids(&verified, "postcommitappendmarker").is_empty());

    let appended = ingest_codex_source_backed_v0(&sessions, &index).unwrap();
    assert_eq!(appended.counters.appended_sources, 1);
    let verified = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        search_event_ids(&verified, "postcommitappendmarker").len(),
        1
    );
}
