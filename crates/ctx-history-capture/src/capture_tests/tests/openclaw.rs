#[allow(unused_imports)]
use super::*;

#[test]
pub(crate) fn openclaw_import_ignores_oversized_session_index_sidecar() {
    let temp = tempdir();
    let root = temp.path().join("openclaw");
    let sessions = root.join("agents/personal-agent/sessions");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("sessions.json"),
        vec![b'x'; MAX_OPENCLAW_SESSION_INDEX_BYTES + 1],
    )
    .unwrap();
    fs::write(
        sessions.join("openclaw-oversized-index.jsonl"),
        format!(
            "{}\n{}\n",
            json!({
                "type": "session",
                "id": "openclaw-oversized-index",
                "timestamp": "2026-06-24T12:00:00Z",
                "cwd": "/workspace"
            }),
            json!({
                "type": "message",
                "id": "openclaw-oversized-index-user",
                "timestamp": "2026-06-24T12:00:01Z",
                "message": {"role": "user", "content": "oversized sidecar should not block import"}
            })
        ),
    )
    .unwrap();
    let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

    let summary = import_openclaw_history(
        &root,
        &mut store,
        OpenClawImportOptions {
            allow_partial_failures: true,
            ..OpenClawImportOptions::default()
        },
    )
    .unwrap();

    assert_eq!(summary.failed, 0);
    assert_eq!(summary.imported_sessions, 1);
    assert_eq!(summary.imported_events, 1);
    let session_id = provider_session_uuid(
        CaptureProvider::OpenClaw,
        "personal-agent/openclaw-oversized-index",
    );
    let session = store.get_session(session_id).unwrap();
    assert_eq!(
        session.external_session_id.as_deref(),
        Some("personal-agent/openclaw-oversized-index")
    );
}
