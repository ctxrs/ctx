use std::{fs, path::Path};

use ctx_history_capture_composition::{
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry, SourceBackedRouteSelection,
};
use ctx_history_core::{
    derive_event_id, derive_native_session_id, AgentScope, CaptureProvider, CertifiedSource,
    CoreRecord, EventIdentityInput, EventType, LiteralFactKind, NativeItemKey, PositionStability,
    ProviderNativeSessionRelationship, SourceKey, StableEntityId, SubrecordSelector, TypedKey,
};
use ctx_history_index::{GenerationWriter, VerifiedIndex, WriterOptions};
use serde_json::Value;

fn tempdir() -> tempfile::TempDir {
    let root = fs::canonicalize(std::env::temp_dir()).unwrap();
    tempfile::Builder::new()
        .prefix("ctx-mistral-vibe-publication-")
        .tempdir_in(root)
        .unwrap()
}

fn has_literal_fact(record: &CoreRecord, kind: LiteralFactKind, value: &str) -> bool {
    record
        .content
        .activity
        .iter()
        .flat_map(|activity| activity.facts.iter())
        .any(|fact| fact.kind == kind && fact.value == value)
}

const SESSION_ID: &str = "mistral-mcp-abstention";
const SOURCE_FORMAT: &str = "mistral_vibe_session_jsonl";
const SOURCE_SCHEMA_VARIANT: &str = "meta-json-messages-jsonl-v1";
const SOURCE_ANCHOR_NAMESPACE: &str = "mistral-vibe-session-id";
const NATIVE_SESSION_NAMESPACE: &str = "mistral-vibe-session";
const NATIVE_EVENT_NAMESPACE: &str = "mistral-vibe-message";
const NATIVE_EVENT_REUSED_TOOL_CALL_POSITION_KIND: &str =
    "mistral-vibe-duplicate-tool-call-id-ordinal";
const LOGICAL_SESSION_KIND: &str = "mistral-vibe-session";
const LOGICAL_EVENT_KIND: &str = "mistral-vibe-event";
const PARSER_REVISION: &str = "mistral-vibe-source-backed-v17-exact-parent-admission";
const STALE_PARSER_REVISION: &str =
    "mistral-vibe-source-backed-v15-optional-admission-record-rejections";

fn source_key(native_session_id: &str) -> SourceKey {
    SourceKey::derive_provider_native(
        CaptureProvider::MistralVibe.as_str(),
        SOURCE_FORMAT,
        SOURCE_SCHEMA_VARIANT,
        1,
        SOURCE_ANCHOR_NAMESPACE,
        TypedKey::utf8(native_session_id).unwrap(),
    )
    .unwrap()
}

fn session_identity(source: &SourceKey, native_session_id: &str) -> StableEntityId {
    derive_native_session_id(
        source,
        LOGICAL_SESSION_KIND,
        NATIVE_SESSION_NAMESPACE,
        TypedKey::utf8(native_session_id).unwrap(),
    )
    .unwrap()
}

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
    write_named_session(root, "session", SESSION_ID, None, messages);
}

fn write_named_session(
    root: &Path,
    directory: &str,
    session_id: &str,
    parent_session_id: Option<&str>,
    messages: &str,
) {
    let metadata = serde_json::json!({
        "session_id": session_id,
        "parent_session_id": parent_session_id,
        "start_time": "2026-01-02T03:04:05Z",
        "environment": {"working_directory": "/tmp/mistral"},
    })
    .to_string();
    write_raw_metadata_session(root, directory, &metadata, messages);
}

fn write_raw_metadata_session(root: &Path, directory: &str, metadata: &str, messages: &str) {
    let session = root.join(directory);
    fs::create_dir_all(&session).unwrap();
    fs::write(session.join("meta.json"), metadata).unwrap();
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
            route_provenance: Default::default(),
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
    published_session(index, SESSION_ID)
}

fn published_session(index: &Path, provider_session_id: &str) -> Vec<CoreRecord> {
    let source = source_key(provider_session_id);
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
fn unchanged_v15_lineage_certificate_is_replaced_by_v17_direct_only_projection() {
    let messages = serde_json::json!({
        "role": "user",
        "message_id": "direct-parent-migration-message",
        "content": "unchanged direct parent migration fixture",
    })
    .to_string()
        + "\n";
    let temp = tempdir();
    let source_root = temp.path().join("source");
    let index = temp.path().join("index");
    write_named_session(
        &source_root,
        "session",
        SESSION_ID,
        Some("mistral-parent"),
        &messages,
    );
    refresh_source_backed_generation(&index, &registry(&source_root), WriterOptions::default())
        .unwrap();

    let source = source_key(SESSION_ID);
    let current = VerifiedIndex::open(&index).unwrap();
    let current_certificate = current
        .manifest()
        .sources
        .iter()
        .find(|certificate| certificate.observation().source() == &source)
        .unwrap()
        .clone();
    let current_routes = current.manifest().source_routes().to_vec();
    let mut stale_records = published_session(&index, SESSION_ID);
    assert_eq!(stale_records.len(), 1);
    let literal_parent = stale_records[0]
        .parent_session_id
        .expect("v17 must publish the exact direct parent claim");
    for record in &mut stale_records {
        record.parser_revision = STALE_PARSER_REVISION.to_owned();
        record.root_session_id = Some(literal_parent);
        record.session_relationship = Some(ProviderNativeSessionRelationship::Forked);
        record.agent_scope = Some(AgentScope::Subagent);
        record.validate_contract().unwrap();
    }
    let stale_certificate = CertifiedSource::certify_with_frontier(
        current_certificate.observation().clone(),
        current_certificate.observation().clone(),
        STALE_PARSER_REVISION,
        *current_certificate.content_digest(),
        current_certificate.counts(),
        current_certificate.frontier().cloned(),
    )
    .unwrap();
    drop(current);

    let mut writer = GenerationWriter::open(&index, WriterOptions::default())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for record in stale_records {
        writer.add_core_record(record).unwrap();
    }
    writer.certify_source(stale_certificate).unwrap();
    writer.set_present_source_routes(current_routes).unwrap();
    let stale_generation = writer.commit(|_| true).unwrap().generation_id;
    let stale = published_session(&index, SESSION_ID);
    assert_eq!(stale[0].parser_revision, STALE_PARSER_REVISION);
    assert_eq!(stale[0].parent_session_id, Some(literal_parent));
    assert_eq!(stale[0].root_session_id, Some(literal_parent));
    assert_eq!(
        stale[0].session_relationship,
        Some(ProviderNativeSessionRelationship::Forked)
    );
    assert_eq!(stale[0].agent_scope, Some(AgentScope::Subagent));

    let replacement =
        refresh_source_backed_generation(&index, &registry(&source_root), WriterOptions::default())
            .unwrap();
    assert_ne!(replacement.commit.generation_id, stale_generation);
    let replaced = published_session(&index, SESSION_ID);
    assert_eq!(replaced.len(), 1);
    assert_eq!(replaced[0].parser_revision, PARSER_REVISION);
    assert_eq!(replaced[0].parent_session_id, Some(literal_parent));
    assert_eq!(replaced[0].root_session_id, None);
    assert_eq!(replaced[0].session_relationship, None);
    assert_eq!(replaced[0].agent_scope, None);
    assert_eq!(
        replaced[0].content.meaningful_text(),
        "unchanged direct parent migration fixture"
    );
    let replaced_index = VerifiedIndex::open(&index).unwrap();
    assert_eq!(
        replaced_index
            .manifest()
            .sources
            .iter()
            .find(|certificate| certificate.observation().source() == &source)
            .unwrap()
            .parser_revision(),
        PARSER_REVISION
    );
}

#[test]
fn exact_parent_metadata_publishes_direct_parent_without_root() {
    let parent_messages = serde_json::json!({
        "role": "user",
        "message_id": "copied-message",
        "content": "parent retained message",
    })
    .to_string()
        + "\n";
    let child_messages = serde_json::json!({
        "role": "user",
        "message_id": "copied-message",
        "content": "child retained message",
    })
    .to_string()
        + "\n";
    let temp = tempdir();
    let index_temp = tempdir();
    write_named_session(
        temp.path(),
        "parent",
        "mistral-parent",
        None,
        &parent_messages,
    );
    write_named_session(
        temp.path(),
        "child",
        "mistral-child",
        Some("mistral-parent"),
        &child_messages,
    );
    let index = index_temp.path().join("index");
    refresh_source_backed_generation(
        &index,
        &registry(temp.path()),
        WriterOptions {
            indexer_threads: 1,
            memory_bytes: 15_000_000,
        },
    )
    .unwrap();

    let parent = published_session(&index, "mistral-parent").remove(0);
    let child = published_session(&index, "mistral-child").remove(0);
    assert_eq!(child.parent_session_id, Some(parent.session_id));
    assert_eq!(child.agent_scope, None);
    assert_eq!(child.root_session_id, None);
    assert_eq!(child.session_relationship, None);
    assert_eq!(parent.agent_scope, None);
    assert_eq!(parent.session_relationship, None);
    assert!(has_literal_fact(
        &parent,
        ctx_history_core::LiteralFactKind::SessionCwd,
        "/tmp/mistral"
    ));
    assert!(has_literal_fact(
        &child,
        ctx_history_core::LiteralFactKind::SessionCwd,
        "/tmp/mistral"
    ));
    assert_eq!(parent.native_event_id, child.native_event_id);
    assert_eq!(parent.event_copy, None);
    assert_eq!(child.event_copy, None);
    assert_ne!(parent.event_id, child.event_id);
}

#[test]
fn ambiguous_parent_evidence_publishes_content_without_normalized_lineage() {
    let temp = tempdir();
    let source_root = temp.path().join("source");
    let index = temp.path().join("index");
    for (directory, session_id, metadata) in [
        (
            "duplicate",
            "mistral-duplicate-child",
            r#"{
                "session_id":"mistral-duplicate-child",
                "parent_session_id":"mistral-parent",
                "parent_session_id":"conflicting-parent",
                "start_time":"2026-01-02T03:04:05Z"
            }"#,
        ),
        (
            "alias-conflict",
            "mistral-alias-child",
            r#"{
                "session_id":"mistral-alias-child",
                "parent_session_id":"mistral-parent",
                "parentSessionId":"conflicting-parent",
                "start_time":"2026-01-02T03:04:05Z"
            }"#,
        ),
        (
            "self-parent",
            "mistral-self-child",
            r#"{
                "session_id":"mistral-self-child",
                "parent_session_id":"mistral-self-child",
                "start_time":"2026-01-02T03:04:05Z"
            }"#,
        ),
        (
            "malformed-parent",
            "mistral-malformed-child",
            r#"{
                "session_id":"mistral-malformed-child",
                "parent_session_id":7,
                "start_time":"2026-01-02T03:04:05Z"
            }"#,
        ),
    ] {
        let messages = serde_json::json!({
            "role": "user",
            "message_id": format!("{session_id}-message"),
            "content": format!("{session_id} retained content"),
        })
        .to_string()
            + "\n";
        write_raw_metadata_session(&source_root, directory, metadata, &messages);
    }

    refresh_source_backed_generation(&index, &registry(&source_root), WriterOptions::default())
        .unwrap();

    for session_id in [
        "mistral-duplicate-child",
        "mistral-alias-child",
        "mistral-self-child",
        "mistral-malformed-child",
    ] {
        let record = published_session(&index, session_id).remove(0);
        assert_eq!(record.parser_revision, PARSER_REVISION);
        assert_eq!(record.parent_session_id, None, "{session_id}");
        assert_eq!(record.root_session_id, None, "{session_id}");
        assert_eq!(record.session_relationship, None, "{session_id}");
        assert_eq!(record.agent_scope, None, "{session_id}");
        assert_eq!(
            record.content.meaningful_text(),
            format!("{session_id} retained content")
        );
    }
}

#[test]
fn multilevel_parent_changes_and_missing_targets_do_not_change_the_child() {
    let messages = |id: &str, body: &str| {
        serde_json::json!({
            "role": "user",
            "message_id": id,
            "content": body,
        })
        .to_string()
            + "\n"
    };
    let temp = tempdir();
    let source_root = temp.path().join("source");
    let index = temp.path().join("index");
    write_named_session(
        &source_root,
        "grandparent",
        "mistral-grandparent",
        None,
        &messages("grandparent-message", "grandparent"),
    );
    write_named_session(
        &source_root,
        "parent",
        "mistral-parent",
        Some("mistral-grandparent"),
        &messages("parent-message", "parent"),
    );
    write_named_session(
        &source_root,
        "child",
        "mistral-child",
        Some("mistral-parent"),
        &messages("child-message", "child"),
    );

    refresh_source_backed_generation(&index, &registry(&source_root), WriterOptions::default())
        .unwrap();
    let before = published_session(&index, "mistral-child").remove(0);
    let parent = published_session(&index, "mistral-parent").remove(0);
    assert_eq!(before.parent_session_id, Some(parent.session_id));
    assert_eq!(before.agent_scope, None);
    assert_eq!(before.root_session_id, None);
    assert_eq!(before.session_relationship, None);

    write_named_session(
        &source_root,
        "parent",
        "mistral-parent",
        Some("mistral-child"),
        &messages("parent-message", "parent"),
    );
    refresh_source_backed_generation(&index, &registry(&source_root), WriterOptions::default())
        .unwrap();
    let changed_parent = published_session(&index, "mistral-child").remove(0);
    assert_eq!(changed_parent, before);

    fs::remove_dir_all(source_root.join("parent")).unwrap();
    refresh_source_backed_generation(&index, &registry(&source_root), WriterOptions::default())
        .unwrap();
    let missing_parent = published_session(&index, "mistral-child").remove(0);
    assert_eq!(missing_parent, before);
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
    let temp = tempdir();
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
    let source = source_key(SESSION_ID);
    let session_id = session_identity(&source, SESSION_ID);
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
        let activity = terminal
            .content
            .activity
            .as_ref()
            .expect("terminal activity");
        assert_eq!(
            activity.provider_call_id.as_ref(),
            Some(&TypedKey::utf8(call_id).unwrap()),
            "{call_id}: {activity:?}"
        );
        assert!(activity.result.is_some());
        let linkage = terminal.content.structured_content.as_ref().unwrap();
        assert!(linkage.get("provider_native_tool_result").is_none());
        assert_eq!(
            linkage.get("tool_call_id").and_then(Value::as_str),
            Some(call_id)
        );
        assert_eq!(
            linkage.get("name").and_then(Value::as_str),
            Some(composite_name)
        );
        assert_eq!(
            linkage.get("status").and_then(Value::as_str),
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
    let temp = tempdir();
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
    let first_terminal = first
        .iter()
        .find(|record| record.content.meaningful_text() == "first terminal result")
        .unwrap();
    let second_terminal = first
        .iter()
        .find(|record| record.content.meaningful_text() == "second terminal result")
        .unwrap();
    assert_ne!(first_terminal.event_id, second_terminal.event_id);

    let source = source_key(SESSION_ID);
    let session_id = session_identity(&source, SESSION_ID);
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
