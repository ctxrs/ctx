use std::fs;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    new_id, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, EntityTimestamps,
    Fidelity, Session, SessionHistoryArchive, SessionStatus, SyncMetadata, SyncState, Visibility,
};
use uuid::Uuid;

pub(super) fn tempdir() -> tempfile::TempDir {
    let root = std::env::var_os("TEST_TMPDIR")
        .map(|path| std::path::PathBuf::from(path).join("test-data"))
        .unwrap_or_else(|| std::env::current_dir().unwrap().join("target/test-data"));
    fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-history-store-schema-")
        .tempdir_in(root)
        .unwrap()
}

pub(super) fn fixed_time() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-06-23T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

pub(super) fn timestamps() -> EntityTimestamps {
    EntityTimestamps {
        created_at: fixed_time(),
        updated_at: fixed_time(),
    }
}

pub(super) fn sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: serde_json::json!({}),
    }
}

pub(super) fn imported_session(external_session_id: &str) -> Session {
    Session {
        id: new_id(),
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: CaptureProvider::Codex,
        external_session_id: Some(external_session_id.into()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".into()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fixed_time(),
        ended_at: None,
        timestamps: timestamps(),
        sync: sync_metadata(),
    }
}

pub(super) fn archive_with_source(source: CaptureSource) -> SessionHistoryArchive {
    SessionHistoryArchive {
        capture_sources: vec![source],
        ..SessionHistoryArchive::default()
    }
}

pub(super) fn archive_with_source_and_session(
    source: CaptureSource,
    session: Session,
) -> SessionHistoryArchive {
    SessionHistoryArchive {
        capture_sources: vec![source],
        sessions: vec![session],
        ..SessionHistoryArchive::default()
    }
}

pub(super) fn provider_archive_source(
    id: &str,
    external_session_id: &str,
    raw_source_path: &str,
) -> CaptureSource {
    provider_archive_source_with_root(id, external_session_id, raw_source_path, "/repo")
}

pub(super) fn provider_archive_source_with_root(
    id: &str,
    external_session_id: &str,
    raw_source_path: &str,
    source_root: &str,
) -> CaptureSource {
    CaptureSource {
        id: Uuid::parse_str(id).unwrap(),
        descriptor: CaptureSourceDescriptor {
            kind: ctx_history_core::CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Claude,
            machine_id: "test-machine".to_owned(),
            process_id: None,
            cwd: Some("/repo".to_owned()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some("claude_projects_jsonl_tree".to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: None,
            external_session_id: Some(external_session_id.to_owned()),
        },
        started_at: fixed_time(),
        ended_at: None,
        sync: sync_metadata(),
    }
}

pub(super) fn provider_archive_session(
    id: &str,
    source_id: Uuid,
    external_session_id: &str,
) -> Session {
    Session {
        id: Uuid::parse_str(id).unwrap(),
        provider: CaptureProvider::Claude,
        capture_source_id: Some(source_id),
        external_session_id: Some(external_session_id.to_owned()),
        ..imported_session(external_session_id)
    }
}
