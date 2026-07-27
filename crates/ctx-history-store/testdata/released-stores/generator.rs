use std::{env, fs, path::PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    EntityTimestamps, Event, EventRole, EventType, Fidelity, Session, SessionStatus, SyncMetadata,
    SyncState, Visibility,
};
use ctx_history_store::Store;
use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

const SOURCE_ID: &str = "11111111-1111-7111-8111-111111111111";
const SESSION_ID: &str = "22222222-2222-7222-8222-222222222222";
const MESSAGE_ID: &str = "33333333-3333-7333-8333-333333333333";
const RESULT_ID: &str = "44444444-4444-7444-8444-444444444444";
const MESSAGE_CANARY: &str = "historical-release-message-canary";
const RESULT_CANARY: &str = "historical-release-raw-result-canary";
const SOURCE_PATH: &str = "/public-fixture/ctx/released-store/session.jsonl";
const SOURCE_IDENTITY: &str = "historical-release-source-identity";
const SOURCE_FORMAT: &str = "codex_session_jsonl";
const MACHINE_ID: &str = "historical-release-machine";

fn id(value: &str) -> Uuid {
    Uuid::parse_str(value).expect("fixture UUID is valid")
}

fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
        .expect("fixture timestamp is valid")
        .with_timezone(&Utc)
}

fn sync(release: &str) -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: json!({"historical_fixture_writer_release": release}),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let output = PathBuf::from(args.next().ok_or("missing output path")?);
    let release = args
        .next()
        .ok_or("missing release label")?
        .into_string()
        .map_err(|_| "release label is not UTF-8")?;
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    if output.exists() {
        fs::remove_file(&output)?;
    }

    let source_id = id(SOURCE_ID);
    let session_id = id(SESSION_ID);
    let store = Store::open(&output)?;
    store.upsert_capture_source(&CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Codex,
            machine_id: MACHINE_ID.to_owned(),
            process_id: None,
            cwd: Some("/public-fixture/workspace".to_owned()),
            raw_source_path: Some(SOURCE_PATH.to_owned()),
            source_format: Some(SOURCE_FORMAT.to_owned()),
            source_root: Some("/public-fixture/ctx/released-store".to_owned()),
            source_identity: Some(SOURCE_IDENTITY.to_owned()),
            external_session_id: Some("historical-release-session".to_owned()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: sync(&release),
    })?;
    store.upsert_session(&Session {
        id: session_id,
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Codex,
        external_session_id: Some("historical-release-session".to_owned()),
        external_agent_id: Some("historical-release-agent".to_owned()),
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: EntityTimestamps {
            created_at: fixed_time(),
            updated_at: fixed_time(),
        },
        sync: sync(&release),
    })?;

    let events = [
        Event {
            id: id(MESSAGE_ID),
            seq: 1,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::Message,
            role: Some(EventRole::Assistant),
            occurred_at: fixed_time(),
            capture_source_id: Some(source_id),
            payload: json!({"text": MESSAGE_CANARY}),
            payload_blob_id: None,
            dedupe_key: Some(format!("historical-fixture:{release}:message")),
            sync: sync(&release),
        },
        Event {
            id: id(RESULT_ID),
            seq: 2,
            history_record_id: None,
            session_id: Some(session_id),
            run_id: None,
            event_type: EventType::ToolOutput,
            role: Some(EventRole::Tool),
            occurred_at: fixed_time(),
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": "codex",
                "provider_session_id": "historical-release-session",
                "provider_event_index": 2,
                "body": {
                    "result_outcome": "failure",
                    "exit_code": 7,
                    "output": RESULT_CANARY,
                    "output_preview": RESULT_CANARY
                }
            }),
            payload_blob_id: None,
            dedupe_key: Some(format!("historical-fixture:{release}:result")),
            sync: sync(&release),
        },
    ];
    for event in &events {
        store.upsert_event(event)?;
    }
    store.optimize_search_index()?;
    assert_eq!(store.search_event_hits(MESSAGE_CANARY, 10)?.len(), 1);
    assert_eq!(store.search_event_hits(RESULT_CANARY, 10)?.len(), 1);
    store.checkpoint_wal_truncate()?;
    drop(store);

    let conn = Connection::open(&output)?;
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version != 46 {
        return Err(format!("{release} produced schema v{version}, expected v46").into());
    }
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(format!("{release} integrity check failed: {integrity}").into());
    }
    let normalized = conn.execute(
        "UPDATE search_projection_stats SET updated_at_ms = ?1",
        [fixed_time().timestamp_millis()],
    )?;
    if normalized != 1 {
        return Err(format!("{release} had {normalized} search cache rows, expected one").into());
    }
    conn.execute_batch("PRAGMA journal_mode = DELETE; VACUUM;")?;
    Ok(())
}
