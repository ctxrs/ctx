use crate::PROVIDER_MAX_TEXT_CHARS;
use rusqlite::Connection;
use serde_json::json;
use std::path::PathBuf;
use tempfile::TempDir;

pub(super) fn write_shelley_adversarial_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley-adversarial.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (
                conversation_id text primary key,
                slug text,
                user_initiated boolean not null default true,
                created_at datetime not null default current_timestamp,
                updated_at datetime not null default current_timestamp,
                cwd text,
                archived boolean not null default false,
                parent_conversation_id text,
                model text,
                conversation_options text not null default '{}',
                current_generation integer not null default 1,
                agent_working boolean not null default false,
                tags text not null default '[]',
                is_draft boolean not null default false,
                draft text not null default '',
                queued_messages text not null default '[]'
            );
            create table messages (
                message_id text primary key,
                conversation_id text not null,
                sequence_id integer not null,
                type text not null,
                llm_data text,
                user_data text,
                usage_data text,
                created_at datetime not null default current_timestamp,
                display_data text,
                excluded_from_context boolean not null default false,
                generation integer not null default 1,
                llm_api_url text,
                model_name text,
                forked_from_message_id text
            );
            create index idx_messages_conversation_sequence
                on messages(conversation_id, sequence_id);",
    )
    .unwrap();
    conn.execute(
        "insert into conversations values (
                'shelley-adversarial', 'adversarial', 1, '2026-06-24 12:00:00',
                '2026-06-24 12:05:00', '/workspace/shelley', 0, null,
                'claude-opus-4-7', '{}', 1, 0, '[]', 0, '', '[]'
            )",
        [],
    )
    .unwrap();
    for (message_id, sequence_id, message_type, text) in [
        ("msg-dup-a", 1, "user", "duplicate sequence first"),
        ("msg-dup-b", 1, "user", "duplicate sequence second"),
        ("msg-git", 2, "gitinfo", "commit abc touched shelley.rs"),
        ("msg-warning", 3, "warning", "warning message for Shelley"),
    ] {
        conn.execute(
            "insert into messages (
                    message_id, conversation_id, sequence_id, type, user_data, created_at
                ) values (?1, 'shelley-adversarial', ?2, ?3, ?4, '2026-06-24 12:00:01')",
            rusqlite::params![
                message_id,
                sequence_id,
                message_type,
                json!({"Content": [{"Type": 2, "Text": text}]}).to_string(),
            ],
        )
        .unwrap();
    }
    conn.execute(
        "insert into messages (
                message_id, conversation_id, sequence_id, type, llm_data, created_at
            ) values ('msg-large', 'shelley-adversarial', 4, 'agent', ?1, '2026-06-24 12:00:04')",
        [json!({
            "Content": [
                {"Type": 2, "Text": "x".repeat(PROVIDER_MAX_TEXT_CHARS + 200)}
            ]
        })
        .to_string()],
    )
    .unwrap();
    path
}

pub(super) fn write_shelley_malformed_db(temp: &TempDir) -> PathBuf {
    let path = temp.path().join("shelley-malformed.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "create table conversations (conversation_id text primary key);
             create table messages (
                message_id text primary key,
                conversation_id text not null
             );",
    )
    .unwrap();
    path
}
