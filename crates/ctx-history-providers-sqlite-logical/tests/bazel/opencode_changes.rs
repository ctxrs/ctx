use super::*;

#[test]
fn opencode_middle_session_refresh_survives_restart_wal_reset_and_old_changes() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let database = temp.path().join("source/opencode.db");
    let writer = create_opencode_wal_database(&database, "prefixmarker");
    for (session, message, part, marker) in [
        (
            "session-0",
            "outer-message-0",
            "outer-part-0",
            "beforemarker",
        ),
        (
            "session-2",
            "outer-message-2",
            "outer-part-2",
            "aftermarker",
        ),
    ] {
        writer
            .execute(
                "insert into session select ?1,parent_id,directory,branch,agent,time_created,time_updated
                 from session where id='session-1'",
                [session],
            )
            .unwrap();
        writer
            .execute(
                "insert into message values (?1,?2,1,1,'{\"role\":\"user\"}')",
                params![message, session],
            )
            .unwrap();
        writer
            .execute(
                "insert into part values (?1,?2,?3,1,1,?4)",
                params![
                    part,
                    message,
                    session,
                    json!({"type":"text","text":marker}).to_string()
                ],
            )
            .unwrap();
    }
    let index_root = temp.path().join("index");
    // Every refresh builds a new adapter/executor and reloads the durable base.
    let refresh = |expected| {
        let source_contents = || {
            ["opencode.db", "opencode.db-wal"]
                .map(|name| fs::read(database.with_file_name(name)).unwrap())
        };
        let before = source_contents();
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route_with_data_root(
            &mut registry,
            provider_source_for_path(CaptureProvider::OpenCode, database.clone()),
            SourceBackedRouteSelection::Automatic,
            &data_root,
        )
        .unwrap();
        let report = SourceBackedRefreshExecutor::new(registry, WriterOptions::default())
            .refresh_scope_with_detailed_progress(
                &index_root,
                SourceBackedRefreshScope::All,
                |_| Ok(()),
            )
            .unwrap();
        assert_eq!(report.successful_route_outcomes.len(), 1);
        assert_eq!(report.sources.len(), 1);
        let counts = report.sources[0].counts();
        assert_eq!(counts.complete_records, expected);
        assert_eq!(counts.retained_records, expected);
        assert_eq!(counts.indexed_documents, expected);
        assert_eq!(counts.rejected_records, 0);
        assert_eq!(counts.ignored_records, 0);
        for (name, (after, before)) in ["DB", "WAL"]
            .into_iter()
            .zip(source_contents().into_iter().zip(before))
        {
            assert!(after == before, "refresh must not modify source {name}");
        }
        report
    };
    let cold = refresh(3);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let original = only_matching_event(&index, "prefixmarker");
    let outer = ["beforemarker", "aftermarker"].map(|marker| only_matching_event(&index, marker));
    drop(index);
    let assert_outer_sessions = || {
        let index = VerifiedIndex::open_pinned(&index_root).unwrap();
        for (marker, expected) in ["beforemarker", "aftermarker"].into_iter().zip(&outer) {
            assert_eq!(&only_matching_event(&index, marker), expected);
        }
    };
    assert_eq!(refresh(3).commit.generation_id, cold.commit.generation_id);

    append_opencode_message(&writer, "suffixmarker");
    let appended = refresh(4);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(only_matching_event(&index, "prefixmarker"), original);
    let suffix = only_matching_event(&index, "suffixmarker");
    assert_ne!(suffix.event_id, original.event_id);
    assert_eq!(suffix.session_id, original.session_id);
    assert_eq!(suffix.event_sequence, 1);
    drop(index);
    assert_outer_sessions();
    assert_eq!(
        refresh(4).commit.generation_id,
        appended.commit.generation_id
    );

    writer
        .execute_batch("pragma wal_checkpoint(truncate)")
        .unwrap();
    writer
        .execute(
            "insert into part values ('part-3','message-2','session-1',3,3,?1)",
            [json!({"type":"text","text":"resetmarker"}).to_string()],
        )
        .unwrap();
    refresh(5);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert_eq!(only_matching_event(&index, "suffixmarker"), suffix);
    assert_eq!(only_matching_event(&index, "resetmarker").event_sequence, 2);
    drop(index);
    assert_outer_sessions();

    let edit =
        "update part set data='{\"type\":\"text\",\"text\":\"editedprefix\"}' where id='part-1'";
    writer.execute_batch("begin immediate").unwrap();
    writer.execute_batch(edit).unwrap();
    writer.cache_flush().unwrap();
    refresh(5);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(matching_events(&index, "editedprefix").is_empty());
    assert_eq!(only_matching_event(&index, "prefixmarker"), original);
    drop(index);
    writer.execute_batch("rollback").unwrap();
    refresh(5);
    writer.execute_batch(edit).unwrap();
    refresh(5);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(matching_events(&index, "prefixmarker").is_empty());
    assert_eq!(only_matching_event(&index, "editedprefix"), original);
    drop(index);
    assert_outer_sessions();

    writer
        .execute("delete from part where id='part-1'", [])
        .unwrap();
    refresh(4);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    assert!(matching_events(&index, "editedprefix").is_empty());
    let mut shifted_suffix = suffix.clone();
    shifted_suffix.event_sequence = 0;
    assert_eq!(only_matching_event(&index, "suffixmarker"), shifted_suffix);
    assert_eq!(only_matching_event(&index, "resetmarker").event_sequence, 1);
    drop(index);
    assert_outer_sessions();

    writer
        .execute(
            "update session set parent_id='session-0' where id='session-1'",
            [],
        )
        .unwrap();
    refresh(4);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    for marker in ["suffixmarker", "resetmarker"] {
        assert_eq!(
            only_matching_event(&index, marker).parent_session_id,
            Some(outer[0].session_id)
        );
    }
    assert_eq!(
        only_matching_event(&index, "suffixmarker").event_id,
        suffix.event_id
    );
    drop(index);
    assert_outer_sessions();
}

#[test]
fn opencode_recertifies_old_checkpoints_and_relocation_preserves_identity() {
    use ctx_history_capture_runtime::{document_full_snapshot_frontier, DocumentLeafFingerprint};
    use ctx_history_core::CertifiedSource;

    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let original = temp.path().join("original/opencode.db");
    let writer = create_opencode_wal_database(&original, "relocationmarker");
    writer
        .pragma_update(None, "journal_mode", "delete")
        .unwrap();
    drop(writer);
    let index_root = temp.path().join("index");
    let executor = |path: &Path| {
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route_with_data_root(
            &mut registry,
            provider_source_for_path(CaptureProvider::OpenCode, path.to_path_buf()),
            SourceBackedRouteSelection::Automatic,
            &data_root,
        )
        .unwrap();
        SourceBackedRefreshExecutor::new(registry, WriterOptions::default())
    };
    let cold = executor(&original)
        .refresh_scope_with_detailed_progress(
            &index_root,
            SourceBackedRefreshScope::All,
            |_| Ok(()),
        )
        .unwrap();
    let before = only_matching_event(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        "relocationmarker",
    );
    let prior = &cold.sources[0];
    // A prior physical-token domain has the same valid 32-byte checkpoint shape.
    // The shared-source test separately freezes the exact released hash algorithm.
    let legacy = CertifiedSource::certify_with_frontier(
        prior.observation().clone(),
        prior.observation().clone(),
        prior.parser_revision(),
        *prior.content_digest(),
        prior.counts(),
        Some(
            document_full_snapshot_frontier(
                DocumentLeafFingerprint::new([0x52; 32]),
                prior.counts().certified_bytes,
                *prior.content_digest(),
            )
            .unwrap(),
        ),
    )
    .unwrap();
    let LogicalSqliteRoutePlan::OpenCodeFamily { adapter, .. } =
        logical_sqlite_route_plan_scoped::<ScopedReplayBinding>(
            provider_source_for_path(CaptureProvider::OpenCode, original.clone()),
            SourceBackedRouteSelection::Automatic,
            &data_root,
            SourceAnchorScope::Unqualified,
        )
        .unwrap()
    else {
        panic!("expected OpenCode adapter")
    };
    let mut recopied = false;
    let tree = adapter
        .discover_complete_with_progress(&[legacy], &mut |_| {
            recopied = true;
            Ok(())
        })
        .unwrap();
    assert!(
        recopied,
        "old checkpoint must fall back to current snapshot admission"
    );
    adapter.revalidate_complete(&tree).unwrap();
    drop(tree);
    let tree = adapter
        .discover_complete_with_progress(&cold.sources, &mut |_| {
            panic!("current checkpoint must retain exact no-copy replay")
        })
        .unwrap();
    adapter.revalidate_complete(&tree).unwrap();
    drop(tree);

    let moved = temp.path().join("moved/opencode.db");
    fs::create_dir(moved.parent().unwrap()).unwrap();
    fs::rename(original, &moved).unwrap();
    let LogicalSqliteRoutePlan::OpenCodeFamily { adapter, .. } =
        logical_sqlite_route_plan_scoped::<ScopedReplayBinding>(
            provider_source_for_path(CaptureProvider::OpenCode, moved.clone()),
            SourceBackedRouteSelection::Automatic,
            &data_root,
            SourceAnchorScope::Unqualified,
        )
        .unwrap()
    else {
        panic!("expected relocated OpenCode adapter")
    };
    let mut recopied = false;
    let tree = adapter
        .discover_complete_with_progress(&cold.sources, &mut |_| {
            recopied = true;
            Ok(())
        })
        .unwrap();
    assert!(recopied, "new native location must be recertified");
    adapter.revalidate_complete(&tree).unwrap();
    drop(tree);

    // Automatic discovered-winner routes retain the same owner across moves,
    // so recertification must replace the source in the existing index.
    let moved_executor = executor(&moved);
    let mut recopied = false;
    let relocated = moved_executor
        .refresh_scope_with_detailed_progress(
            &index_root,
            SourceBackedRefreshScope::All,
            |update| {
                recopied |= update.current_source_progress.is_some();
                Ok(())
            },
        )
        .unwrap();
    assert!(recopied, "new native location must be recertified");
    assert_eq!(relocated.sources.len(), 1);
    assert_eq!(
        relocated.sources[0].content_digest(),
        prior.content_digest()
    );
    assert!(relocated.sources[0]
        .observation()
        .source()
        .exact_descriptor_eq(prior.observation().source()));
    let after = only_matching_event(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        "relocationmarker",
    );
    assert_eq!(after.event_id, before.event_id);
    assert_eq!(after.session_id, before.session_id);
    let repeated = moved_executor
        .refresh_scope_with_detailed_progress(
            &index_root,
            SourceBackedRefreshScope::All,
            |update| {
                assert!(update.current_source_progress.is_none());
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(
        repeated.commit.generation_id,
        relocated.commit.generation_id
    );
}

// Authored adversarial cases, including native message/part foreign-key cascades.
#[test]
fn opencode_changed_database_preserves_transactional_edits_and_deletions() {
    for indexed in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        let database = temp.path().join("source/opencode.db");
        let writer = create_opencode_wal_database(&database, "originalmarker");
        writer.execute_batch("begin").unwrap();
        append_opencode_message(&writer, "deletedpartmarker");
        // Two full hydration batches and a tail exercise statement rebinding.
        for sequence in 3..=131 {
            writer
                .execute(
                    "insert into message values (?1, 'session-1', ?2, ?2, ?3)",
                    params![
                        format!("message-{sequence}"),
                        sequence,
                        r#"{"role":"user"}"#
                    ],
                )
                .unwrap();
            writer
                .execute(
                    "insert into part values (?1, ?2, 'session-1', ?3, ?3, ?4)",
                    params![
                        format!("part-{sequence}"),
                        format!("message-{sequence}"),
                        sequence,
                        r#"{"type":"text","text":"batchmarker"}"#
                    ],
                )
                .unwrap();
        }
        writer.execute_batch("commit").unwrap();
        if !indexed {
            writer
                .execute_batch(
                    "drop index message_session_time_created_id_idx;
                     drop index part_message_id_id_idx;",
                )
                .unwrap();
        }
        let index_root = temp.path().join("index");
        let mut registry = SourceBackedProviderRegistry::new();
        register_landed_source_backed_route_with_data_root(
            &mut registry,
            provider_source_for_path(CaptureProvider::OpenCode, database.clone()),
            SourceBackedRouteSelection::ExplicitManual,
            &data_root,
        )
        .unwrap();
        let executor = SourceBackedRefreshExecutor::new(registry, WriterOptions::default());
        let refresh = |expected| {
            let mut scanned = false;
            let report = executor
                .refresh_scope_with_detailed_progress(
                    &index_root,
                    SourceBackedRefreshScope::All,
                    |update| {
                        scanned |= update.current_source_progress.is_some();
                        Ok(())
                    },
                )
                .unwrap_or_else(|error| {
                    panic!("refresh expected={expected}, indexed={indexed}: {error:?}")
                });
            assert_eq!(report.successful_route_outcomes.len(), 1);
            assert_eq!(
                report.sources[0].counts().indexed_documents,
                expected,
                "indexed={indexed}, scanned={scanned}, changed={}",
                report.successful_route_outcomes[0].changed,
            );
            report.commit.generation_id
        };
        let has = |marker: &str| {
            !matching_events(&VerifiedIndex::open_pinned(&index_root).unwrap(), marker).is_empty()
        };
        let cold = refresh(131);
        assert_eq!(refresh(131), cold);
        assert!(has("originalmarker"));
        assert!(has("batchmarker"));

        let edit = "update part set data = '{\"type\":\"text\",\"text\":\"editedmarker\"}'
                     where id = 'part-1';
                    update message set data = '{\"role\":\"assistant\"}' where id = 'message-1';
                    delete from part where id = 'part-2';";
        let wal = database.with_file_name("opencode.db-wal");
        let committed_bytes = fs::metadata(&wal).unwrap().len();
        writer.execute_batch("begin immediate").unwrap();
        writer.execute_batch(edit).unwrap();
        writer.cache_flush().unwrap();
        assert!(fs::metadata(&wal).unwrap().len() > committed_bytes);
        refresh(131);
        assert!(has("originalmarker"));
        assert!(!has("editedmarker"));
        assert!(has("deletedpartmarker"));
        writer.execute_batch("rollback").unwrap();
        refresh(131);
        assert!(has("originalmarker"));

        writer.execute_batch("begin immediate").unwrap();
        writer.execute_batch(edit).unwrap();
        writer.execute_batch("commit").unwrap();
        assert_eq!(
            writer
                .query_row("select count(*) from part", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            130,
        );
        let edited = refresh(130);
        assert_ne!(edited, cold);
        assert_eq!(refresh(130), edited);
        assert!(!has("originalmarker"));
        assert!(!has("deletedpartmarker"));
        let event = only_matching_event(
            &VerifiedIndex::open_pinned(&index_root).unwrap(),
            "editedmarker",
        );
        assert_eq!(event.role.as_deref(), Some("assistant"));
        let times: (i64, i64) = writer
            .query_row(
                "select time_updated, time_created from part where id='part-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(
            times,
            (1, 1),
            "edit intentionally leaves both timestamps equal"
        );

        // Schema changes may advance the certificate, but preserve records and identity.
        writer
            .execute_batch("alter table part add column extra text")
            .unwrap();
        refresh(130);
        let after_schema = only_matching_event(
            &VerifiedIndex::open_pinned(&index_root).unwrap(),
            "editedmarker",
        );
        assert_eq!(after_schema.event_id, event.event_id);
        assert_eq!(after_schema.role, event.role);
        writer
            .execute("delete from message where id='message-1'", [])
            .unwrap();
        refresh(129);
        assert!(!has("editedmarker"));
        writer
            .execute("delete from session where id='session-1'", [])
            .unwrap();
        refresh(0);
        assert!(!has("batchmarker"));
        drop(writer);

        let replacement = temp.path().join("replacement/opencode.db");
        let replacement_writer = create_opencode_wal_database(&replacement, "replacementmarker");
        replacement_writer
            .pragma_update(None, "journal_mode", "delete")
            .unwrap();
        drop(replacement_writer);
        fs::rename(&replacement, &database).unwrap();
        refresh(1);
        assert!(has("replacementmarker"));
        assert!(!has("originalmarker"));
        assert!(!has("batchmarker"));
    }
}
