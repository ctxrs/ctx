use ctx_history_core::CaptureProvider;
use rusqlite::Connection;
use serde_json::json;

use crate::captured_batch::SourceObservation;
use crate::GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT;

use super::{GOOSE_CAPTURE_REVISION, GOOSE_POLICY_REVISION};

pub(super) fn create_goose_tables(conn: &Connection) {
    conn.execute_batch(
        "create table schema_version (version integer not null);
         insert into schema_version values (14);
         create table sessions (
            id text primary key,
            name text,
            description text,
            user_set_name integer not null default 0,
            session_type text,
            working_dir text,
            created_at text,
            updated_at text,
            extension_data text,
            total_tokens integer,
            input_tokens integer,
            output_tokens integer,
            accumulated_total_tokens integer,
            accumulated_input_tokens integer,
            accumulated_output_tokens integer,
            accumulated_cost real,
            provider_name text,
            model_config_json text,
            goose_mode text,
            archived_at text,
            project_id text
         );
         create table messages (
            id integer primary key,
            message_id text,
            session_id text not null,
            role text not null,
            content_json text not null,
            created_timestamp integer,
            timestamp text,
            tokens text,
            metadata_json text
         );",
    )
    .unwrap();
}

pub(super) fn insert_session(conn: &Connection, id: &str) {
    conn.execute(
        "insert into sessions (
            id, name, user_set_name, session_type, working_dir, created_at, updated_at,
            extension_data, total_tokens, accumulated_cost, provider_name, model_config_json,
            goose_mode, project_id
         ) values (
            ?1, ?2, 1, 'chat', '/workspace/goose', '2026-07-18 00:00:00',
            '2026-07-18 00:01:00', '{\"extension\":\"ctx\"}', 7, 0.25,
            'test-provider', '{\"model_name\":\"test\"}', 'auto', 'goose-project'
         )",
        rusqlite::params![id, format!("Session {id}")],
    )
    .unwrap();
}

pub(super) fn insert_message(conn: &Connection, id: i64, session_id: &str, text: &str) {
    conn.execute(
        "insert into messages (
            id, message_id, session_id, role, content_json, created_timestamp,
            timestamp, tokens, metadata_json
         ) values (?1, ?2, ?3, 'user', ?4, ?5, '2026-07-18 00:00:01',
                   '{\"input\":1}', '{\"source\":\"test\"}')",
        rusqlite::params![
            id,
            format!("message-{id}"),
            session_id,
            json!([{"type": "text", "text": text}]).to_string(),
            1_784_332_800_i64.saturating_add(id),
        ],
    )
    .unwrap();
}

pub(super) fn test_source(revision: &str) -> SourceObservation {
    SourceObservation::new(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        "goose-sqlite:test",
        revision,
        "provider:goose:test",
        GOOSE_CAPTURE_REVISION,
        GOOSE_POLICY_REVISION,
        None,
    )
    .unwrap()
}

mod persistence;
mod position;
mod projection;
mod schema;
mod source;
mod stream;
