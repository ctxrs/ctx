use std::{
    fs::{self, OpenOptions},
    io::Write,
};

use ctx_history_core::LocatorRevisionPolicy;
use serde_json::{json, Value};

use super::*;

#[test]
fn openclaw_source_backed_cold_extraction_retains_full_body_and_is_stable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let complete = format!("{} exact-tail", "bounded lexical projection ".repeat(180));
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("message-1", "user", &complete),
            message("message-2", "assistant", "short answer"),
        ],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    let sources = adapter.discover_selected(&root).unwrap();
    assert_eq!(sources.len(), 1);

    let (first_documents, first_scan) = extract(&adapter, &sources[0]);
    let (second_documents, second_scan) = extract(&adapter, &sources[0]);
    assert_eq!(first_documents.len(), 2);
    assert_eq!(
        first_documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>(),
        second_documents
            .iter()
            .map(|document| document.event_id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        first_documents[0].session_id,
        second_documents[0].session_id
    );
    assert_eq!(
        first_documents[0].source.identity(),
        second_documents[0].source.identity()
    );
    assert_eq!(
        first_documents[0].provider_session_id.as_deref(),
        Some("personal-agent/session-1")
    );
    assert!(first_documents[0].parent_session_id.is_some());
    assert_ne!(
        first_documents[0].root_session_id,
        first_documents[0].session_id
    );
    assert_eq!(
        first_documents[0].branch.as_deref(),
        Some("feature/openclaw")
    );
    assert_eq!(
        first_documents[0].source_path.as_deref(),
        transcript.to_str()
    );
    assert_eq!(first_documents[0].agent_type, "primary");
    assert!(first_documents[0].is_primary);
    assert_eq!(first_documents[0].body, complete);
    assert!(first_documents[0].body.ends_with("exact-tail"));
    assert_eq!(
        first_documents[0].locator.revision_policy(),
        LocatorRevisionPolicy::ExactSourceRevision
    );
    assert!(first_documents[0]
        .locator
        .certified_source_revision_digest()
        .is_some());

    let counts = first_scan.certified_source.counts();
    assert_eq!(counts.complete_records, 3);
    assert_eq!(counts.retained_records, 2);
    assert_eq!(counts.rejected_records, 0);
    assert_eq!(counts.ignored_records, 1);
    assert_eq!(counts.indexed_documents, 2);
    assert_eq!(
        first_scan.certified_source.content_digest(),
        second_scan.certified_source.content_digest()
    );
    assert_eq!(
        first_scan.disposition,
        OpenClawSourceBackedDispositionV0::Cold
    );
    assert!(first_scan.verified_base_prefix.is_none());
}

#[test]
fn openclaw_source_backed_indexes_only_meaningful_core_bodies() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("message-1", "user", "meaningful user body"),
            json!({
                "type": "message",
                "id": "contentless",
                "message": {"role": "assistant"},
            }),
            json!({
                "type": "message",
                "id": "successful-tool-output",
                "message": {
                    "role": "tool",
                    "status": "success",
                    "content": "successful raw tool output must not be indexed",
                },
            }),
        ],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);
    let (documents, scan) = extract(&adapter, &source);

    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, "meaningful user body");
    assert!(!documents[0].body.contains("successful raw tool output"));
    assert_eq!(scan.certified_source.counts().indexed_documents, 1);
}

#[test]
fn openclaw_source_backed_certifies_noop_append_rewrite_and_truncate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("message-1", "user", "first body"),
        ],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);

    let (cold_documents, cold) = extract(&adapter, &source);
    assert_eq!(cold_documents.len(), 1);
    assert_eq!(cold.disposition, OpenClawSourceBackedDispositionV0::Cold);

    let (noop_documents, noop) =
        extract_with_previous(&adapter, &source, Some(&cold.certified_source));
    assert!(noop_documents.is_empty());
    assert_eq!(noop.disposition, OpenClawSourceBackedDispositionV0::Noop);
    assert_eq!(
        noop.certified_source.content_digest(),
        cold.certified_source.content_digest()
    );

    append_record(
        &transcript,
        &message("message-2", "assistant", "appended exact body"),
    );
    let (append_documents, append) =
        extract_with_previous(&adapter, &source, Some(&noop.certified_source));
    assert_eq!(
        append_documents
            .iter()
            .map(|document| document.body.as_str())
            .collect::<Vec<_>>(),
        vec!["appended exact body"]
    );
    assert_eq!(
        append.disposition,
        OpenClawSourceBackedDispositionV0::Append
    );
    assert_eq!(
        append
            .verified_base_prefix
            .expect("append must certify its base")
            .bytes,
        noop.certified_source
            .frontier()
            .expect("noop certificate must retain a frontier")
            .certified_prefix_bytes()
    );

    let rewritten = format!("rewritten {} tail", "complete body ".repeat(220));
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("message-3", "user", &rewritten),
        ],
    );
    let (rewrite_documents, rewrite) =
        extract_with_previous(&adapter, &source, Some(&append.certified_source));
    assert_eq!(rewrite_documents.len(), 1);
    assert_eq!(rewrite_documents[0].body, rewritten);
    assert_eq!(
        rewrite.disposition,
        OpenClawSourceBackedDispositionV0::Replacement
    );

    write_fixture(
        &transcript,
        &[
            header("session-1"),
            message("message-4", "assistant", "short after truncation"),
        ],
    );
    let (truncate_documents, truncate) =
        extract_with_previous(&adapter, &source, Some(&rewrite.certified_source));
    assert_eq!(truncate_documents.len(), 1);
    assert_eq!(truncate_documents[0].body, "short after truncation");
    assert_eq!(
        truncate.disposition,
        OpenClawSourceBackedDispositionV0::Replacement
    );
    assert!(truncate.verified_base_prefix.is_none());
}

#[test]
fn openclaw_source_backed_discovery_distinguishes_delete_from_unavailable_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[header("session-1"), message("message-1", "user", "hello")],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    assert_eq!(adapter.discover_selected(&root).unwrap().len(), 1);

    fs::remove_file(&transcript).unwrap();
    assert!(adapter.discover_selected(&root).unwrap().is_empty());

    fs::remove_dir_all(&root).unwrap();
    assert!(adapter.discover_selected(&root).is_err());
}

#[test]
fn openclaw_source_backed_final_revalidation_rejects_transcript_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    write_fixture(
        &transcript,
        &[header("session-1"), message("message-1", "user", "hello")],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);
    let mut reader = adapter
        .open_source(&source, "2026-07-28T12:00:00Z".parse().unwrap(), None)
        .unwrap();
    while reader.next_page().unwrap().is_some() {}

    append_record(
        &transcript,
        &message("message-2", "assistant", "late mutation"),
    );
    assert!(reader.finish().is_err());
}

#[test]
fn openclaw_source_backed_hydrates_typed_content_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let typed_body = "first typed block\nsecond typed block";
    write_fixture(
        &transcript,
        &[
            header("session-1"),
            json!({
                "type": "message",
                "id": "typed-message",
                "timestamp": "2026-07-28T12:00:01Z",
                "message": {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "first typed block"},
                        {"type": "text", "text": "second typed block"},
                    ],
                },
            }),
        ],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);
    let (documents, _) = extract(&adapter, &source);
    assert_eq!(documents.len(), 1);
    assert_eq!(documents[0].body, typed_body);

    let hydrated = adapter.hydrate(&source, &documents[0].locator).unwrap();
    assert_eq!(hydrated.provider_bytes, typed_body.as_bytes());
    assert_eq!(hydrated.decoded_display_text.as_deref(), Some(typed_body));
}

#[test]
fn openclaw_source_backed_exact_hydration_returns_source_bytes_and_fails_after_append() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let complete = format!("{} exact hydration tail", "long message ".repeat(300));
    write_fixture(
        &transcript,
        &[header("session-1"), message("message-1", "user", &complete)],
    );
    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);
    let (documents, _) = extract(&adapter, &source);
    let hydrated = adapter.hydrate(&source, &documents[0].locator).unwrap();
    assert_eq!(
        hydrated.decoded_display_text.as_deref(),
        Some(complete.as_str())
    );
    assert!(std::str::from_utf8(&hydrated.provider_bytes)
        .unwrap()
        .contains("exact hydration tail"));

    let mut file = OpenOptions::new().append(true).open(&transcript).unwrap();
    serde_json::to_writer(
        &mut file,
        &message("message-2", "assistant", "later append"),
    )
    .unwrap();
    file.write_all(b"\n").unwrap();
    let error = adapter.hydrate(&source, &documents[0].locator).unwrap_err();
    assert!(matches!(
        error,
        OpenClawSourceBackedErrorV0::LocatorSourceRevisionMismatch
    ));
}

#[test]
fn openclaw_source_backed_rejects_current_agent_sqlite_without_format_expansion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let sqlite = root.join("agents/main/agent/openclaw-agent.sqlite");
    fs::create_dir_all(sqlite.parent().unwrap()).unwrap();
    fs::write(&sqlite, b"SQLite format 3\0").unwrap();

    let error = openclaw_source_backed_adapter_v0()
        .discover_selected(&root)
        .unwrap_err();
    match error {
        OpenClawSourceBackedErrorV0::UnsupportedSelectedSource { path, reason } => {
            assert_eq!(path, root);
            assert!(reason.contains("openclaw-agent.sqlite"));
        }
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn compound_authority_openclaw_rejects_missing_auxiliary_before_final_revalidation() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let records = [header("session-1"), message("message-1", "user", "hello")];
    write_fixture(&transcript, &records);
    let index = transcript.parent().unwrap().join("sessions.json");
    fs::remove_file(&index).unwrap();

    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);
    let mut reader = adapter
        .open_source(&source, "2026-07-28T12:00:00Z".parse().unwrap(), None)
        .unwrap();
    while reader.next_page().unwrap().is_some() {}
    fs::write(&index, r#"{"session-1":{"sessionId":"session-1"}}"#).unwrap();

    assert!(reader.finish().is_err());
}

#[cfg(unix)]
#[test]
fn compound_authority_openclaw_rejects_ancestor_swap_and_stale_locator() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("openclaw");
    let transcript = transcript_path(&root);
    let records = [header("session-1"), message("message-1", "user", "hello")];
    write_fixture(&transcript, &records);

    let adapter = openclaw_source_backed_adapter_v0();
    let source = adapter.discover_selected(&root).unwrap().remove(0);
    let (documents, _) = extract(&adapter, &source);
    let retired = temp.path().join("retired-openclaw");
    fs::rename(&root, &retired).unwrap();
    write_fixture(&transcript, &records);

    assert!(adapter.hydrate(&source, &documents[0].locator).is_err());
}

fn extract(
    adapter: &OpenClawSourceBackedAdapterV0,
    source: &OpenClawSourceBackedSourceV0,
) -> (Vec<LexicalDocument>, OpenClawSourceBackedScanV0) {
    extract_with_previous(adapter, source, None)
}

fn extract_with_previous(
    adapter: &OpenClawSourceBackedAdapterV0,
    source: &OpenClawSourceBackedSourceV0,
    previous: Option<&CertifiedSource>,
) -> (Vec<LexicalDocument>, OpenClawSourceBackedScanV0) {
    let mut reader = adapter
        .open_source(source, "2026-07-28T12:00:00Z".parse().unwrap(), previous)
        .unwrap();
    let mut documents = Vec::new();
    while let Some(page) = reader.next_page().unwrap() {
        assert!(page.complete_records > 0);
        assert!(page.retained_records >= page.documents.len() as u64);
        assert_eq!(page.rejected_records, 0);
        assert!(page.certified_prefix_bytes > 0);
        documents.extend(page.documents);
    }
    (documents, reader.finish().unwrap())
}

fn append_record(path: &Path, record: &Value) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    serde_json::to_writer(&mut file, record).unwrap();
    file.write_all(b"\n").unwrap();
}

fn transcript_path(root: &Path) -> PathBuf {
    root.join("agents/personal-agent/sessions/session-1.jsonl")
}

fn header(id: &str) -> Value {
    json!({
        "type": "session",
        "id": id,
        "timestamp": "2026-07-28T12:00:00Z",
        "cwd": "/workspace/openclaw",
    })
}

fn message(id: &str, role: &str, content: &str) -> Value {
    json!({
        "type": "message",
        "id": id,
        "timestamp": "2026-07-28T12:00:01Z",
        "message": {
            "role": role,
            "content": content,
        }
    })
}

fn write_fixture(path: &Path, records: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record).unwrap();
        bytes.push(b'\n');
    }
    fs::write(path, bytes).unwrap();
    fs::write(
        path.parent().unwrap().join("sessions.json"),
        json!({
            "session-1": {
                "sessionId": "session-1",
                "label": "source-backed fixture",
                "parentSessionId": "parent-1",
                "rootSessionId": "root-1",
                "branch": "feature/openclaw",
            }
        })
        .to_string(),
    )
    .unwrap();
}
