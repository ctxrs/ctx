use super::*;

#[test]
fn astrbot_batches_split_at_sixty_four_and_resume_exactly() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    for index in 1..=CAPTURE_BATCH_MAX_RECORDS + 1 {
        insert_conversation(
            &conn,
            i64::try_from(index).unwrap(),
            &format!("session-{index}"),
            &json!([{
                "id": format!("message-{index}"),
                "role": "user",
                "content": format!("message {index}"),
            }])
            .to_string(),
        );
    }
    let source = test_source("paging");
    let batches = produce_all(&conn, source.clone(), initial_astrbot_position().unwrap());
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert_eq!(batches[0].records()[0].ordinal(), 0);
    assert_eq!(batches[0].records()[63].ordinal(), 63);
    assert_eq!(batches[1].records().len(), 1);
    assert_eq!(batches[1].records()[0].ordinal(), 64);
    assert_eq!(batches[0].range_end(), batches[1].range_before());
    let first_end = decode_astrbot_position(batches[0].range_end())
        .unwrap()
        .unwrap();
    assert_eq!(first_end.phase, AstrBotPhase::Conversations);
    assert_eq!(first_end.next_ordinal, CAPTURE_BATCH_MAX_RECORDS as u64);

    let replay = produce_all(&conn, source, batches[0].range_end().clone());
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].records().len(), 1);
    assert_eq!(replay[0].records()[0].ordinal(), 64);
    assert_eq!(replay[0].range_end(), batches[1].range_end());
}

#[test]
fn astrbot_preflight_rejects_oversize_content_before_hydration() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let oversize = i64::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES)
        .unwrap()
        .checked_add(1)
        .unwrap();
    conn.execute(
        "insert into conversations (id, conversation_id, content, created_at) \
         values (1, 'oversize', zeroblob(?1), 1)",
        [oversize],
    )
    .unwrap();
    conn.set_limit(
        Limit::SQLITE_LIMIT_LENGTH,
        i32::try_from(CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES).unwrap(),
    );

    let batches = produce_all(
        &conn,
        test_source("oversize"),
        initial_astrbot_position().unwrap(),
    );
    assert!(matches!(
        batches[0].records()[0].payload(),
        CapturedRecordPayload::StructuralRejection {
            kind: StructuralRejectionKind::OversizeRecord,
            observed_bytes,
        } if *observed_bytes > CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES as u64
    ));
}

#[test]
fn astrbot_resume_queries_use_rowid_seeks_without_sorting() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    let sql = AstrBotSql::new(&conn).unwrap();

    for (table, query) in [
        ("conversations", sql.conversation_candidate_after.as_str()),
        (
            "platform_message_history",
            sql.platform_message_candidate_after.as_deref().unwrap(),
        ),
    ] {
        let plan = explain_query_plan(&conn, query, [1]);
        let plan = plan.join(" | ");
        assert!(
            plan.contains(&format!("SEARCH {table}")) && plan.contains("rowid>?"),
            "AstrBot resume query must use a rowid keyset seek: {plan}"
        );
        assert!(
            !plan.contains("USE TEMP B-TREE"),
            "AstrBot resume query must not sort the remaining table: {plan}"
        );
    }
    for (table, query) in [
        ("conversations", sql.conversation_order_at.as_str()),
        (
            "platform_message_history",
            sql.platform_message_order_at.as_deref().unwrap(),
        ),
    ] {
        let plan = explain_query_plan(&conn, query, [1]).join(" | ");
        assert!(
            plan.contains(&format!("SEARCH {table}")) && plan.contains("rowid=?"),
            "AstrBot resume order check must seek only the keyset predecessor: {plan}"
        );
        assert!(!plan.contains("SCAN"), "{plan}");
        assert!(!plan.contains("USE TEMP B-TREE"), "{plan}");
    }

    astrbot_prepare_relationship_projection(&conn, &sql).unwrap();
    let mut statement = conn
        .prepare(&format!(
            "explain query plan {ASTRBOT_RELATIONSHIP_LOOKUP_SQL}"
        ))
        .unwrap();
    let plan = statement
        .query_map(["checkpoint"], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join(" | ");
    assert_eq!(plan.matches("USING PRIMARY KEY").count(), 2, "{plan}");
    assert!(!plan.contains("USE TEMP B-TREE"));
}

#[test]
fn astrbot_alternating_high_fanout_children_hydrate_each_parent_once() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    for id in 1..=2_i64 {
        insert_conversation(
            &conn,
            id,
            &format!("session-{id}"),
            &json!([{
                "type": "_checkpoint",
                "id": format!("checkpoint-{id}"),
            }])
            .to_string(),
        );
    }
    let child_count = CAPTURE_BATCH_MAX_RECORDS * 2 + 1;
    for index in 0..child_count {
        let id = i64::try_from(index + 1).unwrap();
        let checkpoint = if index % 2 == 0 {
            "checkpoint-1"
        } else {
            "checkpoint-2"
        };
        insert_platform_message(
            &conn,
            id,
            Some(checkpoint),
            &format!("alternating platform message {id}"),
        );
    }
    let start = encode_astrbot_position(AstrBotKeyset {
        phase: AstrBotPhase::PlatformMessages,
        next_ordinal: 2,
        physical_rowid: 0,
    })
    .unwrap();

    astrbot_reset_relationship_projection_test_pacing();
    astrbot_reset_conversation_hydration_test_count();
    let batches = produce_all(&conn, test_source("alternating-fanout"), start);
    astrbot_disable_relationship_projection_test_wait_hook();
    assert_eq!(astrbot_conversation_hydration_test_count(), 2);

    let records = batches
        .iter()
        .flat_map(|batch| batch.records())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), child_count);
    for (index, record) in records.into_iter().enumerate() {
        let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
            panic!("AstrBot platform child must retain SQLite logical values");
        };
        let (message, link) = decode_astrbot_platform_message(values).unwrap();
        assert_eq!(message.id, i64::try_from(index + 1).unwrap());
        let expected_session = if index % 2 == 0 {
            "session-1"
        } else {
            "session-2"
        };
        assert_eq!(
            link.as_ref().map(|link| link.provider_session_id.as_str()),
            Some(expected_session)
        );
    }
    assert_eq!(astrbot_conversation_hydration_test_count(), 2);
}

#[test]
fn astrbot_order_validation_is_frontier_local_and_rejects_unreached_violation() {
    let conn = Connection::open_in_memory().unwrap();
    create_tables(&conn);
    for id in 1..=i64::try_from(CAPTURE_BATCH_MAX_RECORDS + 1).unwrap() {
        insert_conversation(
            &conn,
            id,
            &format!("session-{id}"),
            &format!("message {id}"),
        );
    }
    conn.execute(
        "update conversations set created_at = 0 where id = ?1",
        [i64::try_from(CAPTURE_BATCH_MAX_RECORDS + 1).unwrap()],
    )
    .unwrap();

    let sql = AstrBotSql::new(&conn).unwrap();
    let mut checkpoint = AstrBotParserCheckpoint::empty();
    checkpoint.source_shape_validated = true;
    let mut fetcher = AstrBotRowFetcher::new(&conn, sql, checkpoint).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        test_source("frontier-order"),
        initial_astrbot_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    // The out-of-order suffix is not scanned before the first bounded batch is
    // available. Its order violation is rejected when that row reaches the
    // keyset frontier on the following producer call.
    let first = producer.next_batch().unwrap().unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    let second = producer.next_batch().unwrap().unwrap();
    assert_eq!(second.range_before(), first.range_end());
    assert_eq!(second.records().len(), 1);
    let rejection = &second.records()[0];
    assert_eq!(
        rejection.record_kind().as_str(),
        ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND
    );
    let CapturedRecordPayload::SqliteValues(values) = rejection.payload() else {
        panic!("AstrBot order violation must be a bounded SQLite marker");
    };
    assert!(values.is_empty());
    let mut projector = AstrBotCapturedBatchProjector {
        context: context(None),
        raw_source_path: "astrbot-frontier-order.db".to_owned(),
        user_version: 0,
        schema_fingerprint: "astrbot-frontier-order-schema".to_owned(),
        selected_conversation: None,
        parser_checkpoint: checkpoint,
    };
    let mut output = CollectingProjectionOutput::default();
    projector.project_record(rejection, &mut output).unwrap();
    assert_eq!(output.normalization.summary.failed, 1);
    assert!(output.normalization.summary.failures[0]
        .error
        .contains("not in legacy timestamp/id order by physical rowid"));
    let CapturedBatchCursorFinish::Advance(cursor) = projector.finish_cursor(&second).unwrap()
    else {
        panic!("AstrBot order rejection must advance its captured row");
    };
    let rejected_position = decode_astrbot_position(cursor.native_position())
        .unwrap()
        .unwrap();
    assert_eq!(rejected_position.phase, AstrBotPhase::Conversations);
    assert_eq!(rejected_position.next_ordinal, 65);
    assert_eq!(rejected_position.physical_rowid, 65);

    let sql = AstrBotSql::new(&conn).unwrap();
    let mut fetcher = AstrBotRowFetcher::new(&conn, sql, checkpoint).unwrap();
    let mut resumed = SqliteLogicalRowBatchProducer::new(
        test_source("frontier-order-resume"),
        first.range_end().clone(),
        move |position| fetcher.fetch(position),
    );
    let resumed_rejection = resumed.next_batch().unwrap().unwrap();
    assert_eq!(resumed_rejection.range_before(), first.range_end());
    assert_eq!(resumed_rejection.range_end(), second.range_end());
    assert_eq!(resumed_rejection.records().len(), 1);
    assert_eq!(
        resumed_rejection.records()[0].record_kind().as_str(),
        ASTRBOT_CONVERSATION_ORDER_VIOLATION_RECORD_KIND
    );
}
