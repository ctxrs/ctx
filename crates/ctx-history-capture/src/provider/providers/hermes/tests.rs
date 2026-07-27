use rusqlite::{limits::Limit as SqliteLimit, Connection};

use super::layout::{decode_hermes_message, decode_hermes_session, HermesSchema};
use super::sqlite::{
    decode_hermes_position, decode_hermes_storage_rejection, encode_hermes_position,
    hermes_locator, hermes_message_candidate_sql, hermes_session_candidate_sql, HermesKeyset,
    HermesPhase, HermesRowFetcher,
};
use super::*;
use crate::captured_batch::{CapturedRecordPayload, CAPTURE_BATCH_MAX_RECORDS};

#[test]
fn hermes_result_content_uses_only_the_tool_content_column_without_a_size_cap() {
    let long = "x".repeat(crate::PROVIDER_MAX_TEXT_CHARS + 19);
    assert_eq!(
        hermes_normalized_result_content("tool", &Value::String(long.clone())),
        Some(long)
    );
    assert_eq!(
        hermes_normalized_result_content("assistant", &Value::String("not a result".into())),
        None
    );
    assert_eq!(hermes_normalized_result_content("tool", &Value::Null), None);
}

fn hermes_explain_resume_plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut statement = conn.prepare(&format!("explain query plan {sql}")).unwrap();
    statement
        .query_map([2_047_i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn hermes_record_layout_drives_projection_hydration_and_named_decode() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key, source text not null, parent_session_id text, model text,
            model_config text, started_at real not null, ended_at real, end_reason text,
            message_count integer, tool_call_count integer, input_tokens integer,
            output_tokens integer, cache_read_tokens integer, cache_write_tokens integer,
            reasoning_tokens integer, cwd text, git_branch text, git_repo_root text,
            billing_provider text, billing_base_url text, billing_mode text,
            estimated_cost_usd real, actual_cost_usd real, title text, archived integer
        );
        create table messages (
            id integer primary key, session_id text not null, role text not null, content text,
            tool_call_id text, tool_calls text, tool_name text, timestamp real not null,
            token_count integer, finish_reason text, reasoning text, reasoning_content text,
            reasoning_details text, codex_reasoning_items text, codex_message_items text,
            platform_message_id text, observed integer, active integer, compacted integer
        );
        insert into sessions values (
            'session-id', 'acp', 'parent-id', 'model-id', '{\"temperature\":0.25}',
            1782259200.5, 1782259300.75, 'done', 8, 9, 10, 11, 12, 13, 14,
            '/workspace', 'main', '/repo', 'provider', 'https://billing.invalid', 'token',
            1.25, 2.5, 'title', 1
        );
        insert into messages values (
            41, 'session-id', 'assistant', 'content', 'call-id', '[{\"name\":\"read\"}]',
            'read', 1782259201.5, 15, 'stop', 'reasoning', 'reasoning-content',
            '[{\"detail\":1}]', '[{\"reasoning\":1}]', '[{\"message\":1}]',
            'platform-id', 1, 0, 1
        );",
    )
    .unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();

    assert_eq!(
        schema.sessions().field_names(),
        [
            "id",
            "source",
            "parent_session_id",
            "model",
            "model_config",
            "started_at",
            "ended_at",
            "end_reason",
            "message_count",
            "tool_call_count",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "cwd",
            "git_branch",
            "git_repo_root",
            "billing_provider",
            "billing_base_url",
            "billing_mode",
            "estimated_cost_usd",
            "actual_cost_usd",
            "title",
            "archived",
        ]
    );
    assert_eq!(
        schema.messages().field_names(),
        [
            "id",
            "session_id",
            "role",
            "content",
            "tool_call_id",
            "tool_calls",
            "tool_name",
            "timestamp",
            "token_count",
            "finish_reason",
            "reasoning",
            "reasoning_content",
            "reasoning_details",
            "codex_reasoning_items",
            "codex_message_items",
            "platform_message_id",
            "observed",
            "active",
            "compacted",
        ]
    );

    let session_values = conn
        .query_row(
            &format!("select {} from sessions s", schema.sessions().projection()),
            [],
            |row| schema.sessions().capture_values(row, 0),
        )
        .unwrap();
    let session = decode_hermes_session(&schema, &session_values, 0).unwrap();
    assert_eq!(session.id, "session-id");
    assert_eq!(session.source, "acp");
    assert_eq!(session.parent_session_id.as_deref(), Some("parent-id"));
    assert_eq!(session.model.as_deref(), Some("model-id"));
    assert_eq!(
        session.model_config.as_deref(),
        Some("{\"temperature\":0.25}")
    );
    assert_eq!(session.started_at, 1782259200.5);
    assert_eq!(session.ended_at, Some(1782259300.75));
    assert_eq!(session.end_reason.as_deref(), Some("done"));
    assert_eq!(session.message_count, 8);
    assert_eq!(session.tool_call_count, 9);
    assert_eq!(session.input_tokens, 10);
    assert_eq!(session.output_tokens, 11);
    assert_eq!(session.cache_read_tokens, 12);
    assert_eq!(session.cache_write_tokens, 13);
    assert_eq!(session.reasoning_tokens, 14);
    assert_eq!(session.cwd.as_deref(), Some("/workspace"));
    assert_eq!(session.git_branch.as_deref(), Some("main"));
    assert_eq!(session.git_repo_root.as_deref(), Some("/repo"));
    assert_eq!(session.billing_provider.as_deref(), Some("provider"));
    assert_eq!(
        session.billing_base_url.as_deref(),
        Some("https://billing.invalid")
    );
    assert_eq!(session.billing_mode.as_deref(), Some("token"));
    assert_eq!(session.estimated_cost_usd, Some(1.25));
    assert_eq!(session.actual_cost_usd, Some(2.5));
    assert_eq!(session.title.as_deref(), Some("title"));
    assert_eq!(session.archived, 1);

    let message_values = conn
        .query_row(
            &format!("select {} from messages m", schema.messages().projection()),
            [],
            |row| schema.messages().capture_values(row, 0),
        )
        .unwrap();
    let message = decode_hermes_message(&schema, &message_values).unwrap();
    assert_eq!(message.id, 41);
    assert_eq!(message.session_id, "session-id");
    assert_eq!(message.role, "assistant");
    assert_eq!(message.content.as_deref(), Some("content"));
    assert_eq!(message.tool_call_id.as_deref(), Some("call-id"));
    assert_eq!(message.tool_calls.as_deref(), Some("[{\"name\":\"read\"}]"));
    assert_eq!(message.tool_name.as_deref(), Some("read"));
    assert_eq!(message.timestamp, 1782259201.5);
    assert_eq!(message.token_count, Some(15));
    assert_eq!(message.finish_reason.as_deref(), Some("stop"));
    assert_eq!(message.reasoning.as_deref(), Some("reasoning"));
    assert_eq!(
        message.reasoning_content.as_deref(),
        Some("reasoning-content")
    );
    assert_eq!(
        message.reasoning_details.as_deref(),
        Some("[{\"detail\":1}]")
    );
    assert_eq!(
        message.codex_reasoning_items.as_deref(),
        Some("[{\"reasoning\":1}]")
    );
    assert_eq!(
        message.codex_message_items.as_deref(),
        Some("[{\"message\":1}]")
    );
    assert_eq!(message.platform_message_id.as_deref(), Some("platform-id"));
    assert_eq!(message.observed, 1);
    assert_eq!(message.active, 0);
    assert_eq!(message.compacted, 1);
    assert_eq!(schema.sessions().rejected_column(2).unwrap(), "source");
    assert_eq!(schema.messages().rejected_column(19).unwrap(), "compacted");
}

#[test]
fn hermes_position_and_locator_bytes_remain_stable() {
    let initial = initial_hermes_position().unwrap();
    assert_eq!(initial.kind(), "hermes-sqlite-keyset-v1");
    assert_eq!(initial.value(), [0]);

    let position = encode_hermes_position(HermesKeyset {
        phase: HermesPhase::Messages,
        next_ordinal: 0x0102_0304_0506_0708,
        rowid: -2,
    })
    .unwrap();
    assert_eq!(position.kind(), "hermes-sqlite-keyset-v1");
    assert_eq!(
        position.value(),
        [2, 1, 2, 3, 4, 5, 6, 7, 8, 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,]
    );

    let locator = hermes_locator(HermesPhase::Sessions, -2).unwrap();
    assert_eq!(locator.kind(), "hermes-sqlite-row-v1");
    assert_eq!(
        locator.value(),
        [1, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe]
    );
}

#[test]
fn hermes_releases_provider_read_snapshot_after_each_batch() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            started_at real not null
        );
        create table messages (
            id integer primary key,
            session_id text not null,
            role text not null,
            timestamp real not null
        );",
    )
    .unwrap();
    for index in 0..=CAPTURE_BATCH_MAX_RECORDS {
        conn.execute(
            "insert into sessions values (?1, 'acp', ?2)",
            rusqlite::params![format!("session-{index}"), 1_782_259_200.0 + index as f64],
        )
        .unwrap();
    }
    let schema = HermesSchema::detect(&conn).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        "hermes-sqlite:batch-snapshot-test",
        "hermes-snapshot:batch-snapshot-test",
        "provider:hermes:batch-snapshot-test",
        HERMES_CAPTURE_REVISION,
        HERMES_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut fetcher = HermesRowFetcher::new(&conn, &schema).unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_hermes_position().unwrap(),
        move |position| fetcher.fetch(position),
    );

    let first = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(hermes_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(first.records().len(), CAPTURE_BATCH_MAX_RECORDS);
    assert!(conn.is_autocommit());

    let second = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(hermes_sqlite_batch_error)
    })
    .unwrap()
    .unwrap();
    assert_eq!(second.records().len(), 1);
    assert!(conn.is_autocommit());

    let exhausted = with_sqlite_read_snapshot(&conn, || {
        producer.next_batch().map_err(hermes_sqlite_batch_error)
    })
    .unwrap();
    assert!(exhausted.is_none());
    assert!(conn.is_autocommit());
}

#[test]
fn hermes_alternating_children_never_rehydrate_session_payloads() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table sessions (id text primary key, source text not null, started_at real not null);
         create table messages (id integer primary key, session_id text not null, role text not null, content text, timestamp real not null);
         insert into sessions values ('session-a', 'acp', 1782259200.0);
         insert into sessions values ('session-b', 'acp', 1782259201.0);",
    )
    .unwrap();
    for id in 1..=70_i64 {
        conn.execute(
            "insert into messages values (?1, ?2, 'assistant', ?3, 1782259202.0)",
            rusqlite::params![
                id,
                if id % 2 == 0 {
                    "session-a"
                } else {
                    "session-b"
                },
                format!("message-{id}")
            ],
        )
        .unwrap();
    }
    let schema = HermesSchema::detect(&conn).unwrap();
    let mut fetcher = HermesRowFetcher::new(&conn, &schema).unwrap();
    let mut position = initial_hermes_position().unwrap();
    let mut logical_rows = 0;
    while let Some(row) = fetcher.fetch(position.clone()).unwrap() {
        position = row.next_position().clone();
        logical_rows += 1;
    }

    assert_eq!(logical_rows, 72);
    assert_eq!(fetcher.session_hydration_queries, 2);
    let keyset = decode_hermes_position(&position).unwrap().unwrap();
    assert!(keyset.phase == HermesPhase::Messages);
    assert_eq!(keyset.rowid, 70);
}

#[test]
fn hermes_resume_candidates_use_native_rowid_search_near_tail() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            started_at real not null
        );
        create table messages (
            id integer primary key,
            session_id text not null,
            role text not null,
            content text,
            timestamp real not null,
            active integer not null default 1,
            compacted integer not null default 0
        );
        with recursive counter(value) as (
            values(1) union all select value + 1 from counter where value < 2048
        )
        insert into sessions(id, source, started_at)
        select printf('session-%05d', value), 'acp', 1782259200.0 + value
        from counter;
        with recursive counter(value) as (
            values(1) union all select value + 1 from counter where value < 2048
        )
        insert into messages(id, session_id, role, content, timestamp)
        select value, 'session-00001', 'assistant', printf('message-%05d', value),
               1782259200.0 + value
        from counter;",
    )
    .unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    let session_resume_sql = hermes_session_candidate_sql(
        &schema.sessions().retained_length_expr(),
        &schema.sessions().storage_class_error_expr(),
        true,
    );
    let message_resume_sql = hermes_message_candidate_sql(
        &schema.messages().retained_length_expr(),
        &schema.messages().storage_class_error_expr(),
        schema.message_visibility(),
        true,
    );
    assert!(!session_resume_sql.contains("?1 = 0"));
    assert!(!message_resume_sql.contains("?1 = 0"));
    let session_plan = hermes_explain_resume_plan(&conn, &session_resume_sql);
    assert!(session_plan.iter().any(|detail| {
        detail.contains("SEARCH s USING INTEGER PRIMARY KEY") && detail.contains("rowid>?")
    }));
    let message_plan = hermes_explain_resume_plan(&conn, &message_resume_sql);
    assert!(message_plan.iter().any(|detail| {
        detail.contains("SEARCH m USING INTEGER PRIMARY KEY") && detail.contains("rowid>?")
    }));

    let mut fetcher = HermesRowFetcher::new(&conn, &schema).unwrap();
    let session_position = encode_hermes_position(HermesKeyset {
        phase: HermesPhase::Sessions,
        next_ordinal: 2_047,
        rowid: 2_047,
    })
    .unwrap();
    let session = fetcher.fetch(session_position).unwrap().unwrap();
    assert_eq!(
        decode_hermes_position(session.next_position())
            .unwrap()
            .unwrap()
            .rowid,
        2_048
    );
    let message_position = encode_hermes_position(HermesKeyset {
        phase: HermesPhase::Messages,
        next_ordinal: 4_095,
        rowid: 2_047,
    })
    .unwrap();
    let message = fetcher.fetch(message_position).unwrap().unwrap();
    let message_keyset = decode_hermes_position(message.next_position())
        .unwrap()
        .unwrap();
    assert!(message_keyset.phase == HermesPhase::Messages);
    assert_eq!(message_keyset.rowid, 2_048);
}

#[test]
fn hermes_preflight_is_integer_metadata_only_and_rejects_bad_storage_under_cap() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table sessions (
            id text primary key,
            source text not null,
            started_at real not null
        );
        create table messages (
            id integer primary key,
            session_id text not null,
            role text not null,
            timestamp real not null
        );
        insert into sessions values ('bad', x'0102', 1782259200.0);
        insert into sessions values ('healthy', 'acp', 1782259201.0);",
    )
    .unwrap();
    let schema = HermesSchema::detect(&conn).unwrap();
    for retained_lengths in [
        schema.sessions().retained_length_expr(),
        schema.messages().retained_length_expr(),
    ] {
        assert!(retained_lengths.contains("octet_length("));
        assert!(!retained_lengths.to_ascii_lowercase().contains("cast("));
        assert!(!retained_lengths
            .to_ascii_lowercase()
            .contains("length(cast"));
    }
    let capped_length = 1_024 * 1_024;
    conn.set_limit(SqliteLimit::SQLITE_LIMIT_LENGTH, capped_length);
    let mut fetcher = HermesRowFetcher::new(&conn, &schema).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        "hermes-sqlite:storage-class-test",
        "hermes-snapshot:storage-class-test",
        "provider:hermes:storage-class-test",
        HERMES_CAPTURE_REVISION,
        HERMES_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_hermes_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let batch = producer.next_batch().unwrap().unwrap();
    assert_eq!(batch.records().len(), 2);
    let malformed = &batch.records()[0];
    assert_eq!(
        malformed.record_kind().as_str(),
        HERMES_MALFORMED_RECORD_KIND
    );
    let CapturedRecordPayload::SqliteValues(values) = malformed.payload() else {
        panic!("malformed Hermes row must stay compact");
    };
    assert_eq!(
        decode_hermes_storage_rejection(&schema, values).unwrap(),
        "Hermes session source has an unsupported SQLite storage class"
    );
    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), capped_length);
    let healthy = &batch.records()[1];
    assert_eq!(healthy.record_kind().as_str(), HERMES_SESSION_RECORD_KIND);
    assert_eq!(conn.limit(SqliteLimit::SQLITE_LIMIT_LENGTH), capped_length);
}

#[test]
fn hermes_large_parent_and_many_large_children_remain_individually_representable() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "create table sessions (id text primary key, source text not null, model_config text, started_at real not null);
         create table messages (id integer primary key, session_id text not null, role text not null, content text, timestamp real not null);",
    )
    .unwrap();
    let parent_payload = "p".repeat(8 * 1024 * 1024);
    let child_payload = "c".repeat(8 * 1024 * 1024);
    conn.execute(
        "insert into sessions values ('session', 'acp', ?1, 1782259200.0)",
        [&parent_payload],
    )
    .unwrap();
    for id in 1..=3_i64 {
        conn.execute(
            "insert into messages values (?1, 'session', 'assistant', ?2, 1782259201.0)",
            rusqlite::params![id, &child_payload],
        )
        .unwrap();
    }
    let schema = HermesSchema::detect(&conn).unwrap();
    let mut fetcher = HermesRowFetcher::new(&conn, &schema).unwrap();
    let source = SourceObservation::new(
        CaptureProvider::Hermes,
        HERMES_SQLITE_SOURCE_FORMAT,
        "hermes-sqlite:large-parent",
        "hermes-snapshot:large-parent",
        "provider:hermes:large-parent",
        HERMES_CAPTURE_REVISION,
        HERMES_POLICY_REVISION,
        None,
    )
    .unwrap();
    let mut producer = SqliteLogicalRowBatchProducer::new(
        source,
        initial_hermes_position().unwrap(),
        move |position| fetcher.fetch(position),
    );
    let mut parents = 0;
    let mut messages = 0;
    while let Some(batch) = producer.next_batch().unwrap() {
        for record in batch.records() {
            let CapturedRecordPayload::SqliteValues(values) = record.payload() else {
                panic!("individually representable Hermes row was rejected as oversize");
            };
            match record.record_kind().as_str() {
                HERMES_SESSION_RECORD_KIND => {
                    assert_eq!(
                        decode_hermes_session(&schema, values, 0)
                            .unwrap()
                            .model_config
                            .as_deref(),
                        Some(parent_payload.as_str())
                    );
                    parents += 1;
                }
                HERMES_MESSAGE_RECORD_KIND => {
                    assert_eq!(values.len(), 19, "message record must remain child-local");
                    messages += 1;
                }
                kind => panic!("unexpected Hermes record kind {kind}"),
            }
        }
    }
    assert_eq!(parents, 1);
    assert_eq!(messages, 3);
}
