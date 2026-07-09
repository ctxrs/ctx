use std::{env, path::PathBuf};

use serde_json::json;
use uuid::Uuid;

use crate::{
    blob_dir, config_path, database_path, default_data_root, device_path, history_dir, inbox_dir,
    logs_dir, object_dir, spool_dir, CaptureProvider, Confidence, ContextCitationType, Fidelity,
    HistoryRecord, Session, SyncMetadata, SyncOutboxItem, SyncState, Visibility,
};

#[test]
fn enum_string_roundtrips_and_defaults() {
    let visibility: Visibility = serde_json::from_str("\"sync_metadata\"").unwrap();
    assert_eq!(visibility, Visibility::SyncMetadata);
    assert_eq!(visibility.to_string(), "sync_metadata");
    assert!("not_valid".parse::<Visibility>().is_err());

    assert_eq!(Visibility::default(), Visibility::LocalOnly);
    assert_eq!(Fidelity::default(), Fidelity::Partial);
    assert_eq!(SyncState::default(), SyncState::LocalOnly);
    assert_eq!(Confidence::default(), Confidence::Unknown);
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"copilot_cli\"").unwrap(),
        CaptureProvider::CopilotCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"factory_ai_droid\"").unwrap(),
        CaptureProvider::FactoryAiDroid
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"kilo\"").unwrap(),
        CaptureProvider::Kilo
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"kiro_cli\"").unwrap(),
        CaptureProvider::KiroCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"qwen_code\"").unwrap(),
        CaptureProvider::QwenCode
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"kimi_code_cli\"").unwrap(),
        CaptureProvider::KimiCodeCli
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"forgecode\"").unwrap(),
        CaptureProvider::ForgeCode
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"mistral_vibe\"").unwrap(),
        CaptureProvider::MistralVibe
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"mux\"").unwrap(),
        CaptureProvider::Mux
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"rovodev\"").unwrap(),
        CaptureProvider::RovoDev
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"lingma\"").unwrap(),
        CaptureProvider::Lingma
    );
    assert_eq!(
        serde_json::from_str::<CaptureProvider>("\"mimocode\"").unwrap(),
        CaptureProvider::MiMoCode
    );

    let sync: SyncMetadata = serde_json::from_value(json!({})).unwrap();
    assert_eq!(sync.visibility, Visibility::LocalOnly);
    assert_eq!(sync.fidelity, Fidelity::Partial);
    assert_eq!(sync.sync_state, SyncState::LocalOnly);
    assert_eq!(sync.sync_version, 0);
    assert_eq!(sync.metadata, json!({}));

    let outbox: SyncOutboxItem = serde_json::from_value(json!({
        "id": "018f45d0-0000-7000-8000-000000000010",
        "local_table": "history_records",
        "local_id": "018f45d0-0000-7000-8000-000000000001",
        "operation": "insert",
        "device_id": "device-1",
        "created_at": "2026-06-22T00:00:00Z",
        "updated_at": "2026-06-22T00:00:00Z"
    }))
    .unwrap();
    assert_eq!(outbox.sync_state, SyncState::Pending);
}

#[test]
fn history_record_json_names_are_public_names() {
    let record_id = Uuid::parse_str("018f45d0-0000-7000-8000-000000000001").unwrap();
    let session: Session = serde_json::from_value(json!({
        "id": "018f45d0-0000-7000-8000-000000000002",
        "history_record_id": record_id,
        "provider": "codex",
        "agent_type": "primary",
        "status": "imported",
        "started_at": "2026-06-22T00:00:00Z",
        "created_at": "2026-06-22T00:00:00Z",
        "updated_at": "2026-06-22T00:00:00Z"
    }))
    .unwrap();

    assert_eq!(session.history_record_id, Some(record_id));
    let value = serde_json::to_value(&session).unwrap();
    assert_eq!(value["history_record_id"], record_id.to_string());
    assert_eq!(
        serde_json::to_string(&ContextCitationType::HistoryRecord).unwrap(),
        "\"history_record\""
    );
}

#[test]
fn generated_ids_are_uuid_v7_and_paths_are_centralized() {
    let record = HistoryRecord::new("Task", "body", Vec::new(), "task", None);

    assert_eq!(record.id.get_version_num(), 7);
}

#[test]
fn local_layout_paths_are_flat_under_data_root() {
    let root = PathBuf::from("/tmp/ctx-root");
    assert_eq!(history_dir(root.clone()), PathBuf::from("/tmp/ctx-root"));
    assert_eq!(
        database_path(root.clone()),
        PathBuf::from("/tmp/ctx-root/work.sqlite")
    );
    assert_eq!(
        object_dir(root.clone()),
        PathBuf::from("/tmp/ctx-root/objects")
    );
    assert_eq!(
        blob_dir(root.clone()),
        PathBuf::from("/tmp/ctx-root/objects")
    );
    assert_eq!(
        spool_dir(root.clone()),
        PathBuf::from("/tmp/ctx-root/spool")
    );
    assert_eq!(
        inbox_dir(root.clone()),
        PathBuf::from("/tmp/ctx-root/spool")
    );
    assert_eq!(
        config_path(root.clone()),
        PathBuf::from("/tmp/ctx-root/config.toml")
    );
    assert_eq!(logs_dir(root.clone()), PathBuf::from("/tmp/ctx-root/logs"));
    assert_eq!(
        device_path(root),
        PathBuf::from("/tmp/ctx-root/device.json")
    );
}

#[test]
fn ctx_data_root_env_is_the_ctx_root_itself() {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var_os("CTX_DATA_ROOT");
    env::remove_var("CTX_DATA_ROOT");

    let default_root = default_data_root().unwrap();
    assert!(default_root.ends_with(".ctx"));

    env::set_var("CTX_DATA_ROOT", "/tmp/custom-ctx-root");

    assert_eq!(
        default_data_root().unwrap(),
        PathBuf::from("/tmp/custom-ctx-root")
    );
    assert_eq!(
        database_path(default_data_root().unwrap()),
        PathBuf::from("/tmp/custom-ctx-root/work.sqlite")
    );

    if let Some(previous) = previous {
        env::set_var("CTX_DATA_ROOT", previous);
    } else {
        env::remove_var("CTX_DATA_ROOT");
    }
}
