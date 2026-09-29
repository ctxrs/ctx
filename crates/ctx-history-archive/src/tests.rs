use std::{fs, io::Write, path::Path};

use ctx_history_core::*;
use ctx_history_index::{
    CompiledSearchFilter, EventSearchFilters, GenerationWriter, LexicalExecution, LexicalMode,
    WriterOptions,
};
use tempfile::tempdir;

use super::*;

mod reexport;
mod restore_regressions;

fn identity() -> ArchiveIdentity {
    ArchiveIdentity {
        origin: "synthetic-machine-a".into(),
        view: "retained-work".into(),
    }
}

fn options() -> RestoreOptions {
    let mut options = RestoreOptions::new(ImportBinding {
        namespace: "offline-restore".into(),
        identity: identity(),
    });
    options.writer = writer_options();
    options
}

fn writer_options() -> WriterOptions {
    WriterOptions {
        indexer_threads: 1,
        memory_bytes: 64 * 1024 * 1024,
    }
}

fn search_count(index: &VerifiedIndex, text: &str) -> usize {
    assert!(
        index.document_count() <= 16,
        "small search fixture required"
    );
    let filter = CompiledSearchFilter::compile(EventSearchFilters::default()).unwrap();
    let batch = index
        .execute_lexical(LexicalExecution::new(
            LexicalMode::Search(&[text]),
            &filter,
            16,
        ))
        .unwrap()
        .batch;
    assert!(
        batch.complete,
        "search exhausted its work budget: {:?}",
        batch.exhaustion
    );
    batch.candidates.len()
}

fn source() -> SourceKey {
    SourceKey::derive_provider_native(
        "codex",
        "codex_session_jsonl",
        "session",
        1,
        "synthetic-provider",
        TypedKey::utf8("來源 / native-session").unwrap(),
    )
    .unwrap()
}

fn record(source: &SourceKey, session: &str, sequence: u64, body: &str) -> CoreRecord {
    let key = NativeSessionKey::native_id("session", TypedKey::utf8(session).unwrap()).unwrap();
    let session_id = derive_session_id(SessionIdentityInput {
        source,
        logical_session_kind: "thread",
        native_session_key: &key,
    })
    .unwrap();
    let key =
        NativeItemKey::native_id("event", TypedKey::utf8(sequence.to_string()).unwrap()).unwrap();
    let event_id = derive_event_id(EventIdentityInput {
        source,
        session_id,
        logical_item_kind: "message",
        native_item_key: &key,
        subrecord_selector: None,
    })
    .unwrap();
    CoreRecord::new_selected(
        event_id,
        session_id,
        source.clone(),
        sequence,
        "message",
        "synthetic-parser-v1",
        body,
    )
    .unwrap()
}

fn publish(root: &Path, records: &[CoreRecord], revision: u8) {
    let mut writer = GenerationWriter::open(root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    let source = records[0].source.clone();
    writer.begin_source(source.clone()).unwrap();
    for record in records {
        writer.add_core_record(record.clone()).unwrap();
    }
    let observation = SourceObservation::new(source, "synthetic-v1", vec![revision]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "synthetic-parser-v1",
                [revision; 32],
                ScannedSourceCounts {
                    complete_records: records.len() as u64,
                    retained_records: records.len() as u64,
                    indexed_documents: records.len() as u64,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
}

fn members(path: &Path) -> Vec<SessionMember> {
    let mut members = Vec::new();
    visit_members(path, |member| {
        members.push(member);
        Ok(())
    })
    .unwrap();
    members
}

fn archive_records(path: &Path) -> Vec<CoreRecord> {
    let mut records = Vec::new();
    visit_members(path, |member| {
        visit_records(path, &member, |record| {
            records.push(record);
            Ok(())
        })
    })
    .unwrap();
    records.sort_by_key(|r| r.event_id.digest());
    records
}

fn archive(root: &Path, records: &[CoreRecord]) -> std::path::PathBuf {
    let index_root = root.join("producer-index");
    publish(&index_root, records, 1);
    let archive = root.join("archive");
    export(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        &archive,
        identity(),
        &Selection::default(),
    )
    .unwrap();
    archive
}

#[test]
fn faithful_owned_roundtrip_after_move_and_native_source_loss() {
    let temp = tempdir().unwrap();
    let native = temp.path().join("native α.jsonl");
    fs::write(&native, "synthetic native input").unwrap();
    let source = source();
    let mut selected = record(&source, "主 session", 1, "retainedneedle 雪\nline two 🦀");
    selected.role = Some("assistant".into());
    selected.agent_scope = Some(AgentScope::Primary);
    selected.provider_session_id = Some("主 session".into());
    selected.native_event_id = Some(TypedKey::bytes(vec![0, 128, 255]).unwrap());
    selected.content.structured_content =
        Some(serde_json::json!({"nested": [null, "雪", {"value": 42}]}));
    selected.content.activity = Some(CoreActivity {
        revision: CORE_ACTIVITY_REVISION,
        provider_call_id: Some(TypedKey::utf8("call-1").unwrap()),
        invocation: Some(ActivityInvocation {
            protocol: Some("synthetic".into()),
            server: None,
            tool: "read".into(),
            arguments: ActivityJsonCapture::Present {
                value: serde_json::json!({"path": "../inert/雪"}),
            },
            started_at_unix_ms: None,
        }),
        result: Some(ActivityResult {
            status: Some("completed".into()),
            completed_at_unix_ms: Some(-1000),
            duration_ns: Some(123),
            text: ActivityTextCapture::NormalizedBody,
            structured_content: ActivityJsonCapture::Unavailable,
        }),
        facts: vec![ProviderDeclaredFact {
            kind: LiteralFactKind::SessionCwd,
            value: "/inert/路径".into(),
        }],
    });
    let mut structured = record(&source, "second", 2, "unused");
    structured.content.normalized_body = None;
    structured.content.structured_content = Some(serde_json::json!(["structured only", false]));
    structured.occurred_at_unix_ms = Some(0);
    structured.parent_session_id = Some(selected.session_id);
    structured.root_session_id = Some(selected.session_id);
    structured.session_relationship = Some(ProviderNativeSessionRelationship::Delegated);
    let mut excluded = record(&source, "second", 3, "inert retrieval");
    excluded.content.discovery_exclusion = Some(CoreDiscoveryExclusion::CtxRetrievalDerived);
    excluded.parent_session_id = structured.parent_session_id;
    excluded.root_session_id = structured.root_session_id;
    excluded.session_relationship = structured.session_relationship;
    excluded.event_copy = Some(ProviderNativeEventCopy {
        ancestor_session_id: selected.session_id,
        ancestor_event_id: selected.event_id,
        proof: ProviderNativeCopyProof::NativeCopiedFromField,
    });
    let mut omitted = record(&source, "omitted", 4, "unused");
    omitted.content.normalized_body = None;
    omitted.content.policy_status = CoreContentPolicyStatus::Omitted {
        reason: "retention choice".into(),
    };
    let mut redacted = record(&source, "redacted", 5, "unused");
    redacted.content.normalized_body = Some("[redacted]".into());
    redacted.content.policy_status = CoreContentPolicyStatus::Redacted {
        reason: "synthetic redaction".into(),
    };
    let expected = vec![selected.clone(), structured, excluded, omitted, redacted];
    let archive = archive(temp.path(), &expected);
    fs::remove_file(native).unwrap();
    fs::remove_dir_all(temp.path().join("producer-index")).unwrap();
    let moved = temp.path().join("moved 空间");
    fs::rename(&archive, &moved).unwrap();
    let mut sorted = expected.clone();
    sorted.sort_by_key(|r| r.event_id.digest());
    assert_eq!(archive_records(&moved), sorted);
    let owned = temp.path().join("restored");
    let first = restore(&moved, &owned, &options()).unwrap();
    let second = restore(&moved, &owned, &options()).unwrap();
    assert_eq!(first.generation_id, second.generation_id);
    assert_eq!(second.imported_members, 0);
    assert_eq!(second.unchanged_members, 4);
    fs::remove_dir_all(&moved).unwrap();
    let retained = owned.join("archives").join(first.snapshot_id);
    assert_eq!(archive_records(&retained), sorted);
    let index = VerifiedIndex::open_pinned(owned.join("search/lexical")).unwrap();
    assert_eq!(index.document_count(), 5);
    assert_eq!(search_count(&index, "retainedneedle"), 1);
    for original in expected {
        let id = mapped_event(&options().binding, original.session_id, original.event_id).unwrap();
        let recovered = index.core_record_by_id(id.as_uuid()).unwrap().unwrap();
        assert_eq!(recovered.content, original.content);
        assert_eq!(recovered.occurred_at_unix_ms, original.occurred_at_unix_ms);
        assert_eq!(recovered.native_event_id, original.native_event_id);
        assert_eq!(recovered.provider_session_id, original.provider_session_id);
        assert_eq!(recovered.parser_revision, original.parser_revision);
        assert_eq!(recovered.event_sequence, original.event_sequence);
        assert_eq!(recovered.role, original.role);
        assert_eq!(recovered.agent_scope, original.agent_scope);
        assert!(recovered.parent_session_id.is_none());
        assert!(recovered.event_copy.is_none());
    }
}

#[test]
fn source_session_selection_corrections_and_omission_do_not_delete() {
    let temp = tempdir().unwrap();
    let source = source();
    let old = record(&source, "first", 1, "before correction");
    let other = record(&source, "second", 2, "keep unrelated");
    let initial = archive(temp.path(), &[old.clone(), other]);
    let owned = temp.path().join("restore");
    restore(&initial, &owned, &options()).unwrap();
    let old_member = members(&initial)
        .into_iter()
        .find(|m| m.session_id == old.session_id)
        .unwrap();
    let mut changed = old.clone();
    changed.content.normalized_body = Some("after correction".into());
    publish(&temp.path().join("producer-index"), &[changed.clone()], 2);
    let mut selection = Selection::default();
    selection.sessions.insert(hex(&old.session_id.digest()));
    selection.sources.insert(hex(&source.identity().digest()));
    let update = temp.path().join("update");
    export(
        &VerifiedIndex::open_pinned(temp.path().join("producer-index")).unwrap(),
        &update,
        identity(),
        &selection,
    )
    .unwrap();
    assert!(matches!(
        restore(&update, &owned, &options()),
        Err(ArchiveError::Conflict { .. })
    ));
    let index = VerifiedIndex::open_pinned(owned.join("search/lexical")).unwrap();
    assert_eq!(search_count(&index, "before"), 1);
    let mut authorized = options();
    authorized
        .expected_predecessors
        .insert(old_member.path, old_member.sha256);
    restore(&update, &owned, &authorized).unwrap();
    let index = VerifiedIndex::open_pinned(owned.join("search/lexical")).unwrap();
    assert_eq!(index.document_count(), 2);
    assert_eq!(search_count(&index, "after"), 1);
    assert_eq!(search_count(&index, "unrelated"), 1);
    assert!(matches!(
        restore(&initial, &owned, &options()),
        Err(ArchiveError::Conflict { .. })
    ));
}

#[test]
fn origins_views_and_server_namespaces_cannot_collide_or_spoof_foreign_references() {
    let temp = tempdir().unwrap();
    let source = source();
    let original = record(&source, "shared native ID", 1, "same text");
    let archive = archive(temp.path(), std::slice::from_ref(&original));
    let member = members(&archive).remove(0);
    let base = options().binding;
    let mut publisher = base.clone();
    publisher.namespace = "server-owned-other-publisher".into();
    let mut origin = base.clone();
    origin.identity.origin = "other origin".into();
    let mut view = base.clone();
    view.identity.view = "narrower view".into();
    let original_mapped = map_record(&base, &member, original.clone()).unwrap();
    for binding in [&publisher, &origin, &view] {
        let mut foreign = original.clone();
        foreign.parent_session_id = Some(original_mapped.session_id);
        foreign.root_session_id = Some(original_mapped.session_id);
        foreign.event_copy = Some(ProviderNativeEventCopy {
            ancestor_session_id: original_mapped.session_id,
            ancestor_event_id: original_mapped.event_id,
            proof: ProviderNativeCopyProof::NativeEventIdentity,
        });
        let mapped = map_record(binding, &member, foreign).unwrap();
        assert_ne!(mapped.source, original_mapped.source);
        assert_ne!(mapped.session_id, original_mapped.session_id);
        assert_ne!(mapped.event_id, original_mapped.event_id);
        assert!(mapped.parent_session_id.is_none());
        assert!(mapped.root_session_id.is_none());
        assert!(mapped.event_copy.is_none());
    }
    assert!(verify(&archive)
        .unwrap()
        .require_identity(&origin.identity)
        .is_err());
    assert!(map_record(&base, &member, record(&source, "other session", 1, "x")).is_err());
}

#[test]
fn restore_uses_normal_data_root_layout_and_refuses_native_roots() {
    let temp = tempdir().unwrap();
    let archive = archive(
        temp.path(),
        &[record(&source(), "session", 1, "archiveonly")],
    );
    let empty = temp.path().join("existing-empty");
    fs::create_dir(&empty).unwrap();
    restore(&archive, &empty, &options()).unwrap();
    assert_eq!(
        search_count(
            &VerifiedIndex::open_pinned(empty.join("search/lexical")).unwrap(),
            "archiveonly",
        ),
        1
    );
    assert!(!empty.join("index").exists());

    let native = temp.path().join("native-data-root");
    publish(
        &native.join("search/lexical"),
        &[record(&source(), "native", 1, "nativeonly")],
        2,
    );
    let before = VerifiedIndex::open_pinned(native.join("search/lexical"))
        .unwrap()
        .generation_id()
        .to_owned();
    assert!(restore(&archive, &native, &options()).is_err());
    assert!(!native.join("archives").exists());
    assert!(!native.join("restore.lock").exists());
    assert_eq!(
        VerifiedIndex::open_pinned(native.join("search/lexical"))
            .unwrap()
            .generation_id(),
        before
    );
}

#[test]
fn archive_versions_inventory_checksums_and_empty_selections_are_explicit() {
    let temp = tempdir().unwrap();
    let archive = archive(temp.path(), &[record(&source(), "session", 1, "retained")]);
    let manifest = verify(&archive).unwrap();
    let mut future = manifest.clone();
    future.version += 1;
    fs::write(
        archive.join("manifest.json"),
        serde_json::to_vec(&future).unwrap(),
    )
    .unwrap();
    assert!(verify(&archive).is_err());
    fs::write(
        archive.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(archive.join("inventory.jsonl"))
        .unwrap()
        .write_all(b"\n")
        .unwrap();
    assert!(verify(&archive).is_err());

    let empty = temp.path().join("empty");
    let mut selection = Selection::default();
    selection.sources.insert("0".repeat(64));
    let manifest = export(
        &VerifiedIndex::open_pinned(temp.path().join("producer-index")).unwrap(),
        &empty,
        identity(),
        &selection,
    )
    .unwrap();
    assert_eq!(manifest.records, 0);
    assert_eq!(manifest.members, 0);
    assert!(manifest.selected_subset);
    assert_eq!(verify(&empty).unwrap(), manifest);
    let owned = temp.path().join("empty-root");
    restore(&empty, &owned, &options()).unwrap();
    assert_eq!(
        VerifiedIndex::open_pinned(owned.join("search/lexical"))
            .unwrap()
            .document_count(),
        0
    );
}

#[test]
fn highly_escaped_supported_source_keys_fit_streamed_metadata() {
    let temp = tempdir().unwrap();
    let source = SourceKey::derive_provider_native(
        "synthetic",
        "jsonl",
        "session",
        1,
        "native",
        TypedKey::utf8("\0".repeat(60_000)).unwrap(),
    )
    .unwrap();
    let expected = record(&source, "session", 1, "ordinary supported body");
    let archive = archive(temp.path(), std::slice::from_ref(&expected));
    assert_eq!(archive_records(&archive), vec![expected]);
    assert_eq!(verify(&archive).unwrap().records, 1);
}

#[test]
fn pinned_export_and_independent_session_capture_have_stable_bytes() {
    let temp = tempdir().unwrap();
    let source = source();
    let records: Vec<_> = (0..130)
        .map(|i| record(&source, "large session", i, "before"))
        .collect();
    let index_root = temp.path().join("index");
    publish(&index_root, &records, 1);
    let pinned = VerifiedIndex::open_pinned(&index_root).unwrap();
    publish(
        &index_root,
        &[record(&source, "large session", 0, "after")],
        2,
    );
    let full = temp.path().join("full");
    let manifest = export(&pinned, &full, identity(), &Selection::default()).unwrap();
    assert_eq!(manifest.records, 130);
    let standalone = temp.path().join("upload with spaces");
    let member = export_session(&pinned, &source, records[0].session_id, &standalone, || {
        Ok(())
    })
    .unwrap();
    let full_member = members(&full).remove(0);
    assert_eq!(member.sha256, full_member.sha256);
    assert_eq!(
        fs::read(&standalone).unwrap(),
        fs::read(full.join(&member.path)).unwrap()
    );
    verify_member(&standalone, &member).unwrap();
    let mut seen = 0;
    visit_member_records(&standalone, &member, |r| {
        assert_eq!(r.content.normalized_body.as_deref(), Some("before"));
        seen += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, 130);
}

#[test]
fn cancelled_export_never_publishes_and_does_not_overwrite_existing_snapshot() {
    let temp = tempdir().unwrap();
    let source = source();
    let records: Vec<_> = (0..70)
        .map(|i| record(&source, "session", i, "body"))
        .collect();
    let index_root = temp.path().join("index");
    publish(&index_root, &records, 1);
    let index = VerifiedIndex::open_pinned(&index_root).unwrap();
    let destination = temp.path().join("cancelled");
    let mut calls = 0;
    let result = export_with_control(
        &index,
        &destination,
        identity(),
        &Selection::default(),
        || {
            calls += 1;
            if calls == 10 {
                return Err(std::io::Error::from(std::io::ErrorKind::Interrupted).into());
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!destination.exists());
    let manifest = export(&index, &destination, identity(), &Selection::default()).unwrap();
    assert!(export(&index, &destination, identity(), &Selection::default()).is_err());
    assert_eq!(verify(&destination).unwrap(), manifest);
    let upload = temp.path().join("cancelled-upload");
    assert!(
        export_session(&index, &source, records[0].session_id, &upload, || Err(
            std::io::Error::from(std::io::ErrorKind::Interrupted).into()
        ))
        .is_err()
    );
    assert!(!upload.exists());
}

fn rewrite_inventory(archive: &Path, inventory: &[SessionMember]) {
    let mut file = fs::File::create(archive.join("inventory.jsonl")).unwrap();
    for member in inventory {
        serde_json::to_writer(&mut file, member).unwrap();
        file.write_all(b"\n").unwrap();
    }
    drop(file);
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(archive.join("manifest.json")).unwrap()).unwrap();
    manifest.inventory_sha256 = io::hash_file(&archive.join("inventory.jsonl")).unwrap().0;
    fs::write(
        archive.join("manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn hostile_paths_unknown_payloads_and_bad_checksums_leave_destination_untouched() {
    let temp = tempdir().unwrap();
    let archive = archive(
        temp.path(),
        &[record(&source(), "session", 1, "Unicode 路径")],
    );
    let original = members(&archive).remove(0);
    let owned = temp.path().join("owned");
    restore(&archive, &owned, &options()).unwrap();
    let generation = VerifiedIndex::open_pinned(owned.join("search/lexical"))
        .unwrap()
        .generation_id()
        .to_owned();
    for path in [
        "../escape.jsonl",
        "/absolute.jsonl",
        "members/../../escape",
        "members\\..\\escape",
    ] {
        let mut malicious = original.clone();
        malicious.path = path.into();
        rewrite_inventory(&archive, &[malicious]);
        assert!(restore(&archive, &owned, &options()).is_err());
    }
    rewrite_inventory(&archive, std::slice::from_ref(&original));
    fs::write(archive.join("native-payload"), "must not be admitted").unwrap();
    assert!(verify(&archive).is_err());
    fs::remove_file(archive.join("native-payload")).unwrap();
    assert!(verify(&archive).is_ok());
    let file = archive.join(&original.path);
    let bytes = fs::read(&file).unwrap();
    let corrupted = String::from_utf8(bytes)
        .unwrap()
        .replace("Unicode", "Changed");
    fs::write(&file, corrupted).unwrap();
    assert!(restore(&archive, &owned, &options()).is_err());
    assert_eq!(
        VerifiedIndex::open_pinned(owned.join("search/lexical"))
            .unwrap()
            .generation_id(),
        generation
    );
}

#[test]
fn malformed_truncated_duplicate_and_unknown_core_fields_are_rejected() {
    let temp = tempdir().unwrap();
    let archive = archive(temp.path(), &[record(&source(), "session", 1, "ordinary")]);
    let original = members(&archive).remove(0);
    let valid = fs::read(archive.join(&original.path)).unwrap();
    let mut unknown: serde_json::Value = serde_json::from_slice(&valid).unwrap();
    unknown["source"]["native_plugin"] = serde_json::json!("untrusted");
    let mut unknown = serde_json::to_vec(&unknown).unwrap();
    unknown.push(b'\n');
    let cases = vec![
        b"{bad-json}\n".to_vec(),
        valid[..valid.len() - 1].to_vec(),
        [valid.clone(), valid.clone()].concat(),
        unknown,
    ];
    let staging = temp.path().join("staged");
    for bytes in cases {
        fs::write(&staging, &bytes).unwrap();
        let mut member = original.clone();
        (member.sha256, member.bytes) = io::hash_file(&staging).unwrap();
        assert!(verify_member(&staging, &member).is_err());
    }
    fs::write(&staging, &valid).unwrap();
    verify_member(&staging, &original).unwrap();
    let mut reader = std::io::Cursor::new(vec![b'x'; 1025]);
    let mut line = Vec::new();
    assert!(io::read_line(&mut reader, &mut line, 1024).is_err());
    assert_eq!(line.len(), 1025);
    let mut positive = std::io::Cursor::new([vec![b'x'; 1023], vec![b'\n']].concat());
    assert!(io::read_line(&mut positive, &mut line, 1024).unwrap());
}

#[cfg(unix)]
#[test]
fn symlinks_rejected_while_ordinary_readable_files_and_unicode_directories_work() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempdir().unwrap();
    let archive = archive(temp.path(), &[record(&source(), "session", 1, "ordinary")]);
    let member = members(&archive).remove(0);
    let path = archive.join(&member.path);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    verify(&archive).unwrap();
    let outside = temp.path().join("outside data");
    fs::rename(&path, &outside).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(verify(&archive).is_err());
    assert!(verify_member(&path, &member).is_err());
    fs::remove_file(&path).unwrap();
    fs::rename(&outside, &path).unwrap();
    let directory = archive.join("members");
    let elsewhere = temp.path().join("elsewhere");
    fs::rename(&directory, &elsewhere).unwrap();
    symlink(&elsewhere, &directory).unwrap();
    assert!(verify(&archive).is_err());
}

/// This crosses the legacy Custom catalog's 256 MiB total byte ceiling using
/// individually supported records. It is an explicit nightly resource check.
#[test]
#[ignore = "large retained-history qualification; run the Bazel large_history_tests target"]
fn history_larger_than_legacy_custom_catalog_streams_and_restores() {
    let temp = tempdir().unwrap();
    let source = source();
    let index_root = temp.path().join("index");
    let mut writer = GenerationWriter::open(&index_root, writer_options())
        .unwrap()
        .into_writer()
        .unwrap();
    writer.begin_source(source.clone()).unwrap();
    for sequence in 0..20 {
        writer
            .add_core_record(record(
                &source,
                "large session",
                sequence,
                &"z ".repeat(7 * 1024 * 1024),
            ))
            .unwrap();
    }
    let observation = SourceObservation::new(source, "synthetic-v1", vec![1]).unwrap();
    writer
        .certify_source(
            CertifiedSource::certify(
                observation.clone(),
                observation,
                "synthetic-parser-v1",
                [1; 32],
                ScannedSourceCounts {
                    complete_records: 20,
                    retained_records: 20,
                    indexed_documents: 20,
                    ..ScannedSourceCounts::default()
                },
            )
            .unwrap(),
        )
        .unwrap();
    writer.commit(|_| true).unwrap();
    let archive = temp.path().join("large");
    export(
        &VerifiedIndex::open_pinned(&index_root).unwrap(),
        &archive,
        identity(),
        &Selection::default(),
    )
    .unwrap();
    assert!(members(&archive)[0].bytes > 256 * 1024 * 1024);
    let restored = temp.path().join("restored");
    restore(&archive, &restored, &options()).unwrap();
    assert_eq!(
        VerifiedIndex::open_pinned(restored.join("search/lexical"))
            .unwrap()
            .document_count(),
        20
    );
}
