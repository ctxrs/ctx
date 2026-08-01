use super::*;
#[cfg(unix)]
use ctx_history_core::{
    RepositoryCandidateKind, RepositoryEvidenceKind, RepositoryFileObservationKind,
};
use rusqlite::{params, Connection};
use serde_json::json;
use std::path::Path;
#[cfg(unix)]
use std::{fs, process::Command};

fn write_current_schema(
    path: &Path,
    directory: &Path,
    part_data: &serde_json::Value,
) -> Connection {
    let connection = Connection::open(path).unwrap();
    connection
        .execute_batch(
            "create table session (
                 id text primary key,
                 project_id text not null,
                 workspace_id text,
                 parent_id text,
                 slug text not null,
                 directory text not null,
                 title text not null,
                 version text not null,
                 agent text,
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
             create table message (
                 id text primary key,
                 session_id text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             create table part (
                 id text primary key,
                 message_id text not null,
                 session_id text not null,
                 time_created integer not null,
                 time_updated integer not null,
                 data text not null
             );
             create table event (
                 id text primary key,
                 aggregate_id text not null,
                 seq integer not null,
                 type text not null,
                 data text not null
             );",
        )
        .unwrap();
    connection
        .execute(
            "insert into session values (
                 'current-session', 'project-1', null, null, 'current-session', ?1,
                 'Current session', '1.18.11', 'build', 1782259200000, 1782259202000
             )",
            [directory.to_string_lossy().as_ref()],
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                 'current-user', 'current-session', 1782259200000, 1782259200000, ?1
             )",
            [json!({
                "role": "user",
                "time": {"created": 1782259200000_i64},
                "text": "current OpenCode prompt"
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into message values (
                 'current-assistant', 'current-session', 1782259201000, 1782259202000, ?1
             )",
            [json!({
                "role": "assistant",
                "time": {"created": 1782259201000_i64},
                "modelID": "gpt-test"
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "insert into part values (
                 'current-part', 'current-assistant', 'current-session',
                 1782259201000, 1782259201000, ?1
             )",
            [part_data.to_string()],
        )
        .unwrap();
    for sequence in 0..9_i64 {
        connection
            .execute(
                "insert into event values (?1, 'current-session', ?2, 'history.updated', '{}')",
                params![format!("event-{sequence}"), sequence],
            )
            .unwrap();
    }
    connection
}

fn scan_current_schema(
    path: &Path,
) -> (
    OpenCodeLogicalObservation,
    OpenCodeSourceBackedScan,
    Vec<CoreRecord>,
) {
    let authorized =
        open_root_authorized_snapshot_retained(crate::test_provider_sqlite_data_root(), path)
            .unwrap();
    let observation = observe_logical_source(
        authorized.sqlite_snapshot.connection().unwrap(),
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
    )
    .unwrap();
    let mut records = Vec::new();
    let scan = scan_pinned_source(
        path,
        &crate::provider::providers::opencode::OPENCODE_SQLITE_DIALECT,
        &observation,
        authorized.sqlite_snapshot,
        &mut |output| {
            if let OpenCodeScanOutput::Document(record) = output {
                records.push(record);
            }
            Ok(())
        },
    )
    .unwrap();
    (observation, scan, records)
}

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("../source_backed.rs"),
        include_str!("projection.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("let body = if searchable.is_empty()"));
    assert!(production.contains("RepositoryAttributor"));
    assert!(production.contains("apply_annotation"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
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
    let connection = write_current_schema(
        &database,
        &repository,
        &json!({
            "type": "tool",
            "tool": "edit",
            "state": {
                "input": {
                    "command": "git status --short",
                    "workdir": repository,
                    "path": "src/lib.rs"
                }
            }
        }),
    );
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
    assert!(record
        .content
        .normalized_body
        .as_deref()
        .is_some_and(|body| body.contains("git status --short")));
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
