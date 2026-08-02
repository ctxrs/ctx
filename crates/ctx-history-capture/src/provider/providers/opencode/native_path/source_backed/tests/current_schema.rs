use super::*;

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
fn run_git(path: &Path, arguments: &[&str]) {
    let status = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(path)
        .args(arguments)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {arguments:?} failed");
}

#[cfg(unix)]
#[test]
fn current_schema_projects_shared_repository_attribution_and_preserves_native_metadata() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let repository = temp.path().join("repository");
    fs::create_dir(&repository).unwrap();
    run_git(&repository, &["init", "-q"]);
    run_git(&repository, &["config", "user.name", "ctx test"]);
    run_git(
        &repository,
        &["config", "user.email", "ctx@example.invalid"],
    );
    run_git(
        &repository,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/opencode-current.git",
        ],
    );
    fs::create_dir(repository.join("src")).unwrap();
    fs::write(repository.join("src/lib.rs"), "pub fn current() {}\n").unwrap();
    run_git(&repository, &["add", "src/lib.rs"]);
    run_git(&repository, &["commit", "-qm", "fixture"]);

    let database = temp.path().join("opencode.db");
    let part_data = json!({
        "type": "tool",
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
    let old_lexical_body = "edit\ngit status --short";
    let exact_unit = serde_json::to_string(&part_data).unwrap();
    let expected_body = format!("{old_lexical_body}\n{exact_unit}");
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(expected_body.as_str())
    );
    assert_eq!(&expected_body[..old_lexical_body.len()], old_lexical_body);
    assert!(record.content.structured_content.is_none());
    assert_eq!(
        record.provider_session_id.as_deref(),
        Some("current-session")
    );
    assert!(record.native_event_id.is_some());
    assert_eq!(
        record.cwd.as_deref(),
        Some(repository.to_string_lossy().as_ref())
    );
    assert_eq!(
        record.metadata["provider_native_file_touches"],
        json!(["src/lib.rs"])
    );
    assert!(record.metadata.contains_key("repository_association"));
    assert_eq!(record.repository_bindings.len(), 1);
    assert_eq!(
        record.repository_bindings[0].logical_repository_id,
        "forge:github.com/acme/opencode-current"
    );
    assert!(record.repository_bindings[0]
        .evidence
        .iter()
        .any(|evidence| evidence.kind == RepositoryEvidenceKind::DeclaredToolWorkdir));
    assert_eq!(record.repository_file_observations.len(), 1);
    assert_eq!(
        record.repository_file_observations[0].relative_path,
        "src/lib.rs"
    );
    assert_eq!(
        record.repository_file_observations[0].kind,
        RepositoryFileObservationKind::Modified
    );
    let [invocation] = record.repository_file_invocation_evidence.as_slice() else {
        panic!("expected one strict OpenCode invocation");
    };
    assert_eq!(invocation.operation_ordinal, 0);
    assert_eq!(invocation.relative_path, "src/lib.rs");
    assert_eq!(invocation.kind, RepositoryFileInvocationKind::Modify);
    assert_eq!(invocation.tool_name.as_deref(), Some("edit"));
    let body = record.content.normalized_body.as_deref().unwrap();
    let range = invocation.normalized_text_range.unwrap();
    assert_eq!(&body[range.start as usize..range.end as usize], exact_unit);
    assert_eq!(
        record
            .repository_candidate_evidence
            .paths(RepositoryCandidateKind::SessionCwd)
            .collect::<Vec<_>>(),
        vec![repository.to_string_lossy().as_ref()]
    );
    assert_eq!(
        record
            .repository_candidate_evidence
            .paths(RepositoryCandidateKind::FileActivityPath)
            .collect::<Vec<_>>(),
        vec![repository.join("src/lib.rs").to_string_lossy().as_ref()]
    );
    record.validate_contract().unwrap();
}

#[cfg(unix)]
#[test]
fn indexed_exact_hydration_keeps_the_native_tool_call_body_authoritative() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let repository = temp.path().join("hydration-repository");
    fs::create_dir(&repository).unwrap();
    run_git(&repository, &["init", "-q"]);
    fs::create_dir(repository.join("src")).unwrap();
    fs::write(repository.join("src/hydrated.rs"), "pub fn hydrated() {}\n").unwrap();

    let part_data = json!({
        "type": "tool",
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

    let page = project_fixture(&database, temp.path());
    let [item] = page.items.as_slice() else {
        panic!("expected one hydrated OpenCode tool-call record");
    };
    let record = &item.core_record;
    let exact_unit = serde_json::to_string(&part_data).unwrap();
    let old_lexical_body = "tool call: write_file";
    let expected = format!("{old_lexical_body}\n{exact_unit}");
    assert_eq!(
        record.content.normalized_body.as_deref(),
        Some(expected.as_str())
    );
    assert!(record.content.structured_content.is_none());
    let [invocation] = record.repository_file_invocation_evidence.as_slice() else {
        panic!("expected one hydrated strict invocation");
    };
    assert_eq!(invocation.kind, RepositoryFileInvocationKind::Write);
    assert_eq!(invocation.relative_path, "src/hydrated.rs");
    let range = invocation.normalized_text_range.unwrap();
    assert_eq!(
        &expected[range.start as usize..range.end as usize],
        exact_unit
    );
    assert_eq!(&expected[..old_lexical_body.len()], old_lexical_body);
    assert!(exact_unit.contains("pub fn hydrated() { exact(); }"));
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
    assert_eq!(record.event_type, "tool_output");
    assert!(record.repository_file_invocation_evidence.is_empty());
}
