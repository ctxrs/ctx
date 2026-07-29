use super::*;
use ctx_history_core::{
    new_id, Event, EventRole, EventType, Fidelity, SyncMetadata, SyncState, Visibility,
};

fn test_sync_metadata() -> SyncMetadata {
    SyncMetadata {
        visibility: Visibility::LocalOnly,
        fidelity: Fidelity::Imported,
        sync_state: SyncState::LocalOnly,
        sync_version: 0,
        deleted_at: None,
        metadata: json!({}),
    }
}

fn insert_test_event(store: &Store, text: &str) -> Result<()> {
    store.upsert_event(&Event {
        id: new_id(),
        seq: 1,
        history_record_id: None,
        session_id: None,
        run_id: None,
        event_type: EventType::Message,
        role: Some(EventRole::User),
        occurred_at: utc_now(),
        capture_source_id: None,
        payload: json!({ "text": text }),
        payload_blob_id: None,
        dedupe_key: None,
        sync: test_sync_metadata(),
    })?;
    Ok(())
}

#[test]
fn legacy_semantic_and_hybrid_require_a_fresh_source_generation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::create_dir_all(temp.path())?;
    let store = Store::open(database_path(temp.path().to_path_buf()))?;
    insert_test_event(
        &store,
        "semantic unsupported platform lexical fallback fixture",
    )?;

    let hybrid_error = search_packet_with_backend(
        &store,
        temp.path(),
        "semantic unsupported platform lexical fallback fixture",
        &[],
        &ctx_history_search::PacketOptions::default(),
        SearchBackendArg::Hybrid,
        true,
        0.35,
        RefreshArg::Off,
        false,
    )
    .expect_err("legacy hybrid search must not fall back to Store rows");
    assert!(format!("{hybrid_error:#}").contains("fresh source-backed Core generation"));

    let error = search_packet_with_backend(
        &store,
        temp.path(),
        "semantic unsupported platform lexical fallback fixture",
        &[],
        &ctx_history_search::PacketOptions::default(),
        SearchBackendArg::Semantic,
        true,
        1.0,
        RefreshArg::Off,
        false,
    )
    .expect_err("legacy semantic search must require a source generation");
    assert!(format!("{error:#}").contains("fresh source-backed Core generation"));
    Ok(())
}
