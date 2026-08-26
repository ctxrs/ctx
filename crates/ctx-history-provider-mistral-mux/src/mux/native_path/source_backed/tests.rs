use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
};

use ctx_history_capture_runtime::BaseEventLookup;
use ctx_history_core::{CoreRecord, TypedKey};
use ctx_history_jsonl::JsonlFamilyProjectionMode;
use serde_json::{json, Value};

use super::{
    bind_source, exact_dependency, optional_bound_stream, projection::MuxProjector, MuxBinding,
    MuxStreamKind, ProviderSourceRoot, SourceAnchorScope, SourceKey, MAX_EVENT_SEQUENCE_ORDINAL,
    PARTIAL_EVENT_SEQUENCE_BASE,
};
use crate::mux::source::{
    mux_session_source_from_dir, visit_mux_session_sources, visit_mux_session_sources_with_limits,
};

#[derive(Clone)]
struct EmptyLookup;

#[test]
fn source_and_related_session_identities_are_root_scoped() {
    let released = super::source_key("same-session").unwrap();
    let compatibility =
        super::source_key_scoped("same-session", SourceAnchorScope::Unqualified).unwrap();
    let first =
        super::source_key_scoped("same-session", SourceAnchorScope::Lineage([1; 32])).unwrap();
    let second =
        super::source_key_scoped("same-session", SourceAnchorScope::Lineage([2; 32])).unwrap();

    assert!(released.exact_descriptor_eq(&compatibility));
    assert_ne!(first.identity(), second.identity());
    assert_ne!(
        super::related_session_identity("parent", SourceAnchorScope::Lineage([1; 32])).unwrap(),
        super::related_session_identity("parent", SourceAnchorScope::Lineage([2; 32])).unwrap()
    );
}

impl BaseEventLookup for EmptyLookup {
    type Error = std::convert::Infallible;

    fn contains(&self, _event_id: uuid::Uuid) -> std::result::Result<bool, Self::Error> {
        Ok(false)
    }
}

struct ProjectedSession {
    source: SourceKey,
    binding: MuxBinding,
    records: Vec<CoreRecord>,
}

fn write_jsonl(path: &std::path::Path, values: &[Value]) {
    let mut bytes = values
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes();
    bytes.push(b'\n');
    fs::write(path, bytes).unwrap();
}

fn message(owner: &str, id: Option<&str>, history_sequence: Option<Value>, text: &str) -> Value {
    let mut value = json!({
        "workspaceId": owner,
        "role": "user",
        "parts": [{"type": "text", "text": text}],
        "metadata": {}
    });
    if let Some(id) = id {
        value["id"] = Value::String(id.to_owned());
    }
    if let Some(history_sequence) = history_sequence {
        value["metadata"]["historySequence"] = history_sequence;
    }
    value
}

fn bind(session_dir: &std::path::Path) -> (Arc<ProviderSourceRoot>, SourceKey, MuxBinding) {
    let native = mux_session_source_from_dir(session_dir)
        .unwrap()
        .expect("Mux test session must be discoverable");
    let authority = Arc::new(ProviderSourceRoot::open(session_dir).unwrap());
    let (source, binding) =
        bind_source(&authority, &native, SourceAnchorScope::Unqualified).unwrap();
    (authority, source, binding)
}

fn project(session_dir: &std::path::Path) -> ProjectedSession {
    let (authority, source, binding) = bind(session_dir);
    let mut projector = MuxProjector::<EmptyLookup>::new(
        source.clone(),
        authority,
        binding.clone(),
        JsonlFamilyProjectionMode::Cold,
        None,
    )
    .unwrap();
    let mut records = Vec::new();
    for stream in [
        MuxStreamKind::Archive,
        MuxStreamKind::Chat,
        MuxStreamKind::Partial,
    ] {
        if optional_bound_stream(&binding, stream).is_some() {
            projector
                .project_bound_stream(stream, &mut |record| {
                    records.push(record);
                    Ok(())
                })
                .unwrap();
        }
    }
    projector.finish().unwrap();
    ProjectedSession {
        source,
        binding,
        records,
    }
}

#[test]
fn cold_mixed_session_projects_archive_then_chat_then_partial() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("mixed-session");
    fs::create_dir(&session).unwrap();
    write_jsonl(
        &session.join("chat-archive.jsonl"),
        &[message(
            "mixed-session",
            Some("archive-id"),
            Some(json!(40)),
            "oldest archived message",
        )],
    );
    write_jsonl(
        &session.join("chat.jsonl"),
        &[message(
            "mixed-session",
            Some("shared-id"),
            Some(json!(41)),
            "active chat message",
        )],
    );
    fs::write(
        session.join("partial.json"),
        message(
            "mixed-session",
            Some("shared-id"),
            Some(json!(41)),
            "separate staged partial",
        )
        .to_string(),
    )
    .unwrap();

    let mut discovered = Vec::new();
    assert_eq!(
        visit_mux_session_sources(&session, &mut |source| {
            discovered.push(source);
            Ok(())
        })
        .unwrap(),
        1
    );
    assert_eq!(discovered.len(), 1);

    let projected = project(&session);
    assert_eq!(projected.binding.primary_stream, MuxStreamKind::Archive);
    assert!(projected.binding.archive.is_some());
    assert_eq!(
        projected
            .records
            .iter()
            .map(|record| record.content.meaningful_text())
            .collect::<Vec<_>>(),
        [
            "oldest archived message",
            "active chat message",
            "separate staged partial"
        ]
    );
    assert_eq!(projected.records[0].event_sequence, 40);
    assert_eq!(projected.records[1].event_sequence, 41);
    assert!(projected.records[2].event_sequence >= PARTIAL_EVENT_SEQUENCE_BASE);
    assert_eq!(
        projected.records[1].native_event_id,
        Some(TypedKey::Utf8("shared-id".to_owned()))
    );
    assert_eq!(
        projected.records[2].native_event_id,
        Some(TypedKey::Utf8("partial:shared-id".to_owned()))
    );
    assert_ne!(projected.records[1].event_id, projected.records[2].event_id);
}

#[test]
fn archive_only_session_is_recognized_once_from_directory_or_file() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("archive-only");
    fs::create_dir(&session).unwrap();
    let archive = session.join("chat-archive.jsonl");
    write_jsonl(
        &archive,
        &[message(
            "archive-only",
            Some("archive-only-id"),
            Some(json!(0)),
            "archive-only history",
        )],
    );

    for root in [&session, &archive] {
        let mut sources = Vec::new();
        assert_eq!(
            visit_mux_session_sources(root, &mut |source| {
                sources.push(source);
                Ok(())
            })
            .unwrap(),
            1
        );
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].archive_path.as_deref(), Some(archive.as_path()));
        assert!(sources[0].chat_path.is_none());
    }

    let projected = project(&session);
    assert_eq!(projected.records.len(), 1);
    assert_eq!(
        projected.records[0].content.meaningful_text(),
        "archive-only history"
    );
}

#[test]
fn rotation_preserves_source_session_and_event_identities_without_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("rotating-session");
    fs::create_dir(&session).unwrap();
    let records = [
        message(
            "rotating-session",
            Some("provider-id"),
            Some(json!(0)),
            "provider id message",
        ),
        message(
            "rotating-session",
            None,
            Some(json!(1)),
            "history sequence message",
        ),
        message("rotating-session", None, None, "bounded fallback message"),
    ];
    write_jsonl(&session.join("chat.jsonl"), &records);
    let before = project(&session);

    write_jsonl(&session.join("chat-archive.jsonl"), &records[..2]);
    write_jsonl(&session.join("chat.jsonl"), &records[2..]);
    let after = project(&session);

    assert!(before.source.exact_descriptor_eq(&after.source));
    assert_eq!(before.binding.session_id, after.binding.session_id);
    assert_ne!(
        before.binding.source_revision_digest,
        after.binding.source_revision_digest
    );
    assert_eq!(before.records.len(), 3);
    assert_eq!(before.records, after.records);
    assert_eq!(
        after
            .records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert!(after
        .records
        .iter()
        .all(|record| record.event_sequence <= MAX_EVENT_SEQUENCE_ORDINAL));
    assert_eq!(
        after
            .records
            .iter()
            .map(|record| record.event_id)
            .collect::<HashSet<_>>()
            .len(),
        after.records.len()
    );
}

#[test]
fn crash_window_overlap_is_removed_only_from_the_active_seam() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("crash-overlap-session");
    fs::create_dir(&session).unwrap();
    let records = [
        message(
            "crash-overlap-session",
            Some("archived-provider-id"),
            Some(json!(0)),
            "first sealed message",
        ),
        message(
            "crash-overlap-session",
            None,
            Some(json!(1)),
            "second sealed message",
        ),
        message(
            "crash-overlap-session",
            Some("active-provider-id"),
            Some(json!(2)),
            "first active message",
        ),
        message(
            "crash-overlap-session",
            Some("same-text-distinct-id"),
            Some(json!(3)),
            "second sealed message",
        ),
    ];
    let chat = session.join("chat.jsonl");
    let archive = session.join("chat-archive.jsonl");
    write_jsonl(&chat, &records);
    let before = project(&session);

    // Mux appends and fsyncs the sealed prefix before rewriting chat.jsonl.
    // A crash in that window leaves the same valid sequences at both sides of
    // the archive/chat seam.
    write_jsonl(&archive, &records[..2]);
    write_jsonl(&chat, &records);
    let crash_window = project(&session);

    assert!(before.source.exact_descriptor_eq(&crash_window.source));
    assert_eq!(before.binding.session_id, crash_window.binding.session_id);
    assert_eq!(before.records, crash_window.records);
    assert_eq!(crash_window.records.len(), records.len());
    assert_eq!(
        crash_window
            .records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        crash_window.records[3].content.meaningful_text(),
        "second sealed message"
    );
    assert_ne!(
        crash_window.records[1].event_id,
        crash_window.records[3].event_id
    );

    write_jsonl(&chat, &records[2..]);
    let healed = project(&session);
    assert_eq!(before.records, healed.records);
}

#[test]
fn crash_seam_consumes_one_archived_occurrence_per_equivalent_chat_occurrence() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("multiplicity-overlap-session");
    fs::create_dir(&session).unwrap();
    let repeated_a = message(
        "multiplicity-overlap-session",
        Some("repeated-provider-id"),
        Some(json!(0)),
        "legitimate repeated A",
    );
    let b = message(
        "multiplicity-overlap-session",
        Some("provider-id-b"),
        Some(json!(1)),
        "B",
    );
    let archive = session.join("chat-archive.jsonl");
    let chat = session.join("chat.jsonl");

    write_jsonl(&chat, &[repeated_a.clone(), repeated_a.clone(), b.clone()]);
    let before = project(&session);

    // A crash after appending one A to the archive but before rewriting chat
    // leaves [A] ++ [A, A, B]. Only the first active A is replay evidence;
    // the second is a legitimate provider repetition and must remain visible.
    write_jsonl(&archive, std::slice::from_ref(&repeated_a));
    write_jsonl(&chat, &[repeated_a.clone(), repeated_a.clone(), b.clone()]);
    let crash_window = project(&session);

    write_jsonl(&archive, &[repeated_a.clone(), repeated_a]);
    write_jsonl(&chat, std::slice::from_ref(&b));
    let healed = project(&session);

    assert_eq!(before.records.len(), 3);
    assert_eq!(crash_window.records.len(), 3);
    assert_eq!(healed.records.len(), 3);
    assert_eq!(before.records, crash_window.records);
    assert_eq!(before.records, healed.records);
    assert_eq!(before.records[1].event_id, crash_window.records[1].event_id);
    assert_eq!(before.records[1].event_id, healed.records[1].event_id);
    assert_eq!(
        crash_window
            .records
            .iter()
            .map(|record| record.content.meaningful_text())
            .collect::<Vec<_>>(),
        ["legitimate repeated A", "legitimate repeated A", "B"]
    );
    assert_ne!(
        crash_window.records[0].event_id,
        crash_window.records[1].event_id
    );
    assert_eq!(
        crash_window
            .records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

#[test]
fn crash_overlap_with_provider_ids_and_malformed_sequences_has_one_identity_per_row() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("malformed-overlap-session");
    fs::create_dir(&session).unwrap();
    let records = [
        message(
            "malformed-overlap-session",
            Some("missing-sequence-id"),
            None,
            "missing sequence replay",
        ),
        message(
            "malformed-overlap-session",
            Some("malformed-sequence-id"),
            Some(json!("not-a-sequence")),
            "malformed sequence replay",
        ),
        message(
            "malformed-overlap-session",
            Some("active-id"),
            Some(json!(2)),
            "active row",
        ),
    ];
    let archive = session.join("chat-archive.jsonl");
    let chat = session.join("chat.jsonl");
    write_jsonl(&chat, &records);
    let before = project(&session);

    write_jsonl(&archive, &records[..2]);
    write_jsonl(&chat, &records);
    let crash_window = project(&session);

    assert_eq!(before.records, crash_window.records);
    assert_eq!(crash_window.records.len(), records.len());
    assert_eq!(
        crash_window
            .records
            .iter()
            .map(|record| record.event_id)
            .collect::<HashSet<_>>()
            .len(),
        records.len()
    );
}

#[test]
fn covered_sequence_with_distinct_content_is_not_suppressed_or_identity_collapsed() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("covered-distinct-session");
    fs::create_dir(&session).unwrap();
    let archived = message(
        "covered-distinct-session",
        None,
        Some(json!(7)),
        "archived sequence seven",
    );
    let distinct = message(
        "covered-distinct-session",
        None,
        Some(json!(7)),
        "distinct active sequence seven",
    );
    write_jsonl(&session.join("chat-archive.jsonl"), &[archived]);
    write_jsonl(&session.join("chat.jsonl"), &[distinct]);

    let projected = project(&session);
    assert_eq!(projected.records.len(), 2);
    assert_eq!(
        projected
            .records
            .iter()
            .map(|record| record.content.meaningful_text())
            .collect::<Vec<_>>(),
        ["archived sequence seven", "distinct active sequence seven"]
    );
    assert_ne!(projected.records[0].event_id, projected.records[1].event_id);
    assert_eq!(
        projected
            .records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        [7, 8]
    );
}

#[test]
fn malformed_and_missing_history_sequences_use_monotonic_collision_free_fallbacks() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("malformed-sequence-session");
    fs::create_dir(&session).unwrap();
    let records = [
        message(
            "malformed-sequence-session",
            None,
            Some(json!(5)),
            "valid five",
        ),
        message("malformed-sequence-session", None, None, "missing sequence"),
        message(
            "malformed-sequence-session",
            None,
            Some(json!(7.0)),
            "integral JSON number is valid",
        ),
        message(
            "malformed-sequence-session",
            None,
            Some(json!("6")),
            "numeric string is malformed",
        ),
        message(
            "malformed-sequence-session",
            None,
            Some(json!(6.5)),
            "fractional sequence is malformed",
        ),
        message(
            "malformed-sequence-session",
            None,
            Some(json!(-1)),
            "negative sequence is malformed",
        ),
        message(
            "malformed-sequence-session",
            None,
            Some(json!(6)),
            "valid six after fallback slots",
        ),
        message(
            "malformed-sequence-session",
            None,
            Some(json!(100)),
            "valid hundred",
        ),
        message(
            "malformed-sequence-session",
            None,
            None,
            "missing after hundred",
        ),
        message(
            "malformed-sequence-session",
            None,
            Some(json!(101)),
            "valid one hundred one after fallback",
        ),
    ];
    let chat = session.join("chat.jsonl");
    write_jsonl(&chat, &records);
    let before = project(&session);

    assert_eq!(before.records.len(), records.len());
    assert_eq!(
        before
            .records
            .iter()
            .map(|record| record.event_sequence)
            .collect::<Vec<_>>(),
        [5, 6, 7, 8, 9, 10, 11, 100, 101, 102]
    );
    assert!(before
        .records
        .windows(2)
        .all(|window| window[0].event_sequence < window[1].event_sequence));
    assert_eq!(
        before.records[0].native_event_id,
        Some(TypedKey::Utf8("historySequence:5".to_owned()))
    );
    assert_ne!(
        before.records[3].native_event_id,
        Some(TypedKey::Utf8("historySequence:6".to_owned()))
    );
    assert_eq!(
        before.records[2].native_event_id,
        Some(TypedKey::Utf8("historySequence:7".to_owned()))
    );
    assert_eq!(
        before.records[6].native_event_id,
        Some(TypedKey::Utf8("historySequence:6".to_owned()))
    );

    write_jsonl(&session.join("chat-archive.jsonl"), &records[..2]);
    write_jsonl(&chat, &records[2..]);
    let rotated = project(&session);
    assert_eq!(before.records, rotated.records);
}

#[test]
fn archive_append_rewrite_and_delete_change_compound_source_revision() {
    let temp = tempfile::tempdir().unwrap();
    let session = temp.path().join("revision-session");
    fs::create_dir(&session).unwrap();
    let archive = session.join("chat-archive.jsonl");
    let chat = session.join("chat.jsonl");
    let partial = session.join("partial.json");
    write_jsonl(
        &archive,
        &[message(
            "revision-session",
            Some("archive-0"),
            Some(json!(0)),
            "archive zero",
        )],
    );
    write_jsonl(
        &chat,
        &[message(
            "revision-session",
            Some("chat-1"),
            Some(json!(2)),
            "chat one",
        )],
    );
    fs::write(
        &partial,
        message(
            "revision-session",
            Some("partial-2"),
            Some(json!(3)),
            "partial two",
        )
        .to_string(),
    )
    .unwrap();

    let (authority, initial_source, initial) = bind(&session);
    let chat_dependency = exact_dependency(&authority, initial.chat.as_ref().unwrap()).unwrap();
    assert!(initial.archive.as_ref().is_some_and(|bound| {
        bound.relative_path == std::path::Path::new("chat-archive.jsonl")
    }));

    let appended = message(
        "revision-session",
        Some("archive-appended"),
        Some(json!(1)),
        "archive appended",
    );
    let mut file = OpenOptions::new().append(true).open(&archive).unwrap();
    writeln!(file, "{appended}").unwrap();
    drop(file);
    let (_, appended_source, appended_binding) = bind(&session);
    assert!(initial_source.exact_descriptor_eq(&appended_source));
    assert_ne!(
        initial.source_revision_digest,
        appended_binding.source_revision_digest
    );
    assert_eq!(project(&session).records.len(), 4);

    write_jsonl(
        &archive,
        &[message(
            "revision-session",
            Some("archive-rewritten"),
            Some(json!(0)),
            "rewritten archive with a different byte length",
        )],
    );
    let (_, rewritten_source, rewritten_binding) = bind(&session);
    assert!(initial_source.exact_descriptor_eq(&rewritten_source));
    assert_ne!(
        appended_binding.source_revision_digest,
        rewritten_binding.source_revision_digest
    );

    let mut chat_file = OpenOptions::new().append(true).open(&chat).unwrap();
    writeln!(
        chat_file,
        "{}",
        message(
            "revision-session",
            Some("chat-appended"),
            Some(json!(2)),
            "chat dependency changed"
        )
    )
    .unwrap();
    drop(chat_file);
    assert!(chat_dependency.revalidate_dependency().is_err());

    fs::remove_file(&archive).unwrap();
    let (_, deleted_source, deleted_binding) = bind(&session);
    assert!(initial_source.exact_descriptor_eq(&deleted_source));
    assert_eq!(deleted_binding.primary_stream, MuxStreamKind::Chat);
    assert_ne!(
        rewritten_binding.source_revision_digest,
        deleted_binding.source_revision_digest
    );

    fs::remove_file(&chat).unwrap();
    fs::remove_file(&partial).unwrap();
    assert!(mux_session_source_from_dir(&session).unwrap().is_none());
}

#[test]
fn aggregate_traversal_limit_fails_before_any_source_is_accumulated() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("over-limit");
    fs::create_dir(&root).unwrap();
    for name in ["a", "b", "c"] {
        fs::create_dir(root.join(name)).unwrap();
    }

    let mut sources = Vec::new();
    let error = visit_mux_session_sources_with_limits(&root, 2, 2, &mut |source| {
        sources.push(source);
        Ok(())
    })
    .unwrap_err();

    assert!(sources.is_empty());
    assert!(error
        .to_string()
        .contains("Mux session traversal exceeds the supported directory entry limit"));
}

#[test]
fn aggregate_source_limit_fails_without_exposing_a_partial_inventory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("source-over-limit");
    for name in ["a", "b"] {
        let session = root.join(name);
        fs::create_dir_all(&session).unwrap();
        write_jsonl(
            &session.join("chat.jsonl"),
            &[message(name, Some(name), Some(json!(0)), name)],
        );
    }

    let mut sources = Vec::new();
    let error = visit_mux_session_sources_with_limits(&root, 8, 1, &mut |source| {
        sources.push(source);
        Ok(())
    })
    .unwrap_err();

    assert!(sources.is_empty());
    assert!(error
        .to_string()
        .contains("Mux session traversal exceeds the supported source limit"));
}
