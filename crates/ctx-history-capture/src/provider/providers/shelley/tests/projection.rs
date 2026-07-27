use super::*;

#[test]
fn shelley_rowid_keyset_matches_native_order_normalized_output() {
    fn populate(conn: &Connection, messages: &[(&str, &str, i64, &str)]) {
        create_shelley_tables(conn);
        insert_conversation(conn, "zeta", "2026-07-18T00:00:00Z");
        insert_conversation(conn, "alpha", "2026-07-18T00:00:30Z");
        insert_conversation(conn, "empty", "2026-07-18T00:01:00Z");
        for (message_id, conversation_id, sequence_id, text) in messages {
            insert_message(conn, message_id, conversation_id, *sequence_id, text);
        }
    }

    fn projected_captures(
        conn: &Connection,
        revision: &str,
        native_message_order: bool,
    ) -> Vec<(usize, ProviderCaptureEnvelope)> {
        let context = test_context(Path::new("shelley.db"));
        let mut projector = ShelleyCapturedBatchProjector::new(
            context,
            "shelley.db".to_owned(),
            0,
            "shelley-schema".to_owned(),
        );
        let mut output = CollectingProjectionOutput::default();
        let batches = produce_all(
            conn,
            test_source(revision),
            initial_shelley_position().unwrap(),
        );
        let mut records = batches
            .iter()
            .flat_map(|batch| batch.records())
            .collect::<Vec<_>>();
        if native_message_order {
            records.sort_by_key(|record| {
                if matches!(
                    record.record_kind().as_str(),
                    SHELLEY_MESSAGE_RECORD_KIND | SHELLEY_MESSAGE_CHILD_RECORD_KIND
                ) {
                    let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                        panic!("Shelley parity fixture unexpectedly rejected a message");
                    };
                    let message = decode_shelley_message(values).unwrap();
                    (
                        0_u8,
                        message.conversation_id,
                        message.sequence_id,
                        message.rowid,
                    )
                } else {
                    (
                        1_u8,
                        String::new(),
                        i64::try_from(record.ordinal()).unwrap(),
                        0,
                    )
                }
            });
        }
        for record in records {
            projector.project_record(record, &mut output).unwrap();
        }
        assert_eq!(output.normalization.summary.failed, 0);
        output
            .normalization
            .captures
            .sort_by(|(_, left), (_, right)| {
                left.session
                    .provider_session_id
                    .cmp(&right.session.provider_session_id)
                    .then_with(|| {
                        left.event
                            .as_ref()
                            .and_then(|event| event.provider_event_hash.as_deref())
                            .cmp(
                                &right
                                    .event
                                    .as_ref()
                                    .and_then(|event| event.provider_event_hash.as_deref()),
                            )
                    })
            });
        output.normalization.captures
    }

    let interleaved_rowids = Connection::open_in_memory().unwrap();
    populate(
        &interleaved_rowids,
        &[
            ("zeta-2", "zeta", 2, "zeta two"),
            ("alpha-1", "alpha", 1, "alpha one"),
            ("zeta-1", "zeta", 1, "zeta one"),
            ("alpha-2", "alpha", 2, "alpha two"),
        ],
    );

    let expected = projected_captures(&interleaved_rowids, "shelley-snapshot:native-order", true);
    let actual = projected_captures(&interleaved_rowids, "shelley-snapshot:rowid-order", false);
    assert_eq!(actual, expected);
    assert_eq!(actual.len(), 5);
    assert!(actual
        .iter()
        .all(|(_, capture)| capture.source.raw_source_path.as_deref() == Some("shelley.db")));

    for (message_id, conversation_id, sequence_id) in [
        ("alpha-1", "alpha", 1_u64),
        ("alpha-2", "alpha", 2),
        ("zeta-1", "zeta", 1),
        ("zeta-2", "zeta", 2),
    ] {
        let event = actual
            .iter()
            .filter_map(|(_, capture)| capture.event.as_ref())
            .find(|event| event.provider_event_hash.as_deref() == Some(message_id))
            .unwrap();
        let event_index =
            sequence_id * 4_096 + text_id_index(&format!("{conversation_id}:{message_id}"), 4_096);
        assert_eq!(event.provider_event_index, event_index);
        let expected_cursor =
            format!("conversation:{conversation_id}:sequence:{sequence_id}:message:{message_id}");
        assert_eq!(event.cursor.as_deref(), Some(expected_cursor.as_str()));
        let expected_idempotency_key =
            format!("provider-event:shelley:{conversation_id}:{event_index}");
        assert_eq!(
            event.idempotency_key.as_deref(),
            Some(expected_idempotency_key.as_str())
        );
    }
}

#[test]
fn shelley_projector_rejects_malformed_complete_rows_without_fatal_error() {
    let context = test_context(Path::new("shelley.db"));
    let mut projector = ShelleyCapturedBatchProjector::new(
        context,
        "shelley.db".to_owned(),
        0,
        "shelley-schema".to_owned(),
    );
    let mut output = CollectingProjectionOutput::default();
    let record = CapturedRecord::sqlite_logical(
        0,
        shelley_locator(ShelleyCapturePhase::Messages, 1).unwrap(),
        ProviderRecordKind::new(SHELLEY_MESSAGE_RECORD_KIND).unwrap(),
        vec![CapturedSqliteValue::Integer(1)],
    )
    .unwrap();
    projector.project_record(&record, &mut output).unwrap();
    assert_eq!(output.normalization.summary.failed, 1);
    assert_eq!(output.normalization.summary.failures[0].line, 1);
    assert!(output.normalization.captures.is_empty());
}
