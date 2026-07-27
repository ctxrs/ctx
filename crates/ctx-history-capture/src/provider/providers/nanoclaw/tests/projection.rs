use ctx_history_core::EventRole;

use super::super::position::initial_nanoclaw_position;
use super::super::projection::NanoClawCapturedBatchProjector;
use super::*;

#[test]
fn bounded_projection_preserves_message_and_session_metadata() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = create_project(&temp, "parity", 1);
    let (inbound, outbound) = create_message_stores(&root, "session-0000");
    insert_inbound(
        &inbound,
        "in-1",
        1,
        1_782_259_201_000,
        r#"{"text":"legacy parity user"}"#,
    );
    insert_outbound(
        &outbound,
        "out-1",
        2,
        1_782_259_202_000,
        r#"{"text":"legacy parity assistant"}"#,
    );
    let context = context(&root);
    let central_path = root.join("data").join("v2.db");
    let conn = open_provider_sqlite_readonly(&central_path).unwrap();
    let user_version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let schema_fingerprint = sqlite_schema_fingerprint(&conn).unwrap();
    let mut projector = NanoClawCapturedBatchProjector::new(
        context.clone(),
        root.display().to_string(),
        central_path.display().to_string(),
        user_version,
        schema_fingerprint,
    );
    let mut output = CollectingOutput {
        normalization: ProviderNormalizationResult::default(),
    };
    for record in capture_batches(&root, initial_nanoclaw_position().unwrap())
        .iter()
        .flat_map(|batch| batch.records())
    {
        if matches!(record.payload(), CapturedRecordPayload::SqliteValues(_)) {
            projector.project_record(record, &mut output).unwrap();
        }
    }
    assert!(output.normalization.files_touched.is_empty());
    assert_eq!(output.normalization.captures.len(), 2);
    assert_eq!(
        output
            .normalization
            .captures
            .iter()
            .map(|(_, capture)| capture.session.provider_session_id.as_str())
            .collect::<Vec<_>>(),
        vec!["ag-1/session-0000", "ag-1/session-0000"]
    );
    assert_eq!(
        output
            .normalization
            .captures
            .iter()
            .map(|(_, capture)| {
                let event = capture.event.as_ref().unwrap();
                (
                    event.role,
                    event.payload["text"].as_str().unwrap(),
                    event.metadata["source"].as_str().unwrap(),
                )
            })
            .collect::<Vec<_>>(),
        vec![
            (
                Some(EventRole::User),
                r#"{"text":"legacy parity user"}"#,
                "nanoclaw_inbound",
            ),
            (
                Some(EventRole::Assistant),
                r#"{"text":"legacy parity assistant"}"#,
                "nanoclaw_outbound",
            ),
        ]
    );
    let session = &output.normalization.captures[0].1.session;
    assert_eq!(session.metadata["agent_group_id"], "ag-1");
    assert_eq!(session.metadata["agent_group_name"], "Personal");
    assert_eq!(session.metadata["messaging"]["channel_type"], "telegram");
    assert_eq!(session.cwd.as_deref(), Some("/workspace/nanoclaw"));
}
