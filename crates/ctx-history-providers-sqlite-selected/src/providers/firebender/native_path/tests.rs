use ctx_history_core::{ActivityJsonCapture, AgentScope, EventType, LiteralFactKind, SourceAnchor};
use serde_json::json;

use super::{
    source_backed::{
        firebender_core_record, firebender_database_path_and_source, firebender_session_id,
        firebender_source_key_scoped,
    },
    FirebenderRow, FIREBENDER_SELECTED_CATALOG_LINEAGE_V1, FIREBENDER_SOURCE_IDENTITY_REVISION,
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
fn root_scope_separates_identical_firebender_sessions_and_unqualified_is_released() {
    use ctx_history_core::{CaptureProvider, SourceAnchorScope, SourceKey};

    let released = SourceKey::derive(
        CaptureProvider::Firebender.as_str(),
        crate::FIREBENDER_SQLITE_SOURCE_FORMAT,
        super::source_backed::FIREBENDER_SOURCE_SCHEMA_VARIANT,
        FIREBENDER_SOURCE_IDENTITY_REVISION,
        SourceAnchor::CatalogLineage(FIREBENDER_SELECTED_CATALOG_LINEAGE_V1),
    )
    .unwrap();
    let unqualified = firebender_source_key_scoped(SourceAnchorScope::Unqualified).unwrap();
    assert!(released.exact_descriptor_eq(&unqualified));
    assert_eq!(
        released.identity().encode_canonical().unwrap(),
        unqualified.identity().encode_canonical().unwrap()
    );

    let first = firebender_source_key_scoped(SourceAnchorScope::Lineage([0x11; 32])).unwrap();
    let second = firebender_source_key_scoped(SourceAnchorScope::Lineage([0x22; 32])).unwrap();
    assert_ne!(
        firebender_session_id(&first, "shared-session").unwrap(),
        firebender_session_id(&second, "shared-session").unwrap()
    );
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
    assert_eq!(record.agent_scope, Some(AgentScope::Primary));
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
    for (index, (status, body)) in [
        (Some("completed"), "complete success body"),
        (Some("failed"), "complete failure body"),
        (None, "complete status-absent body"),
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
        assert_eq!(record.content.structured_content.as_ref(), Some(&message));
        let result = record
            .content
            .activity
            .as_ref()
            .unwrap()
            .result
            .as_ref()
            .unwrap();
        assert_eq!(result.status.as_deref(), status);
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

#[test]
fn firebender_argument_aliases_are_exact_and_nested_metadata_never_becomes_facts() {
    let (_, source) = firebender_database_path_and_source(
        "/tmp/firebender-alias-neutrality/chat_history.db".as_ref(),
    )
    .unwrap();
    let row = FirebenderRow {
        rowid: 9,
        id: "session-aliases".to_owned(),
        name: "aliases".to_owned(),
        created_at: 1_700_000_000_000,
        updated_at: 1_700_000_000_001,
        messages_json: "[]".to_owned(),
        metadata_json: "{}".to_owned(),
        messages: Vec::new(),
    };
    let session_id = firebender_session_id(&source, &row.id).unwrap();

    let equivalent = json!({
        "id": "equivalent-aliases",
        "role": "assistant",
        "tool_calls": [{}],
        "tool_call_id": "call-equivalent",
        "name": "exact_tool",
        "arguments": {"x": 1},
        "args": {"x": 1},
        "metadata": {
            "path": "src/firebender-decoy.rs",
            "nested": {
                "branch": "decoy-branch",
                "commit": "decoy-commit",
                "command": "decoy-command"
            }
        }
    });
    let equivalent = firebender_core_record(
        &source,
        session_id,
        Some("/schema-known-workspace"),
        &row,
        0,
        &equivalent,
    )
    .unwrap()
    .unwrap();
    let equivalent_activity = equivalent.content.activity.as_ref().unwrap();
    assert_eq!(equivalent_activity.facts.len(), 1);
    assert_eq!(
        equivalent_activity.facts[0].kind,
        LiteralFactKind::Workspace
    );
    assert_eq!(
        equivalent_activity.facts[0].value,
        "/schema-known-workspace"
    );
    assert_eq!(
        equivalent_activity.invocation.as_ref().unwrap().arguments,
        ActivityJsonCapture::Present {
            value: json!({"x": 1}),
        }
    );

    let conflicting = json!({
        "id": "conflicting-aliases",
        "role": "assistant",
        "tool_calls": [{}],
        "tool_call_id": "call-conflicting",
        "name": "exact_tool",
        "arguments": {"selected": "first"},
        "input": {"selected": "last"}
    });
    let conflicting = firebender_core_record(&source, session_id, None, &row, 1, &conflicting)
        .unwrap()
        .unwrap();
    assert_eq!(
        conflicting
            .content
            .activity
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap()
            .arguments,
        ActivityJsonCapture::Unavailable
    );

    let absent = json!({
        "id": "absent-aliases",
        "role": "assistant",
        "tool_calls": [{}],
        "tool_call_id": "call-absent",
        "name": "exact_tool"
    });
    let absent = firebender_core_record(&source, session_id, None, &row, 2, &absent)
        .unwrap()
        .unwrap();
    assert_eq!(
        absent
            .content
            .activity
            .as_ref()
            .unwrap()
            .invocation
            .as_ref()
            .unwrap()
            .arguments,
        ActivityJsonCapture::Absent
    );
}
