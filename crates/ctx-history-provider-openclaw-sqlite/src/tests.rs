use std::{collections::BTreeSet, fs, io::Cursor, path::Path};

use rusqlite::{params, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;

#[test]
fn root_scope_separates_identical_openclaw_sessions_and_unqualified_is_released() {
    use ctx_history_core::{SourceAnchorScope, SourceKey};

    let released = SourceKey::derive_provider_native(
        CaptureProvider::OpenClaw.as_str(),
        OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8("shared-agent").unwrap(),
    )
    .unwrap();
    let unqualified = source_key_scoped("shared-agent", SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first = source_key_scoped("shared-agent", SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second = source_key_scoped("shared-agent", SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    let first_record = project_event(
        &first,
        "shared-session",
        SessionGeneration::Active,
        1,
        "shared-event",
        &json!({"role": "user", "content": "same body"}),
        1,
    )
    .unwrap();
    let second_record = project_event(
        &second,
        "shared-session",
        SessionGeneration::Active,
        1,
        "shared-event",
        &json!({"role": "user", "content": "same body"}),
        1,
    )
    .unwrap();
    assert_ne!(first_record.session_id, second_record.session_id);
    assert_ne!(first_record.event_id, second_record.event_id);

    let sibling =
        source_key_scoped("sibling-agent", SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    assert_ne!(first.identity(), sibling.identity());
}

struct Fixture {
    root: TempDir,
    path: PathBuf,
    connection: Connection,
}

#[derive(Debug)]
struct Snapshot {
    source: SourceKey,
    records: Vec<CoreRecord>,
    receipt: ProjectionReceipt,
}

impl Fixture {
    fn new(agent_id: &str, wal: bool) -> Self {
        Self::with_schema(agent_id, wal, SCHEMA)
    }

    fn with_schema(agent_id: &str, wal: bool, schema: &str) -> Self {
        let root = tempfile::tempdir().expect("create fixture root");
        let path = root
            .path()
            .join("agents")
            .join(agent_id)
            .join("agent")
            .join(DATABASE_LEAF);
        fs::create_dir_all(path.parent().expect("database parent"))
            .expect("create agent database directory");
        let connection = Connection::open(&path).expect("open fixture database");
        if wal {
            connection
                .query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))
                .expect("enable WAL");
            connection
                .execute_batch("PRAGMA wal_autocheckpoint=0;")
                .expect("disable WAL autocheckpoint");
        }
        connection
            .execute_batch(schema)
            .expect("create OpenClaw fixture schema");
        connection
            .execute(
                "INSERT INTO schema_meta\
                   (meta_key, role, schema_version, agent_id, app_version, created_at, updated_at)\
                 VALUES ('primary', 'agent', ?1, ?2, 'test', 1, 1)",
                params![OPENCLAW_AGENT_SCHEMA_VERSION, agent_id],
            )
            .expect("insert schema owner");
        Self {
            root,
            path,
            connection,
        }
    }

    fn read(&self) -> Result<Snapshot> {
        read_path(self.root.path(), &self.path, MAX_ARCHIVE_DECODED_BYTES)
    }
}

fn read_path(data_root: &Path, path: &Path, archive_limit: usize) -> Result<Snapshot> {
    let agent_id = path_agent_id(path)?;
    let connection = open_database(data_root, path)?;
    let operation = (|| {
        validate_database(&connection, &agent_id)?;
        let source = source_key(&agent_id)?;
        let mut records = Vec::new();
        let receipt = project_database(
            &connection,
            &agent_id,
            &source,
            archive_limit,
            &mut |record| {
                records.push(record);
                Ok(())
            },
        )?;
        Ok(Snapshot {
            source,
            records,
            receipt,
        })
    })();
    finalize_result(connection, operation)
}

fn insert_active(
    connection: &Connection,
    session_id: &str,
    seq: i64,
    position: i64,
    event_id: &str,
    text: &str,
) {
    connection
        .execute(
            "INSERT OR IGNORE INTO session_windows\
               (session_id, session_key, reason, session_scope, created_at, updated_at,\
                session_entry_provenance, acp_owned)\
             VALUES (?1, ?2, 'initial', 'conversation', 1000, 1000, 0, 0)",
            params![session_id, format!("agent:test:{session_id}")],
        )
        .expect("insert session window");
    let event = json!({
        "type": "message",
        "id": event_id,
        "timestamp": 1_700_000_000_000_i64 + seq,
        "message": {"role": "user", "content": text}
    });
    connection
        .execute(
            "INSERT INTO transcript_events (session_id, seq, event_json, created_at)\
             VALUES (?1, ?2, ?3, ?4)",
            params![
                session_id,
                seq,
                event.to_string(),
                1_700_000_000_000_i64 + seq
            ],
        )
        .expect("insert transcript event");
    connection
        .execute(
            "INSERT INTO transcript_event_identities\
               (session_id, event_id, seq, event_type, parent_id, message_idempotency_key, created_at)\
             VALUES (?1, ?2, ?3, 'message', NULL, NULL, ?4)",
            params![session_id, event_id, seq, 1_700_000_000_000_i64 + seq],
        )
        .expect("insert transcript identity");
    connection
        .execute(
            "INSERT INTO session_transcript_active_events\
               (session_id, active_position, event_seq, message_position)\
             VALUES (?1, ?2, ?3, ?2)",
            params![session_id, position, seq],
        )
        .expect("insert active projection");
    connection
        .execute(
            r#"INSERT INTO session_transcript_index_state
                 (session_id, indexed_seq, leaf_event_id, needs_rebuild, active_event_count,
                  active_message_count, updated_at)
               VALUES (?1, ?2, ?3, 0, 1, 1, 1)
               ON CONFLICT(session_id) DO UPDATE SET
                 indexed_seq = excluded.indexed_seq,
                 leaf_event_id = excluded.leaf_event_id,
                 needs_rebuild = 0,
                 active_event_count = (SELECT count(*) FROM session_transcript_active_events
                                        WHERE session_id = excluded.session_id),
                 active_message_count = (SELECT count(*) FROM session_transcript_active_events
                                          WHERE session_id = excluded.session_id
                                            AND message_position IS NOT NULL),
                 updated_at = excluded.updated_at"#,
            params![session_id, seq, event_id],
        )
        .expect("update active index state");
}

fn rewrite_active(connection: &Connection, session_id: &str, seq: i64, event_id: &str, text: &str) {
    let event = json!({
        "type": "message",
        "id": event_id,
        "timestamp": 1_700_000_000_000_i64 + seq,
        "message": {"role": "assistant", "content": text}
    });
    connection
        .execute(
            "UPDATE transcript_events SET event_json = ?3 WHERE session_id = ?1 AND seq = ?2",
            params![session_id, seq, event.to_string()],
        )
        .expect("rewrite active event");
}

fn rewrite_active_event(
    connection: &Connection,
    session_id: &str,
    seq: i64,
    event: &serde_json::Value,
) {
    connection
        .execute(
            "UPDATE transcript_events SET event_json = ?3 WHERE session_id = ?1 AND seq = ?2",
            params![session_id, seq, event.to_string()],
        )
        .expect("rewrite active event JSON");
}

fn insert_archive(
    connection: &Connection,
    session_id: &str,
    generation: &str,
    reason: &str,
    encoding: &str,
    event_id: &str,
    text: &str,
) {
    let line = format!(
        "{}\n",
        json!({
            "type": "message",
            "id": event_id,
            "timestamp": 1_690_000_000_000_i64,
            "message": {"role": "assistant", "content": text}
        })
    );
    let blob = match encoding {
        "identity" => line.into_bytes(),
        "zstd" => zstd::stream::encode_all(Cursor::new(line.into_bytes()), 1)
            .expect("compress archive fixture"),
        _ => panic!("unsupported fixture encoding"),
    };
    let digest = Sha256::digest(&blob);
    let sha = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    connection
        .execute(
            "INSERT INTO session_transcript_archives\
               (session_id, generation, session_key, reason, encoding, archive_blob,\
                archive_sha256, archive_name, created_at, published_at, publish_attempts,\
                last_publish_attempt_at, last_publish_error)\
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 10, NULL, 0, NULL, NULL)",
            params![
                session_id,
                generation,
                format!("agent:test:{session_id}"),
                reason,
                encoding,
                blob,
                sha,
                format!("{session_id}-{generation}.{reason}")
            ],
        )
        .expect("insert transcript archive");
}

fn insert_raw_archive(
    connection: &Connection,
    session_id: &str,
    generation: &str,
    encoding: &str,
    blob: Vec<u8>,
) {
    let sha = Sha256::digest(&blob)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    connection
        .execute(
            "INSERT INTO session_transcript_archives\
               (session_id, generation, session_key, reason, encoding, archive_blob,\
                archive_sha256, archive_name, created_at, published_at, publish_attempts,\
                last_publish_attempt_at, last_publish_error)\
             VALUES (?1, ?2, ?3, 'reset', ?4, ?5, ?6, ?7, 10, NULL, 0, NULL, NULL)",
            params![
                session_id,
                generation,
                format!("agent:test:{session_id}"),
                encoding,
                blob,
                sha,
                format!("{session_id}-{generation}.reset")
            ],
        )
        .expect("insert raw transcript archive");
}

#[test]
fn cold_snapshot_projects_active_events_in_active_order() {
    let fixture = Fixture::new("main", false);
    insert_active(&fixture.connection, "session-a", 4, 0, "event-a", "first");
    insert_active(&fixture.connection, "session-a", 9, 1, "event-b", "second");

    let snapshot = fixture.read().expect("read cold SQLite snapshot");

    assert_eq!(snapshot.records.len(), 2);
    assert_eq!(snapshot.records[0].event_sequence, 0);
    assert_eq!(snapshot.records[1].event_sequence, 1);
    assert_eq!(snapshot.records[0].content.meaningful_text(), "first");
    assert_eq!(snapshot.records[1].content.meaningful_text(), "second");
    assert_eq!(snapshot.receipt.counts.complete_records, 2);
    assert_eq!(snapshot.receipt.counts.indexed_documents, 2);
}

#[test]
fn wal_snapshot_reads_uncheckpointed_commits() {
    let fixture = Fixture::new("wal-agent", true);
    insert_active(
        &fixture.connection,
        "wal-session",
        1,
        0,
        "wal-event",
        "visible from WAL",
    );
    let wal_path = PathBuf::from(format!("{}-wal", fixture.path.display()));
    assert!(
        wal_path.is_file(),
        "fixture must retain an uncheckpointed WAL"
    );

    let snapshot = fixture.read().expect("read WAL-aware SQLite snapshot");

    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(
        snapshot.records[0].content.meaningful_text(),
        "visible from WAL"
    );
}

#[test]
fn append_preserves_source_session_and_existing_event_identity() {
    let fixture = Fixture::new("append-agent", true);
    insert_active(&fixture.connection, "session-a", 1, 0, "event-a", "one");
    let before = fixture.read().expect("read initial snapshot");

    insert_active(&fixture.connection, "session-a", 2, 1, "event-b", "two");
    let after = fixture.read().expect("read appended snapshot");

    assert!(before.source.exact_descriptor_eq(&after.source));
    assert_eq!(before.records[0].session_id, after.records[0].session_id);
    assert_eq!(before.records[0].event_id, after.records[0].event_id);
    assert_eq!(after.records.len(), 2);
    assert_ne!(before.receipt.content_digest, after.receipt.content_digest);
}

#[test]
fn rewrite_keeps_native_identity_while_replacing_content() {
    let fixture = Fixture::new("rewrite-agent", false);
    insert_active(&fixture.connection, "session-a", 1, 0, "event-a", "before");
    let before = fixture.read().expect("read initial snapshot");

    rewrite_active(&fixture.connection, "session-a", 1, "event-a", "after");
    let after = fixture.read().expect("read rewritten snapshot");

    assert_eq!(before.records[0].session_id, after.records[0].session_id);
    assert_eq!(before.records[0].event_id, after.records[0].event_id);
    assert_eq!(after.records[0].content.meaningful_text(), "after");
    assert_ne!(before.receipt.content_digest, after.receipt.content_digest);
}

#[test]
fn complete_native_content_is_preserved_or_the_row_is_rejected() {
    let fixture = Fixture::new("content-agent", false);
    let retained = format!("{}tail-marker", "x".repeat(70 * 1024));
    insert_active(&fixture.connection, "session-a", 1, 0, "event-a", &retained);
    let snapshot = fixture.read().expect("read complete long event");
    assert_eq!(
        snapshot.records[0].content.normalized_body.as_deref(),
        Some(retained.as_str())
    );
    assert_eq!(
        snapshot.records[0]
            .content
            .structured_content
            .as_ref()
            .and_then(|event| event.pointer("/message/content"))
            .and_then(serde_json::Value::as_str),
        Some(retained.as_str())
    );

    let oversized = "z".repeat(ctx_history_core::MAX_CORE_CONTENT_BYTES / 2 + 1);
    rewrite_active(&fixture.connection, "session-a", 1, "event-a", &oversized);
    assert!(matches!(
        fixture
            .read()
            .expect_err("aggregate oversized native row must be rejected"),
        OpenClawSqliteError::Capture(CaptureError::InvalidPayload(_))
    ));
}

#[test]
fn empty_lexical_event_retains_native_json_without_synthesized_text_or_scope() {
    let fixture = Fixture::new("empty-agent", false);
    insert_active(&fixture.connection, "session-a", 1, 0, "event-a", "seed");
    let event = json!({
        "type": "custom_status",
        "id": "event-a",
        "timestamp": 1_700_000_000_001_i64,
        "status": {"phase": "waiting", "detail": 7}
    });
    rewrite_active_event(&fixture.connection, "session-a", 1, &event);

    let snapshot = fixture.read().expect("read empty lexical event");
    let record = &snapshot.records[0];
    assert_eq!(record.content.normalized_body, None);
    assert_eq!(record.content.structured_content.as_ref(), Some(&event));
    assert_eq!(record.agent_scope, None);
}

#[test]
fn tool_only_events_retain_exact_structure_without_invented_message_text() {
    let fixture = Fixture::new("tool-only-agent", false);
    insert_active(&fixture.connection, "session-a", 1, 0, "call-event", "seed");
    insert_active(
        &fixture.connection,
        "session-a",
        2,
        1,
        "result-event",
        "seed",
    );
    let call = json!({
        "type": "message",
        "id": "call-event",
        "timestamp": 1_700_000_000_001_i64,
        "message": {
            "role": "assistant",
            "content": [{
                "type": "toolCall",
                "id": "call-1",
                "name": "read_file",
                "arguments": {"path": "notes.txt", "offset": 4}
            }]
        }
    });
    let result = json!({
        "type": "message",
        "id": "result-event",
        "timestamp": 1_700_000_000_002_i64,
        "message": {
            "role": "toolResult",
            "content": [{
                "type": "tool_result",
                "toolCallId": "call-1",
                "result": {"bytes": 12, "ok": true}
            }]
        }
    });
    rewrite_active_event(&fixture.connection, "session-a", 1, &call);
    rewrite_active_event(&fixture.connection, "session-a", 2, &result);

    let snapshot = fixture.read().expect("read tool-only native events");
    assert_eq!(snapshot.records.len(), 2);
    for (record, event) in snapshot.records.iter().zip([&call, &result]) {
        assert_eq!(record.content.normalized_body, None);
        assert_eq!(record.content.structured_content.as_ref(), Some(event));
    }
}

#[test]
fn reset_and_deleted_archives_are_generation_qualified_sessions() {
    let fixture = Fixture::new("archive-agent", false);
    insert_active(
        &fixture.connection,
        "current-session",
        1,
        0,
        "current-event",
        "current",
    );
    insert_archive(
        &fixture.connection,
        "old-session",
        "generation-reset",
        "reset",
        "identity",
        "old-reset-event",
        "before reset",
    );
    insert_archive(
        &fixture.connection,
        "old-session",
        "generation-deleted",
        "deleted",
        "zstd",
        "old-deleted-event",
        "before deletion",
    );

    let snapshot = fixture
        .read()
        .expect("read active and archived generations");

    assert_eq!(snapshot.records.len(), 3);
    let sessions = snapshot
        .records
        .iter()
        .map(|record| record.session_id.digest())
        .collect::<BTreeSet<_>>();
    assert_eq!(sessions.len(), 3);
    let provider_sessions = snapshot
        .records
        .iter()
        .filter_map(|record| record.provider_session_id.as_deref())
        .collect::<BTreeSet<_>>();
    assert!(provider_sessions.contains("current-session"));
    assert_eq!(provider_sessions.len(), 1);
    assert!(
        snapshot
            .records
            .iter()
            .filter(|record| {
                record.provider_session_id.is_none()
                    && record.content.meaningful_text().starts_with("before ")
            })
            .count()
            == 2
    );
}

#[test]
fn unsupported_archive_is_typed_and_never_certified_as_complete() {
    let fixture = Fixture::new("bounded-agent", false);
    insert_archive(
        &fixture.connection,
        "old-session",
        "large-generation",
        "reset",
        "identity",
        "large-event",
        "larger than the test decode limit",
    );

    let error = read_path(fixture.root.path(), &fixture.path, 8)
        .expect_err("oversized archive must not be silently omitted");

    assert!(matches!(
        &error,
        OpenClawSqliteError::UnsupportedArchive {
            ref session_id,
            ref generation,
            ..
        } if session_id == "old-session" && generation == "large-generation"
    ));
}

#[test]
fn corrupt_and_over_limit_zstd_archives_are_typed_unsupported() {
    let corrupt = Fixture::new("corrupt-zstd-agent", false);
    insert_raw_archive(
        &corrupt.connection,
        "old-session",
        "corrupt-generation",
        "zstd",
        b"not a zstd frame".to_vec(),
    );
    assert!(matches!(
        corrupt
            .read()
            .expect_err("corrupt zstd must not produce partial history"),
        OpenClawSqliteError::UnsupportedArchive { .. }
    ));

    let oversized = Fixture::new("oversized-zstd-agent", false);
    let line = format!(
        "{}\n",
        json!({
            "type": "message",
            "id": "large-event",
            "message": {"role": "assistant", "content": "x".repeat(4096)}
        })
    );
    let compressed = zstd::stream::encode_all(Cursor::new(line), 1).expect("compress fixture");
    insert_raw_archive(
        &oversized.connection,
        "old-session",
        "oversized-generation",
        "zstd",
        compressed,
    );
    assert!(matches!(
        read_path(oversized.root.path(), &oversized.path, 128)
            .expect_err("over-limit zstd must not produce partial history"),
        OpenClawSqliteError::UnsupportedArchive { .. }
    ));
}

#[test]
fn schema_version_and_owner_must_match_the_current_agent_database() {
    let version = Fixture::new("version-agent", false);
    version
        .connection
        .execute_batch("PRAGMA user_version=16;")
        .expect("downgrade fixture version");
    assert!(matches!(
        version.read().expect_err("schema mismatch must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));

    let owner = Fixture::new("path-agent", false);
    owner
        .connection
        .execute(
            "UPDATE schema_meta SET agent_id = 'different-agent' WHERE meta_key = 'primary'",
            [],
        )
        .expect("change fixture owner");
    assert!(matches!(
        owner.read().expect_err("owner mismatch must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));

    let shape = Fixture::new("shape-agent", false);
    shape
        .connection
        .execute_batch("DROP TABLE session_transcript_archives;")
        .expect("remove required fixture table");
    assert!(matches!(
        shape.read().expect_err("required table mismatch must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));
}

#[test]
fn schema_affinity_nullability_keys_and_indexes_must_match_v17_exactly() {
    let affinity_schema =
        SCHEMA.replacen("event_json TEXT NOT NULL", "event_json BLOB NOT NULL", 1);
    let affinity = Fixture::with_schema("affinity-agent", false, &affinity_schema);
    assert!(matches!(
        affinity.read().expect_err("wrong affinity must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));

    let nullable_schema = SCHEMA.replacen(
        "created_at INTEGER NOT NULL, PRIMARY KEY (session_id, seq)",
        "created_at INTEGER, PRIMARY KEY (session_id, seq)",
        1,
    );
    let nullable = Fixture::with_schema("nullable-agent", false, &nullable_schema);
    assert!(matches!(
        nullable.read().expect_err("wrong nullability must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));

    let key_schema = SCHEMA.replacen(
        "PRIMARY KEY (session_id, seq)",
        "UNIQUE (session_id, seq)",
        1,
    );
    let key = Fixture::with_schema("key-agent", false, &key_schema);
    assert!(matches!(
        key.read().expect_err("wrong key origin must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));

    let index = Fixture::new("index-agent", false);
    index
        .connection
        .execute_batch("DROP INDEX idx_agent_transcript_active_messages;")
        .expect("remove required v17 index");
    assert!(matches!(
        index.read().expect_err("missing exact index must fail"),
        OpenClawSqliteError::Capture(CaptureError::UnsupportedSchema(_))
    ));
}

#[test]
fn stale_active_projection_is_not_certified_as_complete() {
    let fixture = Fixture::new("stale-agent", false);
    insert_active(&fixture.connection, "session-a", 1, 0, "event-a", "stale");
    fixture
        .connection
        .execute(
            "UPDATE session_transcript_index_state SET needs_rebuild = 1",
            [],
        )
        .expect("mark fixture projection stale");

    assert!(matches!(
        fixture
            .read()
            .expect_err("stale active projection must fail closed"),
        OpenClawSqliteError::Capture(CaptureError::InvalidPayload(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlink_database_leaf_is_rejected() {
    use std::os::unix::fs::symlink;

    let real = Fixture::new("symlink-agent", false);
    let link_root = tempfile::tempdir().expect("create symlink root");
    let link_path = link_root
        .path()
        .join("agents")
        .join("symlink-agent")
        .join("agent")
        .join(DATABASE_LEAF);
    fs::create_dir_all(link_path.parent().expect("symlink parent")).expect("create symlink parent");
    symlink(&real.path, &link_path).expect("create database symlink");

    let error = read_path(link_root.path(), &link_path, MAX_ARCHIVE_DECODED_BYTES)
        .expect_err("symlink database must be rejected");

    assert!(
        matches!(
            &error,
            OpenClawSqliteError::Capture(CaptureError::InvalidProviderTranscriptPath { .. })
        ),
        "unexpected symlink rejection: {error:?}"
    );
}

#[test]
fn overlap_policy_requires_sqlite_to_suppress_legacy_jsonl_per_agent() {
    assert!(OPENCLAW_JSONL_SQLITE_OVERLAP_POLICY.contains("suppresses legacy OpenClaw JSONL"));
    assert_ne!(
        OPENCLAW_AGENT_SQLITE_SOURCE_FORMAT,
        ctx_history_providers_jsonl_shared::OPENCLAW_SOURCE_FORMAT
    );
}

const SCHEMA: &str = r#"
PRAGMA user_version=17;
CREATE TABLE schema_meta (
  meta_key TEXT NOT NULL PRIMARY KEY, role TEXT NOT NULL, schema_version INTEGER NOT NULL,
  agent_id TEXT, app_version TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE session_windows (
  session_id TEXT NOT NULL PRIMARY KEY, session_key TEXT NOT NULL, previous_session_id TEXT,
  reason TEXT, session_scope TEXT NOT NULL DEFAULT 'conversation', created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL, transcript_updated_at INTEGER DEFAULT NULL,
  transcript_observed_at INTEGER DEFAULT NULL,
  session_entry_provenance INTEGER NOT NULL DEFAULT 0, acp_owned INTEGER NOT NULL DEFAULT 0,
  plugin_owner_id TEXT, hook_external_content_source TEXT, started_at INTEGER, ended_at INTEGER,
  status TEXT, chat_type TEXT, channel TEXT, account_id TEXT, primary_conversation_id TEXT,
  model_provider TEXT, model TEXT, agent_harness_id TEXT, parent_session_key TEXT, spawned_by TEXT,
  display_name TEXT
) STRICT;
CREATE INDEX idx_agent_session_windows_updated_at
  ON session_windows(updated_at DESC, session_id);
CREATE INDEX idx_agent_session_windows_created_at
  ON session_windows(created_at DESC, session_id);
CREATE INDEX idx_agent_session_windows_conversation
  ON session_windows(primary_conversation_id, updated_at DESC, session_id)
  WHERE primary_conversation_id IS NOT NULL;
CREATE TABLE transcript_events (
  session_id TEXT NOT NULL, seq INTEGER NOT NULL, event_json TEXT NOT NULL,
  created_at INTEGER NOT NULL, PRIMARY KEY (session_id, seq)
) STRICT;
CREATE TABLE session_transcript_archives (
  session_id TEXT NOT NULL, generation TEXT NOT NULL, session_key TEXT NOT NULL,
  reason TEXT NOT NULL, encoding TEXT NOT NULL, archive_blob BLOB NOT NULL,
  archive_sha256 TEXT NOT NULL, archive_name TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL,
  published_at INTEGER, publish_attempts INTEGER NOT NULL DEFAULT 0,
  last_publish_attempt_at INTEGER, last_publish_error TEXT,
  PRIMARY KEY (session_id, generation)
) STRICT;
CREATE INDEX idx_agent_session_transcript_archives_pending
  ON session_transcript_archives(created_at, session_id, generation)
  WHERE published_at IS NULL;
CREATE INDEX idx_agent_session_transcript_archives_retention
  ON session_transcript_archives(created_at, session_id, generation);
CREATE TABLE transcript_event_identities (
  session_id TEXT NOT NULL, event_id TEXT NOT NULL, seq INTEGER NOT NULL, event_type TEXT,
  parent_id TEXT, message_idempotency_key TEXT, created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, event_id)
) STRICT;
CREATE UNIQUE INDEX idx_agent_transcript_message_idempotency
  ON transcript_event_identities(session_id, message_idempotency_key)
  WHERE message_idempotency_key IS NOT NULL;
CREATE INDEX idx_agent_transcript_event_parent
  ON transcript_event_identities(session_id, parent_id)
  WHERE parent_id IS NOT NULL;
CREATE INDEX idx_agent_transcript_event_sequence
  ON transcript_event_identities(session_id, event_type, seq DESC);
CREATE TABLE session_transcript_index_state (
  session_id TEXT NOT NULL PRIMARY KEY, indexed_seq INTEGER NOT NULL, leaf_event_id TEXT,
  needs_rebuild INTEGER NOT NULL DEFAULT 0, active_event_count INTEGER NOT NULL DEFAULT 0,
  active_message_count INTEGER NOT NULL DEFAULT 0, updated_at INTEGER NOT NULL
) STRICT;
CREATE TABLE session_transcript_active_events (
  session_id TEXT NOT NULL, active_position INTEGER NOT NULL, event_seq INTEGER NOT NULL,
  message_position INTEGER, PRIMARY KEY (session_id, active_position)
) STRICT;
CREATE UNIQUE INDEX idx_agent_transcript_active_event_seq
  ON session_transcript_active_events(session_id, event_seq);
CREATE UNIQUE INDEX idx_agent_transcript_active_messages
  ON session_transcript_active_events(session_id, message_position)
  WHERE message_position IS NOT NULL;
"#;
