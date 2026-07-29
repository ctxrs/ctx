use std::{
    fs,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, HydrationFailureKind,
    LocatorRevisionPolicy, NativeRecordCoordinate, SourceRecordLocator, TypedKey,
};
use ctx_history_index::LexicalDocument;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::{
    provider::providers::{astrbot, firebender, kiro, lingma, opencode, trae},
    DiscoveryContext, DiscoveryPlatform, DiscoveryPlatformDirs, KIRO_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS,
};

mod compound;
mod firebender_warp;
mod ordinary;
mod row_contained;
mod security;

fn long_body(label: &str) -> String {
    format!(
        "{label}\nUnicode: 🦀 café 東京\nEscaped: \"quoted\" \\ slash\n{}",
        "x".repeat(PROVIDER_MAX_TEXT_CHARS + 1_024)
    )
}

fn event_request(document: &LexicalDocument) -> EventHydrationRequest {
    EventHydrationRequest::new(document.event_id, document.locator.clone()).unwrap()
}

fn locator_with_evidence(
    document: &LexicalDocument,
    coordinate: NativeRecordCoordinate,
    record_digest: [u8; 32],
) -> SourceRecordLocator {
    SourceRecordLocator::new(
        document.source.clone(),
        coordinate,
        document.locator.revision_policy(),
        document.locator.certified_source_revision_digest().copied(),
        record_digest,
    )
    .unwrap()
}

fn create_opencode_session_message_database(path: &Path, bodies: &[&str]) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table session (
             id text primary key,
             parent_id text,
             title text,
             directory text,
             branch text,
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
         insert into session (
             id, parent_id, title, directory, branch, agent, time_created, time_updated
         ) values (
             'session-root', null, 'Root', '/workspace/root', 'main', 'primary', 1, 2
         );",
    )
    .unwrap();
    for (sequence, body) in bodies.iter().enumerate() {
        let sequence = i64::try_from(sequence).unwrap();
        conn.execute(
            "insert into session_message (
                 id, session_id, type, seq, time_created, time_updated, data
             ) values (?1, 'session-root', 'message', ?2, ?3, ?3, ?4)",
            params![
                format!("message-{sequence}"),
                sequence,
                1_800_000_000_000_i64 + sequence,
                json!({
                    "role": if sequence % 2 == 0 { "user" } else { "assistant" },
                    "text": body,
                })
                .to_string(),
            ],
        )
        .unwrap();
    }
}

fn collect_opencode_documents(
    registration: opencode::OpenCodeSourceBackedRegistration,
    path: &Path,
) -> Vec<LexicalDocument> {
    let mut documents = Vec::new();
    registration
        .scan(path, &mut |page| {
            documents.extend(page);
            Ok(())
        })
        .unwrap();
    documents
}

fn sqlite_provider_directory_bytes(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let parent = path.parent().unwrap();
    let mut files: Vec<_> = fs::read_dir(parent)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (entry.path(), fs::read(entry.path()).unwrap())
        })
        .collect();
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

fn create_astrbot_database(path: &Path, session: &str, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "pragma user_version = 4;
         create table conversations (
             id integer primary key,
             inner_conversation_id text,
             conversation_id text,
             platform_id text,
             user_id text,
             content text not null,
             title text,
             persona_id text,
             token_usage text,
             created_at integer,
             updated_at integer
         );
         create table platform_message_history (
             id integer primary key,
             platform_id text,
             user_id text,
             sender_id text,
             sender_name text,
             content text,
             llm_checkpoint_id text,
             created_at integer
         );",
    )
    .unwrap();
    conn.execute(
        "insert into conversations (
             id, inner_conversation_id, conversation_id, platform_id, user_id,
             content, title, persona_id, token_usage, created_at, updated_at
         ) values (
             1, ?1, ?2, 'webchat', 'user', ?3, 'title', 'persona',
             '{\"prompt\":1,\"completion\":2}', 1780000000000, 1780000001000
         )",
        params![
            session,
            format!("conversation-{session}"),
            json!([{
                "id": format!("message-{session}"),
                "role": "user",
                "content": body,
            }])
            .to_string(),
        ],
    )
    .unwrap();
}

fn astrbot_discovery_context(home: &Path, cwd: &Path) -> DiscoveryContext {
    DiscoveryContext::new(
        home,
        cwd,
        DiscoveryPlatform::Linux,
        DiscoveryPlatformDirs::default(),
    )
}

fn create_lingma_database(path: &Path, body: &str) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "create table chat_record (
             session_id text not null,
             request_id text,
             chat_prompt text,
             summary text,
             error_result text,
             gmt_create integer,
             extra text
         );",
    )
    .unwrap();
    conn.execute(
        "insert into chat_record (
             session_id, request_id, chat_prompt, summary, error_result, gmt_create, extra
         ) values ('lingma-session', 'lingma-request', ?1, 'assistant summary', null,
                   1700000000, null)",
        [body],
    )
    .unwrap();
}

fn create_trae_database(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch("create table ItemTable (key text primary key, value);")
        .unwrap();
    conn.execute(
        "insert into ItemTable (key, value) values (?1, ?2)",
        params![
            trae::TRAE_CHAT_KEYS[0],
            json!({
                "list": [{
                    "id": "native-session",
                    "messages": [{
                        "id": "native-message",
                        "role": "user",
                        "content": body,
                        "createdAt": "2026-07-28T12:00:00Z",
                    }],
                }],
            })
            .to_string(),
        ],
    )
    .unwrap();
}

fn replace_trae_value(path: &Path, body: &str) {
    Connection::open(path)
        .unwrap()
        .execute(
            "update ItemTable set value = ?1 where key = ?2",
            params![
                json!({
                    "list": [{
                        "id": "native-session",
                        "messages": [{
                            "id": "native-message",
                            "role": "user",
                            "content": body,
                            "createdAt": "2026-07-28T12:00:00Z",
                        }],
                    }],
                })
                .to_string(),
                trae::TRAE_CHAT_KEYS[0],
            ],
        )
        .unwrap();
}

fn firebender_message_from_hydrated_row(
    hydrated: &firebender::native_path::FirebenderHydratedSourceRow,
) -> String {
    let messages: Value = serde_json::from_slice(hydrated.messages_json()).unwrap();
    let message = messages
        .as_array()
        .and_then(|messages| messages.get(hydrated.message_index() as usize))
        .unwrap();
    firebender::firebender_message_text(message).unwrap()
}
