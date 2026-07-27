use super::*;

#[test]
fn production_import_requests_batch_five_only_after_group_publication() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let source_conn = Connection::open(&source_path).unwrap();
    create_tables(&source_conn);
    insert_checkpoint(&source_conn, "thread-a", "checkpoint-a");
    let large_text = "x".repeat(CAPTURE_BATCH_MAX_PAYLOAD_BYTES / 2 + 64 * 1024);
    for index in 0..257_i64 {
        let message_id = if index == 64 {
            "message-id-0".to_owned()
        } else {
            format!("message-id-{index}")
        };
        let text = if matches!(index, 192 | 193) {
            large_text.clone()
        } else {
            format!("message-{index}")
        };
        insert_write(
            &source_conn,
            "thread-a",
            "checkpoint-a",
            "task-a",
            index,
            &message_blob(vec![message_value("human", &text, &message_id)]),
        );
    }
    let group_five_boundary_rowid = source_conn
        .query_row("select rowid from writes where idx = 193", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    drop(source_conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    DEEPAGENTS_IMPORT_TRACE.with(|trace| {
        *trace.borrow_mut() = Some(Vec::new());
    });
    let summary = import_deepagents_sqlite_batched(
        &source_path,
        &mut store,
        context(Some(source_path.clone())),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(
        summary.imported_events, 256,
        "batch two must see the first batch's committed message identity",
    );
    let trace = DEEPAGENTS_IMPORT_TRACE
        .with(|trace| trace.borrow_mut().take())
        .unwrap();

    let lifecycle = trace
        .iter()
        .filter(|event| {
            !matches!(
                event,
                DeepAgentsImportTraceEvent::WriteKeyHydrated(_)
                    | DeepAgentsImportTraceEvent::WriteHydrated(_)
                    | DeepAgentsImportTraceEvent::CheckpointMetadataPreflightQueried(_)
                    | DeepAgentsImportTraceEvent::CheckpointMetadataHydrated(_)
                    | DeepAgentsImportTraceEvent::ThreadMetadataHydrated(_)
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle,
        vec![
            DeepAgentsImportTraceEvent::BatchRequested(1),
            DeepAgentsImportTraceEvent::BatchRequested(2),
            DeepAgentsImportTraceEvent::BatchRequested(3),
            DeepAgentsImportTraceEvent::BatchRequested(4),
            DeepAgentsImportTraceEvent::GroupPublished(4),
            DeepAgentsImportTraceEvent::BatchRequested(5),
            DeepAgentsImportTraceEvent::GroupPublished(1),
            DeepAgentsImportTraceEvent::SourceExhausted,
        ],
    );
    assert_eq!(
        lifecycle
            .iter()
            .filter(|event| matches!(event, DeepAgentsImportTraceEvent::BatchRequested(_)))
            .count(),
        5,
        "the exact source-exhausted tag must eliminate a sixth terminal poll",
    );
    let hydrated = trace
        .iter()
        .filter_map(|event| match event {
            DeepAgentsImportTraceEvent::WriteHydrated(rowid) => Some(*rowid),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(hydrated.len(), 257);
    assert_eq!(hydrated.iter().copied().collect::<BTreeSet<_>>().len(), 257);
    assert_eq!(
        hydrated
            .iter()
            .filter(|rowid| **rowid == group_five_boundary_rowid)
            .count(),
        1,
        "the hydrated group-five lookahead must survive group-four publication",
    );
    let boundary_hydrated = trace
        .iter()
        .position(|event| {
            *event == DeepAgentsImportTraceEvent::WriteHydrated(group_five_boundary_rowid)
        })
        .unwrap();
    let group_four_published = trace
        .iter()
        .position(|event| *event == DeepAgentsImportTraceEvent::GroupPublished(4))
        .unwrap();
    let batch_five_requested = trace
        .iter()
        .position(|event| *event == DeepAgentsImportTraceEvent::BatchRequested(5))
        .unwrap();
    assert!(boundary_hydrated < group_four_published);
    assert!(group_four_published < batch_five_requested);
}

#[test]
fn terminal_session_insert_is_independent_of_the_raw_batch_boundary() {
    let import = |write_count: usize| {
        let directory = crate::test_support_paths::tempdir().unwrap();
        let source_path = directory.path().join("sessions.db");
        let source_conn = Connection::open(&source_path).unwrap();
        create_tables(&source_conn);
        insert_checkpoint(&source_conn, "thread-a", "checkpoint-a");
        for index in 0..write_count {
            insert_write(
                &source_conn,
                "thread-a",
                "checkpoint-a",
                "task-a",
                i64::try_from(index).unwrap(),
                &message_blob(vec![message_value(
                    "human",
                    &format!("message-{index}"),
                    &format!("message-id-{index}"),
                )]),
            );
        }
        drop(source_conn);

        let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
        let summary = import_deepagents_sqlite_batched(
            &source_path,
            &mut store,
            context(Some(source_path.clone())),
            NormalizedProviderImportOptions::default(),
        )
        .unwrap();
        let session = store
            .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
            .unwrap()
            .unwrap();
        let source = store
            .get_capture_source(session.capture_source_id.unwrap())
            .unwrap();
        let cursor = source.sync.metadata["cursor"]["after"]["cursor"]
            .as_str()
            .unwrap()
            .to_owned();
        (summary, cursor)
    };

    let (same_batch, same_batch_cursor) = import(CAPTURE_BATCH_MAX_RECORDS - 1);
    let (next_batch, next_batch_cursor) = import(CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(same_batch.imported_sessions, 1);
    assert_eq!(next_batch.imported_sessions, 1);
    assert_eq!(same_batch.skipped_sessions, 0);
    assert_eq!(next_batch.skipped_sessions, 0);
    assert_eq!(same_batch_cursor, next_batch_cursor);
    assert_eq!(same_batch_cursor, "thread:thread-a:checkpoint:checkpoint-a");
}

#[test]
fn terminal_updates_replacement_with_only_duplicate_noise_and_rejection() {
    let directory = crate::test_support_paths::tempdir().unwrap();
    let source_path = directory.path().join("sessions.db");
    let source_conn = Connection::open(&source_path).unwrap();
    create_tables(&source_conn);
    insert_checkpoint(&source_conn, "thread-a", "checkpoint-a");
    insert_write(
        &source_conn,
        "thread-a",
        "checkpoint-a",
        "task-a",
        0,
        &message_blob(vec![message_value("human", "eventful", "event-a")]),
    );
    drop(source_conn);

    let mut store = Store::open(directory.path().join("store.sqlite")).unwrap();
    let first = import_deepagents_sqlite_batched(
        &source_path,
        &mut store,
        context(Some(source_path.clone())),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(first.imported_events, 1);
    let session = store
        .session_by_external_session(CaptureProvider::DeepAgents, "thread-a")
        .unwrap()
        .unwrap();
    let first_source = store
        .get_capture_source(session.capture_source_id.unwrap())
        .unwrap();
    let first_revision = first_source.sync.metadata["source_metadata"]
        ["source_observation_revision"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        first_source.sync.metadata["cursor"]["after"]["cursor"],
        "thread:thread-a:checkpoint:checkpoint-a"
    );

    let source_conn = Connection::open(&source_path).unwrap();
    source_conn
        .execute_batch("delete from writes; delete from checkpoints; pragma user_version=1;")
        .unwrap();
    insert_checkpoint(&source_conn, "thread-a", "checkpoint-replacement");
    insert_write(
        &source_conn,
        "thread-a",
        "checkpoint-replacement",
        "task-a",
        0,
        &message_blob(vec![message_value(
            "human",
            "already committed stable identity",
            "event-a",
        )]),
    );
    insert_write(
        &source_conn,
        "thread-a",
        "checkpoint-replacement",
        "task-a",
        1,
        &message_blob(vec![message_value(
            "system",
            "noise-only replacement",
            "noise-a",
        )]),
    );
    insert_write(
        &source_conn,
        "thread-a",
        "checkpoint-replacement",
        "task-a",
        2,
        &[0xd9],
    );
    drop(source_conn);

    let replacement = import_deepagents_sqlite_batched(
        &source_path,
        &mut store,
        context(Some(source_path.clone())),
        NormalizedProviderImportOptions::default(),
    )
    .unwrap();
    assert_eq!(replacement.imported_events, 0);
    assert_eq!(replacement.failed, 1);
    assert_eq!(replacement.skipped_sessions, 1);
    let replaced_session = store.get_session(session.id).unwrap();
    assert_eq!(
        replaced_session.sync.metadata["metadata"]["latest_checkpoint_id"],
        "checkpoint-replacement"
    );
    let replaced_source = store
        .get_capture_source(replaced_session.capture_source_id.unwrap())
        .unwrap();
    let replaced_revision = replaced_source.sync.metadata["source_metadata"]
        ["source_observation_revision"]
        .as_str()
        .unwrap();
    assert_ne!(replaced_revision, first_revision);
    assert_eq!(
        replaced_source.sync.metadata["cursor"]["after"]["cursor"],
        "thread:thread-a:checkpoint:checkpoint-replacement"
    );
    assert_eq!(store.events_for_session(session.id).unwrap().len(), 1);
}
