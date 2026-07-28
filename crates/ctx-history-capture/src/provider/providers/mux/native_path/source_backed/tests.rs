use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{EventType, LocatorRevisionPolicy, NativeRecordCoordinate, TypedKey};
use serde_json::{json, Value};

use super::*;
use crate::{
    complete_content::{
        verified_content_profile_for_locator, AuthorizedSourceRoute, CompleteContentErrorKind,
        CompleteContentHashAuthority, CompleteContentResolverRegistry, CompleteContentSourceFamily,
        CompleteMessageRequest, JsonlCompleteContentResolver, SourceAccessBroker, SourceSnapshot,
        VerifiedContentRole,
    },
    test_support_paths::tempdir,
};

fn message(id: &str, role: &str, sequence: i64, text: &str) -> Value {
    json!({
        "id": id,
        "workspaceId": "session-1",
        "role": role,
        "createdAt": "2026-07-28T12:00:00Z",
        "parts": [{"type": "text", "text": text}],
        "metadata": {"historySequence": sequence},
    })
}

fn write_metadata(session: &Path, workspace_id: &str) {
    fs::write(
        session.join("metadata.json"),
        serde_json::to_vec(&json!({
            "workspaceId": workspace_id,
            "createdAt": "2026-07-28T11:59:00Z",
            "projectPath": "/work/mux-project",
            "model": "mux-test-model",
        }))
        .unwrap(),
    )
    .unwrap();
}

fn write_chat(session: &Path, rows: &[Value]) {
    let mut bytes = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut bytes, row).unwrap();
        bytes.push(b'\n');
    }
    fs::write(session.join("chat.jsonl"), bytes).unwrap();
}

fn source_candidates(root: &Path) -> Vec<MuxSourceBackedCandidate> {
    discover_mux_source_backed_sources(root, "2026-07-28T12:30:00Z".parse().unwrap()).unwrap()
}

fn collect_scan(
    candidate: &MuxSourceBackedCandidate,
    base: Option<&CertifiedSource>,
) -> (
    MuxSourceBackedScanReceipt,
    Vec<MuxSourceBackedRecord>,
    Vec<MuxUnaddressableRecord>,
) {
    let mut records = Vec::new();
    let mut unaddressable = Vec::new();
    let receipt = scan_mux_source_backed(candidate, base, |page| {
        assert_eq!(page.source, candidate.source_key);
        assert_eq!(page.session_id, candidate.session_id);
        assert!(page.records.len() + page.unaddressable.len() <= 8);
        records.extend(page.records);
        unaddressable.extend(page.unaddressable);
        Ok(())
    })
    .unwrap();
    (receipt, records, unaddressable)
}

#[test]
fn cold_append_and_rewrite_keep_stable_ids_and_bounded_projection() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    let long = "x".repeat(MAX_BODY_PREVIEW_CHARS + 200);
    let first_rows = vec![
        message("message-0", "user", 0, &long),
        message("message-1", "assistant", 1, "answer"),
    ];
    write_chat(&session, &first_rows);
    let candidate = source_candidates(&root).pop().unwrap();

    let (cold, cold_records, cold_unaddressable) = collect_scan(&candidate, None);
    assert!(matches!(cold.disposition, MuxSourceBackedDisposition::Cold));
    assert!(cold_unaddressable.is_empty());
    assert_eq!(cold_records.len(), 2);
    assert_eq!(cold.emitted_documents, 2);
    assert_eq!(
        cold_records[0].document.body.chars().count(),
        MAX_BODY_PREVIEW_CHARS
    );
    let cold_ids = cold_records
        .iter()
        .map(|record| record.document.event_id)
        .collect::<Vec<_>>();

    let (_, rebuilt_records, _) = collect_scan(&candidate, None);
    assert_eq!(
        rebuilt_records
            .iter()
            .map(|record| record.document.event_id)
            .collect::<Vec<_>>(),
        cold_ids
    );

    let (unchanged, unchanged_records, _) = collect_scan(&candidate, Some(&cold.certificate));
    assert!(matches!(
        unchanged.disposition,
        MuxSourceBackedDisposition::Unchanged
    ));
    assert!(unchanged_records.is_empty());

    let appended = message("message-2", "assistant", 2, "appended");
    let mut chat = OpenOptions::new()
        .append(true)
        .open(session.join("chat.jsonl"))
        .unwrap();
    writeln!(chat, "{appended}").unwrap();
    chat.sync_all().unwrap();
    drop(chat);

    let (append, append_records, _) = collect_scan(&candidate, Some(&cold.certificate));
    assert!(matches!(
        append.disposition,
        MuxSourceBackedDisposition::Append { .. }
    ));
    assert_eq!(append_records.len(), 1);
    assert_eq!(append.emitted_documents, 1);
    let (_, after_append_rebuild, _) = collect_scan(&candidate, None);
    assert_eq!(
        after_append_rebuild
            .iter()
            .take(2)
            .map(|record| record.document.event_id)
            .collect::<Vec<_>>(),
        cold_ids
    );

    let rewritten_first = message("message-0", "user", 0, "rewritten source body");
    write_chat(
        &session,
        &[rewritten_first, first_rows[1].clone(), appended.clone()],
    );
    let (replacement, replacement_records, _) = collect_scan(&candidate, Some(&append.certificate));
    let MuxSourceBackedDisposition::Replacement { evidence } = replacement.disposition else {
        panic!("chat rewrite must be replacement");
    };
    assert_eq!(evidence.reason, MuxReplacementReason::ChatPrefixChanged);
    assert_ne!(
        evidence.prior_content_digest,
        evidence.replacement_content_digest
    );
    assert_eq!(replacement_records[0].document.event_id, cold_ids[0]);
    assert_eq!(
        replacement_records[0].document.body,
        "rewritten source body"
    );
}

#[test]
fn partial_snapshot_uses_exact_revision_and_replacement_evidence() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    let chat_body = format!("{}-chat", "c".repeat(MAX_BODY_PREVIEW_CHARS + 32));
    let partial_body = format!("{}-partial", "p".repeat(MAX_BODY_PREVIEW_CHARS + 32));
    write_chat(&session, &[message("chat-0", "user", 0, &chat_body)]);
    fs::write(
        session.join("partial.json"),
        serde_json::to_vec(&message("partial-1", "assistant", 1, &partial_body)).unwrap(),
    )
    .unwrap();
    let candidate = source_candidates(&root).pop().unwrap();

    let (cold, records, _) = collect_scan(&candidate, None);
    assert_eq!(records.len(), 2);
    let chat = records
        .iter()
        .find(|record| record.stream_kind == MuxStreamKind::Chat)
        .unwrap();
    let partial = records
        .iter()
        .find(|record| record.stream_kind == MuxStreamKind::Partial)
        .unwrap();
    assert_eq!(
        chat.document.locator.revision_policy(),
        LocatorRevisionPolicy::StableRecordEvidence
    );
    assert!(chat
        .document
        .locator
        .certified_source_revision_digest()
        .is_none());
    assert_eq!(
        partial.document.locator.revision_policy(),
        LocatorRevisionPolicy::ExactSourceRevision
    );
    assert!(partial
        .document
        .locator
        .certified_source_revision_digest()
        .is_some());
    assert_eq!(
        mux_source_path_for_locator(&candidate, &chat.document.locator),
        Some(session.join("chat.jsonl").as_path())
    );
    assert_eq!(
        mux_source_path_for_locator(&candidate, &partial.document.locator),
        Some(session.join("partial.json").as_path())
    );
    assert_eq!(resolve_exact(&candidate, chat).unwrap(), chat_body);
    assert_eq!(resolve_exact(&candidate, partial).unwrap(), partial_body);

    let partial_id = partial.document.event_id;
    fs::write(
        session.join("partial.json"),
        serde_json::to_vec(&message(
            "partial-1",
            "assistant",
            1,
            "replacement partial body",
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        resolve_exact(&candidate, partial).unwrap_err(),
        CompleteContentErrorKind::SourceChanged
    );

    let (replacement, replacement_records, _) = collect_scan(&candidate, Some(&cold.certificate));
    let MuxSourceBackedDisposition::Replacement { evidence } = replacement.disposition else {
        panic!("partial mutation must be replacement");
    };
    assert_eq!(
        evidence.reason,
        MuxReplacementReason::PartialSnapshotChanged
    );
    let replaced_partial = replacement_records
        .iter()
        .find(|record| record.stream_kind == MuxStreamKind::Partial)
        .unwrap();
    assert_eq!(replaced_partial.document.event_id, partial_id);
}

#[test]
fn redacted_and_missing_outputs_are_explicitly_unaddressable() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    write_chat(
        &session,
        &[
            message("message-0", "user", 0, "ordinary"),
            json!({
                "id": "redacted-output",
                "workspaceId": "session-1",
                "role": "assistant",
                "createdAt": "2026-07-28T12:00:01Z",
                "parts": [{
                    "type": "dynamic-tool",
                    "toolCallId": "redacted-call",
                    "toolName": "shell",
                    "state": "output-redacted",
                    "success": false,
                }],
                "metadata": {"historySequence": 1},
            }),
            json!({
                "id": "missing-output",
                "workspaceId": "session-1",
                "role": "assistant",
                "createdAt": "2026-07-28T12:00:02Z",
                "parts": [{
                    "type": "dynamic-tool",
                    "toolCallId": "missing-call",
                    "toolName": "shell",
                    "state": "output-available",
                    "success": false,
                }],
                "metadata": {"historySequence": 2},
            }),
        ],
    );
    let candidate = source_candidates(&root).pop().unwrap();

    let (receipt, records, unaddressable) = collect_scan(&candidate, None);
    assert_eq!(records.len(), 1);
    assert_eq!(receipt.emitted_unaddressable, 2);
    assert_eq!(unaddressable.len(), 2);
    assert_eq!(
        unaddressable
            .iter()
            .map(|record| record.reason)
            .collect::<Vec<_>>(),
        vec![
            MuxUnaddressableReason::RedactedOutput,
            MuxUnaddressableReason::MissingOutput,
        ]
    );
    assert!(unaddressable[0].bounded_projection.is_none());
    assert!(unaddressable[1].bounded_projection.is_some());
}

#[test]
fn subagent_chat_is_supported_but_chat_archive_is_not() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let parent = root.join("parent");
    let child = parent.join("subagent-transcripts").join("child");
    let archive_only = root.join("archive-only");
    fs::create_dir_all(&parent).unwrap();
    fs::create_dir_all(&child).unwrap();
    fs::create_dir_all(&archive_only).unwrap();
    write_metadata(&parent, "parent");
    write_metadata(&child, "child");
    write_chat(
        &parent,
        &[json!({
            "id": "parent-message",
            "workspaceId": "parent",
            "role": "user",
            "parts": [{"type": "text", "text": "parent"}],
        })],
    );
    write_chat(
        &child,
        &[json!({
            "id": "child-message",
            "workspaceId": "child",
            "role": "assistant",
            "parts": [{"type": "text", "text": "child"}],
        })],
    );
    fs::write(
        archive_only.join("chat-archive.jsonl"),
        format!("{}\n", message("archive", "assistant", 0, "unsupported")),
    )
    .unwrap();

    let candidates = source_candidates(&root);
    assert_eq!(candidates.len(), 2);
    let child = candidates
        .iter()
        .find(|candidate| candidate.provider_session_id() == "child")
        .unwrap();
    assert_eq!(child.parent_provider_session_id(), Some("parent"));
    assert!(candidates
        .iter()
        .all(|candidate| candidate.provider_session_id() != "archive-only"));
}

fn resolve_exact(
    candidate: &MuxSourceBackedCandidate,
    record: &MuxSourceBackedRecord,
) -> Result<String, CompleteContentErrorKind> {
    let locator = mux_complete_content_locator(&record.document.locator)
        .expect("source-backed locator must bridge to the existing Mux route");
    assert_eq!(locator, record.complete_content_locator);
    let path = mux_source_path_for_locator(candidate, &record.document.locator).unwrap();
    let event_id = record.document.event_id.as_uuid();
    let source_access = SourceAccessBroker::new()
        .admit(
            AuthorizedSourceRoute {
                source_id: candidate.source_key.identity().as_uuid(),
                provider: CaptureProvider::Mux,
                source_format: MUX_SOURCE_FORMAT.to_owned(),
                family: CompleteContentSourceFamily::Jsonl,
                raw_source_path: path.to_path_buf(),
                source_root: Some(candidate.source.session_dir.clone()),
                source_identity: Some(candidate.source_key.identity().to_string()),
                source_snapshot: SourceSnapshot {
                    size_bytes: Some(fs::metadata(path).unwrap().len()),
                    ..SourceSnapshot::default()
                },
            },
            event_id,
        )
        .unwrap();
    let request = CompleteMessageRequest {
        event_id,
        provider: CaptureProvider::Mux,
        source_format: MUX_SOURCE_FORMAT.to_owned(),
        source_access,
        source_family: Some(CompleteContentSourceFamily::Jsonl),
        content_profile: verified_content_profile_for_locator(
            CaptureProvider::Mux,
            MUX_SOURCE_FORMAT,
            CompleteContentSourceFamily::Jsonl,
            VerifiedContentRole::MessageBody,
            locator.kind(),
        )
        .unwrap()
        .to_owned(),
        source_locator: Some(locator),
        provider_session_id: Some(candidate.provider_session_id().to_owned()),
        source_record_ordinal: record.source_record_ordinal,
        source_record_subrecord_index: 0,
        expected_provider_event_hash: record.native_record_id.clone(),
        expected_hash_authority: CompleteContentHashAuthority::ProviderSupplied,
        expected_native_record_id: Some(record.native_record_id.clone()),
        expected_record_digest: Some(record.complete_content_record_digest.clone()),
        expected_content_ref: record.message_content_ref.clone(),
        indexed_text: record.document.body.clone(),
        indexed_limit_chars: MAX_BODY_PREVIEW_CHARS,
    };
    let mut registry = CompleteContentResolverRegistry::new();
    registry.register(JsonlCompleteContentResolver::new());
    registry
        .resolve(&[request])
        .map(|messages| messages[0].text.clone())
        .map_err(|error| error.kind)
}

#[test]
fn provider_native_locator_is_tagged_and_rejects_foreign_coordinates() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    write_chat(&session, &[message("message-0", "user", 0, "hello")]);
    let candidate = source_candidates(&root).pop().unwrap();
    let (_, records, _) = collect_scan(&candidate, None);
    let locator = &records[0].document.locator;
    let NativeRecordCoordinate::ProviderNative {
        namespace,
        coordinate: TypedKey::Bytes(value),
    } = locator.coordinate()
    else {
        panic!("Mux locator must be provider-native");
    };
    assert_eq!(namespace, MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE);
    assert_eq!(value.first(), Some(&1));
    assert_eq!(records[0].document.event_type, EventType::Message.as_str());
}
