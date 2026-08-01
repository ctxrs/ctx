use ctx_history_core::{EventType, SourceAnchor};
use serde_json::json;

use super::{
    source_backed::{
        firebender_core_record, firebender_database_path_and_source, firebender_session_id,
    },
    FirebenderRow, FIREBENDER_SOURCE_IDENTITY_REVISION,
};

#[test]
fn direct_core_projection_is_complete_and_self_contained() {
    let sources = [
        include_str!("source_backed.rs"),
        include_str!("source_backed/direct.rs"),
    ];
    let production = sources.join("\n");
    assert!(production.contains("CoreRecord::new_selected"));
    assert!(production.contains("native_event_id = Some"));
    assert!(production.contains("DIRECT_PARSER_REVISION"));
    assert!(production.contains("validate_contract"));
    assert!(production.contains("event.text"));
    for removed_api in [
        concat!("Lexical", "Document"),
        concat!("SourceRecord", "Locator"),
        concat!("hyd", "rate_"),
        concat!("resol", "ver"),
    ] {
        assert!(!production.contains(removed_api), "found {removed_api}");
    }
    assert!(!production.contains("body.truncate"));
    assert!(!production.contains("body.chars().take"));
}

#[test]
fn serialized_current_core_record_does_not_disclose_firebender_database_path() {
    let physical_path = "/private/home/alice/secret-project/.idea/firebender/chat_history.db";
    let (_, source) = firebender_database_path_and_source(physical_path.as_ref()).unwrap();
    assert_eq!(
        source.provider_identity_version(),
        FIREBENDER_SOURCE_IDENTITY_REVISION
    );
    assert!(matches!(source.anchor(), SourceAnchor::CatalogLineage(_)));

    let row = FirebenderRow {
        rowid: 1,
        id: "session-1".to_owned(),
        name: "opaque lineage".to_owned(),
        created_at: 1_700_000_000_000,
        updated_at: 1_700_000_000_001,
        messages_json: "[]".to_owned(),
        metadata_json: "{}".to_owned(),
        messages: Vec::new(),
    };
    let session_id = firebender_session_id(&source, &row.id).unwrap();
    let message = json!({
        "id": "message-1",
        "role": "user",
        "content": "exact Firebender body"
    });
    let record = firebender_core_record(&source, session_id, None, &row, 0, &message)
        .unwrap()
        .unwrap();
    let core_wire = serde_json::to_string(&record).unwrap();

    let stored_wire = String::from_utf8(record.encode_stored().unwrap()).unwrap();
    let reversible_path_hex = physical_path
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();

    for serialized in [&core_wire, &stored_wire] {
        assert!(!serialized.contains(physical_path));
        assert!(!serialized.contains(&reversible_path_hex));
        assert!(!serialized.contains("provider-path-v1"));
    }
}

#[test]
fn tool_results_keep_complete_success_failure_and_unknown_content_once() {
    let (_, source) = firebender_database_path_and_source(
        "/tmp/firebender-result-completeness/chat_history.db".as_ref(),
    )
    .unwrap();
    let row = FirebenderRow {
        rowid: 7,
        id: "session-results".to_owned(),
        name: "results".to_owned(),
        created_at: 1_700_000_000_000,
        updated_at: 1_700_000_000_001,
        messages_json: "[]".to_owned(),
        metadata_json: "{}".to_owned(),
        messages: Vec::new(),
    };
    let session_id = firebender_session_id(&source, &row.id).unwrap();
    for (index, (status, body, outcome)) in [
        (Some("completed"), "complete success body", "success"),
        (Some("failed"), "complete failure body", "failure"),
        (None, "complete unknown body", "unknown"),
    ]
    .into_iter()
    .enumerate()
    {
        let message = json!({
            "id": format!("result-{index}"),
            "role": "tool",
            "tool_call_id": format!("call-{index}"),
            "name": "shell",
            "content": body,
            "status": status,
        });
        let record = firebender_core_record(&source, session_id, None, &row, index, &message)
            .unwrap()
            .unwrap();
        assert_eq!(record.event_type, EventType::ToolOutput.as_str());
        assert_eq!(record.content.meaningful_text(), body);
        let linkage = record
            .content
            .structured_content
            .as_ref()
            .unwrap()
            .get("provider_native_result")
            .unwrap();
        assert_eq!(linkage["result_outcome"], outcome);
        assert_eq!(linkage["call_id"], format!("call-{index}"));
        assert!(!linkage.to_string().contains(body));
    }

    let status_only = json!({
        "id": "status-only",
        "role": "tool",
        "status": "failed",
    });
    assert!(
        firebender_core_record(&source, session_id, None, &row, 4, &status_only)
            .unwrap()
            .is_none()
    );
}
