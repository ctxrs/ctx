use std::fs;

use tempfile::tempdir;

use super::*;

#[test]
fn pre_pr3_gemini_header_prefers_timestamp_over_start_time() {
    let temp = tempdir().unwrap();
    let path = temp.path().join("gemini-dual-time.jsonl");
    fs::write(
        &path,
        format!(
            "{}\n",
            json!({
                "sessionId": "gemini-dual-time",
                "timestamp": "2026-07-14T11:00:00Z",
                "startTime": "2026-07-14T10:00:00Z",
                "type": "user",
                "content": "timestamp precedence"
            })
        ),
    )
    .unwrap();
    let context = ProviderAdapterContext {
        machine_id: "native-jsonl-precedence-test".to_owned(),
        source_path: Some(path.clone()),
        source_root: Some(temp.path().to_path_buf()),
        imported_at: "2026-07-14T12:00:00Z".parse().unwrap(),
    };

    let normalized = normalize_native_jsonl_session_file(
        &path,
        &context,
        CaptureProvider::Gemini,
        "gemini_cli_chat_recording_jsonl",
    )
    .unwrap();

    assert_eq!(normalized.summary.failed, 0);
    assert!(!normalized.captures.is_empty());
    let expected: DateTime<Utc> = "2026-07-14T11:00:00Z".parse().unwrap();
    assert!(normalized
        .captures
        .iter()
        .all(|(_, capture)| capture.session.started_at == expected));
}
