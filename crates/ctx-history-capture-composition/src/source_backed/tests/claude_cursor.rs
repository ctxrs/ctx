use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{CaptureProvider, CoreRecord};
use ctx_history_index::{VerifiedIndex, WriterOptions};
use serde_json::{json, Value};

use crate::{
    provider::source_backed::family::jsonl::set_after_jsonl_semantic_preflight_hook,
    refresh_source_backed_generation, register_landed_source_backed_route, ProviderCatalogSupport,
    ProviderImportSupport, ProviderSource, ProviderSourceKind, ProviderSourceStatus,
    SourceBackedProviderRegistry, SourceBackedRouteSelection, SourceBackedSourceFailureClass,
};

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 15_000_000,
    }
}

fn registry(
    provider: CaptureProvider,
    source_format: &'static str,
    root: &Path,
) -> SourceBackedProviderRegistry {
    let mut registry = SourceBackedProviderRegistry::new();
    register_landed_source_backed_route(
        &mut registry,
        ProviderSource {
            provider,
            path: root.to_path_buf(),
            exists: true,
            source_format,
            source_kind: ProviderSourceKind::NativeHistory,
            import_support: ProviderImportSupport::Native,
            catalog_support: ProviderCatalogSupport::None,
            status: ProviderSourceStatus::Available,
            unsupported_reason: None,
        },
        SourceBackedRouteSelection::Automatic,
    )
    .unwrap();
    assert_eq!(registry.routes().len(), 1);
    registry
}

fn write_transcript(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
}

fn append_transcript(path: &Path, row: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, row).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
}

fn indexed_records(
    index: &Path,
    provider: CaptureProvider,
    native_session_id: &str,
) -> Vec<CoreRecord> {
    let verified = VerifiedIndex::open(index).unwrap();
    let mut records = verified
        .manifest()
        .sources
        .iter()
        .filter(|source| source.observation().source().provider() == provider.as_str())
        .flat_map(|source| {
            verified
                .core_source_event_page(source.observation().source(), None, 64)
                .unwrap()
                .items
                .into_iter()
                .map(|item| item.core_record)
        })
        .filter(|record| record.provider_session_id.as_deref() == Some(native_session_id))
        .collect::<Vec<_>>();
    records.sort_by_key(|record| record.event_sequence);
    records
}

fn certified_prefix_bytes(index: &Path, provider: CaptureProvider) -> u64 {
    let verified = VerifiedIndex::open(index).unwrap();
    verified
        .manifest()
        .sources
        .iter()
        .find(|source| source.observation().source().provider() == provider.as_str())
        .unwrap()
        .frontier()
        .expect("JSONL publication must persist a checkpoint frontier")
        .certified_prefix_bytes()
}

fn assert_literal_bodies(records: &[CoreRecord], expected: &[&str]) {
    assert_eq!(
        records
            .iter()
            .map(|record| record.content.normalized_body.as_deref().unwrap())
            .collect::<Vec<_>>(),
        expected
    );
}

#[allow(clippy::too_many_arguments)]
fn exercise_lifecycle(
    provider: CaptureProvider,
    source_format: &'static str,
    source_root: &Path,
    transcript: &Path,
    index: &Path,
    native_session_id: &str,
    first: Value,
    second: Value,
    racing: Value,
) {
    write_transcript(transcript, &[first]);
    let registry = registry(provider, source_format, source_root);

    let cold = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(cold.failed_routes.is_empty());
    assert_eq!(cold.successful_route_ids.len(), 1);
    let cold_records = indexed_records(index, provider, native_session_id);
    assert_literal_bodies(&cold_records, &["literal first"]);
    let cold_checkpoint = certified_prefix_bytes(index, provider);
    assert_eq!(cold_checkpoint, fs::metadata(transcript).unwrap().len());

    append_transcript(transcript, &second);
    let appended = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(appended.failed_routes.is_empty());
    let appended_records = indexed_records(index, provider, native_session_id);
    assert_literal_bodies(&appended_records, &["literal first", "literal second"]);
    assert_eq!(appended_records[0].event_id, cold_records[0].event_id);
    let appended_checkpoint = certified_prefix_bytes(index, provider);
    assert!(appended_checkpoint > cold_checkpoint);
    assert_eq!(appended_checkpoint, fs::metadata(transcript).unwrap().len());

    append_transcript(transcript, &racing);
    let hook_path = fs::canonicalize(transcript).unwrap();
    set_after_jsonl_semantic_preflight_hook(hook_path.clone(), move || {
        let before = fs::read_to_string(&hook_path).unwrap();
        let after = before.replace("race-before", "race-after!");
        assert_eq!(before.len(), after.len());
        assert_ne!(before, after);
        fs::write(&hook_path, after).unwrap();
    });

    let failed = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(matches!(
        failed.failed_routes.as_slice(),
        [failure]
            if failure.class == SourceBackedSourceFailureClass::SourceChanged
                && failure.carried_forward
    ));
    assert_eq!(certified_prefix_bytes(index, provider), appended_checkpoint);
    assert_eq!(
        indexed_records(index, provider, native_session_id),
        appended_records
    );

    let recovered = refresh_source_backed_generation(index, &registry, writer_options()).unwrap();
    assert!(recovered.failed_routes.is_empty());
    let recovered_records = indexed_records(index, provider, native_session_id);
    assert_literal_bodies(
        &recovered_records,
        &["literal first", "literal second", "race-after!"],
    );
    assert_eq!(recovered_records[0].event_id, cold_records[0].event_id);
    assert_eq!(
        certified_prefix_bytes(index, provider),
        fs::metadata(transcript).unwrap().len()
    );
}

fn cursor_message(role: &str, timestamp: &str, text: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "role": role,
        "message": {
            "role": role,
            "content": [{"type": "text", "text": text}]
        }
    })
}

#[test]
fn cursor_route_publishes_cold_append_and_recovers_from_carried_checkpoint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let root = temp.path().join("cursor-data");
    let native_session_id = "neutral-cursor-session";
    let transcript = root
        .join("projects/project/agent-transcripts")
        .join(native_session_id)
        .join(format!("{native_session_id}.jsonl"));
    exercise_lifecycle(
        CaptureProvider::Cursor,
        "cursor_agent_transcript_jsonl_tree",
        &root,
        &transcript,
        &temp.path().join("cursor-index"),
        native_session_id,
        cursor_message("user", "2026-08-16T00:00:00Z", "literal first"),
        cursor_message("assistant", "2026-08-16T00:00:01Z", "literal second"),
        cursor_message("assistant", "2026-08-16T00:00:02Z", "race-before"),
    );
}

fn claude_message(kind: &str, uuid: &str, session_id: &str, text: &str) -> Value {
    json!({
        "type": kind,
        "uuid": uuid,
        "sessionId": session_id,
        "message": {"role": kind, "content": text}
    })
}

#[test]
fn claude_route_publishes_cold_append_and_recovers_from_carried_checkpoint() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let native_session_id = "neutral-claude-session";
    let transcript = projects
        .join("project")
        .join(format!("{native_session_id}.jsonl"));
    exercise_lifecycle(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
        &transcript,
        &temp.path().join("claude-index"),
        native_session_id,
        claude_message("user", "literal-first", native_session_id, "literal first"),
        claude_message(
            "assistant",
            "literal-second",
            native_session_id,
            "literal second",
        ),
        claude_message(
            "assistant",
            "literal-racing",
            native_session_id,
            "race-before",
        ),
    );
}

#[test]
fn claude_route_indexes_repeated_native_record_id_as_distinct_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let native_session_id = "repeated-uuid-claude-session";
    let transcript = projects
        .join("project")
        .join(format!("{native_session_id}.jsonl"));
    // Claude Code can re-emit a record that reuses an earlier `uuid`; both rows
    // stay distinct events, so neither may be dropped or collapsed.
    write_transcript(
        &transcript,
        &[
            claude_message("user", "repeated-uuid", native_session_id, "literal first"),
            claude_message(
                "assistant",
                "repeated-uuid",
                native_session_id,
                "literal second",
            ),
        ],
    );
    let registry = registry(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
    );
    let index = temp.path().join("claude-repeated-uuid-index");

    let published = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();

    assert!(
        published.failed_routes.is_empty(),
        "{:?}",
        published.failed_routes
    );
    let records = indexed_records(&index, CaptureProvider::Claude, native_session_id);
    assert_literal_bodies(&records, &["literal first", "literal second"]);
    assert_ne!(records[0].event_id, records[1].event_id);
}

fn claude_hook_attachment(uuid: &str, session_id: &str, slug: Option<&str>) -> Value {
    let mut row = json!({
        "type": "attachment",
        "uuid": uuid,
        "sessionId": session_id,
        "parentUuid": "attachment-parent",
        "attachment": {
            "type": "hook_success",
            "hookName": "PreToolUse:ToolSearch",
            "hookEvent": "PreToolUse",
            "toolUseID": "toolu_repeated",
        }
    });
    if let Some(slug) = slug {
        row["slug"] = json!(slug);
    }
    row
}

#[test]
fn claude_route_indexes_re_emitted_attachment_uuid_as_distinct_events() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let native_session_id = "re-emitted-attachment-claude-session";
    let transcript = projects
        .join("project")
        .join(format!("{native_session_id}.jsonl"));
    // Observed shape: Claude Code re-emits a hook attachment under the same
    // `uuid`, the second copy carrying one extra field.
    write_transcript(
        &transcript,
        &[
            claude_hook_attachment("repeated-attachment-uuid", native_session_id, None),
            claude_hook_attachment(
                "repeated-attachment-uuid",
                native_session_id,
                Some("re-emitted"),
            ),
        ],
    );
    let registry = registry(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
    );
    let index = temp.path().join("claude-attachment-index");

    let published = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();

    assert!(
        published.failed_routes.is_empty(),
        "{:?}",
        published.failed_routes
    );
    let records = indexed_records(&index, CaptureProvider::Claude, native_session_id);
    assert_eq!(records.len(), 2);
    assert_ne!(records[0].event_id, records[1].event_id);
}

fn claude_message_without_uuid(kind: &str, session_id: &str, text: &str) -> Value {
    json!({
        "type": kind,
        "sessionId": session_id,
        "message": {"role": kind, "content": text}
    })
}

#[test]
fn claude_route_keeps_separating_repeated_rows_without_a_native_record_id() {
    let temp = crate::test_support_paths::tempdir().unwrap();
    let projects = temp.path().join("projects");
    let native_session_id = "no-uuid-claude-session";
    let transcript = projects
        .join("project")
        .join(format!("{native_session_id}.jsonl"));
    // Rows without a native record id keep using the content-digest fallback,
    // whose occurrence still separates byte-identical repeats.
    write_transcript(
        &transcript,
        &[
            claude_message_without_uuid("user", native_session_id, "identical body"),
            claude_message_without_uuid("user", native_session_id, "identical body"),
        ],
    );
    let registry = registry(
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
        &projects,
    );
    let index = temp.path().join("claude-no-uuid-index");

    let published = refresh_source_backed_generation(&index, &registry, writer_options()).unwrap();

    assert!(
        published.failed_routes.is_empty(),
        "{:?}",
        published.failed_routes
    );
    let records = indexed_records(&index, CaptureProvider::Claude, native_session_id);
    assert_literal_bodies(&records, &["identical body", "identical body"]);
    assert_ne!(records[0].event_id, records[1].event_id);
}
