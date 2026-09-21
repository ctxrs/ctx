use super::*;
use ctx_history_capture_runtime::SourceBackedRecordRejectionClass;

fn padded_json_value(mut value: serde_json::Value, bytes: usize) -> String {
    let fixed_bytes = value.to_string().len();
    let padding = value
        .as_object_mut()
        .and_then(|object| object.get_mut("padding"))
        .expect("padded JSON object must contain a padding field");
    assert!(fixed_bytes <= bytes);
    *padding = serde_json::Value::String("x".repeat(bytes - fixed_bytes));
    let encoded = value.to_string();
    assert_eq!(encoded.len(), bytes);
    encoded
}

#[test]
fn current_file_parts_are_ignored_without_indexing_attachment_payloads() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("current-file-part-opencode.db");
    let attachment_sentinel = "attachment-payload-must-not-be-indexed";
    drop(write_current_schema(
        &database,
        temp.path(),
        &json!({
            "type": "file",
            "mime": "image/png",
            "filename": "diagram.png",
            "url": format!("data:image/png;base64,{attachment_sentinel}"),
            "source": {
                "type": "file",
                "path": "diagram.png",
                "text": {"value": attachment_sentinel, "start": 0, "end": 1}
            }
        }),
    ));

    let (_, scan, records, rejections) = scan_current_schema_with_rejections(&database);

    assert!(records.is_empty());
    assert!(rejections.is_empty());
    assert_eq!(scan.certificate.counts().complete_records, 1);
    assert_eq!(scan.certificate.counts().retained_records, 0);
    assert_eq!(scan.certificate.counts().rejected_records, 0);
    assert_eq!(scan.certificate.counts().ignored_records, 1);
}

#[test]
fn unsupported_current_parts_emit_bounded_row_diagnostics() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("unsupported-current-part-opencode.db");
    drop(write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "future_part", "value": "unsupported"}),
    ));

    let (_, scan, records, rejections) = scan_current_schema_with_rejections(&database);

    assert!(records.is_empty());
    assert_eq!(scan.certificate.counts().rejected_records, 1);
    let [rejection] = rejections.as_slice() else {
        panic!("expected one bounded OpenCode row diagnostic");
    };
    assert_eq!(rejection.provider, CaptureProvider::OpenCode);
    assert_eq!(rejection.source_selector, database.to_string_lossy());
    assert_eq!(rejection.line_number, 1);
    assert_eq!(rejection.payload_type.as_deref(), Some("sqlite_row"));
    assert_eq!(
        rejection.class,
        SourceBackedRecordRejectionClass::UnsupportedRecord
    );
    assert!(rejection.detail.contains("unsupported record type"));
}

#[test]
fn invalid_timestamp_diagnostics_do_not_echo_provider_values() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("invalid-timestamp-opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "invalid timestamp"}),
    );
    connection
        .execute("update part set time_created = ?1", [i64::MAX])
        .unwrap();
    drop(connection);

    let (_, scan, records, rejections) = scan_current_schema_with_rejections(&database);

    assert!(records.is_empty());
    assert_eq!(scan.certificate.counts().rejected_records, 1);
    let [rejection] = rejections.as_slice() else {
        panic!("expected one invalid timestamp diagnostic");
    };
    assert_eq!(
        rejection.class,
        SourceBackedRecordRejectionClass::MalformedRecord
    );
    assert!(rejection.detail.contains("invalid timestamp"));
    assert!(!rejection.detail.contains(&i64::MAX.to_string()));
}

#[test]
fn core_projection_failures_emit_row_diagnostics() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("oversized-invalid-call-opencode.db");
    let mut body = json!({
        "type": "tool",
        "call_id": "",
        "tool": "read_file",
        "state": {"input": {"payload": ""}}
    });
    let fixed_bytes = body.to_string().len();
    body["state"]["input"]["payload"] = serde_json::Value::String(
        "x".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES - fixed_bytes - 8),
    );
    assert_eq!(
        body.to_string().len(),
        ctx_history_core::MAX_CORE_CONTENT_BYTES - 8
    );
    drop(write_current_schema(&database, temp.path(), &body));

    let (_, scan, records, rejections) = scan_current_schema_with_rejections(&database);

    assert!(records.is_empty());
    let counts = scan.certificate.counts();
    assert_eq!(counts.complete_records, 1);
    assert_eq!(counts.retained_records, 0);
    assert_eq!(counts.rejected_records, 1);
    let [rejection] = rejections.as_slice() else {
        panic!("expected one Core projection rejection diagnostic");
    };
    assert_eq!(
        rejection.class,
        SourceBackedRecordRejectionClass::UnsupportedRecord
    );
    assert!(rejection.detail.contains("Core projection limits"));
}

#[test]
fn oversized_message_and_part_values_are_record_local_across_ordering_paths() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("oversized-current-schema.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "valid before oversized rows"}),
    );
    let cap = MAX_PROVIDER_SQLITE_VALUE_BYTES;
    let exact_parent = padded_json_value(
        json!({
            "role": "assistant",
            "time": {"created": 1782259202000_i64},
            "padding": ""
        }),
        cap,
    );
    let exact_part = padded_json_value(json!({"type": "file", "padding": ""}), cap);
    let oversized_parent = padded_json_value(
        json!({
            "role": "assistant",
            "time": {"created": 1782259203000_i64},
            "padding": ""
        }),
        cap + 1,
    );
    let oversized_part =
        padded_json_value(json!({"type": "text", "text": "", "padding": ""}), cap + 1);
    let oversized_type = "x".repeat(cap + 1);

    connection
        .execute(
            "insert into session_message values (
                 'oversized-metadata', 'current-session', ?1, 1,
                 1782259200000, 1782259200000, ?2
             )",
            params![oversized_type.as_str(), oversized_parent.as_str()],
        )
        .unwrap();
    connection
        .execute("alter table part add column type text", [])
        .unwrap();

    for (message_id, created, data) in [
        ("exact-message", 1782259202000_i64, exact_parent.as_str()),
        (
            "oversized-parent-message",
            1782259203000_i64,
            oversized_parent.as_str(),
        ),
        (
            "oversized-part-message",
            1782259204000_i64,
            r#"{"role":"assistant","time":{"created":1782259204000}}"#,
        ),
        (
            "valid-after-message",
            1782259205000_i64,
            r#"{"role":"assistant","time":{"created":1782259205000}}"#,
        ),
    ] {
        connection
            .execute(
                "insert into message values (?1, 'current-session', ?2, ?2, ?3)",
                params![message_id, created, data],
            )
            .unwrap();
    }
    for (rowid, part_id, message_id, created, data) in [
        (
            10_i64,
            "exact-part",
            "exact-message",
            1782259202000_i64,
            exact_part.as_str(),
        ),
        (
            20_i64,
            "oversized-parent-part",
            "oversized-parent-message",
            1782259203000_i64,
            exact_part.as_str(),
        ),
        (
            30_i64,
            "oversized-part",
            "oversized-part-message",
            1782259204000_i64,
            oversized_part.as_str(),
        ),
        (
            40_i64,
            "valid-after-part",
            "valid-after-message",
            1782259205000_i64,
            r#"{"type":"text","text":"valid after oversized rows"}"#,
        ),
    ] {
        connection
            .execute(
                "insert into part(rowid,id,message_id,session_id,time_created,time_updated,data)
                 values (?1, ?2, ?3, 'current-session', ?4, ?4, ?5)",
                params![rowid, part_id, message_id, created, data],
            )
            .unwrap();
    }
    connection
        .execute(
            "update part set type = ?1 where id = 'oversized-parent-part'",
            [oversized_type],
        )
        .unwrap();
    drop(connection);

    let indexed = scan_current_schema_with_rejections(&database);
    assert!(indexed.0.schema.message_part_indexed_streaming);
    Connection::open(&database)
        .unwrap()
        .execute("drop index part_message_id_id_idx", [])
        .unwrap();
    let fallback = scan_current_schema_with_rejections(&database);
    assert!(!fallback.0.schema.message_part_indexed_streaming);

    for (_, scan, records, rejections) in [&indexed, &fallback] {
        let counts = scan.certificate.counts();
        assert_eq!(counts.complete_records, 5);
        assert_eq!(counts.retained_records, 2);
        assert_eq!(counts.indexed_documents, 2);
        assert_eq!(counts.rejected_records, 2);
        assert_eq!(counts.ignored_records, 1);
        assert_eq!(scan.bounds.max_buffered_payload_bytes, 2 * cap as u64);
        assert!(scan.bounds.max_buffered_payload_bytes <= OPENCODE_HYDRATION_SINGLETON_MAX_BYTES);
        assert_eq!(
            records
                .iter()
                .map(|record| record.content.meaningful_text())
                .collect::<Vec<_>>(),
            ["valid before oversized rows", "valid after oversized rows"]
        );
        assert_eq!(
            rejections
                .iter()
                .map(|rejection| rejection.line_number)
                .collect::<Vec<_>>(),
            [20, 30]
        );
        assert!(rejections.iter().all(|rejection| {
            rejection.class == SourceBackedRecordRejectionClass::UnsupportedRecord
                && rejection.detail.contains("retained-content size limit")
        }));
    }
    assert_eq!(indexed.2, fallback.2);
    assert_eq!(
        indexed.1.certificate.counts(),
        fallback.1.certificate.counts()
    );
    assert_eq!(
        indexed.1.certificate.content_digest(),
        fallback.1.certificate.content_digest()
    );
    assert_eq!(
        indexed
            .3
            .iter()
            .map(|rejection| (rejection.line_number, rejection.class))
            .collect::<Vec<_>>(),
        fallback
            .3
            .iter()
            .map(|rejection| (rejection.line_number, rejection.class))
            .collect::<Vec<_>>()
    );
    assert_ne!(
        PARSER_REVISION,
        "opencode-family-source-backed-v12-known-file-carriers"
    );
}

#[test]
fn current_11811_shape_selects_populated_message_part_over_empty_session_message() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "current OpenCode response"}),
    );
    let counts = connection
        .query_row(
            "select
                 (select count(*) from event),
                 (select count(*) from message),
                 (select count(*) from part),
                 (select count(*) from session),
                 (select count(*) from session_message)",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(counts, (9, 2, 1, 1, 0));

    let schema = OpenCodeNativeSchema::probe(
        &connection,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    assert_eq!(schema.family, OpenCodeNativeSchemaFamily::MessagePart);
    assert!(!schema.capability_digest.is_empty());
}

#[test]
fn native_task_parent_is_delegated_unique_but_fresh_id_fork_shape_stays_root_unknown() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "shared payload"}),
    );
    connection
        .execute(
            "insert into session values (
                 'task-child', 'project-1', null, 'current-session', 'task-child', ?1,
                 'Task child', '1.18.11', 'build', 1782259203000, 1782259204000
             )",
            [temp.path().to_string_lossy().as_ref()],
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                 'task-message', 'task-child', 1782259203000, 1782259203000, ?1
             )",
            [json!({"role": "assistant", "time": {"created": 1782259203000_i64}}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                 'task-part', 'task-message', 'task-child',
                 1782259203000, 1782259203000, ?1
             )",
            [json!({"type": "text", "text": "task-owned payload"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into session values (
                 'interactive-fork', 'project-1', null, null, 'interactive-fork', ?1,
                 'Interactive fork', '1.18.11', 'build', 1782259205000, 1782259206000
             )",
            [temp.path().to_string_lossy().as_ref()],
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                 'fresh-fork-message', 'interactive-fork',
                 1782259205000, 1782259205000, ?1
             )",
            [json!({"role": "assistant", "time": {"created": 1782259205000_i64}}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                 'fresh-fork-part', 'fresh-fork-message', 'interactive-fork',
                 1782259205000, 1782259205000, ?1
             )",
            [json!({"type": "text", "text": "shared payload"}).to_string()],
        )
        .unwrap();
    drop(connection);

    let (_, _, records) = scan_current_schema(&database);
    let task = records
        .iter()
        .find(|record| record.provider_session_id.as_deref() == Some("task-child"))
        .unwrap();
    assert_eq!(
        task.session_relationship,
        Some(ProviderNativeSessionRelationship::Delegated)
    );
    assert!(task.parent_session_id.is_some());
    assert!(task.root_session_id.is_none());

    let fork = records
        .iter()
        .find(|record| record.provider_session_id.as_deref() == Some("interactive-fork"))
        .unwrap();
    assert_eq!(fork.session_relationship, None);
    assert_eq!(fork.parent_session_id, None);
    assert_eq!(fork.root_session_id, None);
}

#[test]
fn independently_populated_representations_fail_closed() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "message part representation"}),
    );
    connection
        .execute(
            "insert into session_message values (
                 'competing-message', 'current-session', 'user', 1,
                 1782259200000, 1782259200000, ?1
             )",
            [json!({
                "role": "user",
                "time": {"created": 1782259200000_i64},
                "text": "session message representation"
            })
            .to_string()],
        )
        .unwrap();

    let error = OpenCodeNativeSchema::probe(
        &connection,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap_err();
    assert!(error.to_string().contains(
        "ambiguous populated message schema families: session_message_seq, message_part"
    ));
}

#[cfg(unix)]
#[test]
fn current_schema_preserves_literal_workdir_command_and_file_facts() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    fs::create_dir(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "pub fn current() {}\n").unwrap();

    let database = temp.path().join("opencode.db");
    let part_data = json!({
        "type": "tool",
        "call_id": "current-call",
        "tool": "edit",
        "state": {
            "input": {
                "command": "git status --short",
                "workdir": repository,
                "path": "src/lib.rs"
            }
        }
    });
    let connection = write_current_schema(&database, &repository, &part_data);
    drop(connection);
    let mut permissions = fs::metadata(&database).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&database, permissions).unwrap();

    let (observation, scan, records) = scan_current_schema(&database);
    assert_eq!(
        observation.schema.family,
        OpenCodeNativeSchemaFamily::MessagePart
    );
    assert_eq!(scan.certificate.counts().complete_records, 1);
    assert_eq!(scan.certificate.counts().indexed_documents, 1);
    let [record] = records.as_slice() else {
        panic!("expected one current-schema Core record");
    };
    assert_eq!(record.content.meaningful_text(), "edit\ngit status --short");
    assert_eq!(record.content.structured_content.as_ref(), Some(&part_data));
    assert_eq!(
        record.provider_session_id.as_deref(),
        Some("current-session")
    );
    assert!(record.native_event_id.is_some());
    let activity = record.content.activity.as_ref().unwrap();
    assert_eq!(
        activity.provider_call_id,
        Some(TypedKey::Utf8("current-call".to_owned()))
    );
    let invocation = activity.invocation.as_ref().unwrap();
    assert_eq!(invocation.tool, "edit");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: part_data.pointer("/state/input").unwrap().clone(),
        }
    );
    assert!(activity.facts.iter().any(|fact| {
        fact.kind == LiteralFactKind::SessionCwd && fact.value == repository.to_string_lossy()
    }));
    assert!(activity.facts.iter().any(|fact| {
        fact.kind == LiteralFactKind::ToolWorkdir && fact.value == repository.to_string_lossy()
    }));
    assert!(activity.facts.iter().any(|fact| {
        fact.kind == LiteralFactKind::Command && fact.value == "git status --short"
    }));
    assert!(activity
        .facts
        .iter()
        .any(|fact| { fact.kind == LiteralFactKind::File && fact.value == "src/lib.rs" }));
    record.validate_contract().unwrap();
}

#[cfg(unix)]
#[test]
fn indexed_exact_hydration_keeps_the_native_tool_call_body_authoritative() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let repository = temp.path().join("hydration-repository");
    fs::create_dir(&repository).unwrap();
    fs::create_dir(repository.join("src")).unwrap();
    fs::write(repository.join("src/hydrated.rs"), "pub fn hydrated() {}\n").unwrap();

    let part_data = json!({
        "type": "tool",
        "call_id": "hydrated-call",
        "tool": "write_file",
        "state": {
            "status": "running",
            "input": {
                "workdir": repository,
                "path": "src/hydrated.rs",
                "content": "pub fn hydrated() { exact(); }"
            }
        }
    });
    let database = temp.path().join("hydration-opencode.db");
    drop(write_current_schema(&database, &repository, &part_data));

    let (_, _, records) = scan_current_schema(&database);
    let [record] = records.as_slice() else {
        panic!("expected one hydrated OpenCode tool-call record");
    };
    assert_eq!(record.content.meaningful_text(), "tool call: write_file");
    assert_eq!(record.content.structured_content.as_ref(), Some(&part_data));
    let invocation = record
        .content
        .activity
        .as_ref()
        .and_then(|activity| activity.invocation.as_ref())
        .unwrap();
    assert_eq!(invocation.tool, "write_file");
    assert_eq!(
        invocation.arguments,
        ActivityJsonCapture::Present {
            value: part_data.pointer("/state/input").unwrap().clone(),
        }
    );
    record.validate_contract().unwrap();
}

#[test]
fn failed_tool_result_record_never_invents_file_invocation_evidence() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("failed-result-opencode.db");
    drop(write_current_schema(
        &database,
        temp.path(),
        &json!({
            "type": "tool",
            "tool": "edit_file",
            "state": {
                "status": "failed",
                "input": {"path": "src/result-only.rs"},
                "output": "provider-native failure"
            }
        }),
    ));

    let (_, scan, records) = scan_current_schema(&database);
    assert_eq!(scan.certificate.counts().indexed_documents, 1);
    let [record] = records.as_slice() else {
        panic!("expected one retained failed-result record");
    };
    assert_eq!(record.event_type, "tool_call");
    assert_eq!(
        record.content.structured_content.as_ref().unwrap()["state"]["status"],
        "failed"
    );
    let activity = record.content.activity.as_ref().unwrap();
    assert!(activity.invocation.is_none());
    assert!(activity.result.is_none());
    assert!(activity
        .facts
        .iter()
        .any(|fact| { fact.kind == LiteralFactKind::File && fact.value == "src/result-only.rs" }));
}

#[test]
fn expression_index_uses_conservative_plan_and_keeps_sparse_rejection_rowid() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let database = temp.path().join("expression-index.db");
    let connection = write_current_schema(
        &database,
        temp.path(),
        &json!({"type": "text", "text": "retained expression-index conversation"}),
    );
    connection
        .execute_batch(
            "DROP INDEX message_session_time_created_id_idx;
        CREATE INDEX message_expression ON message(lower(id));
        UPDATE part SET rowid=401;
        INSERT INTO part(rowid,id,message_id,session_id,time_created,time_updated,data)
            SELECT 909,'malformed-part',message_id,session_id,time_created,time_updated,'{broken'
            FROM part WHERE rowid=401;",
        )
        .unwrap();
    drop(connection);
    let (observation, scan, records, rejections) = scan_current_schema_with_rejections(&database);
    assert!(!observation.schema.message_part_indexed_streaming);
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].content.normalized_body.as_deref(),
        Some("retained expression-index conversation")
    );
    assert_eq!(scan.certificate.counts().rejected_records, 1);
    assert_eq!(rejections.len(), 1);
    assert_eq!(rejections[0].line_number, 909);
}

/// Optional no-write comparison against a user-supplied native database. The
/// ordinary suite stays hermetic; this diagnostic needs an explicit fixture.
#[test]
#[ignore = "requires CTX_TEST_OPENCODE_DATABASE pointing to a disposable native capture"]
fn native_selected_snapshot_matches_full_family_capture() {
    let database = std::path::PathBuf::from(
        std::env::var_os("CTX_TEST_OPENCODE_DATABASE").expect("native capture path"),
    );
    let temp = crate::test_support_paths::tempdir().unwrap();
    let retained = retain_root_authorized_source(temp.path(), &database).unwrap();
    let full = retained
        .sqlite_authority
        .open_stable_snapshot(&retained.database_leaf)
        .unwrap();
    let selected = open_root_authorized_snapshot_retained(temp.path(), &database)
        .unwrap()
        .sqlite_snapshot;
    let tables = [
        "session",
        "message",
        "part",
        "session_message",
        "session_entry",
    ];
    for table in tables {
        let schema = "SELECT type,name,sql FROM sqlite_schema WHERE tbl_name=?1 AND type IN ('table','index') ORDER BY name";
        let schemas = |snapshot: &SqliteSourceReadSnapshot| {
            snapshot
                .connection()
                .unwrap()
                .prepare(schema)
                .unwrap()
                .query_map([table], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let expected = schemas(&full);
        assert_eq!(expected, schemas(&selected), "schema for {table}");
        if expected.is_empty() {
            continue;
        }
        let sql = format!("SELECT rowid,* FROM {table} ORDER BY rowid");
        let mut left = full.connection().unwrap().prepare(&sql).unwrap();
        let mut right = selected.connection().unwrap().prepare(&sql).unwrap();
        let columns = left.column_count();
        let mut left = left.query([]).unwrap();
        let mut right = right.query([]).unwrap();
        while let Some(row) = left.next().unwrap() {
            let copy = right.next().unwrap().expect("copied row");
            for column in 0..columns {
                assert_eq!(
                    row.get_ref(column).unwrap(),
                    copy.get_ref(column).unwrap(),
                    "native storage-class/value/rowid mismatch in {table}"
                );
            }
        }
        assert!(right.next().unwrap().is_none());
    }
    let scan = |snapshot: SqliteSourceReadSnapshot| {
        let dialect = &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT;
        let observation = observe_logical_source(snapshot.connection().unwrap(), dialect).unwrap();
        let mut records = Vec::new();
        let scan = scan_pinned_source(&database, dialect, &observation, snapshot, &mut |output| {
            if let OpenCodeScanOutput::Document(record) = output {
                records.push(record);
            }
            Ok(())
        })
        .unwrap();
        (scan.certificate, records)
    };
    let expected = scan(full);
    let actual = scan(selected);
    assert!(
        !actual.1.is_empty(),
        "capture must exercise conversation content"
    );
    assert_eq!(actual, expected);
}
