use std::{fs, path::Path};

use ctx_history_core::{CoreRecord, EventIdentityInput, NativeItemKey, TypedKey};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::Value;

use super::*;
use crate::{
    provider::source_backed::{
        refresh_source_backed_generation, register_landed_source_backed_route,
        SourceBackedProviderRegistry, SourceBackedRouteSelection,
    },
    ProviderCatalogSupport, ProviderImportSupport, ProviderSource, ProviderSourceKind,
    ProviderSourceStatus,
};

const SESSION_ID: &str = "mistral-mcp-abstention";

fn call(id: &str, composite_name: &str) -> String {
    serde_json::json!({
        "role": "assistant",
        "content": format!("call {id}"),
        "tool_calls": [{
            "id": id,
            "type": "function",
            "function": {"name": composite_name},
        }],
    })
    .to_string()
}

fn terminal(id: &str, composite_name: &str, tool: &str, transport: &str, content: &str) -> String {
    serde_json::json!({
        "role": "tool",
        "content": content,
        "name": composite_name,
        "tool_call_id": id,
        "status": "success",
        "tool_result": {
            "output": {
                "ok": true,
                "server": transport,
                "tool": tool,
            },
            "cancelled": false,
        },
    })
    .to_string()
}

fn write_session(root: &Path, messages: &str) {
    let session = root.join("session");
    fs::create_dir_all(&session).unwrap();
    fs::write(
        session.join("meta.json"),
        serde_json::json!({
            "session_id": SESSION_ID,
            "start_time": "2026-01-02T03:04:05Z",
            "environment": {"working_directory": "/tmp/mistral"},
        })
        .to_string(),
    )
    .unwrap();
    fs::write(session.join("messages.jsonl"), messages).unwrap();
}

fn registry(root: &Path) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider: CaptureProvider::MistralVibe,
            path: root.to_path_buf(),
            exists: true,
            source_format: "mistral_vibe_session_jsonl_tree",
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    registry
}

fn publish(root: &Path, index: &Path) -> Vec<CoreRecord> {
    refresh_source_backed_generation(
        index,
        &registry(root),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();
    let source = source_key(SESSION_ID).unwrap();
    let mut records = VerifiedIndex::open(index)
        .unwrap()
        .core_source_event_page(&source, None, 64)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.core_record)
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

#[test]
fn url_and_stdio_transport_metadata_publish_terminal_content_without_mcp_identity() {
    let messages = [
        call("url-call", "docs_server_read_document"),
        terminal(
            "url-call",
            "docs_server_read_document",
            "read_document",
            "https://mcp.example.test/mcp",
            "URL terminal result",
        ),
        call("stdio-call", "files_server_read_file"),
        terminal(
            "stdio-call",
            "files_server_read_file",
            "read_file",
            "uvx mcp-server-filesystem /tmp",
            "stdio terminal result",
        ),
    ]
    .join("\n")
        + "\n";
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_root = temp.path().join("source");
    write_session(&source_root, &messages);

    let first = publish(&source_root, &temp.path().join("index-a"));
    let second = publish(&source_root, &temp.path().join("index-b"));
    assert_eq!(first.len(), 4);
    assert_eq!(
        first
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>()
    );
    assert!(first.iter().all(|record| record.mcp_tool_call.is_none()));

    let source = source_key(SESSION_ID).unwrap();
    let session_id = session_identity(&source, SESSION_ID).unwrap();
    for (call_id, composite_name, expected_content) in [
        (
            "url-call",
            "docs_server_read_document",
            "URL terminal result",
        ),
        (
            "stdio-call",
            "files_server_read_file",
            "stdio terminal result",
        ),
    ] {
        let terminal = first
            .iter()
            .find(|record| record.content.meaningful_text() == expected_content)
            .unwrap();
        assert_eq!(terminal.event_type, EventType::ToolOutput.as_str());
        assert_eq!(terminal.parser_revision, PARSER_REVISION);
        let linkage = terminal.content.structured_content.as_ref().unwrap();
        assert_eq!(
            linkage
                .pointer("/provider_native_tool_result/call_id")
                .and_then(Value::as_str),
            Some(call_id)
        );
        assert_eq!(
            linkage
                .pointer("/provider_native_tool_result/tool_name")
                .and_then(Value::as_str),
            Some(composite_name)
        );
        assert_eq!(
            linkage
                .pointer("/provider_native_tool_result/outcome")
                .and_then(Value::as_str),
            Some("success")
        );

        let native_item_key =
            NativeItemKey::native_id(NATIVE_EVENT_NAMESPACE, TypedKey::utf8(call_id).unwrap())
                .unwrap();
        let expected_id = derive_event_id(EventIdentityInput {
            source: &source,
            session_id,
            logical_item_kind: LOGICAL_EVENT_KIND,
            native_item_key: &native_item_key,
            subrecord_selector: None,
        })
        .unwrap();
        assert_eq!(terminal.event_id, expected_id);
    }
}

#[test]
fn reused_tool_result_ids_keep_existing_native_and_collision_identities() {
    let messages = [
        call("reused-call", "docs_server_read_document"),
        terminal(
            "reused-call",
            "docs_server_read_document",
            "read_document",
            "https://mcp.example.test/mcp",
            "first terminal result",
        ),
        terminal(
            "reused-call",
            "docs_server_read_document",
            "read_document",
            "uvx mcp-server-filesystem /tmp",
            "second terminal result",
        ),
    ]
    .join("\n")
        + "\n";
    let temp = crate::test_support_paths::tempdir().unwrap();
    let source_root = temp.path().join("source");
    write_session(&source_root, &messages);

    let first = publish(&source_root, &temp.path().join("index-a"));
    let second = publish(&source_root, &temp.path().join("index-b"));
    assert_eq!(first.len(), 3);
    assert_eq!(
        first
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>(),
        second
            .iter()
            .map(|record| record.event_id)
            .collect::<Vec<_>>()
    );
    assert!(first.iter().all(|record| record.mcp_tool_call.is_none()));

    let first_terminal = first
        .iter()
        .find(|record| record.content.meaningful_text() == "first terminal result")
        .unwrap();
    let second_terminal = first
        .iter()
        .find(|record| record.content.meaningful_text() == "second terminal result")
        .unwrap();
    assert_ne!(first_terminal.event_id, second_terminal.event_id);

    let source = source_key(SESSION_ID).unwrap();
    let session_id = session_identity(&source, SESSION_ID).unwrap();
    let native_item_key = NativeItemKey::native_id(
        NATIVE_EVENT_NAMESPACE,
        TypedKey::utf8("reused-call").unwrap(),
    )
    .unwrap();
    let expected_first = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: None,
    })
    .unwrap();
    assert_eq!(first_terminal.event_id, expected_first);

    let collision_selector = SubrecordSelector::certified_position(
        NATIVE_EVENT_REUSED_TOOL_CALL_POSITION_KIND,
        TypedKey::U64(2),
        PositionStability::AppendStable,
    )
    .unwrap();
    let expected_second = derive_event_id(EventIdentityInput {
        source: &source,
        session_id,
        logical_item_kind: LOGICAL_EVENT_KIND,
        native_item_key: &native_item_key,
        subrecord_selector: Some(&collision_selector),
    })
    .unwrap();
    assert_eq!(second_terminal.event_id, expected_second);
}
