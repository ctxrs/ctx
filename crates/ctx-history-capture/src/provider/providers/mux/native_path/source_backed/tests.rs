use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use ctx_history_core::{
    BatchHydrationRequest, ContentSourceResolver, EventHydrationRequest, EventType,
    HydrationFailureKind, LocatorRevisionPolicy, NativeRecordCoordinate, SessionHydrationRequest,
    SourceRecordLocator, TypedKey,
};
use serde_json::{json, Value};

use super::*;
use crate::test_support_paths::tempdir;

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

fn hydration_resolver(root: &Path) -> MuxSourceBackedResolverV0 {
    MuxSourceBackedResolverV0::discover(root, "2026-07-28T12:30:00Z".parse().unwrap()).unwrap()
}

fn hydration_request(record: &MuxSourceBackedRecord) -> EventHydrationRequest {
    EventHydrationRequest::new(record.document.event_id, record.document.locator.clone()).unwrap()
}

fn hydrate_exact(root: &Path, record: &MuxSourceBackedRecord) -> Vec<u8> {
    hydration_resolver(root)
        .hydrate_event(&hydration_request(record))
        .unwrap()
        .provider_bytes
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
fn exact_hydration_indexes_full_body_tail_terms_and_preserves_order() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    let long_message = format!("message-head-{}-message-tail", "m".repeat(4_096));
    let long_input = format!("input-head-{}-input-tail", "i".repeat(4_096));
    let long_output = format!("output-head-{}-output-tail", "o".repeat(4_096));
    write_chat(
        &session,
        &[
            message("message-0", "user", 0, &long_message),
            json!({
                "id": "tool-call",
                "workspaceId": "session-1",
                "role": "assistant",
                "createdAt": "2026-07-28T12:00:01Z",
                "parts": [{
                    "type": "dynamic-tool",
                    "toolCallId": "call-1",
                    "toolName": "shell",
                    "state": "input-available",
                    "input": long_input.clone(),
                }],
                "metadata": {"historySequence": 1},
            }),
            json!({
                "id": "failed-output",
                "workspaceId": "session-1",
                "role": "assistant",
                "createdAt": "2026-07-28T12:00:02Z",
                "parts": [{
                    "type": "dynamic-tool",
                    "toolCallId": "call-1",
                    "toolName": "shell",
                    "state": "output-available",
                    "success": false,
                    "output": long_output.clone(),
                }],
                "metadata": {"historySequence": 2},
            }),
        ],
    );
    let candidate = source_candidates(&root).pop().unwrap();
    let (_, records, unaddressable) = collect_scan(&candidate, None);
    assert!(unaddressable.is_empty());
    assert_eq!(records.len(), 3);
    assert!(records.iter().all(|record| {
        record
            .document
            .locator
            .certified_source_revision_digest()
            .is_some()
    }));
    let expected = vec![
        long_message.clone(),
        format!("tool call: shell\ninput: {long_input}"),
        long_output.clone(),
    ];
    assert_eq!(
        records
            .iter()
            .map(|record| record.document.body.clone())
            .collect::<Vec<_>>(),
        expected
    );
    assert!(records[0].document.body.contains("message-tail"));
    assert!(records[1].document.body.contains("input-tail"));
    assert!(records[2].document.body.contains("output-tail"));

    let request_order = [2_usize, 1, 0];
    let requests = request_order
        .iter()
        .map(|index| hydration_request(&records[*index]))
        .collect::<Vec<_>>();
    let session_request =
        SessionHydrationRequest::new(records[0].document.session_id, requests).unwrap();
    let hydrated = hydration_resolver(&root)
        .hydrate_session(&session_request)
        .unwrap()
        .into_iter()
        .map(|record| String::from_utf8(record.provider_bytes).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        hydrated,
        vec![
            long_output,
            format!("tool call: shell\ninput: {long_input}"),
            long_message,
        ]
    );
}

#[test]
fn rewrite_digest_and_truncation_fail_with_distinct_typed_evidence() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    let original = [
        message("message-0", "user", 0, "before"),
        message("message-1", "assistant", 1, "second"),
    ];
    write_chat(&session, &original);
    let candidate = source_candidates(&root).pop().unwrap();
    let (_, records, _) = collect_scan(&candidate, None);
    let first_request = hydration_request(&records[0]);
    let second_request = hydration_request(&records[1]);

    write_chat(
        &session,
        &[
            message("message-0", "user", 0, "rewrit"),
            original[1].clone(),
        ],
    );
    let stale = hydration_resolver(&root)
        .hydrate_event(&first_request)
        .unwrap_err();
    assert_eq!(stale.kind, HydrationFailureKind::StaleRecordEvidence);

    write_chat(&session, &original[..1]);
    let missing = hydration_resolver(&root)
        .hydrate_event(&second_request)
        .unwrap_err();
    assert_eq!(missing.kind, HydrationFailureKind::MissingRecord);
    let batch =
        BatchHydrationRequest::new(vec![first_request.clone(), second_request.clone()]).unwrap();
    let failed_batch = hydration_resolver(&root).hydrate_batch(&batch).unwrap_err();
    assert_eq!(failed_batch.kind, HydrationFailureKind::MissingRecord);
}

#[test]
fn source_deletion_and_unavailable_root_are_not_conflated() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    write_chat(&session, &[message("message-0", "user", 0, "present")]);
    let candidate = source_candidates(&root).pop().unwrap();
    let (_, records, _) = collect_scan(&candidate, None);
    let request = hydration_request(&records[0]);

    fs::remove_dir_all(&session).unwrap();
    let deleted = hydration_resolver(&root)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(deleted.kind, HydrationFailureKind::ConfirmedDeleted);

    fs::remove_dir_all(&root).unwrap();
    let unavailable = MuxSourceBackedResolverV0::discover_for_hydration(
        &root,
        "2026-07-28T12:30:00Z".parse().unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        unavailable.kind,
        HydrationFailureKind::TemporarilyUnavailable
    );
}

#[test]
fn digest_matching_malformed_native_record_reports_unsupported_parser_revision() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    write_chat(&session, &[message("message-0", "user", 0, "present")]);
    let candidate = source_candidates(&root).pop().unwrap();
    let (_, records, _) = collect_scan(&candidate, None);
    let original = &records[0].document;
    let decoded = decode_mux_coordinate(&original.locator).unwrap();
    let malformed_payload = b"{not-json}";
    let mut legacy_locator = vec![1_u8];
    legacy_locator.extend_from_slice(&0_u64.to_be_bytes());
    legacy_locator.extend_from_slice(
        &u64::try_from(malformed_payload.len() + 1)
            .unwrap()
            .to_be_bytes(),
    );
    let locator = SourceRecordLocator::new(
        original.source.clone(),
        NativeRecordCoordinate::ProviderNative {
            namespace: MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE.to_owned(),
            coordinate: encode_mux_coordinate(
                MuxStreamKind::Chat,
                &legacy_locator,
                decoded.source_record_ordinal,
                decoded.event_sequence,
                &decoded.native_record_id,
            )
            .unwrap(),
        },
        LocatorRevisionPolicy::StableRecordEvidence,
        original.locator.certified_source_revision_digest().copied(),
        Sha256::digest(malformed_payload).into(),
    )
    .unwrap();
    fs::write(
        session.join("chat.jsonl"),
        [malformed_payload.as_slice(), b"\n"].concat(),
    )
    .unwrap();
    let request = EventHydrationRequest::new(original.event_id, locator).unwrap();
    let failure = hydration_resolver(&root)
        .hydrate_event(&request)
        .unwrap_err();
    assert_eq!(
        failure.kind,
        HydrationFailureKind::UnsupportedParserRevision
    );
}

#[test]
fn source_backed_mux_has_no_preview_complete_or_legacy_store_publication_fallback() {
    let provider_source = [
        include_str!("../source_backed.rs"),
        include_str!("../source_backed/projection.rs"),
        include_str!("../source_backed/resolver.rs"),
    ]
    .concat();
    let native_path_source = include_str!("../../native_path.rs");
    let native_source = include_str!("../source.rs");
    let model_source = include_str!("../model.rs");
    let parse_source = include_str!("../parse.rs");
    let registry_source = include_str!("../../../../source_backed.rs");
    for source in [
        provider_source.as_str(),
        native_source,
        model_source,
        parse_source,
    ] {
        for forbidden in [
            ["ctx_history_", "store"].concat(),
            ["Store", "::"].concat(),
            ["import_mux_", "native_path("].concat(),
            ["mux_legacy_", "bridge("].concat(),
            ["provider_bytes: ", "projection.body"].concat(),
            "MAX_BODY_PREVIEW_CHARS".to_owned(),
            "provider_local_preview".to_owned(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "Mux source-backed route contains forbidden architecture token {forbidden:?}"
            );
        }
    }
    assert!(!provider_source.contains("legacy_bridge"));
    assert!(provider_source.contains("exact_mux_lexical_body"));
    assert!(provider_source.contains("fn hydrate_batch("));
    assert!(provider_source.contains("self.hydrate_requests(request.events())"));
    assert!(!native_path_source.contains("mod output;"));
    assert!(!native_path_source.contains("mod core;"));
    assert!(!native_path_source.contains("mod lifecycle;"));
    assert!(!native_path_source.contains("mod projection;"));
    assert!(!native_path_source.contains("mod publication;"));
    let legacy_store_type = ["ctx_history_", "store::Store"].concat();
    assert_eq!(
        native_path_source.matches(&legacy_store_type).count(),
        0,
        "Mux production code must not retain a Store compatibility shim"
    );
    assert!(!native_path_source.contains("legacy Store publication"));
    assert!(registry_source.contains("MuxSourceBackedResolverV0::discover_for_hydration"));
    assert!(registry_source.contains(".with_batch_hydration(move |request|"));
    let unsupported_fallback = ["Mux exact content requires", "brokered compound-file"].concat();
    assert!(!registry_source.contains(&unsupported_fallback));
}

#[test]
fn cold_append_and_rewrite_keep_stable_ids_and_exact_lexical_body() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    let long = format!("{}mux-cold-tail-term", "x".repeat(4_096));
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
    assert_eq!(cold_records[0].document.body, long);
    assert!(cold_records[0].document.body.contains("mux-cold-tail-term"));
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
    assert!(revalidate_mux_source_backed(&candidate, &cold.certificate).unwrap());

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
    assert_eq!(evidence.reason, MuxReplacementReason::ChatTruncated);
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
    let chat_body = format!("{}-chat", "c".repeat(4_096));
    let partial_body = format!("{}-partial", "p".repeat(4_096));
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
        .is_some());
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
        chat.document.source_path.as_deref(),
        session.join("chat.jsonl").to_str()
    );
    assert_eq!(
        partial.document.source_path.as_deref(),
        session.join("partial.json").to_str()
    );
    assert_eq!(hydrate_exact(&root, chat), chat_body.as_bytes());
    assert_eq!(hydrate_exact(&root, partial), partial_body.as_bytes());

    let partial_id = partial.document.event_id;
    let partial_request = hydration_request(partial);
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
    let failure = hydration_resolver(&root)
        .hydrate_event(&partial_request)
        .unwrap_err();
    assert_eq!(failure.kind, HydrationFailureKind::StaleSourceEvidence);

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
    let (_, child_records, _) = collect_scan(child, None);
    assert_eq!(
        child_records[0].document.parent_session_id,
        child.parent_session_id()
    );
    assert_eq!(
        child_records[0].document.root_session_id,
        child.root_session_id()
    );
    assert_eq!(
        child_records[0].document.provider_session_id.as_deref(),
        Some("child")
    );
    assert_eq!(child_records[0].document.branch, None);
    assert_eq!(
        child_records[0].document.source_path.as_deref(),
        child
            .source
            .chat_path
            .as_deref()
            .map(|path| path.to_str().unwrap())
    );
    assert_eq!(child_records[0].document.agent_type, "subagent");
    assert!(!child_records[0].document.is_primary);
    assert!(candidates
        .iter()
        .all(|candidate| candidate.provider_session_id() != "archive-only"));
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
        coordinate: TypedKey::Composite(value),
    } = locator.coordinate()
    else {
        panic!("Mux locator must be provider-native");
    };
    assert_eq!(namespace, MUX_PROVIDER_NATIVE_LOCATOR_NAMESPACE);
    assert_eq!(value.first(), Some(&TypedKey::U64(2)));
    assert_eq!(
        decode_mux_coordinate(locator).unwrap().stream_kind,
        MuxStreamKind::Chat
    );
    assert_eq!(records[0].document.event_type, EventType::Message.as_str());
}

#[test]
fn compound_authority_mux_rejects_missing_auxiliary_and_sibling_swap() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_chat(&session, &[message("message-0", "user", 0, "hello")]);
    let candidate = source_candidates(&root).pop().unwrap();

    write_metadata(&session, "session-1");
    assert!(matches!(
        scan_mux_source_backed(&candidate, None, |_| Ok(())),
        Err(MuxSourceBackedError::CandidateChanged)
    ));

    fs::remove_file(session.join("metadata.json")).unwrap();
    let candidate = source_candidates(&root).pop().unwrap();
    let result = scan_mux_source_backed(&candidate, None, |_| {
        fs::write(
            session.join("partial.json"),
            serde_json::to_vec(&message("partial", "assistant", 1, "replacement")).unwrap(),
        )
        .unwrap();
        Ok(())
    });
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn compound_authority_mux_rejects_ancestor_swap_and_stale_locator() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("sessions");
    let session = root.join("session-1");
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    write_chat(&session, &[message("message-0", "user", 0, "hello")]);
    let candidate = source_candidates(&root).pop().unwrap();
    let (_, records, _) = collect_scan(&candidate, None);
    let resolver = hydration_resolver(&root);
    let request = hydration_request(&records[0]);

    fs::rename(&root, temp.path().join("retired-sessions")).unwrap();
    fs::create_dir_all(&session).unwrap();
    write_metadata(&session, "session-1");
    write_chat(&session, &[message("message-0", "user", 0, "hello")]);

    assert!(resolver.hydrate_event(&request).is_err());
}
