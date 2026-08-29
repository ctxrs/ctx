use super::*;
use ctx_history_capture_runtime::SourceBackedRecordRejectionClass;

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

pub(super) fn create_current_fixture(path: &Path) -> Connection {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 time_created integer not null,
                 time_updated integer not null
             );
             create table session_message (
                 id text primary key,
                 session_id text not null,
                 type text not null,
                 seq integer not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             insert into session values ('session-1', 1, 1);",
        )
        .unwrap();
    connection
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
