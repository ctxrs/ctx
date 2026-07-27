use super::super::position::decode_nanoclaw_position;
use super::super::project::{
    nanoclaw_inventory_scans, nanoclaw_set_before_commit_revalidation_hook,
};
use super::*;
use crate::captured_batch::CAPTURE_BATCH_MAX_BATCHES_PER_GROUP;

fn import_options() -> NormalizedProviderImportOptions {
    NormalizedProviderImportOptions {
        history_record_id: None,
        persist_cursors: true,
        wrap_transaction: true,
        fast_event_inserts: true,
        capture_work_limit: crate::CaptureWorkLimit::Drain,
        inventory_observation_token: None,
    }
}

fn nanoclaw_cursor_stream(root: &Path) -> String {
    let canonical_root = fs::canonicalize(root).unwrap();
    let cursor_path = provider_path_identity(&canonical_root).unwrap();
    provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &cursor_path,
    )
}

fn assert_one_nanoclaw_hit(store: &Store, marker: &str) {
    let hits = store
        .search_event_hits(marker, 10)
        .unwrap()
        .into_iter()
        .filter(|hit| hit.provider == Some(CaptureProvider::NanoClaw))
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1, "expected one NanoClaw hit for {marker}");
}

fn nanoclaw_event_count(store: &Store) -> usize {
    let archive = store.export_archive().unwrap();
    let source_ids = archive
        .capture_sources
        .iter()
        .filter(|source| source.descriptor.provider == CaptureProvider::NanoClaw)
        .map(|source| source.id)
        .collect::<std::collections::BTreeSet<_>>();
    archive
        .events
        .iter()
        .filter(|event| {
            event
                .capture_source_id
                .is_some_and(|source_id| source_ids.contains(&source_id))
        })
        .count()
}

#[test]
fn multi_group_import_preserves_scope_without_repeated_project_inventory() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "bounded-inventory", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    let mut messages = Connection::open(&inbound).unwrap();
    let transaction = messages.transaction().unwrap();
    for index in 0..(CAPTURE_BATCH_MAX_RECORDS * CAPTURE_BATCH_MAX_BATCHES_PER_GROUP + 1) {
        transaction
            .execute(
                "insert into messages_in values (
                    ?1, ?2, 'chat', ?3, 'done', 'message', 'chat-1', 'telegram',
                    'thread', 'bounded inventory', null, 0
                )",
                rusqlite::params![
                    format!("message-{index:04}"),
                    i64::try_from(index).unwrap(),
                    i64::try_from(30_000 + index).unwrap(),
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(messages);

    let logical_source_root = temp.path().join("logical-inventory-root");
    let mut import_context = context(&logical_source_root);
    import_context.source_path = Some(logical_source_root.clone());
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
    let scans_before = nanoclaw_inventory_scans();
    let summary =
        import_nanoclaw_project_batched(&root, &mut store, import_context, import_options())
            .unwrap();

    assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    assert_eq!(
        nanoclaw_inventory_scans() - scans_before,
        1,
        "project inventory should be frozen once and reused by every commit revalidation",
    );
    let logical_source_root = logical_source_root.display().to_string();
    let archive = store.export_archive().unwrap();
    let nanoclaw_sources = archive
        .capture_sources
        .iter()
        .filter(|source| source.descriptor.provider == CaptureProvider::NanoClaw)
        .collect::<Vec<_>>();
    assert!(!nanoclaw_sources.is_empty());
    assert!(nanoclaw_sources.iter().all(|source| {
        source.descriptor.raw_source_path.as_deref() == Some(logical_source_root.as_str())
            && source.descriptor.source_root.as_deref() == Some(logical_source_root.as_str())
    }));
}

#[derive(Clone, Copy)]
enum CommitBoundaryChange {
    Mutate,
    Add,
    Remove,
}

#[test]
fn constituent_changes_before_commit_replay_without_gaps_or_duplicates() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    for (name, change) in [
        ("commit-mutate", CommitBoundaryChange::Mutate),
        ("commit-add", CommitBoundaryChange::Add),
        ("commit-remove", CommitBoundaryChange::Remove),
    ] {
        let root = create_project(&temp, name, 1);
        let (inbound, outbound) = create_message_stores(&root, "session-0000");
        let initial_marker = format!("{name}-initial-marker");
        insert_inbound(&inbound, "in-1", 1, 1_000, &initial_marker);
        match change {
            CommitBoundaryChange::Mutate => {}
            CommitBoundaryChange::Add => fs::remove_file(&outbound).unwrap(),
            CommitBoundaryChange::Remove => {
                insert_outbound(&outbound, "out-1", 2, 2_000, "removed-boundary-marker");
            }
        }

        let stream = nanoclaw_cursor_stream(&root);
        let mut store = Store::open(temp.path().join(format!("{name}.sqlite"))).unwrap();
        let inbound_for_hook = inbound.clone();
        let outbound_for_hook = outbound.clone();
        let error = {
            let _hook = nanoclaw_set_before_commit_revalidation_hook(move |ordinal| {
                if ordinal != 1 {
                    return;
                }
                match change {
                    CommitBoundaryChange::Mutate => {
                        Connection::open(&inbound_for_hook)
                            .unwrap()
                            .execute(
                                "update messages_in set content = 'mutated-boundary-marker' where id = 'in-1'",
                                [],
                            )
                            .unwrap();
                    }
                    CommitBoundaryChange::Add => {
                        Connection::open(&outbound_for_hook)
                            .unwrap()
                            .execute_batch(
                                "create table messages_out (
                                    id text primary key, seq integer, in_reply_to text,
                                    timestamp integer, kind text, platform_id text,
                                    channel_type text, thread_id text, content text
                                );
                                insert into messages_out values (
                                    'out-1', 2, null, 2000, 'chat', 'chat-1',
                                    'telegram', 'thread', 'added-boundary-marker'
                                );",
                            )
                            .unwrap();
                    }
                    CommitBoundaryChange::Remove => {
                        fs::remove_file(&outbound_for_hook).unwrap();
                    }
                }
            });
            import_nanoclaw_project_batched(&root, &mut store, context(&root), import_options())
                .expect_err("constituent change must fail before cursor publication")
        };
        assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
        assert!(store
            .get_sync_cursor(None, "machine-nanoclaw-test", &stream)
            .unwrap()
            .is_none());

        let replay =
            import_nanoclaw_project_batched(&root, &mut store, context(&root), import_options())
                .unwrap();
        assert_eq!(replay.failed, 0, "{:?}", replay.failures);
        assert!(store
            .get_sync_cursor(None, "machine-nanoclaw-test", &stream)
            .unwrap()
            .is_some());

        match change {
            CommitBoundaryChange::Mutate => {
                // The shared importer deliberately treats a replayed provider event identity as
                // already accepted; snapshot consolidation must not change that replacement
                // policy. The first bounded write remains singular and the cursor is repaired.
                assert_one_nanoclaw_hit(&store, &initial_marker);
                assert!(store
                    .search_event_hits("mutated-boundary-marker", 10)
                    .unwrap()
                    .is_empty());
                assert_eq!(nanoclaw_event_count(&store), 1);
            }
            CommitBoundaryChange::Add => {
                assert_one_nanoclaw_hit(&store, &initial_marker);
                assert_one_nanoclaw_hit(&store, "added-boundary-marker");
                assert_eq!(nanoclaw_event_count(&store), 2);
            }
            CommitBoundaryChange::Remove => {
                assert_one_nanoclaw_hit(&store, &initial_marker);
                assert_one_nanoclaw_hit(&store, "removed-boundary-marker");
                assert_eq!(nanoclaw_event_count(&store), 2);
            }
        }
    }
}

#[test]
fn crash_replay_from_last_committed_group_is_complete_and_idempotent() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "committed-group-replay", 1);
    let (inbound, _) = create_message_stores(&root, "session-0000");
    let initial_messages = CAPTURE_BATCH_MAX_RECORDS * CAPTURE_BATCH_MAX_BATCHES_PER_GROUP + 1;
    let mut messages = Connection::open(&inbound).unwrap();
    let transaction = messages.transaction().unwrap();
    for index in 0..initial_messages {
        transaction
            .execute(
                "insert into messages_in values (
                    ?1, ?2, 'chat', ?3, 'done', 'message', 'chat-1', 'telegram',
                    'thread', ?4, null, 0
                )",
                rusqlite::params![
                    format!("message-{index:04}"),
                    i64::try_from(index).unwrap(),
                    i64::try_from(30_000 + index).unwrap(),
                    format!("replay-marker-{index:04}"),
                ],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    drop(messages);

    let stream = nanoclaw_cursor_stream(&root);
    let mut store = Store::open(temp.path().join("committed-group-replay.sqlite")).unwrap();
    let inbound_for_hook = inbound.clone();
    let error = {
        let _hook = nanoclaw_set_before_commit_revalidation_hook(move |ordinal| {
            if ordinal == 2 {
                insert_inbound(
                    &inbound_for_hook,
                    "message-tail",
                    i64::try_from(initial_messages).unwrap(),
                    99_000,
                    "replay-tail-marker",
                );
            }
        });
        import_nanoclaw_project_batched(&root, &mut store, context(&root), import_options())
            .expect_err("second group mutation must reject its cursor")
    };
    assert!(matches!(error, CaptureError::SourceChangedDuringCapture));
    let committed = store
        .get_sync_cursor(None, "machine-nanoclaw-test", &stream)
        .unwrap()
        .expect("the first bounded group cursor must remain committed");
    let certified = CertifiedProviderCursor::decode_if_certified(&committed.cursor)
        .unwrap()
        .unwrap();
    assert_eq!(
        decode_nanoclaw_position(certified.native_position())
            .unwrap()
            .unwrap()
            .next_ordinal,
        u64::try_from(CAPTURE_BATCH_MAX_RECORDS * CAPTURE_BATCH_MAX_BATCHES_PER_GROUP).unwrap()
    );

    let replay =
        import_nanoclaw_project_batched(&root, &mut store, context(&root), import_options())
            .unwrap();
    assert_eq!(replay.failed, 0, "{:?}", replay.failures);
    assert_eq!(nanoclaw_event_count(&store), initial_messages + 1);
    assert_one_nanoclaw_hit(&store, "replay-marker-0000");
    assert_one_nanoclaw_hit(&store, "replay-tail-marker");

    let idempotent =
        import_nanoclaw_project_batched(&root, &mut store, context(&root), import_options())
            .unwrap();
    assert_eq!(idempotent.failed, 0, "{:?}", idempotent.failures);
    assert_eq!(nanoclaw_event_count(&store), initial_messages + 1);
}

#[test]
fn projects_keep_distinct_source_identities_and_cursors() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let first = create_project(&temp, "first-project", 1);
    let second = create_project(&temp, "second-project", 1);
    let (first_inbound, _) = create_message_stores(&first, "session-0000");
    let (second_inbound, _) = create_message_stores(&second, "session-0000");
    insert_inbound(
        &first_inbound,
        "shared-id",
        1,
        1_000,
        "first-project-marker",
    );
    insert_inbound(
        &second_inbound,
        "shared-id",
        1,
        1_000,
        "second-project-marker",
    );
    let first_stream = nanoclaw_cursor_stream(&first);
    let second_stream = nanoclaw_cursor_stream(&second);
    assert_ne!(first_stream, second_stream);

    let mut store = Store::open(temp.path().join("separate-projects.sqlite")).unwrap();
    for root in [&first, &second] {
        let summary =
            import_nanoclaw_project_batched(root, &mut store, context(root), import_options())
                .unwrap();
        assert_eq!(summary.failed, 0, "{:?}", summary.failures);
    }
    assert!(store
        .get_sync_cursor(None, "machine-nanoclaw-test", &first_stream)
        .unwrap()
        .is_some());
    assert!(store
        .get_sync_cursor(None, "machine-nanoclaw-test", &second_stream)
        .unwrap()
        .is_some());
    let mut identities = store
        .list_capture_sources()
        .unwrap()
        .into_iter()
        .filter(|source| source.descriptor.provider == CaptureProvider::NanoClaw)
        .filter_map(|source| source.descriptor.source_identity)
        .collect::<Vec<_>>();
    identities.sort();
    identities.dedup();
    assert_eq!(identities.len(), 2);
    assert_eq!(nanoclaw_event_count(&store), 2);
    assert_one_nanoclaw_hit(&store, "first-project-marker");
    assert_one_nanoclaw_hit(&store, "second-project-marker");
}
